use super::*;
use crate::IdleTurnAdmission;
use crate::ObservationProfile;
use codex_protocol::ThreadId;

#[derive(Debug)]
struct OriginalPolicy;
impl IdleTurnAdmission for OriginalPolicy {
    fn reserve_if_allowed(&self, reserve: &mut dyn FnMut()) -> bool {
        reserve();
        true
    }
}

fn admitted_slot() -> (
    Arc<ObservationSlot>,
    mpsc::Receiver<ObservationEvent>,
    ObservationOwner,
) {
    let (slot, events, owner, _) = setup();
    let mut model = codex_models_manager::model_info::model_info_from_slug("gpt-oss-20b");
    model.context_window = Some(131_072);
    model.effective_context_window_percent = 100;
    slot.initialize_budget(owner, &model, ObservationProfile::HarmonyGptOss)
        .unwrap();
    slot.set_for_request_at_budget(
        owner,
        /*revision*/ 1,
        frame("A", /*expires_at*/ 160),
        Uuid::new_v4(),
        /*budget_generation*/ 1,
    )
    .unwrap();
    (Arc::new(slot), events, owner)
}

fn intent(slot: &ObservationSlot, owner: ObservationOwner) -> ObservationWakeIntent {
    let metadata = slot.read(owner).unwrap();
    ObservationWakeIntent {
        sequence: 1,
        frame_revision: metadata.revision,
        frame_hash: metadata.hash.unwrap(),
        budget_generation: metadata.frame_budget_generation.unwrap(),
        expected_commit_order: metadata.commit_order,
    }
}

#[test]
fn accepted_unfinalized_capture_and_later_finalization_both_fence_wake() {
    let (slot, mut events, owner) = admitted_slot();
    let capture = slot.capture("turn").unwrap();
    let attempt = slot.begin_attempt(capture.decision_id).unwrap();
    slot.attempt_accepted(attempt).unwrap();
    let original = intent(&slot, owner);
    let guard = slot
        .prepare_wake(owner, original.clone(), Arc::new(OriginalPolicy))
        .unwrap();
    let thread = ThreadId::new();
    assert!(
        !guard.reserve_turn_if_allowed(&thread, "next", &mut || panic!("active capture admitted"))
    );
    assert_eq!(
        slot.read(owner).unwrap().commit_order,
        original.expected_commit_order
    );
    slot.release(capture.decision_id).unwrap();
    assert!(
        !guard.reserve_turn_if_allowed(&thread, "next", &mut || panic!("stale audit cut admitted"))
    );
    let mut accepted = false;
    while let Ok(event) = events.try_recv() {
        match event {
            ObservationEvent::Submitted(record) => {
                assert_eq!(record.outcome, ObservationOutcome::Accepted);
                accepted = true;
            }
            _ => {}
        }
    }
    // Obtain the actual post-release read through the original FIFO, not a fake
    // consumer exposure receipt or an invented accepted watermark.
    let current = slot.read(owner).unwrap();
    assert_eq!(current.revision, original.frame_revision);
    assert_eq!(current.hash, Some(original.frame_hash));
    assert!(current.commit_order > original.expected_commit_order);
    assert!(accepted);
    assert_eq!(events.try_recv().unwrap(), ObservationEvent::Read(current));
}

#[test]
fn invalidation_before_reservation_suppresses_but_after_keeps_real_result() {
    for invalidate_first in [true, false] {
        let (slot, _events, owner) = admitted_slot();
        let original = intent(&slot, owner);
        let guard = slot
            .prepare_wake(owner, original.clone(), Arc::new(OriginalPolicy))
            .unwrap();
        if invalidate_first {
            slot.invalidate_observation_wake(owner, /*sequence*/ 2)
                .unwrap();
        }
        let mut reserved = false;
        assert_eq!(
            guard.reserve_turn_if_allowed(&ThreadId::new(), "actual-turn", &mut || reserved = true),
            !invalidate_first
        );
        assert_eq!(reserved, !invalidate_first);
        if !invalidate_first {
            slot.invalidate_observation_wake(owner, /*sequence*/ 2)
                .unwrap();
        }
        let outcome = if invalidate_first {
            ObservationWakeOutcome::Suppressed
        } else {
            ObservationWakeOutcome::Started {
                turn_id: "actual-turn".into(),
            }
        };
        assert_eq!(
            guard.finish(outcome.clone()).unwrap(),
            ObservationWakeReceipt {
                sequence: 1,
                operand_digest: original.operand_digest(owner),
                outcome,
            }
        );
    }
}

#[test]
fn exact_readback_does_not_redispatch_or_evict_uncertain_receipt() {
    let (slot, _events, owner) = admitted_slot();
    let original = intent(&slot, owner);
    let digest = original.operand_digest(owner);
    let guard = slot
        .prepare_wake(owner, original.clone(), Arc::new(OriginalPolicy))
        .unwrap();
    assert_eq!(
        slot.read_observation_wake(owner, /*sequence*/ 1, &digest)
            .unwrap(),
        ObservationWakeReceipt {
            sequence: 1,
            operand_digest: digest.clone(),
            outcome: ObservationWakeOutcome::Pending { turn_id: None },
        }
    );
    assert_eq!(
        slot.prepare_wake(owner, original.clone(), Arc::new(OriginalPolicy))
            .unwrap_err(),
        ObservationError::RevisionMismatch
    );
    assert_eq!(
        slot.retire_observation_wake(owner, /*sequence*/ 1, &digest),
        Err(ObservationError::Unavailable)
    );
    let mut changed = original.clone();
    changed.sequence = 2;
    assert_eq!(
        slot.prepare_wake(owner, changed, Arc::new(OriginalPolicy))
            .unwrap_err(),
        ObservationError::RevisionMismatch
    );
    let final_receipt = guard.finish(ObservationWakeOutcome::Suppressed).unwrap();
    assert_eq!(
        slot.read_observation_wake(owner, /*sequence*/ 1, &digest)
            .unwrap(),
        final_receipt
    );
    slot.retire_observation_wake(owner, /*sequence*/ 1, &digest)
        .unwrap();
    assert_eq!(
        slot.prepare_wake(owner, original, Arc::new(OriginalPolicy))
            .unwrap_err(),
        ObservationError::RevisionMismatch
    );
    assert_eq!(
        slot.read_observation_wake(owner, /*sequence*/ 1, &digest),
        Err(ObservationError::RevisionMismatch)
    );
}

#[test]
fn reserved_is_not_started_and_owner_loss_never_reconstructs_receipt() {
    let (slot, _events, owner) = admitted_slot();
    let original = intent(&slot, owner);
    let digest = original.operand_digest(owner);
    let guard = slot
        .prepare_wake(owner, original, Arc::new(OriginalPolicy))
        .unwrap();
    assert!(guard.reserve_turn_if_allowed(&ThreadId::new(), "actual-turn", &mut || {}));
    assert_eq!(
        slot.read_observation_wake(owner, /*sequence*/ 1, &digest)
            .unwrap()
            .outcome,
        ObservationWakeOutcome::Pending {
            turn_id: Some("actual-turn".into())
        }
    );
    assert_eq!(
        guard
            .finish(ObservationWakeOutcome::Suppressed)
            .unwrap()
            .outcome,
        ObservationWakeOutcome::Suppressed
    );
    slot.revoke().unwrap();
    assert_eq!(
        slot.read_observation_wake(owner, /*sequence*/ 1, &digest),
        Err(ObservationError::StaleOwner)
    );
}

#[test]
fn stale_frame_budget_and_audit_cut_never_reserve() {
    let changes: [fn(&mut ObservationWakeIntent); 4] = [
        |intent| intent.frame_hash = "0".repeat(64),
        |intent| intent.frame_revision += 1,
        |intent| intent.budget_generation += 1,
        |intent| intent.expected_commit_order += 1,
    ];
    for change in changes {
        let (slot, _events, owner) = admitted_slot();
        let mut original = intent(&slot, owner);
        change(&mut original);
        let guard = slot
            .prepare_wake(owner, original, Arc::new(OriginalPolicy))
            .unwrap();
        assert!(
            !guard.reserve_turn_if_allowed(&ThreadId::new(), "turn", &mut || panic!(
                "stale intent reserved"
            ))
        );
    }
}
