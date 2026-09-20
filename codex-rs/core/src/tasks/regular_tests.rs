use super::*;
use codex_protocol::user_input::UserInput;
use std::task::Poll;

#[tokio::test]
async fn original_task_retains_unresolved_input_through_abort_without_replay() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let task = Arc::new(RegularTask::new());
    *task.preparation.lock().await = Some(TurnStartCustody::new(vec![TurnInput::DeveloperInput {
        content: vec![UserInput::Text {
            text: "original unresolved input".to_owned(),
            text_elements: Vec::new(),
        }],
    }]));
    let state = session.state.lock().await;
    {
        let mut owner = task.preparation.lock().await;
        let record = owner
            .as_mut()
            .unwrap()
            .record_initial_input(&session, &turn);
        tokio::pin!(record);
        assert!(matches!(futures::poll!(record.as_mut()), Poll::Pending));
    }
    drop(state);
    task.abort(Arc::clone(&session), Arc::clone(&turn)).await;
    let mut owner = task.preparation.lock().await;
    let custody = owner.as_mut().unwrap();
    assert!(custody.has_unresolved_input());
    // The same original owner refuses the interrupted operation, not a replay.
    assert!(custody.record_initial_input(&session, &turn).await.is_err());
}
