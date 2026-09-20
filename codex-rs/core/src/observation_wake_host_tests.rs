use super::*;
use crate::ObservationHostPreparation;
use pretty_assertions::assert_eq;

#[test]
fn host_provenance_fences_cached_read_retirement_and_cross_owner() {
    let (slot, _events, owner, _time) = admitted_slot();
    let intent = intent(&slot, owner);
    let digest = intent.operand_digest(owner);
    let prep =
        ObservationHostPreparation::from_wire(/*cooldown_revision*/ 1, "3ff8000000000000").unwrap();
    let guard = slot
        .prepare_host_wake(
            owner,
            intent.clone(),
            Arc::new(OriginalPolicy),
            prep.clone(),
        )
        .unwrap();
    let receipt = guard.finish(ObservationWakeOutcome::Suppressed).unwrap();
    let expected = ObservationWakeSnapshot {
        intent_floor: 1,
        receipt: Some(receipt),
    };
    assert_eq!(
        slot.host_observation_wake_snapshot(owner, /*sequence*/ 1, &digest, &prep)
            .unwrap(),
        expected
    );
    for changed in [
        ObservationHostPreparation::from_wire(/*cooldown_revision*/ 2, "3ff8000000000000").unwrap(),
        ObservationHostPreparation::from_wire(/*cooldown_revision*/ 1, "0000000000000001").unwrap(),
    ] {
        assert_eq!(
            slot.host_observation_wake_snapshot(owner, /*sequence*/ 1, &digest, &changed),
            Err(ObservationError::RevisionMismatch)
        );
        assert_eq!(
            slot.retire_host_observation_wake(owner, /*sequence*/ 1, &digest, changed.clone()),
            Err(ObservationError::RevisionMismatch)
        );
        assert_eq!(
            slot.prepare_host_wake(owner, intent.clone(), Arc::new(OriginalPolicy), changed)
                .unwrap_err(),
            ObservationError::RevisionMismatch
        );
        assert_eq!(
            slot.host_observation_wake_snapshot(owner, /*sequence*/ 1, &digest, &prep)
                .unwrap(),
            expected
        );
    }
    let foreign = ObservationOwner {
        epoch: Uuid::new_v4(),
        ..owner
    };
    assert_eq!(
        slot.host_observation_wake_snapshot(foreign, /*sequence*/ 1, &digest, &prep),
        Err(ObservationError::StaleOwner)
    );
    assert_eq!(
        slot.retire_host_observation_wake(foreign, /*sequence*/ 1, &digest, prep.clone()),
        Err(ObservationError::StaleOwner)
    );
    assert_eq!(
        slot.retire_observation_wake(owner, /*sequence*/ 1, &digest),
        Err(ObservationError::RevisionMismatch)
    );
    assert_eq!(
        slot.retire_host_observation_wake(owner, /*sequence*/ 1, &digest, prep.clone())
            .unwrap(),
        1
    );
    assert_eq!(
        slot.host_observation_wake_snapshot(owner, /*sequence*/ 1, &digest, &prep)
            .unwrap(),
        ObservationWakeSnapshot {
            intent_floor: 1,
            receipt: None
        }
    );
    assert_eq!(
        slot.prepare_host_wake(owner, intent, Arc::new(OriginalPolicy), prep)
            .unwrap_err(),
        ObservationError::RevisionMismatch
    );
}

#[test]
fn host_read_cannot_adopt_an_embedding_receipt() {
    let (slot, _events, owner, _time) = admitted_slot();
    let intent = intent(&slot, owner);
    let digest = intent.operand_digest(owner);
    let guard = slot
        .prepare_wake(owner, intent, Arc::new(OriginalPolicy))
        .unwrap();
    let receipt = guard.finish(ObservationWakeOutcome::Suppressed).unwrap();
    let prep =
        ObservationHostPreparation::from_wire(/*cooldown_revision*/ 1, "3ff8000000000000").unwrap();
    assert_eq!(
        slot.host_observation_wake_snapshot(owner, /*sequence*/ 1, &digest, &prep),
        Err(ObservationError::RevisionMismatch)
    );
    assert_eq!(
        slot.read_observation_wake(owner, /*sequence*/ 1, &digest)
            .unwrap(),
        receipt
    );
    assert_eq!(
        slot.retire_observation_wake(owner, /*sequence*/ 1, &digest)
            .unwrap(),
        1
    );
}
