use super::*;
use crate::ObservationProfile;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::openai_models::ModelInfo;
use pretty_assertions::assert_eq;

fn model() -> ModelInfo {
    let mut model = model_info_from_slug("gpt-oss-20b");
    model.context_window = Some(131_072);
    model.effective_context_window_percent = 100;
    model
}

#[test]
fn lost_ack_retains_original_commit_but_revalidation_never_relabels_or_renews_it() {
    let (slot, mut events, owner, _) = setup();
    let model = model();
    slot.initialize_budget(owner, &model, ObservationProfile::HarmonyGptOss)
        .unwrap();
    let request_id = Uuid::new_v4();
    slot.set_for_request_at_budget(
        owner,
        /*revision*/ 1,
        frame("A", /*expires_at*/ 160),
        request_id,
        /*budget_generation*/ 1,
    )
    .unwrap();
    let ObservationEvent::Control {
        metadata: committed,
        ..
    } = events.try_recv().unwrap()
    else {
        panic!("correlated commit")
    };
    slot.invalidate_budget().unwrap();
    let mut expected = committed.clone();
    expected.commit_order += 1;
    expected.status = ObservationStatus::Unavailable;
    let reservation = expected.native_reservation.as_mut().unwrap();
    reservation.generation = 2;
    reservation.state = ObservationReservationState::Invalid;
    assert_eq!(slot.read(owner).unwrap(), expected);
    slot.revalidate_budget(owner, /*generation*/ 2, &model)
        .unwrap();
    expected.commit_order += 1;
    let reservation = expected.native_reservation.as_mut().unwrap();
    reservation.generation = 3;
    reservation.state = ObservationReservationState::Valid;
    assert_eq!(slot.read(owner).unwrap(), expected);
    assert_eq!(
        slot.capture_checked("turn", Some(&model), |_| Ok(())),
        Err(ObservationError::BudgetInvalid)
    );
    assert_eq!(
        slot.set_for_request_at_budget(
            owner,
            /*revision*/ 1,
            frame("A", /*expires_at*/ 159),
            Uuid::new_v4(),
            /*budget_generation*/ 3
        ),
        Err(ObservationError::BudgetGenerationMismatch)
    );
    slot.set_for_request_at_budget(
        owner,
        /*revision*/ 2,
        frame("A", /*expires_at*/ 160),
        Uuid::new_v4(),
        /*budget_generation*/ 3,
    )
    .unwrap();
    expected.revision = 2;
    expected.commit_order += 1;
    expected.status = ObservationStatus::Current;
    expected.frame_budget_generation = Some(3);
    assert_eq!(slot.read(owner).unwrap(), expected);
    let captured = slot
        .capture_checked("turn", Some(&model), |_| Ok(()))
        .unwrap();
    let original = captured.clone();
    slot.invalidate_budget().unwrap();
    assert_eq!(
        slot.begin_attempt(captured.decision_id),
        Err(ObservationError::BudgetInvalid)
    );
    assert_eq!(captured, original);
    slot.release(captured.decision_id).unwrap();
}

#[test]
fn shrink_allows_current_generation_cleanup_but_not_old_generation_clear() {
    let (slot, _events, owner, _) = setup();
    let mut model = model();
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
    model.context_window = Some(4000);
    assert_eq!(
        slot.check_budget_model(&model),
        Err(ObservationError::BudgetInvalid)
    );
    assert_eq!(
        slot.set_for_request_at_budget(
            owner,
            /*revision*/ 2,
            /*frame*/ None,
            Uuid::new_v4(),
            /*budget_generation*/ 1
        ),
        Err(ObservationError::BudgetGenerationMismatch)
    );
    slot.set_for_request_at_budget(
        owner,
        /*revision*/ 2,
        /*frame*/ None,
        Uuid::new_v4(),
        /*budget_generation*/ 2,
    )
    .unwrap();
    slot.revalidate_budget(owner, /*generation*/ 2, &model)
        .unwrap();
    let mut expected = slot.read(owner).unwrap();
    assert_eq!(
        expected.native_reservation.as_ref().unwrap().state,
        ObservationReservationState::Unsupported
    );
    slot.set_for_request_at_budget(
        owner,
        /*revision*/ 3,
        /*frame*/ None,
        Uuid::new_v4(),
        /*budget_generation*/ 3,
    )
    .unwrap();
    expected.revision = 3;
    expected.commit_order += 1;
    assert_eq!(slot.read(owner).unwrap(), expected);
    assert_eq!(
        slot.set_for_request_at_budget(
            owner,
            /*revision*/ 4,
            frame("B", /*expires_at*/ 160),
            Uuid::new_v4(),
            /*budget_generation*/ 3
        ),
        Err(ObservationError::BudgetInvalid)
    );
}

#[test]
fn read_revalidation_conflict_aba_and_close_cannot_restore_an_old_generation() {
    let (slot, _events, owner, _) = setup();
    let model = model();
    slot.initialize_budget(owner, &model, ObservationProfile::HarmonyGptOss)
        .unwrap();
    let before = slot.native_reservation(owner).unwrap();
    slot.invalidate_budget().unwrap();
    slot.invalidate_budget().unwrap();
    let invalid = slot.native_reservation(owner).unwrap();
    assert_eq!(
        slot.revalidate_budget(owner, before.generation, &model),
        Err(ObservationError::BudgetGenerationMismatch)
    );
    assert_eq!(slot.native_reservation(owner).unwrap(), invalid);
    slot.revalidate_budget(owner, invalid.generation, &model)
        .unwrap();
    let mut expected = before;
    expected.generation = 4;
    assert_eq!(slot.native_reservation(owner).unwrap(), expected);
    slot.revoke().unwrap();
    assert_eq!(
        slot.revalidate_budget(owner, /*generation*/ 4, &model),
        Err(ObservationError::StaleOwner)
    );
    assert_eq!(slot.read(owner), Err(ObservationError::StaleOwner));
}

#[test]
fn already_sent_acceptance_keeps_original_capture_after_budget_invalidation() {
    let (slot, mut events, owner, _) = setup();
    let model = model();
    slot.initialize_budget(owner, &model, ObservationProfile::HarmonyGptOss)
        .unwrap();
    let captured = slot
        .capture_checked("turn", Some(&model), |_| Ok(()))
        .unwrap();
    let attempt = slot.begin_attempt(captured.decision_id).unwrap();
    slot.invalidate_budget().unwrap();
    slot.attempt_accepted(attempt).unwrap();
    slot.release(captured.decision_id).unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Captured(captured.clone())
    );
    let invalid = slot.native_reservation(owner).unwrap();
    assert_eq!(
        events.try_recv().unwrap(),
        ObservationEvent::Budget {
            owner,
            commit_order: 2,
            reservation: invalid
        }
    );
    let ObservationEvent::Submitted(record) = events.try_recv().unwrap() else {
        panic!("terminal submission")
    };
    assert_eq!(
        record,
        ObservationSubmitted {
            turn_id: "turn".into(),
            decision_id: captured.decision_id,
            attempt_id: attempt.attempt_id,
            request_id: record.request_id,
            provider_request_id: None,
            metadata: captured.metadata,
            captured_at: captured.captured_at,
            commit_order: 3,
            outcome: ObservationOutcome::Accepted,
            terminal_decision: true,
        }
    );
}

#[test]
fn configured_model_read_cannot_certify_over_an_effective_fallback() {
    let (slot, _events, owner, _) = setup();
    let model = model();
    slot.initialize_budget(owner, &model, ObservationProfile::HarmonyGptOss)
        .unwrap();
    let mut fallback = model.clone();
    fallback.context_window = Some(4000);
    assert_eq!(
        slot.check_budget_model(&fallback),
        Err(ObservationError::BudgetInvalid)
    );
    let invalid = slot.native_reservation(owner).unwrap();
    slot.revalidate_budget(owner, invalid.generation, &model)
        .unwrap();
    assert_eq!(slot.native_reservation(owner).unwrap(), invalid);
    assert_eq!(
        slot.check_budget_model(&fallback),
        Err(ObservationError::BudgetInvalid)
    );
    assert_eq!(slot.native_reservation(owner).unwrap(), invalid);
    assert_eq!(
        slot.check_budget_model(&model),
        Err(ObservationError::BudgetInvalid)
    );
    let returned = slot.native_reservation(owner).unwrap();
    assert!(returned.generation > invalid.generation);
    slot.revalidate_budget(owner, returned.generation, &model)
        .unwrap();
    let mut expected = invalid;
    expected.generation = returned.generation + 1;
    expected.state = ObservationReservationState::Valid;
    assert_eq!(slot.native_reservation(owner).unwrap(), expected);
}

#[test]
fn expired_retained_frame_is_not_hidden_by_budget_unavailability() {
    let (slot, _events, owner, time) = setup();
    slot.initialize_budget(owner, &model(), ObservationProfile::HarmonyGptOss)
        .unwrap();
    slot.set_for_request_at_budget(
        owner,
        /*revision*/ 1,
        frame("A", /*expires_at*/ 160),
        Uuid::new_v4(),
        /*budget_generation*/ 1,
    )
    .unwrap();
    let mut expected = slot.read(owner).unwrap();
    slot.invalidate_budget().unwrap();
    let now = *time.lock().unwrap();
    *time.lock().unwrap() = later(now, /*seconds*/ 60);
    expected.commit_order += 1;
    expected.status = ObservationStatus::Expired;
    let reservation = expected.native_reservation.as_mut().unwrap();
    reservation.generation = 2;
    reservation.state = ObservationReservationState::Invalid;
    assert_eq!(slot.read(owner).unwrap(), expected);
}

#[test]
fn lost_invalidation_capacity_and_generation_overflow_revoke_instead_of_going_silent() {
    for overflow in [false, true] {
        let (slot, _events, owner, _) = setup();
        slot.initialize_budget(owner, &model(), ObservationProfile::HarmonyGptOss)
            .unwrap();
        if overflow {
            slot.state
                .lock()
                .unwrap()
                .budget
                .as_mut()
                .unwrap()
                .snapshot
                .generation = MAX_SEQUENCE;
        } else {
            for _ in 0..EVENT_CAPACITY {
                slot.read(owner).unwrap();
            }
        }
        assert_eq!(
            slot.invalidate_budget(),
            Err(ObservationError::ResourceLimit)
        );
        assert_eq!(
            slot.native_reservation(owner),
            Err(ObservationError::StaleOwner)
        );
        assert_eq!(
            slot.set_for_request_at_budget(
                owner,
                /*revision*/ 1,
                /*frame*/ None,
                Uuid::new_v4(),
                /*budget_generation*/ 1
            ),
            Err(ObservationError::StaleOwner)
        );
    }
}
