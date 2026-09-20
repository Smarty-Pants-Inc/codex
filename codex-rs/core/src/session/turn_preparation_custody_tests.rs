use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn retained_complete_preparation_is_reused_without_repeating_effects() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let mut custody = TurnStartCustody::new(Vec::new());
    assert!(
        custody
            .prepare_once(&session, &turn, &CancellationToken::new())
            .await
            .unwrap()
    );
    let step = Arc::clone(&custody.prepared.as_ref().unwrap().first_step_context);
    let history = session
        .clone_history()
        .await
        .raw_items()
        .cloned()
        .collect::<Vec<_>>();
    // A repeated materialization would overwrite these settings and recapture
    // the step. Returning the original outputs must do neither.
    let sentinel = PreviousTurnSettings {
        model: "changed-after-preparation".to_owned(),
        comp_hash: Some("new-settings".to_owned()),
        realtime_active: Some(false),
    };
    session
        .set_previous_turn_settings(Some(sentinel.clone()))
        .await;
    assert!(
        custody
            .prepare_once(&session, &turn, &CancellationToken::new())
            .await
            .unwrap()
    );
    assert!(Arc::ptr_eq(
        &step,
        &custody.prepared.as_ref().unwrap().first_step_context
    ));
    assert_eq!(session.previous_turn_settings().await, Some(sentinel));
    assert_eq!(
        session
            .clone_history()
            .await
            .raw_items()
            .cloned()
            .collect::<Vec<_>>(),
        history
    );
    // Reusing custody is NOT permission to send under changed settings.
}
