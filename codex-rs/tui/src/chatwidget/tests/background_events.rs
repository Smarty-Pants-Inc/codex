use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn background_event_updates_status_header() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.handle_codex_event(Event {
        id: "bg-1".into(),
        msg: EventMsg::BackgroundEvent(BackgroundEventEvent {
            message: "Waiting for `vim`".to_string(),
        }),
    });

    assert!(chat.bottom_pane.status_indicator_visible());
    assert_eq!(chat.current_status.header, "Waiting for `vim`");
    assert!(drain_insert_history(&mut rx).is_empty());
}

#[tokio::test]
async fn streaming_deltas_only_enqueue_commit_animation_once_per_active_stream() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.handle_streaming_delta("one\n".to_string());
    chat.handle_streaming_delta("two\n".to_string());

    let mut start_count = 0usize;
    while let Ok(event) = rx.try_recv() {
        if matches!(event, AppEvent::StartCommitAnimation) {
            start_count += 1;
        }
    }

    assert_eq!(
        start_count, 1,
        "expected one start event for the active stream"
    );
    assert!(chat.commit_animation_requested);

    chat.stop_commit_animation();
    assert!(!chat.commit_animation_requested);
    assert!(matches!(rx.try_recv(), Ok(AppEvent::StopCommitAnimation)));

    chat.handle_streaming_delta("three\n".to_string());
    assert!(chat.commit_animation_requested);
    assert!(matches!(rx.try_recv(), Ok(AppEvent::StartCommitAnimation)));
}

#[tokio::test]
async fn task_start_stops_stale_commit_animation() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.handle_streaming_delta("one\n".to_string());
    while rx.try_recv().is_ok() {}
    assert!(chat.commit_animation_requested);

    chat.on_task_started();

    assert!(!chat.commit_animation_requested);
    assert!(matches!(rx.try_recv(), Ok(AppEvent::StopCommitAnimation)));
}

#[tokio::test]
async fn finalize_turn_stops_stale_commit_animation() {
    let (mut chat, mut rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.handle_streaming_delta("one\n".to_string());
    while rx.try_recv().is_ok() {}
    assert!(chat.commit_animation_requested);

    chat.finalize_turn();

    assert!(!chat.commit_animation_requested);
    assert!(matches!(rx.try_recv(), Ok(AppEvent::StopCommitAnimation)));
}
