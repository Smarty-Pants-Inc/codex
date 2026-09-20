use super::*;
use pretty_assertions::assert_eq;

fn fixture() -> (ObservationWakeBudgetSelection, ObservationWakeDebit) {
    (
        ObservationWakeBudgetSelection {
            thread_id: ThreadId::new(),
            rollout_id: ThreadId::new(),
            selection_digest: [7; 32],
            reservation_limit: NonZeroU64::new(2).unwrap(),
        },
        ObservationWakeDebit {
            ordinal: NonZeroU64::MIN,
            owner_epoch: [3; 16],
            sequence: NonZeroU64::MIN,
            operand_digest: [9; 32],
            cooldown_revision: NonZeroU64::MIN,
            wake_not_before_bits: 0.125_f64.to_bits(),
        },
    )
}

#[test]
fn duplicate_original_operation_reconciles_without_another_charge() {
    let (selection, first) = fixture();
    let mut replay = ObservationWakeBudgetReplay::new(selection.clone());
    replay
        .observe(&ObservationWakeBudgetRecord::InitializedV1(selection))
        .unwrap();
    let mut second = first.clone();
    second.ordinal = NonZeroU64::new(2).unwrap();
    second.sequence = NonZeroU64::new(2).unwrap();
    for debit in [first.clone(), second, first] {
        replay
            .observe(&ObservationWakeBudgetRecord::DebitedV1(debit))
            .unwrap();
    }
    assert_eq!(replay.charged(), 2);
}

#[test]
fn conflict_poisoning_retains_original_debt_and_cannot_be_skipped() {
    let (selection, first) = fixture();
    let mut replay = ObservationWakeBudgetReplay::new(selection.clone());
    replay
        .observe(&ObservationWakeBudgetRecord::InitializedV1(selection))
        .unwrap();
    let record = ObservationWakeBudgetRecord::DebitedV1(first.clone());
    replay.observe(&record).unwrap();
    let mut conflicting = first;
    conflicting.wake_not_before_bits = 0.25_f64.to_bits();
    assert_eq!(
        replay.observe(&ObservationWakeBudgetRecord::DebitedV1(conflicting)),
        Err(ObservationWakeBudgetReplayError::ConflictingOperation)
    );
    assert_eq!(
        replay.observe(&record),
        Err(ObservationWakeBudgetReplayError::PreviouslyInvalid)
    );
    assert_eq!(replay.charged(), 1);
}

#[test]
fn lineage_and_limit_fail_closed_without_refunding_debt() {
    let (mut selection, first) = fixture();
    selection.reservation_limit = NonZeroU64::MIN;
    let mut replay = ObservationWakeBudgetReplay::new(selection.clone());
    replay
        .observe(&ObservationWakeBudgetRecord::InitializedV1(
            selection.clone(),
        ))
        .unwrap();
    replay
        .observe(&ObservationWakeBudgetRecord::DebitedV1(first.clone()))
        .unwrap();
    let mut second = first;
    second.ordinal = NonZeroU64::new(2).unwrap();
    second.sequence = NonZeroU64::new(2).unwrap();
    assert_eq!(
        replay.observe(&ObservationWakeBudgetRecord::DebitedV1(second)),
        Err(ObservationWakeBudgetReplayError::LimitExceeded)
    );
    assert_eq!(replay.charged(), 1);
    let mut wrong_lineage = ObservationWakeBudgetReplay::new(selection.clone());
    selection.thread_id = ThreadId::new();
    assert_eq!(
        wrong_lineage.observe(&ObservationWakeBudgetRecord::InitializedV1(selection)),
        Err(ObservationWakeBudgetReplayError::InvalidLineage)
    );
}

#[test]
fn exact_positive_binary64_and_strict_wire_decode() {
    let (selection, first) = fixture();
    for bits in [1, 0.125_f64.to_bits(), f64::MAX.to_bits()] {
        let mut debit = first.clone();
        debit.wake_not_before_bits = bits;
        let record = ObservationWakeBudgetRecord::DebitedV1(debit);
        let encoded =
            serde_json::to_vec(&crate::RolloutItem::ObservationWakeBudget(record.clone())).unwrap();
        assert!(encoded.len() < 1024);
        let decoded = serde_json::from_slice::<crate::RolloutItem>(&encoded).unwrap();
        let crate::RolloutItem::ObservationWakeBudget(decoded) = decoded else {
            panic!("debit must retain its model-invisible rollout variant");
        };
        assert_eq!(decoded, record);
        let mut replay = ObservationWakeBudgetReplay::new(selection.clone());
        replay
            .observe(&ObservationWakeBudgetRecord::InitializedV1(
                selection.clone(),
            ))
            .unwrap();
        replay.observe(&record).unwrap();
    }
    for bits in [
        0,
        (-0.0_f64).to_bits(),
        (-1.0_f64).to_bits(),
        f64::INFINITY.to_bits(),
        f64::NAN.to_bits(),
    ] {
        let mut debit = first.clone();
        debit.wake_not_before_bits = bits;
        let mut replay = ObservationWakeBudgetReplay::new(selection.clone());
        replay
            .observe(&ObservationWakeBudgetRecord::InitializedV1(
                selection.clone(),
            ))
            .unwrap();
        assert_eq!(
            replay.observe(&ObservationWakeBudgetRecord::DebitedV1(debit)),
            Err(ObservationWakeBudgetReplayError::InvalidDebit)
        );
    }
    let mut encoded = serde_json::to_value(ObservationWakeBudgetRecord::DebitedV1(first)).unwrap();
    encoded["payload"]["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<ObservationWakeBudgetRecord>(encoded).is_err());
}
