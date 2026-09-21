use super::*;
use bytes::Bytes;
use codex_client::TransportError;
use http::HeaderMap;
use http::StatusCode;
use pretty_assertions::assert_eq;
use std::sync::Mutex;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

struct DropFlag(Arc<AtomicUsize>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(/*val*/ 1, Ordering::SeqCst);
    }
}

type Completion = (usize, String, Option<TokenUsage>);

struct Telemetry {
    current: AtomicUsize,
    accepted: Arc<Mutex<Vec<usize>>>,
    completed: Arc<Mutex<Vec<Completion>>>,
}

impl SseTelemetry for Telemetry {
    fn response_created_callback(&self) -> Option<Arc<dyn Fn() + Send + Sync>> {
        let attempt = self.current.load(Ordering::SeqCst);
        let accepted = Arc::clone(&self.accepted);
        Some(Arc::new(move || accepted.lock().unwrap().push(attempt)))
    }

    fn response_completed_callback(&self) -> Option<crate::ResponseCompletedCallback> {
        let attempt = self.current.load(Ordering::SeqCst);
        let completed = Arc::clone(&self.completed);
        Some(Arc::new(move |id, usage| {
            completed
                .lock()
                .unwrap()
                .push((attempt, id.to_owned(), usage.cloned()));
        }))
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
    for (payload, expected) in [
        ("", vec![]),
        ("data: {\"type\":\"response.created\"}\n\n", vec![]),
        ("data: {invalid json}\n\n", vec![]),
        (
            "data: {\"type\":\"response.created\",\"response\":{\"id\":\"r\"}}\n\n",
            vec![7],
        ),
    ] {
        let accepted = Arc::new(Mutex::new(Vec::new()));
        let telemetry = Arc::new(Telemetry {
            current: AtomicUsize::new(7),
            accepted: Arc::clone(&accepted),
            completed: Default::default(),
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
        assert_eq!(*accepted.lock().unwrap(), expected);
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
        /*response_completed*/ None,
    )
    .await;
    assert_eq!(accepted.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn completion_is_attempt_bound_once_and_missing_usage_stays_unknown() {
    for response in [r#"{"id":"r"}"#, r#"{"id":"r","usage":null}"#] {
        let completed = Arc::new(Mutex::new(Vec::new()));
        let telemetry = Arc::new(Telemetry {
            current: AtomicUsize::new(/*v*/ 7),
            accepted: Default::default(),
            completed: Arc::clone(&completed),
        });
        let payload =
            format!("data: {{\"type\":\"response.completed\",\"response\":{response}}}\n\n");
        let stream = spawn_response_stream(
            StreamResponse {
                status: StatusCode::OK,
                headers: HeaderMap::new(),
                bytes: futures::stream::iter([Ok(Bytes::from(payload.repeat(2)))]).boxed(),
            },
            Duration::from_secs(/*secs*/ 1),
            Some(telemetry.clone()),
            /*turn_state*/ None,
        );
        telemetry.current.store(/*val*/ 8, Ordering::SeqCst);
        let events: Vec<_> = stream.collect().await;
        assert!(events.iter().all(Result::is_ok));
        assert_eq!(*completed.lock().unwrap(), vec![(7, "r".into(), None)]);
    }
}

#[tokio::test]
async fn decoded_usage_is_recorded_before_closed_consumer_delivery() {
    let completed = Arc::new(Mutex::new(None));
    let result = Arc::clone(&completed);
    let (tx, rx) = mpsc::channel(/*buffer*/ 1);
    drop(rx);
    process_sse_with_treatment(
        futures::stream::iter([Ok(Bytes::from_static(
            b"data: {\"type\":\"response.completed\",\"response\":{\"id\":\"r\",\"usage\":{\"input_tokens\":2,\"output_tokens\":3,\"total_tokens\":5}}}\n\n",
        ))]).boxed(),
        tx,
        Duration::from_secs(/*secs*/ 1),
        /*telemetry*/ None,
        SafetyBufferingTreatment::default(),
        /*response_created*/ None,
        Some(Arc::new(move |response_id, usage| {
            *result.lock().unwrap() = Some((response_id.to_owned(), usage.cloned()));
        })),
    ).await;
    assert_eq!(
        *completed.lock().unwrap(),
        Some((
            "r".into(),
            Some(TokenUsage {
                input_tokens: 2,
                output_tokens: 3,
                total_tokens: 5,
                ..Default::default()
            })
        ))
    );
}

#[tokio::test]
async fn closing_original_consumer_drops_an_idle_decoder_stream() {
    let dropped = Arc::new(AtomicUsize::new(/*v*/ 0));
    let original = DropFlag(Arc::clone(&dropped));
    let stream = futures::stream::once(async move {
        let _original = original;
        futures::future::pending::<Result<Bytes, TransportError>>().await
    })
    .boxed();
    let (tx, rx) = mpsc::channel(/*buffer*/ 1);
    drop(rx);
    timeout(
        Duration::from_secs(/*secs*/ 1),
        process_sse_with_treatment(
            stream,
            tx,
            Duration::from_secs(/*secs*/ 3600),
            /*telemetry*/ None,
            SafetyBufferingTreatment::default(),
            /*response_created*/ None,
            /*response_completed*/ None,
        ),
    )
    .await
    .expect("closed consumer must release the actual pending decoder");
    assert_eq!(dropped.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn closed_consumer_stops_malformed_ready_backlog_and_drops_stream_and_callback_lease() {
    let polls = Arc::new(AtomicUsize::new(/*v*/ 0));
    let stream_drops = Arc::new(AtomicUsize::new(/*v*/ 0));
    let lease_drops = Arc::new(AtomicUsize::new(/*v*/ 0));
    let original_stream = DropFlag(Arc::clone(&stream_drops));
    let poll_count = Arc::clone(&polls);
    // Finite even before the repair: a regression fails the poll assertion, not
    // the test executor. Timeout completion is never used as a drop receipt.
    let mut remaining = 32;
    let stream = futures::stream::poll_fn(move |_| {
        let _original_stream = &original_stream;
        poll_count.fetch_add(/*val*/ 1, Ordering::SeqCst);
        if remaining == 0 {
            return std::task::Poll::Ready(None);
        }
        remaining -= 1;
        std::task::Poll::Ready(Some(Ok(Bytes::from_static(b"data: malformed-json\n\n"))))
    })
    .boxed();
    // Model the same shared callback ownership as the original Core lease. Core's
    // separate lease test owns its actual counter/drain assertions.
    let created_lease = Arc::new(DropFlag(Arc::clone(&lease_drops)));
    let completed_lease = Arc::clone(&created_lease);
    let (tx, rx) = mpsc::channel(/*buffer*/ 1);
    drop(rx);
    process_sse_with_treatment(
        stream,
        tx,
        Duration::from_secs(/*secs*/ 3600),
        /*telemetry*/ None,
        SafetyBufferingTreatment::default(),
        Some(Arc::new(move || {
            let _lease = &created_lease;
            panic!("malformed input cannot mark acceptance");
        })),
        Some(Arc::new(move |_, _| {
            let _lease = &completed_lease;
            panic!("malformed input cannot mark usage");
        })),
    )
    .await;
    assert_eq!(
        (
            polls.load(Ordering::SeqCst),
            stream_drops.load(Ordering::SeqCst),
            lease_drops.load(Ordering::SeqCst),
        ),
        (1, 1, 1)
    );
}
