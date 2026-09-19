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
