use super::*;
use codex_core::NativePilotIssuer;
use codex_core::ObservationBinding;
use codex_core::ObservationProfile;
use codex_core::PilotAuthorityError;
use codex_core::PilotGrantClaims;
use codex_core::PilotRequestAuthentication;
use codex_core::PilotRequestReservation;
use codex_http_client::Request;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::openai_models::ModelsResponse;
use core_test_support::test_codex::test_codex;
use pretty_assertions::assert_eq;
use std::num::NonZeroU64;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use wiremock::MockServer;

struct FixtureIssuer {
    claims: PilotGrantClaims,
    revoked: AtomicBool,
}
impl NativePilotIssuer for FixtureIssuer {
    fn verify_grant(&self, envelope: &[u8]) -> Result<PilotGrantClaims, PilotAuthorityError> {
        if envelope != b"fixture-only" {
            return Err(PilotAuthorityError::Denied);
        }
        Ok(self.claims.clone())
    }
    fn recheck_grant(&self, _: &PilotGrantClaims) -> Result<(), PilotAuthorityError> {
        if self.revoked.load(Ordering::SeqCst) {
            Err(PilotAuthorityError::Expired)
        } else {
            Ok(())
        }
    }
    fn revoke(&self) {
        self.revoked.store(true, Ordering::SeqCst);
    }
    fn authenticate_request(
        &self,
        _: &PilotGrantClaims,
        _: &mut Request,
    ) -> Result<PilotRequestAuthentication, PilotAuthorityError> {
        Err(PilotAuthorityError::Unavailable)
    }
    fn qualify_request(
        &self,
        _: &PilotGrantClaims,
        _: &Request,
    ) -> Result<PilotRequestReservation, PilotAuthorityError> {
        Err(PilotAuthorityError::Unavailable)
    }
}

#[tokio::test]
async fn original_control_enforces_rights_and_retains_one_retirement_after_waiter_cancel()
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
    let (slot, _events, owner) = ObservationSlot::new(/*connection_id*/ 7);
    let slot = Arc::new(slot);
    test.codex
        .install_observation_binding(ObservationBinding {
            slot: Arc::clone(&slot),
            profile: ObservationProfile::HarmonyGptOss,
        })
        .await?;
    let issuer = Arc::new(FixtureIssuer {
        claims: PilotGrantClaims {
            grant_id: Uuid::now_v7(),
            issuer_generation: 1,
            owner,
            thread_id: test.session_configured.session_id.into(),
            scope: "fixture-source".into(),
            permissions: [PilotPermission::PrepareSource].into(),
            expires_at: chrono::Utc::now().timestamp() + 60,
            cooldown: Duration::from_secs(/*secs*/ 1),
            max_turns: 1,
            max_attempts: 1,
            max_reserved_tokens: NonZeroU64::new(/*n*/ 100).unwrap(),
        },
        revoked: AtomicBool::new(false),
    });
    test.codex
        .install_pilot_authority(owner, b"fixture-only", issuer.clone())
        .await?;
    let control = PilotControl::new(
        Arc::clone(&test.codex),
        Arc::clone(&slot),
        owner,
        "fixture-source".into(),
        "fixture-instruction".into(),
        "fixture-allocation".into(),
        "a".repeat(64),
    )
    .expect("original fixture binding");
    control
        .check(PilotSourceOperation::PrepareSource)
        .expect("received source right");
    assert!(control.check(PilotSourceOperation::ActOnSource).is_err());
    assert_eq!(
        control
            .start("bounded automatic opportunity".into())
            .await
            .expect("native refusal"),
        ThreadPilotStartResponse {
            started: false,
            turn_id: None
        }
    );
    assert!(control.start("x".repeat(1025)).await.is_err());
    control
        .begin_retirement()
        .expect("original retirement task");
    assert!(issuer.revoked.load(Ordering::SeqCst)); // Before any retirement await.
    assert!(control.check(PilotSourceOperation::PrepareSource).is_err());
    let cancelled = Arc::clone(&control);
    let waiter = tokio::spawn(async move { cancelled.retire().await });
    waiter.abort();
    let _cancelled = waiter.await;
    let (first, second) = tokio::join!(control.retire(), control.retire());
    let first = first.expect("first original join");
    assert_eq!(first, second.expect("same original join"));
    assert_eq!(
        first,
        control.retire().await.expect("retained original report")
    );
    assert_eq!(
        first,
        ThreadPilotRetireResponse {
            identity: control.binding.identity.clone(),
            revoked: true,
            active_decision: None,
            admitted_turns: Default::default(),
            reserved_tokens: 0,
            attempts: Vec::new(),
        }
    );
    assert!(server.received_requests().await.unwrap().is_empty());
    Ok(())
}
