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

#[derive(Clone, Copy)]
enum PreparedDecision {
    Allow,
    Reject,
}

const METADATA_SENTINEL: &str = "private-request-metadata-5c83490e";

#[derive(Clone, Debug, PartialEq, Eq)]
struct RequestMetadata(&'static str);

struct MutatingAuth;

impl AuthProvider for MutatingAuth {
    fn add_auth_headers(&self, headers: &mut HeaderMap) {
        StaticAuth::new("fixture-token", "fixture-account").add_auth_headers(headers);
    }

    fn apply_auth(&self, mut request: Request) -> codex_api::AuthProviderFuture<'_> {
        self.add_auth_headers(&mut request.headers);
        request
            .extensions
            .insert(RequestMetadata(METADATA_SENTINEL));
        request
            .headers
            .insert("x-auth-returned", HeaderValue::from_static("mutated"));
        request.url.push_str(if request.url.contains('?') {
            "&auth=returned"
        } else {
            "?auth=returned"
        });
        Box::pin(async move { Ok(request) })
    }
}

struct PreparedAdmission {
    expected_method: http::Method,
    expected_url: String,
    expected_body: Option<Vec<u8>>,
    calls: AtomicUsize,
    decision: PreparedDecision,
}

impl RequestTelemetry for PreparedAdmission {
    fn on_request_prepared(&self, request: &Request) -> std::result::Result<(), String> {
        let body = request.prepare_body_for_send()?;
        assert_eq!(
            request.extensions.get::<RequestMetadata>(),
            Some(&RequestMetadata(METADATA_SENTINEL))
        );
        assert_eq!(
            (
                &request.method,
                &request.url,
                request.headers.get(http::header::AUTHORIZATION),
                request.headers.get("ChatGPT-Account-ID"),
                request.headers.get("x-admission-fixture"),
                request.headers.get("x-auth-returned"),
                request.compression,
                body.body.as_deref(),
            ),
            (
                &self.expected_method,
                &self.expected_url,
                Some(&HeaderValue::from_static("Bearer fixture-token")),
                Some(&HeaderValue::from_static("fixture-account")),
                Some(&HeaderValue::from_static("present")),
                Some(&HeaderValue::from_static("mutated")),
                codex_client::RequestCompression::None,
                self.expected_body.as_deref(),
            )
        );
        self.calls.fetch_add(/*val*/ 1, Ordering::SeqCst);
        match self.decision {
            PreparedDecision::Allow => Ok(()),
            PreparedDecision::Reject => Err("prepared request rejected".into()),
        }
    }

    fn on_request_start(&self) -> std::result::Result<(), String> {
        panic!("an override must not also invoke the compatibility hook");
    }

    fn on_request(
        &self,
        _attempt: u64,
        _status: Option<StatusCode>,
        _error: Option<&TransportError>,
        _duration: Duration,
    ) {
    }
}

#[tokio::test]
async fn prepared_admission_sees_post_auth_request_and_refuses_both_transports() -> Result<()> {
    for (method, decision) in [
        (http::Method::GET, PreparedDecision::Allow),
        (http::Method::GET, PreparedDecision::Reject),
        (http::Method::POST, PreparedDecision::Allow),
        (http::Method::POST, PreparedDecision::Reject),
    ] {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::any())
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"models": []})),
            )
            .mount(&server)
            .await;
        let transport =
            ReqwestTransport::from_http_client(HttpClientBuilder::new().build_direct()?);
        let mut provider = provider("openai");
        provider.base_url = server.uri();
        provider.retry.max_attempts = 4;
        provider.retry.retry_5xx = true;
        let body = serde_json::json!({"input": "exact \"fixture\" body\nλ"});
        let url = if method == http::Method::GET {
            ModelsClient::<ReqwestTransport>::request_url(&provider, "0.1.0")
        } else {
            format!("{}/responses", server.uri())
        };
        let expected_url = format!(
            "{url}{}auth=returned",
            if url.contains('?') { "&" } else { "?" }
        );
        let admission = Arc::new(PreparedAdmission {
            expected_method: method.clone(),
            expected_url,
            expected_body: if method == http::Method::POST {
                Some(serde_json::to_vec(&body)?)
            } else {
                None
            },
            calls: AtomicUsize::new(/*v*/ 0),
            decision,
        });
        let auth = Arc::new(MutatingAuth);
        let headers = HeaderMap::from_iter([(
            http::header::HeaderName::from_static("x-admission-fixture"),
            HeaderValue::from_static("present"),
        )]);
        let result = if method == http::Method::GET {
            ModelsClient::new(transport, provider, auth)
                .with_telemetry(Some(admission.clone()))
                .list_models(url, headers)
                .await
                .map(|_| ())
        } else {
            ResponsesClient::new(transport, provider, auth)
                .with_telemetry(Some(admission.clone()), /*sse*/ None)
                .stream(body, headers, Compression::None, /*turn_state*/ None)
                .await
                .map(|_| ())
        };
        let requests = server.received_requests().await.unwrap();
        match decision {
            PreparedDecision::Reject => {
                assert!(
                    matches!(result, Err(ApiError::Transport(TransportError::Build(ref text))) if text == "prepared request rejected")
                );
                assert!(requests.is_empty());
            }
            PreparedDecision::Allow => {
                result?;
                assert_eq!(requests.len(), 1);
                let sent = &requests[0];
                assert!(!sent.url.as_str().contains(METADATA_SENTINEL));
                for (name, value) in &sent.headers {
                    assert!(!name.as_str().contains(METADATA_SENTINEL));
                    assert!(
                        !value
                            .as_bytes()
                            .windows(METADATA_SENTINEL.len())
                            .any(|bytes| bytes == METADATA_SENTINEL.as_bytes())
                    );
                }
                assert!(
                    !sent
                        .body
                        .windows(METADATA_SENTINEL.len())
                        .any(|bytes| bytes == METADATA_SENTINEL.as_bytes())
                );
                let expected_url = url::Url::parse(&admission.expected_url)?;
                // Wiremock reconstructs origin-form request targets with a localhost
                // base. The actual destination authority is in the Host header.
                assert_eq!(
                    (
                        &sent.method,
                        sent.url.path(),
                        sent.url.query(),
                        sent.headers.get(http::header::HOST),
                        sent.body.as_slice(),
                    ),
                    (
                        &admission.expected_method,
                        expected_url.path(),
                        expected_url.query(),
                        Some(&HeaderValue::from_str(&server.address().to_string())?),
                        admission.expected_body.as_deref().unwrap_or_default(),
                    )
                );
                for (name, expected) in [
                    ("authorization", "Bearer fixture-token"),
                    ("chatgpt-account-id", "fixture-account"),
                    ("x-admission-fixture", "present"),
                    ("x-auth-returned", "mutated"),
                ] {
                    assert_eq!(
                        sent.headers.get(name),
                        Some(&HeaderValue::from_static(expected))
                    );
                }
            }
        }
        assert_eq!(admission.calls.load(Ordering::SeqCst), 1);
    }
    Ok(())
}

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
