use super::*;
use bytes::Bytes;
use codex_client::RetryOn;
use http::Method;
use pretty_assertions::assert_eq;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);

impl RequestTelemetry for Recorder {
    fn on_response_headers(&self, headers: &HeaderMap) {
        self.0
            .lock()
            .unwrap()
            .push(headers["x-request-id"].to_str().unwrap().into());
    }

    fn on_request(
        &self,
        attempt: u64,
        status: Option<StatusCode>,
        _error: Option<&TransportError>,
        _duration: Duration,
    ) {
        self.0
            .lock()
            .unwrap()
            .push(format!("{attempt}:{}", status.unwrap().as_u16()));
    }
}

#[tokio::test]
async fn outcome_callbacks_cover_lower_retries_and_counter_restart() {
    let recorder = Arc::new(Recorder::default());
    let sends = Arc::new(AtomicUsize::new(0));
    // The wrapper reports preparation/send outcomes; the HTTP retry counter
    // resets between invocations. Post-auth admission is tested through the API.
    for _ in 0..2 {
        let sends = Arc::clone(&sends);
        run_with_request_telemetry(
            RetryPolicy {
                max_attempts: 1,
                base_delay: Duration::ZERO,
                retry_on: RetryOn {
                    retry_429: true,
                    retry_5xx: false,
                    retry_transport: false,
                },
            },
            Some(recorder.clone()),
            || Request::new(Method::POST, "http://unused.invalid/responses".into()),
            move |_| {
                let n = sends.fetch_add(1, Ordering::SeqCst);
                async move {
                    let headers = HeaderMap::from_iter([(
                        "x-request-id".parse().unwrap(),
                        format!("wire-{n}").parse().unwrap(),
                    )]);
                    if n.is_multiple_of(/*rhs*/ 2) {
                        Err(TransportError::Http {
                            status: StatusCode::TOO_MANY_REQUESTS,
                            headers: Some(headers),
                            url: None,
                            body: None,
                            retry_after: None,
                        })
                    } else {
                        Ok(Response {
                            status: StatusCode::OK,
                            headers,
                            body: Bytes::new(),
                        })
                    }
                }
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(
        *recorder.0.lock().unwrap(),
        vec![
            "wire-0", "0:429", "wire-1", "1:200", "wire-2", "0:429", "wire-3", "1:200",
        ]
    );
}
