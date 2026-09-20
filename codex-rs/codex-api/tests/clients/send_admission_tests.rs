use super::*;
use codex_api::ModelsClient;
use codex_client::RequestTelemetry;
use codex_client::ReqwestTransport;
use codex_http_client::HttpClientBuilder;
use pretty_assertions::assert_eq;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;
use wiremock::matchers::method;
use wiremock::matchers::path;

struct AdmissionState {
    remaining: usize,
    events: Vec<&'static str>,
}

struct Admission(Mutex<AdmissionState>);

impl RequestTelemetry for Admission {
    fn on_request_start(&self) -> std::result::Result<(), String> {
        let mut state = self
            .0
            .lock()
            .expect("admission state should not be poisoned");
        if state.remaining == 0 {
            state.events.push("blocked");
            return Err("audit capacity unavailable".into());
        }
        state.remaining -= 1;
        state.events.push("admitted");
        Ok(())
    }

    fn on_request(
        &self,
        _attempt: u64,
        _status: Option<StatusCode>,
        _error: Option<&TransportError>,
        _duration: Duration,
    ) {
        self.0
            .lock()
            .expect("admission state should not be poisoned")
            .events
            .push("outcome");
    }
}

#[tokio::test]
async fn execute_admission_can_stop_initial_send_or_lower_retry() -> Result<()> {
    for (capacity, expected) in [
        (0, vec!["outcome", "blocked", "outcome"]),
        (
            1,
            vec!["outcome", "admitted", "outcome", "blocked", "outcome"],
        ),
        (
            2,
            vec!["outcome", "admitted", "outcome", "admitted", "outcome"],
        ),
    ] {
        let server = MockServer::start().await;
        let sends = AtomicUsize::new(0);
        Mock::given(method("GET"))
            .and(path("/models"))
            .respond_with(move |_: &wiremock::Request| {
                if sends.fetch_add(1, Ordering::SeqCst) == 0 {
                    ResponseTemplate::new(503)
                } else {
                    ResponseTemplate::new(200).set_body_json(serde_json::json!({"models": []}))
                }
            })
            .mount(&server)
            .await;
        let admission = Arc::new(Admission(Mutex::new(AdmissionState {
            remaining: capacity,
            events: Vec::new(),
        })));
        // Auth fails before admission; the first actual execute receives a retryable 503.
        // Repeated Build rejections would add events and auth attempts to these exact counts.
        let auth = Arc::new(FailsOnceAuth::transient());
        let transport =
            ReqwestTransport::from_http_client(HttpClientBuilder::new().build_direct()?);
        let mut provider = provider("openai");
        provider.base_url = server.uri();
        provider.retry.max_attempts = 4;
        provider.retry.retry_5xx = true;
        let request_url = ModelsClient::<ReqwestTransport>::request_url(&provider, "0.1.0");
        let client = ModelsClient::new(transport, provider, auth.clone())
            .with_telemetry(Some(admission.clone()));
        let result = client.list_models(request_url, HeaderMap::new()).await;
        if capacity < 2 {
            assert!(
                matches!(result, Err(ApiError::Transport(TransportError::Build(ref message)))
                if message == "audit capacity unavailable")
            );
        } else {
            assert_eq!(result?, (vec![], None));
        }
        let requests = server
            .received_requests()
            .await
            .expect("request recording enabled");
        let state = admission
            .0
            .lock()
            .expect("admission state should not be poisoned");
        assert_eq!((&state.events, state.remaining), (&expected, 0));
        assert_eq!(requests.len(), capacity);
        assert_eq!(auth.attempts(), if capacity == 0 { 2 } else { 3 });
    }
    Ok(())
}

#[tokio::test]
async fn post_auth_admission_can_stop_initial_send_or_lower_retry() -> Result<()> {
    for (capacity, expected) in [
        (0, vec!["outcome", "blocked", "outcome"]),
        (
            1,
            vec!["outcome", "admitted", "outcome", "blocked", "outcome"],
        ),
        (
            2,
            vec!["outcome", "admitted", "outcome", "admitted", "outcome"],
        ),
    ] {
        let admission = Arc::new(Admission(Mutex::new(AdmissionState {
            remaining: capacity,
            events: Vec::new(),
        })));
        // First auth attempt fails before transport; first actual send fails.
        // Neither auth retry nor transport retry may bypass the admission hook.
        let auth = Arc::new(FailsOnceAuth::transient());
        let transport = FlakyTransport::new();
        let mut provider = provider("openai");
        provider.retry.max_attempts = 4;
        let client = ResponsesClient::new(transport.clone(), provider, auth.clone())
            .with_telemetry(Some(admission.clone()), /*sse*/ None);
        let result = client
            .stream(
                serde_json::json!({"input": []}),
                HeaderMap::new(),
                Compression::None,
                /*turn_state*/ None,
            )
            .await;
        if capacity < 2 {
            assert!(
                matches!(result, Err(ApiError::Transport(TransportError::Build(ref message)))
                if message == "audit capacity unavailable")
            );
        } else {
            assert!(result.is_ok());
        }
        let state = admission.0.lock().unwrap();
        assert_eq!((&state.events, state.remaining), (&expected, 0));
        assert_eq!(transport.attempts(), capacity as i64);
        assert_eq!(auth.attempts(), if capacity == 0 { 2 } else { 3 });
    }
    Ok(())
}
