use codex_core::IdleTurnAdmission;
use codex_core::ObservationBinding;
use codex_core::ObservationFrame;
use codex_core::ObservationProfile;
use codex_core::ObservationSlot;
use codex_core::ObservationWakeIntent;
use codex_core::ObservationWakeOutcome;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::EventMsg;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use sha2::Digest;
use sha2::Sha256;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

#[derive(Debug)]
struct DeniedPolicy;
impl IdleTurnAdmission for DeniedPolicy {
    fn reserve_if_allowed(&self, _reserve: &mut dyn FnMut()) -> bool {
        false
    }
}
#[derive(Debug)]
struct FixturePolicy;
impl IdleTurnAdmission for FixturePolicy {
    fn reserve_if_allowed(&self, reserve: &mut dyn FnMut()) -> bool {
        reserve();
        true
    }
}

#[tokio::test]
async fn unbound_session_does_not_send_observation_output_ceiling() -> anyhow::Result<()> {
    let server = wiremock::MockServer::start().await;
    let test = test_codex()
        .with_config(|config| {
            config.observation_max_output_tokens = std::num::NonZeroU64::new(128);
        })
        .build_with_auto_env(&server)
        .await?;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("unbound"),
            responses::ev_completed("unbound"),
        ]),
    )
    .await;
    test.submit_turn("hello").await?;
    assert_eq!(
        mock.single_request().body_json().get("max_output_tokens"),
        None
    );
    test.codex.shutdown_and_wait().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_native_wake_starts_once_without_manufacturing_user_input() -> anyhow::Result<()> {
    let server = wiremock::MockServer::start().await;
    let test = test_codex()
        .with_config(|config| {
            config.model = Some("gpt-oss-20b".into());
            config.observation_max_output_tokens = std::num::NonZeroU64::new(128);
            let mut model = model_info_from_slug("gpt-oss-20b");
            model.context_window = Some(131_072);
            model.effective_context_window_percent = 100;
            config.model_catalog = Some(ModelsResponse {
                models: vec![model],
            });
        })
        .build_with_auto_env(&server)
        .await?;
    let (slot, _events, owner) = ObservationSlot::new(/*connection_id*/ 1);
    let slot = Arc::new(slot);
    test.codex
        .install_budgeted_observation_binding(
            ObservationBinding {
                slot: Arc::clone(&slot),
                profile: ObservationProfile::HarmonyGptOss,
            },
            owner,
        )
        .await?;
    let text = "CURRENT_VIEW_ONLY";
    slot.set_for_request_at_budget(
        owner,
        /*revision*/ 1,
        Some(ObservationFrame {
            text: Arc::from(text),
            hash: format!("{:x}", Sha256::digest(text.as_bytes())),
            expires_at: i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?
                + 60,
        }),
        uuid::Uuid::new_v4(),
        /*budget_generation*/ 1,
    )?;
    let snapshot = slot.read(owner)?;
    let mut intent = ObservationWakeIntent {
        sequence: 1,
        frame_revision: snapshot.revision,
        frame_hash: snapshot.hash.unwrap(),
        budget_generation: 1,
        expected_commit_order: snapshot.commit_order,
    };
    // Real native policy denial, not a sampled JS canStart preflight.
    let denied = test
        .codex
        .start_observation_wake(owner, intent.clone(), Arc::new(DeniedPolicy))
        .await?;
    assert_eq!(denied.outcome, ObservationWakeOutcome::Suppressed);
    slot.retire_observation_wake(owner, denied.sequence, &denied.operand_digest)?;
    intent.sequence = 2;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("wake-response"),
            responses::ev_completed("wake-response"),
        ]),
    )
    .await;
    let started = test
        .codex
        .start_observation_wake(owner, intent.clone(), Arc::new(FixturePolicy))
        .await?;
    let ObservationWakeOutcome::Started { ref turn_id } = started.outcome else {
        panic!("actual native start required");
    };
    assert!(!turn_id.is_empty());
    assert_eq!(
        slot.read_observation_wake(owner, intent.sequence, &intent.operand_digest(owner))?,
        started
    );
    assert!(
        test.codex
            .start_observation_wake(owner, intent, Arc::new(FixturePolicy))
            .await
            .is_err()
    );
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    let request = mock.single_request();
    assert_eq!(
        request.body_json()["max_output_tokens"],
        serde_json::json!(128)
    );
    let user_text = request.message_input_texts("user");
    let overlays: Vec<_> = user_text
        .iter()
        .filter(|text| text.starts_with("<current_observations>"))
        .collect();
    assert_eq!(overlays.len(), 1);
    assert!(overlays[0].contains(text));
    test.codex.flush_rollout().await?;
    let history = test.codex.load_history(/*include_archived*/ false).await?;
    assert!(!history.items.iter().any(|item| matches!(
        item,
        codex_history::RolloutItem::EventMsg(EventMsg::UserMessage(_))
    )));
    assert!(!serde_json::to_string(&history.items)?.contains("CURRENT_VIEW_ONLY"));
    test.codex.shutdown_and_wait().await?;
    Ok(())
}
