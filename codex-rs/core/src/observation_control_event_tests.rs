use super::*;
use pretty_assertions::assert_eq;

#[test]
fn correlated_read_and_set_share_capture_and_terminal_order() {
    let (slot, mut events, owner) = ObservationSlot::new(/*connection_id*/ 1);
    let read_id = Uuid::new_v4();
    let set_id = Uuid::new_v4();
    let before = slot.state.lock().unwrap().metadata();
    slot.read_for_request(owner, read_id).unwrap();
    let capture = slot.capture("turn-a").unwrap();
    slot.set_for_request(owner, /*revision*/ 1, /*frame*/ None, set_id)
        .unwrap();
    let after = slot.state.lock().unwrap().metadata();
    slot.release(capture.decision_id).unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Control {
            request_id: read_id,
            metadata: before
        }
    );
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Captured(capture)
    );
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Control {
            request_id: set_id,
            metadata: after.clone()
        }
    );
    let ObservationEvent::Submitted(terminal) = events.try_recv().unwrap() else {
        panic!("terminal event")
    };
    assert!(terminal.terminal_decision);
    assert!(terminal.commit_order > after.commit_order);
    assert!(events.try_recv().is_err());
}

#[test]
fn precommit_failure_has_no_correlated_ack_or_revision_change() {
    let (slot, mut events, owner) = ObservationSlot::new(/*connection_id*/ 1);
    let before = slot.state.lock().unwrap().metadata();
    assert_eq!(
        slot.set_for_request(
            owner,
            /*revision*/ 0,
            /*frame*/ None,
            Uuid::new_v4()
        ),
        Err(ObservationError::RevisionMismatch)
    );
    assert_eq!(slot.state.lock().unwrap().metadata(), before);
    assert!(events.try_recv().is_err());
    while slot.read(owner).is_ok() {}
    assert_eq!(
        slot.set_for_request(
            owner,
            /*revision*/ 1,
            /*frame*/ None,
            Uuid::new_v4()
        ),
        Err(ObservationError::ResourceLimit)
    );
    assert_eq!(slot.state.lock().unwrap().metadata(), before);
    while let Ok(event) = events.try_recv() {
        assert_eq!(event, ObservationEvent::Read(before.clone()));
    }
}
