use codex_core::ObservationBinding;
use codex_core::ObservationFrame;
use codex_core::ObservationProfile;
use codex_core::ObservationSlot;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::openai_models::ModelsResponse;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use sha2::Digest;
use sha2::Sha256;
use std::sync::Arc;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use wiremock::MockServer;

/// Exercises the foreground sampler, not only websocket prefix comparison.
/// This request-only group fixture qualifies no external model/provider profile.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn observation_clear_keeps_foreground_requests_full_context_http() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let test = test_codex()
        .with_config(|config| {
            config.model_provider.supports_websockets = true;
            config.model = Some("gpt-oss-20b".into());
            let mut model = model_info_from_slug("gpt-oss-20b");
            model.context_window = Some(131_072);
            model.effective_context_window_percent = 100;
            model.use_responses_lite = false;
            config.model_catalog = Some(ModelsResponse {
                models: vec![model],
            });
        })
        .build_with_auto_env(&server)
        .await?;
    let (slot, _events, owner) = ObservationSlot::new(/*connection_id*/ 1);
    let store = test.codex.thread_extension_data();
    let slot = Arc::new(slot);
    store.insert(ObservationBinding {
        slot: Arc::clone(&slot),
        profile: ObservationProfile::HarmonyGptOss,
    });
    let text = "FULL_GROUP_CANARY".repeat(2048);
    slot.set(
        owner,
        /*revision*/ 1,
        Some(ObservationFrame {
            text: Arc::from(text.as_str()),
            hash: format!("{:x}", Sha256::digest(text.as_bytes())),
            expires_at: SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs() as i64 + 60,
        }),
    )?;
    let first = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("one", "answer one"),
            responses::ev_completed("response-one"),
        ]),
    )
    .await;
    test.submit_text_turn("first prompt").await?;
    let first_request = first.single_request();
    assert!(
        first_request
            .body_json()
            .get("previous_response_id")
            .is_none()
    );

    assert!(
        first_request
            .message_input_texts("user")
            .iter()
            .filter(|text| text.starts_with("<current_observations>"))
            .count()
            > 1
    );
    slot.set(owner, /*revision*/ 2, /*frame*/ None)?;
    let second = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_assistant_message("two", "answer two"),
            responses::ev_completed("response-two"),
        ]),
    )
    .await;
    test.submit_text_turn("second prompt").await?;
    let second_request = second.single_request();
    assert!(
        second_request
            .body_json()
            .get("previous_response_id")
            .is_none()
    );
    assert_eq!(
        second_request
            .message_input_texts("user")
            .into_iter()
            .filter(|text| text == "first prompt" || text == "second prompt")
            .collect::<Vec<_>>(),
        vec!["first prompt", "second prompt"],
    );
    assert!(second_request.body_contains_text("answer one"));
    assert!(!second_request.body_contains_text("FULL_GROUP_CANARY"));
    assert!(!second_request.body_contains_text("<current_observations>"));
    Ok(())
}
