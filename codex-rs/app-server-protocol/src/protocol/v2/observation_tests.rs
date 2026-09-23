use super::ObservationFrameUpdate;
use super::ThreadObservationSetParams;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn clear_requires_explicit_null_and_survives_serialization() {
    let mut request = json!({
        "threadId": "owned-thread",
        "ownerEpoch": "native-epoch",
        "revision": 2,
        "expectedBudgetGeneration": 4
    });
    assert!(serde_json::from_value::<ThreadObservationSetParams>(request.clone()).is_err());
    request["frame"] = serde_json::Value::Null;
    let parsed: ThreadObservationSetParams = serde_json::from_value(request.clone()).unwrap();
    assert_eq!(
        parsed,
        ThreadObservationSetParams {
            thread_id: "owned-thread".to_string(),
            owner_epoch: "native-epoch".to_string(),
            revision: 2,
            expected_budget_generation: 4,
            frame: ObservationFrameUpdate::Clear(()),
        }
    );
    assert_eq!(serde_json::to_value(parsed).unwrap(), request);
}
