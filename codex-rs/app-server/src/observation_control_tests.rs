use super::*;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::RequestId;
use codex_core::ObservationEvent;
use codex_core::ObservationFrame;
use codex_core::ObservationOwner;
use codex_core::ObservationSlot;
use pretty_assertions::assert_eq;
use serde_json::json;

// The production bridge uses the budgeted mutation path. These slot tests
// isolate the same pre-commit error mapping without becoming a success ACK path.
fn set(
    slot: &ObservationSlot,
    owner: ObservationOwner,
    revision: u64,
    frame: Option<ObservationFrame>,
) -> Result<(), JSONRPCErrorError> {
    slot.set(owner, revision, frame)
        .map(|_| ())
        .map_err(store_error)
}

#[test]
fn revision_rejection_has_exact_wire_envelope_and_does_not_publish() {
    let (slot, mut events, owner) = ObservationSlot::new(/*connection_id*/ 7);
    set(&slot, owner, /*revision*/ 1, /*frame*/ None).unwrap();
    let ObservationEvent::Published(published) = events.try_recv().unwrap() else {
        panic!("missing publication")
    };
    let response = JSONRPCError {
        id: RequestId::String("set-2".into()),
        error: set(&slot, owner, /*revision*/ 1, /*frame*/ None).unwrap_err(),
    };
    assert_eq!(
        serde_json::to_value(response).unwrap(),
        json!({
            "id": "set-2",
            "error": {
                "code": -32002,
                "message": "observation control request rejected",
                "data": {"type": "threadObservationRejected", "protocol": 2, "code": "REVISION_MISMATCH"}
            }
        })
    );
    assert!(events.try_recv().is_err());
    assert_eq!(slot.read(owner).unwrap(), published);
}

#[test]
fn backpressure_rejection_does_not_claim_a_commit_or_leak_metadata() {
    let (slot, mut events, owner) = ObservationSlot::new(/*connection_id*/ 7);
    for revision in 1..=32 {
        set(&slot, owner, revision, /*frame*/ None).unwrap();
    }
    let error = set(&slot, owner, /*revision*/ 33, /*frame*/ None).unwrap_err();
    assert_eq!(
        error,
        rejected(ThreadObservationRejectionCode::ResourceLimit)
    );
    assert_eq!(
        error.data,
        Some(json!({"type": "threadObservationRejected", "protocol": 2, "code": "RESOURCE_LIMIT"}))
    );
    let mut last = None;
    while let Ok(event) = events.try_recv() {
        if let ObservationEvent::Published(metadata) = event {
            last = Some(metadata);
        }
    }
    assert_eq!(Some(slot.read(owner).unwrap()), last);
}
