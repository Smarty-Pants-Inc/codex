use super::*;
use bytes::Bytes;
use codex_client::TransportError;
use http::HeaderMap;
use http::StatusCode;
use pretty_assertions::assert_eq;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

struct Telemetry {
    current: AtomicUsize,
    accepted: Arc<Mutex<Vec<usize>>>,
}

impl SseTelemetry for Telemetry {
    fn response_created_callback(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let attempt = self.current.load(Ordering::SeqCst);
        let accepted = Arc::clone(&self.accepted);
        Some(Arc::new(move || accepted.lock().unwrap().push(attempt)))
    }

    fn on_sse_poll(
        &self,
        _result: &Result<
            Option<
                Result<
                    eventsource_stream::Event,
                    eventsource_stream::EventStreamError<TransportError>,
                >,
            >,
            tokio::time::error::Elapsed,
        >,
        _duration: Duration,
    ) {
    }
}

#[tokio::test]
async fn created_is_attempt_bound_once_and_survives_later_stream_failure() {
    for payload in [
        "",
        "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\"}}\n\n",
    ] {
        let accepted = Arc::new(Mutex::new(Vec::new()));
        let telemetry = Arc::new(Telemetry {
            current: AtomicUsize::new(7),
            accepted: Arc::clone(&accepted),
        });
        let stream = spawn_response_stream(
            StreamResponse {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                bytes: futures::stream::iter([Ok(Bytes::from(payload.repeat(2)))]).boxed(),
            },
            Duration::from_secs(1),
            Some(telemetry.clone()),
            /*turn_state*/ None,
        );
        // The decoder must use the callback bound when this stream opened, not
        // consult a mutable next-attempt counter when its task later runs.
        telemetry.current.store(8, Ordering::SeqCst);
        let events: Vec<_> = stream.collect().await;
        assert!(events.last().unwrap().is_err()); // no response.completed
        assert_eq!(
            *accepted.lock().unwrap(),
            if payload.is_empty() { vec![] } else { vec![7] }
        );
    }
}

#[tokio::test]
async fn decoded_created_is_recorded_even_when_event_delivery_is_closed() {
    let accepted = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&accepted);
    let (tx, rx) = mpsc::channel(1);
    drop(rx);
    process_sse_with_treatment(
        futures::stream::iter([Ok(Bytes::from_static(
            b"data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\"}}\n\n",
        ))])
        .boxed(),
        tx,
        Duration::from_secs(1),
        /*telemetry*/ None,
        SafetyBufferingTreatment::default(),
        Some(Arc::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        })),
    )
    .await;
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}
