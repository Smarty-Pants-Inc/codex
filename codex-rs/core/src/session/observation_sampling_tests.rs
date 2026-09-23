use super::*;
use crate::ObservationEvent;
use crate::ObservationFrame;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::models::ContentItem;
use pretty_assertions::assert_eq;
use sha2::Digest;
use sha2::Sha256;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

fn frame(text: &str) -> ObservationFrame {
    ObservationFrame {
        text: Arc::from(text),
        hash: format!("{:x}", Sha256::digest(text.as_bytes())),
        expires_at: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
            + 60,
    }
}

fn model() -> ModelInfo {
    let mut model = model_info_from_slug("gpt-oss-20b");
    model.context_window = Some(131_072);
    model.effective_context_window_percent = 100;
    model.use_responses_lite = false;
    model
}

#[test]
fn unchanged_retry_retains_a_and_changed_canonical_input_captures_b() {
    let (slot, mut events, owner) = ObservationSlot::new(/*connection_id*/ 1);
    let slot = Arc::new(slot);
    slot.set(owner, /*revision*/ 1, Some(frame("view A")))
        .unwrap();
    let mut sampling = ObservationSampling::new(
        Arc::clone(&slot),
        ObservationProfile::HarmonyGptOss,
        "turn-a".into(),
    );
    let a = sampling.prepare(vec![], &model()).unwrap();
    let a_id = sampling.active.as_ref().unwrap().capture.decision_id;
    let mut shrunken = model();
    shrunken.context_window = Some(crate::observation::RESERVED_TOKENS);
    assert_eq!(
        sampling.prepare(vec![], &shrunken),
        Err(ObservationError::Unavailable)
    );
    slot.set(owner, /*revision*/ 2, Some(frame("view B")))
        .unwrap();
    assert_eq!(sampling.prepare(vec![], &model()).unwrap(), a);
    assert_eq!(sampling.active.as_ref().unwrap().capture.decision_id, a_id);
    let input = vec![ResponseItem::Message {
        id: None,
        role: "user".into(),
        content: vec![ContentItem::InputText {
            text: "new input".into(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }];
    let b = sampling.prepare(input.clone(), &model()).unwrap();
    assert_ne!(a, b);
    assert_eq!(sampling.active.as_ref().unwrap().canonical_input, input);
    assert_ne!(sampling.active.as_ref().unwrap().capture.decision_id, a_id);
    let mut captured = vec![];
    while let Ok(event) = events.try_recv() {
        if let ObservationEvent::Captured(capture) = event {
            captured.push(capture.text);
        }
    }
    assert_eq!(
        captured,
        vec![Some(Arc::from("view A")), Some(Arc::from("view B"))]
    );
    drop(sampling);
    assert!(slot.capture("turn-b").is_ok());
}

#[test]
fn rejected_profile_does_not_commit_or_publish_a_capture() {
    let (slot, mut events, owner) = ObservationSlot::new(/*connection_id*/ 1);
    let slot = Arc::new(slot);
    let published = slot
        .set(owner, /*revision*/ 1, Some(frame(&"&".repeat(4096))))
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Published(published.clone())
    );
    let mut sampling = ObservationSampling::new(
        Arc::clone(&slot),
        ObservationProfile::HarmonyGptOss,
        "turn-a".into(),
    );
    let mut unsupported = model();
    unsupported.context_window = Some(crate::observation::RESERVED_TOKENS);
    assert_eq!(
        sampling.prepare(vec![], &unsupported),
        Err(ObservationError::Unavailable)
    );
    assert!(events.try_recv().is_err());
    assert_eq!(slot.read(owner).unwrap(), published);
    assert!(sampling.active.is_none());
}

#[test]
fn receipt_requires_complete_group_and_cancellation_releases_one_unknown_outcome() {
    let (slot, mut events, owner) = ObservationSlot::new(/*connection_id*/ 1);
    let slot = Arc::new(slot);
    slot.set(
        owner,
        /*revision*/ 1,
        Some(frame(&"group-data".repeat(2048))),
    )
    .unwrap();
    let mut sampling = ObservationSampling::new(
        Arc::clone(&slot),
        ObservationProfile::HarmonyGptOss,
        "group-turn".into(),
    );
    let (items, decision) = sampling.prepare(vec![], &model()).unwrap();
    assert!(items.len() > 1);
    let body = serde_json::json!({"input": items, "stream": true, "store": false});
    let original = body["input"].as_array().unwrap();
    let mut missing = original.clone();
    missing.pop();
    let mut duplicate = original.clone();
    duplicate.push(original[0].clone());
    let mut reordered = original.clone();
    reordered.swap(/*a*/ 0, /*b*/ 1);
    let mut mixed = original.clone();
    mixed[0]["content"][0]["text"] = serde_json::Value::String(
        mixed[0]["content"][0]["text"]
            .as_str()
            .unwrap()
            .replace(&decision.to_string(), &uuid::Uuid::new_v4().to_string()),
    );
    let mut wrong_role = original.clone();
    wrong_role[0]["role"] = "developer".into();
    let mut annotated = original.clone();
    annotated[0]["internal_chat_message_metadata_passthrough"] =
        serde_json::json!({"turn_id": "foreign"});
    for input in [
        vec![],
        missing,
        duplicate,
        reordered,
        mixed,
        wrong_role,
        annotated,
    ] {
        let request =
            codex_client::Request::new(http::Method::POST, "http://fixture/responses".into())
                .with_json(&serde_json::json!({"input": input, "stream": true, "store": false}));
        assert_eq!(
            slot.begin_attempt_for_request(decision, &request),
            Err(ObservationError::InvalidFrame)
        );
    }
    assert_eq!(
        slot.begin_attempt(decision),
        Err(ObservationError::InvalidFrame)
    );
    let mut incremental = body.clone();
    incremental["previous_response_id"] = "old-response".into();
    let request = codex_client::Request::new(http::Method::POST, "http://fixture/responses".into())
        .with_json(&incremental);
    assert_eq!(
        slot.begin_attempt_for_request(decision, &request),
        Err(ObservationError::InvalidFrame)
    );
    let mut request =
        codex_client::Request::new(http::Method::POST, "http://fixture/responses".into())
            .with_json(&body);
    request.compression = codex_client::RequestCompression::Zstd;
    assert_eq!(
        slot.begin_attempt_for_request(decision, &request),
        Err(ObservationError::InvalidFrame)
    );
    request.compression = codex_client::RequestCompression::None;
    let request = request.into_prepared().unwrap();
    slot.begin_attempt_for_request(decision, &request).unwrap();
    drop(sampling);
    let submitted = std::iter::from_fn(|| events.try_recv().ok())
        .filter_map(|event| {
            if let ObservationEvent::Submitted(record) = event {
                Some(record)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    assert_eq!(submitted.len(), 1);
    assert_eq!(
        (
            submitted[0].decision_id,
            submitted[0].outcome,
            submitted[0].terminal_decision
        ),
        (decision, crate::ObservationOutcome::Unknown, true)
    );
    assert!(slot.capture("after-cancellation").is_ok());
}
