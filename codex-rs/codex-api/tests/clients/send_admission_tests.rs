use super::*;
use codex_client::RequestTelemetry;
use pretty_assertions::assert_eq;

struct AdmissionState {
    remaining: usize,
    events: Vec<&'static str>,
}

struct Admission(Mutex<AdmissionState>);

impl RequestTelemetry for Admission {
    fn on_request_start(&self) -> std::result::Result<(), String> {
        let mut state = self.0.lock().unwrap();
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
        self.0.lock().unwrap().events.push("outcome");
    }
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
