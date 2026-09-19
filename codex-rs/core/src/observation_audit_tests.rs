use super::*;
use pretty_assertions::assert_eq;

#[test]
fn full_fifo_stops_retry_but_cannot_consume_terminal_capacity() {
    let (slot, mut events, owner) = ObservationSlot::new(/*connection_id*/ 1);
    let capture = slot.capture("turn-a").unwrap();
    let attempt = slot.begin_attempt(capture.decision_id).unwrap();
    while slot.read(owner).is_ok() {}
    assert_eq!(
        slot.begin_attempt(capture.decision_id),
        Err(ObservationError::ResourceLimit)
    );
    let mut expected = slot
        .state
        .lock()
        .unwrap()
        .active_capture
        .as_ref()
        .unwrap()
        .record
        .clone();
    expected.commit_order = capture.metadata.commit_order + 1;
    expected.terminal_decision = true;
    slot.release(capture.decision_id).unwrap();
    // A callback from a retired attempt cannot revise an already queued outcome.
    slot.attempt_accepted(attempt).unwrap();
    let mut submitted = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let ObservationEvent::Submitted(record) = event {
            submitted.push(record);
        }
    }
    assert_eq!(submitted, vec![expected]);
    assert!(slot.state.lock().unwrap().active_capture.is_none());
}

#[test]
fn immutable_attempts_bound_header_ids_and_latch_acceptance() {
    let (slot, mut events, _) = ObservationSlot::new(/*connection_id*/ 1);
    let capture = slot.capture("turn-a").unwrap();
    let first = slot.begin_attempt(capture.decision_id).unwrap();
    slot.attempt_headers(first, Some("upstream-first")).unwrap();
    slot.attempt_accepted(first).unwrap();
    let mut expected_first = slot
        .state
        .lock()
        .unwrap()
        .active_capture
        .as_ref()
        .unwrap()
        .record
        .clone();
    expected_first.commit_order = 2;
    let second = slot.begin_attempt(capture.decision_id).unwrap();
    assert_ne!(first, second);
    slot.attempt_headers(second, Some(&"x".repeat(257)))
        .unwrap();
    slot.attempt_accepted(first).unwrap();
    slot.attempt_headers(first, Some("must-not-touch-second"))
        .unwrap();
    let mut expected_second = slot
        .state
        .lock()
        .unwrap()
        .active_capture
        .as_ref()
        .unwrap()
        .record
        .clone();
    assert_eq!(
        (
            expected_second.provider_request_id.clone(),
            expected_second.outcome
        ),
        (None, ObservationOutcome::Unknown)
    );
    assert_ne!(expected_first.request_id, expected_second.request_id);
    expected_second.commit_order = 3;
    expected_second.terminal_decision = true;
    slot.release(capture.decision_id).unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Captured(capture)
    );
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Submitted(expected_first)
    );
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Submitted(expected_second)
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn unsent_and_sequence_exhaustion_still_emit_one_rejected_terminal() {
    let (slot, mut events, owner) = ObservationSlot::new(/*connection_id*/ 1);
    slot.state.lock().unwrap().commit_order = MAX_SEQUENCE - 2;
    let capture = slot.capture("turn-a").unwrap();
    assert_eq!(
        slot.set(owner, /*revision*/ 1, /*frame*/ None),
        Err(ObservationError::ResourceLimit)
    );
    slot.release(capture.decision_id).unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Captured(capture.clone())
    );
    let ObservationEvent::Submitted(record) = events.try_recv().unwrap() else {
        panic!("terminal record")
    };
    assert_eq!(
        record,
        ObservationSubmitted {
            turn_id: capture.turn_id,
            decision_id: capture.decision_id,
            attempt_id: record.attempt_id,
            request_id: record.request_id,
            provider_request_id: None,
            metadata: capture.metadata,
            captured_at: capture.captured_at,
            commit_order: MAX_SEQUENCE,
            outcome: ObservationOutcome::Rejected,
            terminal_decision: true,
        }
    );
    assert!(events.try_recv().is_err());
}

#[test]
fn closed_receiver_or_revoked_owner_prevents_the_first_send() {
    let (slot, events, _) = ObservationSlot::new(/*connection_id*/ 1);
    let capture = slot.capture("turn-a").unwrap();
    drop(events);
    assert_eq!(
        slot.begin_attempt(capture.decision_id),
        Err(ObservationError::Unavailable)
    );
    slot.release(capture.decision_id).unwrap();
    assert!(slot.state.lock().unwrap().active_capture.is_none());

    let (slot, _events, _) = ObservationSlot::new(/*connection_id*/ 1);
    let capture = slot.capture("turn-a").unwrap();
    slot.revoke().unwrap();
    assert_eq!(
        slot.begin_attempt(capture.decision_id),
        Err(ObservationError::Unavailable)
    );
}

#[test]
fn admission_does_not_wait_for_a_busy_slot() {
    let (slot, _events, _) = ObservationSlot::new(/*connection_id*/ 1);
    let capture = slot.capture("turn-a").unwrap();
    let _held = slot.state.lock().unwrap();
    assert_eq!(
        slot.begin_attempt(capture.decision_id),
        Err(ObservationError::ResourceLimit)
    );
}
