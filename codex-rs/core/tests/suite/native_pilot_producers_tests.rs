use codex_core::NativePilotIssuer;
use codex_core::ObservationBinding;
use codex_core::ObservationProfile;
use codex_core::ObservationSlot;
use codex_core::PilotAuthorityError;
use codex_core::PilotGrantClaims;
use codex_core::PilotPermission;
use codex_core::PilotRequestReservation;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::TokenUsage;
use codex_protocol::turn_input::NotSubmittedReason;
use codex_protocol::turn_input::StartIfIdleSubmission;
use codex_protocol::turn_input::TurnInput;
use codex_protocol::turn_input::TurnInputRequest;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;
use wiremock::MockServer;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn host_startup_selection_fences_both_session_construction_routes() -> anyhow::Result<()> {
    let server = MockServer::start().await;
    let test = test_codex()
        .with_config(|config| {
            config.model = Some("gpt-oss-20b".into());
            config.model_catalog = Some(ModelsResponse {
                models: vec![model_info_from_slug("gpt-oss-20b")],
            });
        })
        .build_with_auto_env(&server)
        .await?;
    test.codex.shutdown_and_wait().await?;
    let mut config = test.config.clone();
    config.model = None;
    let mut init = codex_extension_api::ExtensionDataInit::new();
    init.insert(codex_core::ProviderStartupPolicy::NativePilot);
    let started = test
        .thread_manager
        .start_thread(codex_core::StartThreadOptions {
            thread_extension_init: init.clone(),
            ..codex_core::StartThreadOptions::new(config.clone())
        })
        .await;
    assert!(
        matches!(started, Err(codex_protocol::error::CodexErr::InvalidRequest(ref message))
        if message == "native pilot requires an explicit model")
    );
    // The actual resume entry must pass the attachment before constructing the
    // session, not restore it from history. This fixture proves that ordering,
    // not persisted-history or observation-authority restoration.
    let resumed = test
        .thread_manager
        .resume_thread_with_history_and_init(
            config.clone(),
            codex_history::InitialHistory::Resumed(codex_history::ResumedHistory {
                conversation_id: test.session_configured.session_id,
                history: Arc::new(vec![]),
                rollout_path: None,
            }),
            test.thread_manager.auth_manager(),
            /*parent_trace*/ None,
            Default::default(),
            init,
        )
        .await;
    assert!(
        matches!(resumed, Err(codex_protocol::error::CodexErr::InvalidRequest(ref message))
        if message == "native pilot requires an explicit model")
    );
    let ordinary = test
        .thread_manager
        .start_thread(codex_core::StartThreadOptions::new(config))
        .await?;
    ordinary.thread.shutdown_and_wait().await?;
    Ok(())
}

// Controlled loopback seam only: these UUIDs qualify no actual credential or model context.
struct FixtureIssuer {
    claims: PilotGrantClaims,
    reservation: PilotRequestReservation,
}
impl NativePilotIssuer for FixtureIssuer {
    fn authenticate_request(
        &self,
        _: &PilotGrantClaims,
        request: &mut codex_client::Request,
    ) -> Result<codex_client::RequestAuthentication, PilotAuthorityError> {
        let mut header = http::HeaderValue::from_static("Bearer native-fixture-only");
        header.set_sensitive(true);
        request.headers.insert(http::header::AUTHORIZATION, header);
        Ok(codex_client::RequestAuthentication::Prepared)
    }

    fn verify_grant(&self, envelope: &[u8]) -> Result<PilotGrantClaims, PilotAuthorityError> {
        if envelope != b"fixture-only" {
            return Err(PilotAuthorityError::Denied);
        }
        Ok(self.claims.clone())
    }
    // This loopback fixture has no external mutable issuer state.
    fn revoke(&self) {}

    fn recheck_grant(&self, _: &PilotGrantClaims) -> Result<(), PilotAuthorityError> {
        Ok(())
    }
    fn qualify_request(
        &self,
        _: &PilotGrantClaims,
        request: &codex_client::Request,
    ) -> Result<PilotRequestReservation, PilotAuthorityError> {
        if request.method != http::Method::POST
            || !request.url.ends_with("/responses")
            || request.body.is_none()
            || request.headers.get(http::header::AUTHORIZATION)
                != Some(&http::HeaderValue::from_static(
                    "Bearer native-fixture-only",
                ))
        {
            return Err(PilotAuthorityError::Unavailable);
        }
        Ok(self.reservation.clone())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn original_thread_admission_send_usage_and_shutdown_share_native_custody()
-> anyhow::Result<()> {
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
    let (slot, mut events, owner) = ObservationSlot::new(/*connection_id*/ 7);
    let slot = Arc::new(slot);
    test.codex
        .install_observation_binding(ObservationBinding {
            slot: Arc::clone(&slot),
            profile: ObservationProfile::HarmonyGptOss,
        })
        .await?;
    let input = TurnInputRequest::new(TurnInput::ResponseItem(responses::user_message_item(
        "controlled native opportunity",
    )));
    let nonce = Uuid::new_v4();
    let denied = StartIfIdleSubmission::NotSubmitted {
        reason: NotSubmittedReason::NotIdle,
    };
    assert_eq!(
        test.codex
            .start_pilot_turn(owner, "fixture-scope".into(), nonce, input.clone())
            .await?,
        denied
    );
    let issuer = Arc::new(FixtureIssuer {
        claims: PilotGrantClaims {
            grant_id: Uuid::new_v4(),
            issuer_generation: 1,
            owner,
            thread_id: test.session_configured.session_id,
            scope: "fixture-scope".into(),
            permissions: [PilotPermission::AutomaticTurn].into(),
            expires_at: chrono::Utc::now().timestamp() + 3600,
            cooldown: Duration::from_secs(/*secs*/ 3600),
            max_turns: 2,
            max_attempts: 2,
            max_reserved_tokens: NonZeroU64::new(/*n*/ 2000).unwrap(),
        },
        reservation: PilotRequestReservation {
            token_ceiling: NonZeroU64::new(/*n*/ 1000).unwrap(),
            credential_receipt: Uuid::new_v4(),
            context_receipt: Uuid::new_v4(),
        },
    });
    test.codex
        .install_pilot_authority(owner, b"fixture-only", issuer.clone())
        .await?;
    let before = codex_core::PilotReport {
        owner,
        thread_id: test.session_configured.session_id,
        grant_id: issuer.claims.grant_id,
        revoked: false,
        active_decision: None,
        admitted_turns: std::collections::BTreeMap::new(),
        reserved_tokens: 0,
        attempts: Vec::new(),
    };
    assert_eq!(slot.pilot_report(owner)?, before);
    assert_eq!(
        test.codex
            .start_pilot_turn(owner, "wrong-scope".into(), nonce, input.clone())
            .await?,
        denied
    );
    assert_eq!(slot.pilot_report(owner)?, before);
    let mock = responses::mount_sse_once(
        &server,
        responses::sse(vec![
            responses::ev_response_created("response-one"),
            responses::ev_completed("response-one"),
        ]),
    )
    .await;
    let StartIfIdleSubmission::Started { turn_id } = test
        .codex
        .start_pilot_turn(owner, "fixture-scope".into(), nonce, input.clone())
        .await?
    else {
        anyhow::bail!("expected native admission");
    };
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 10), async {
        while test.codex.has_active_turn().await {
            tokio::task::yield_now().await;
        }
    })
    .await?;
    let report = slot.pilot_report(owner)?;
    assert_eq!(report.attempts.len(), 1);
    let mut expected = before;
    expected.admitted_turns.insert(turn_id.clone(), nonce);
    expected.reserved_tokens = 1000;
    let mut attempt = report.attempts[0].clone();
    attempt.turn_id = turn_id;
    attempt.admission_request_id = Some(nonce);
    attempt.reservation = issuer.reservation.clone();
    attempt.response_id = Some("response-one".into());
    attempt.response_complete = true;
    attempt.usage = Some(TokenUsage::default());
    attempt.usage_conflict = false;
    expected.attempts.push(attempt);
    assert_eq!(report, expected);
    assert_eq!(
        test.codex
            .start_pilot_turn(owner, "fixture-scope".into(), Uuid::new_v4(), input)
            .await?,
        denied
    );
    assert_eq!(slot.pilot_report(owner)?, expected);
    assert_eq!(
        mock.single_request().header("authorization"),
        Some("Bearer native-fixture-only".into())
    );
    let mut submitted = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let codex_core::ObservationEvent::Submitted(record) = event {
            submitted.push((
                record.decision_id,
                record.attempt_id,
                record.request_id,
                record.terminal_decision,
            ));
        }
    }
    let record = &expected.attempts[0];
    assert_eq!(
        submitted,
        vec![(
            record.decision_id,
            record.attempt_id,
            record.request_id,
            true
        )]
    );
    let retired = test.codex.retire_pilot(owner).await?;
    expected.revoked = true;
    assert_eq!(retired.report, expected);
    assert_eq!(test.codex.retire_pilot(owner).await?, retired);
    Ok(())
}
