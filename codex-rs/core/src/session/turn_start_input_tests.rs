use super::*;
use pretty_assertions::assert_eq;
use std::task::Poll;

fn original_input() -> Vec<TurnInput> {
    vec![TurnInput::DeveloperInput {
        content: vec![UserInput::Text {
            text: "retain this original preparation input".to_owned(),
            text_elements: Vec::new(),
        }],
    }]
}

#[tokio::test]
async fn committed_input_is_not_replayed_on_second_preparation_commit() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let original = original_input();
    let before = session
        .clone_history()
        .await
        .raw_items()
        .cloned()
        .collect::<Vec<_>>();
    let mut input = TurnStartInput::new(original.clone());
    assert!(
        !input
            .record(&session, &turn, PersistContext::Standard)
            .await
            .unwrap()
    );
    let committed = session
        .clone_history()
        .await
        .raw_items()
        .cloned()
        .collect::<Vec<_>>();
    assert_ne!(before, committed);
    assert!(
        !input
            .record(&session, &turn, PersistContext::Standard)
            .await
            .unwrap()
    );
    assert_eq!(
        session
            .clone_history()
            .await
            .raw_items()
            .cloned()
            .collect::<Vec<_>>(),
        committed
    );
    assert_eq!(input.original(), original.as_slice());
    assert_eq!(input.progress, InputProgress::Recorded { blocked: false });
}

#[tokio::test]
async fn dropped_recording_retains_original_input_but_refuses_blind_replay() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let original = original_input();
    let mut input = TurnStartInput::new(original.clone());
    // Hold the original state lock so the real recorder cannot finish. Dropping
    // this actual pending future models cancellation, not a manufactured flag.
    let state = session.state.lock().await;
    {
        let recording = input.record(&session, &turn, PersistContext::Standard);
        tokio::pin!(recording);
        assert!(matches!(futures::poll!(recording.as_mut()), Poll::Pending));
    }
    drop(state);
    assert_eq!(input.progress, InputProgress::Recording);
    assert_eq!(input.original(), original.as_slice());
    let before = session
        .clone_history()
        .await
        .raw_items()
        .cloned()
        .collect::<Vec<_>>();
    let error = input
        .record(&session, &turn, PersistContext::Standard)
        .await
        .unwrap_err();
    assert!(matches!(error, CodexErr::InvalidRequest(message)
        if message == "original turn input recording was interrupted; replay is not permitted"));
    assert_eq!(
        session
            .clone_history()
            .await
            .raw_items()
            .cloned()
            .collect::<Vec<_>>(),
        before
    );
}

#[tokio::test]
async fn preparation_retains_previous_settings_before_original_update() {
    let (session, turn) = crate::session::tests::make_session_and_context().await;
    let session = Arc::new(session);
    let turn = Arc::new(turn);
    let previous = PreviousTurnSettings {
        model: "previous-model".to_owned(),
        comp_hash: Some("previous-compaction-identity".to_owned()),
        realtime_active: Some(false),
    };
    session
        .set_previous_turn_settings(Some(previous.clone()))
        .await;
    let mut input = TurnStartInput::new(original_input());
    let prepared = crate::session::turn::turn_start_preparation::prepare_turn_start(
        &session,
        &turn,
        &mut input,
        &CancellationToken::new(),
    )
    .await
    .unwrap()
    .expect("ordinary first-step preparation");
    assert_eq!(prepared.previous_turn_settings, Some(previous));
    assert_eq!(
        session.previous_turn_settings().await,
        Some(PreviousTurnSettings {
            model: turn.model_info.slug.clone(),
            comp_hash: turn.model_info.comp_hash.clone(),
            realtime_active: Some(turn.realtime_active),
        })
    );
    assert_eq!(input.progress, InputProgress::Recorded { blocked: false });
}
