use super::*;
use pretty_assertions::assert_eq;

#[test]
fn floor_snapshot_preserves_uncertain_receipt_and_survives_budget_invalidation() {
    let (slot, _events, owner, _time) = admitted_slot();
    let original = intent(&slot, owner);
    let digest = original.operand_digest(owner);
    let guard = slot
        .prepare_wake(owner, original, Arc::new(OriginalPolicy))
        .unwrap();
    let receipt = slot
        .read_observation_wake(owner, /*sequence*/ 1, &digest)
        .unwrap();
    assert_eq!(
        slot.observation_wake_snapshot(owner).unwrap(),
        ObservationWakeSnapshot {
            intent_floor: 1,
            receipt: Some(receipt.clone()),
        }
    );
    slot.invalidate_observation_wake(owner, /*sequence*/ 2)
        .unwrap();
    slot.invalidate_budget().unwrap();
    assert_eq!(
        slot.observation_wake_snapshot(owner).unwrap(),
        ObservationWakeSnapshot {
            intent_floor: 2,
            receipt: Some(receipt),
        }
    );
    guard.finish(ObservationWakeOutcome::Suppressed).unwrap();
    slot.retire_observation_wake(owner, /*sequence*/ 1, &digest)
        .unwrap();
    assert_eq!(
        slot.observation_wake_snapshot(owner).unwrap(),
        ObservationWakeSnapshot {
            intent_floor: 2,
            receipt: None,
        }
    );
    slot.revoke().unwrap();
    assert_eq!(
        slot.observation_wake_snapshot(owner),
        Err(ObservationError::StaleOwner)
    );
}

#[test]
fn digest_matches_shared_uuid_big_endian_ascii_vector() {
    let owner = ObservationOwner {
        connection_id: 1,
        epoch: Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap(),
    };
    let intent = ObservationWakeIntent {
        sequence: 1,
        frame_revision: 2,
        budget_generation: 3,
        expected_commit_order: 4,
        frame_hash: "a".repeat(64),
    };
    let mut bytes = b"codex-observation-wake-v1\0\x00\x11\x22\x33\x44\x55\x66\x77\x88\x99\xaa\xbb\xcc\xdd\xee\xff".to_vec();
    bytes.extend_from_slice(
        b"\0\0\0\0\0\0\0\x01\0\0\0\0\0\0\0\x02\0\0\0\0\0\0\0\x03\0\0\0\0\0\0\0\x04",
    );
    bytes.extend_from_slice(&[b'a'; 64]);
    let expected = format!("{:x}", Sha256::digest(bytes));
    assert_eq!(intent.operand_digest(owner), expected);
    let changes: [fn(&mut ObservationWakeIntent); 5] = [
        |value| value.sequence += 1,
        |value| value.frame_revision += 1,
        |value| value.budget_generation += 1,
        |value| value.expected_commit_order += 1,
        |value| value.frame_hash = "b".repeat(64),
    ];
    for change in changes {
        let mut changed = intent.clone();
        change(&mut changed);
        assert_ne!(changed.operand_digest(owner), expected);
    }
}
