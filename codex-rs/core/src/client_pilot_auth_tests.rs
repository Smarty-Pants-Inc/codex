use super::*;
use crate::NativePilotIssuer;
use crate::ObservationSlot;
use crate::PilotAuthorityError;
use crate::PilotGrantClaims;
use crate::PilotPermission;
use crate::PilotRequestReservation;
use codex_client::Request;
use codex_client::RequestAuthentication;
use codex_model_provider::ProviderAuthScope;
use codex_model_provider::ResolvedProviderAuth;
use std::num::NonZeroU64;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use uuid::Uuid;

#[derive(Debug)]
struct FailingAmbientProvider {
    inner: SharedModelProvider,
    info: ModelProviderInfo,
    calls: Arc<[AtomicUsize; 5]>,
}

impl ModelProvider for FailingAmbientProvider {
    fn info(&self) -> &ModelProviderInfo {
        &self.info
    }

    fn auth_manager(&self) -> Option<Arc<AuthManager>> {
        self.calls[0].fetch_add(/*val*/ 1, Ordering::SeqCst);
        None
    }

    fn auth(&self) -> ModelProviderFuture<'_, Option<CodexAuth>> {
        self.calls[1].fetch_add(/*val*/ 1, Ordering::SeqCst);
        Box::pin(async { None })
    }

    fn api_provider(&self) -> ModelProviderFuture<'_, Result<codex_api::Provider, CodexErr>> {
        self.calls[2].fetch_add(/*val*/ 1, Ordering::SeqCst);
        Box::pin(async { Err(CodexErr::InvalidRequest("ambient setup forbidden".into())) })
    }

    fn api_auth_for_scope(
        &self,
        _: ProviderAuthScope,
    ) -> ModelProviderFuture<'_, Result<ResolvedProviderAuth, CodexErr>> {
        self.calls[3].fetch_add(/*val*/ 1, Ordering::SeqCst);
        Box::pin(async { Err(CodexErr::InvalidRequest("ambient auth forbidden".into())) })
    }

    fn recover_from_unauthorized(
        &self,
    ) -> ModelProviderFuture<'_, Result<ProviderUnauthorizedRecovery, CodexErr>> {
        self.calls[4].fetch_add(/*val*/ 1, Ordering::SeqCst);
        Box::pin(async {
            Err(CodexErr::InvalidRequest(
                "ambient recovery forbidden".into(),
            ))
        })
    }

    fn account_state(&self) -> ProviderAccountResult {
        self.inner.account_state()
    }

    fn models_manager(
        &self,
        codex_home: PathBuf,
        config_model_catalog: Option<ModelsResponse>,
    ) -> SharedModelsManager {
        self.inner.models_manager(codex_home, config_model_catalog)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NativeCase {
    Prepared,
    Unavailable,
    Expired,
    PostAuthRefusal,
    UnexpectedProvider,
    Unauthorized,
}

// Synthetic issuer only: exercises the real Core client, not production credential
// custody/count qualification. No fixture receipt is a live grant.
struct ClientFixtureIssuer {
    claims: PilotGrantClaims,
    mode: Mutex<NativeCase>,
    attachments: AtomicUsize,
}

impl NativePilotIssuer for ClientFixtureIssuer {
    fn verify_grant(&self, envelope: &[u8]) -> Result<PilotGrantClaims, PilotAuthorityError> {
        assert_eq!(envelope, b"client-fixture-only");
        Ok(self.claims.clone())
    }

    fn recheck_grant(&self, _: &PilotGrantClaims) -> Result<(), PilotAuthorityError> {
        if *self.mode.lock().unwrap() == NativeCase::Expired {
            return Err(PilotAuthorityError::Expired);
        }
        Ok(())
    }

    fn revoke(&self) {
        *self.mode.lock().unwrap() = NativeCase::Expired;
    }

    fn authenticate_request(
        &self,
        _: &PilotGrantClaims,
        request: &mut Request,
    ) -> Result<RequestAuthentication, PilotAuthorityError> {
        self.attachments.fetch_add(/*val*/ 1, Ordering::SeqCst);
        match *self.mode.lock().unwrap() {
            NativeCase::Unavailable => return Err(PilotAuthorityError::Unavailable),
            NativeCase::Expired => return Err(PilotAuthorityError::Expired),
            NativeCase::UnexpectedProvider => return Ok(RequestAuthentication::Provider),
            NativeCase::Prepared | NativeCase::PostAuthRefusal | NativeCase::Unauthorized => {}
        }
        assert!(!request.headers.contains_key(http::header::AUTHORIZATION));
        request
            .prepare_body_for_send()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        let mut header = http::HeaderValue::from_static("Bearer native-fixture-only");
        header.set_sensitive(true);
        request.headers.insert(http::header::AUTHORIZATION, header);
        Ok(RequestAuthentication::Prepared)
    }

    fn qualify_request(
        &self,
        _: &PilotGrantClaims,
        request: &Request,
    ) -> Result<PilotRequestReservation, PilotAuthorityError> {
        assert_eq!(
            request.headers.get(http::header::AUTHORIZATION),
            Some(&http::HeaderValue::from_static(
                "Bearer native-fixture-only"
            ))
        );
        if *self.mode.lock().unwrap() == NativeCase::PostAuthRefusal {
            return Err(PilotAuthorityError::Unavailable);
        }
        Ok(PilotRequestReservation {
            token_ceiling: NonZeroU64::new(/*n*/ 100).unwrap(),
            credential_receipt: Uuid::new_v4(),
            context_receipt: Uuid::new_v4(),
        })
    }
}

#[tokio::test]
async fn original_core_client_never_resolves_ambient_auth_for_installed_pilot() -> anyhow::Result<()>
{
    for mode in [
        NativeCase::Prepared,
        NativeCase::Unavailable,
        NativeCase::Expired,
        NativeCase::PostAuthRefusal,
        NativeCase::UnexpectedProvider,
        NativeCase::Unauthorized,
    ] {
        let server = MockServer::start().await;
        let response = if mode == NativeCase::Unauthorized {
            ResponseTemplate::new(/*status*/ 401)
        } else {
            ResponseTemplate::new(/*status*/ 200)
                .insert_header("content-type", "text/event-stream")
                .set_body_string(concat!(
                    "data: {\"type\":\"response.created\",\"response\":{\"id\":\"native\"}}\n\n",
                    "data: {\"type\":\"response.completed\",\"response\":{\"id\":\"native\",\"output\":[]}}\n\n"
                ))
        };
        Mock::given(method("POST"))
            .and(path("/v1/responses"))
            .respond_with(response)
            .mount(&server)
            .await;
        let calls = Arc::new(std::array::from_fn(|_| AtomicUsize::new(/*v*/ 0)));
        let (mut client, attestation_calls) =
            model_client_with_counting_attestation(/*include_attestation*/ true);
        client = client.with_provider_startup_policy(crate::ProviderStartupPolicy::NativePilot);
        let mut info =
            create_oss_provider_with_base_url(&format!("{}/v1", server.uri()), WireApi::Responses);
        info.http_headers = Some(std::collections::HashMap::from([(
            "Authorization".into(),
            "Bearer forbidden-ambient-fixture".to_string().into(),
        )]));
        Arc::get_mut(&mut client.state).unwrap().provider = Arc::new(FailingAmbientProvider {
            inner: test_model_provider(),
            info,
            calls: Arc::clone(&calls),
        });
        let (slot, _events, owner) = ObservationSlot::new(/*connection_id*/ 7);
        let slot = Arc::new(slot);
        let issuer = Arc::new(ClientFixtureIssuer {
            claims: PilotGrantClaims {
                grant_id: Uuid::new_v4(),
                issuer_generation: 1,
                owner,
                thread_id: client.state.thread_id,
                scope: "client-fixture".into(),
                permissions: [PilotPermission::ForegroundTurn].into(),
                expires_at: i64::try_from(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())?
                    + 60,
                cooldown: Duration::from_secs(/*secs*/ 1),
                max_turns: 1,
                max_attempts: 1,
                max_reserved_tokens: NonZeroU64::new(/*n*/ 100).unwrap(),
            },
            mode: Mutex::new(NativeCase::Prepared),
            attachments: AtomicUsize::new(/*v*/ 0),
        });
        slot.install_pilot_authority(
            owner,
            client.state.thread_id,
            b"client-fixture-only",
            issuer.clone(),
        )?;
        *issuer.mode.lock().unwrap() = mode;
        let capture = slot.capture("turn")?;
        let metadata = test_responses_metadata_for_client(
            &client,
            Some("turn"),
            "window".into(),
            /*parent_thread_id*/ None,
            TestCodexResponsesRequestKind::Turn,
        );
        let mut session = client.new_session();
        session.enable_observation_full_context();
        session.observation_decision = Some((Arc::clone(&slot), capture.decision_id));
        let result = session
            .stream(
                &Prompt::default(),
                &test_model_info(),
                &test_session_telemetry(),
                /*effort*/ None,
                codex_protocol::config_types::ReasoningSummary::None,
                /*service_tier*/ None,
                &metadata,
                &InferenceTraceContext::disabled(),
            )
            .await;
        if mode == NativeCase::Prepared {
            let mut stream = result?;
            let mut completed = false;
            while let Some(event) = stream.next().await {
                completed |= matches!(event?, ResponseEvent::Completed { .. });
            }
            assert!(completed);
        } else {
            assert!(result.is_err(), "{mode:?}");
        }
        assert_eq!(
            calls.each_ref().map(|value| value.load(Ordering::SeqCst)),
            [0; 5]
        );
        assert_eq!(attestation_calls.load(Ordering::SeqCst), 0);
        assert_eq!(
            issuer.attachments.load(Ordering::SeqCst),
            usize::from(mode != NativeCase::Expired)
        );
        let requests = server.received_requests().await.unwrap();
        assert_eq!(
            requests.len(),
            usize::from(matches!(
                mode,
                NativeCase::Prepared | NativeCase::Unauthorized
            ))
        );
        for request in requests {
            assert_eq!(
                request.headers.get("authorization").unwrap(),
                "Bearer native-fixture-only"
            );
        }
    }
    Ok(())
}

#[test]
fn startup_selection_precedes_even_the_ambient_metadata_reader() {
    let calls = AtomicUsize::new(/*v*/ 0);
    crate::ProviderStartupPolicy::NativePilot.read_auth_env_metadata(|| {
        calls.fetch_add(/*val*/ 1, Ordering::SeqCst);
        panic!("pilot constructor must not inspect credential environment values");
    });
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    crate::ProviderStartupPolicy::Ordinary.read_auth_env_metadata(|| {
        calls.fetch_add(/*val*/ 1, Ordering::SeqCst);
        Default::default()
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn selected_pilot_startup_refuses_prewarm_and_unbound_send_before_ambient_resolution() {
    for policy in [
        crate::ProviderStartupPolicy::Ordinary,
        crate::ProviderStartupPolicy::NativePilot,
    ] {
        let calls = Arc::new(std::array::from_fn(|_| AtomicUsize::new(/*v*/ 0)));
        let mut client = test_model_client(SessionSource::Cli).with_provider_startup_policy(policy);
        let mut info =
            create_oss_provider_with_base_url("http://unused.invalid/v1", WireApi::Responses);
        info.supports_websockets = true;
        Arc::get_mut(&mut client.state).unwrap().provider = Arc::new(FailingAmbientProvider {
            inner: test_model_provider(),
            info,
            calls: Arc::clone(&calls),
        });
        // This is the same entry used by startup's scheduled auth prewarm, before
        // any observation slot or credential authority exists.
        assert!(client.prewarm_auth().await.is_err());
        let metadata = test_responses_metadata_for_client(
            &client,
            /*turn_id*/ None,
            "window".into(),
            /*parent_thread_id*/ None,
            TestCodexResponsesRequestKind::Turn,
        );
        let result = client
            .new_session()
            .stream(
                &Prompt::default(),
                &test_model_info(),
                &test_session_telemetry(),
                /*effort*/ None,
                codex_protocol::config_types::ReasoningSummary::None,
                /*service_tier*/ None,
                &metadata,
                &InferenceTraceContext::disabled(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(
            calls.each_ref().map(|value| value.load(Ordering::SeqCst)),
            match policy {
                crate::ProviderStartupPolicy::Ordinary => [1, 2, 2, 0, 0],
                crate::ProviderStartupPolicy::NativePilot => [0; 5],
            }
        );
    }
}

#[tokio::test]
async fn nonpilot_core_client_keeps_ordinary_provider_setup() {
    let calls = Arc::new(std::array::from_fn(|_| AtomicUsize::new(/*v*/ 0)));
    let mut client = test_model_client(SessionSource::Cli);
    Arc::get_mut(&mut client.state).unwrap().provider = Arc::new(FailingAmbientProvider {
        inner: test_model_provider(),
        info: create_oss_provider_with_base_url("http://unused.invalid/v1", WireApi::Responses),
        calls: Arc::clone(&calls),
    });
    let metadata = test_responses_metadata_for_client(
        &client,
        /*turn_id*/ None,
        "window".into(),
        /*parent_thread_id*/ None,
        TestCodexResponsesRequestKind::Turn,
    );
    let result = client
        .new_session()
        .stream(
            &Prompt::default(),
            &test_model_info(),
            &test_session_telemetry(),
            /*effort*/ None,
            codex_protocol::config_types::ReasoningSummary::None,
            /*service_tier*/ None,
            &metadata,
            &InferenceTraceContext::disabled(),
        )
        .await;
    assert!(result.is_err());
    assert_eq!(
        calls.each_ref().map(|value| value.load(Ordering::SeqCst)),
        [1, 1, 1, 0, 0]
    );
}
