use super::*;
use pretty_assertions::assert_eq;

#[test]
fn capture_is_ordered_before_later_publication_and_immutable_until_release() {
    let (slot, mut events, owner, _time) = setup();
    let first = slot
        .set(owner, /*revision*/ 1, frame("A", /*expires_at*/ 160))
        .unwrap();
    let capture = slot.capture("turn-a").unwrap();
    let second = slot
        .set(owner, /*revision*/ 2, frame("B", /*expires_at*/ 160))
        .unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Published(first)
    );
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Captured(capture.clone())
    );
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Published(second.clone())
    );
    let retained_retry = capture.clone();
    assert_eq!(retained_retry, capture);
    assert_eq!(retained_retry.text.as_deref(), Some("A"));
    assert_eq!(slot.capture("turn-b"), Err(ObservationError::ResourceLimit));
    assert_eq!(
        slot.release(Uuid::new_v4()),
        Err(ObservationError::RevisionMismatch)
    );
    slot.release(capture.decision_id).unwrap();
    let next = slot.capture("turn-b").unwrap();
    let mut expected = second;
    // The preceding decision's terminal ledger record commits before recapture.
    expected.commit_order += 2;
    assert_eq!(next.metadata, expected);
    assert_eq!(next.text.as_deref(), Some("B"));
    assert_ne!(next.decision_id, capture.decision_id);
}

#[test]
fn delayed_capture_samples_expiry_inside_the_same_lock() {
    let (slot, _events, owner, time) = setup();
    let initial = slot
        .set(
            owner,
            /*revision*/ 1,
            frame("old success", /*expires_at*/ 160),
        )
        .unwrap();
    let held = slot.state.lock().unwrap();
    let ready = Barrier::new(/*n*/ 2);
    let captured = std::thread::scope(|scope| {
        let capture = scope.spawn(|| {
            ready.wait();
            slot.capture("turn-a")
        });
        ready.wait();
        let now = *time.lock().unwrap();
        *time.lock().unwrap() = later(now, /*seconds*/ 60);
        drop(held);
        capture.join().unwrap().unwrap()
    });
    let marker = "Current observations unavailable.";
    assert_eq!(
        captured,
        ObservationCapture {
            turn_id: "turn-a".into(),
            decision_id: captured.decision_id,
            captured_at: 160,
            text: Some(Arc::from(marker)),
            metadata: ObservationMetadata {
                commit_order: 2,
                status: ObservationStatus::Expired,
                hash: Some(format!("{:x}", Sha256::digest(marker.as_bytes()))),
                ..initial
            },
        }
    );
}

#[test]
fn clear_revoke_and_full_fifo_never_capture_an_old_body() {
    let (slot, mut events, owner, _time) = setup();
    let cleared = slot.capture("turn-a").unwrap();
    assert_eq!(cleared.metadata.status, ObservationStatus::Cleared);
    assert_eq!(cleared.text, None);
    assert_eq!(cleared.metadata.hash, None);
    slot.release(cleared.decision_id).unwrap();
    slot.set(owner, /*revision*/ 1, frame("A", /*expires_at*/ 160))
        .unwrap();
    while slot.read(owner).is_ok() {}
    assert_eq!(slot.capture("turn-b"), Err(ObservationError::ResourceLimit));
    while events.try_recv().is_ok() {}
    slot.revoke().unwrap();
    let revoked = slot.capture("turn-b").unwrap();
    assert_eq!(revoked.metadata.status, ObservationStatus::Unavailable);
    assert_eq!(
        revoked.text.as_deref(),
        Some("Current observations unavailable.")
    );
}
