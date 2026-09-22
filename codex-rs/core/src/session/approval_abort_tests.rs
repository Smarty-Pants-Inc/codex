use super::*;
use crate::state::ApprovalAbortBehavior;

#[tokio::test]
async fn matched_old_command_abort_leaves_replacement_turn_active() {
    let (session, context, _events) = make_session_and_context_with_rx().await;
    session
        .spawn_task(
            Arc::clone(&context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;
    let (tx, rx) = tokio::sync::oneshot::channel();
    session
        .register_pending_approval(
            "late-old-command".to_string(),
            "old-turn".to_string(),
            ApprovalAbortBehavior::InterruptTurn,
            tx,
        )
        .await;
    assert!(
        session
            .notify_approval("late-old-command", Some("old-turn"), ReviewDecision::Abort,)
            .await
    );
    assert_eq!(rx.await.expect("original decision"), ReviewDecision::Abort);
    let active_id = session
        .active_turn
        .lock()
        .await
        .as_ref()
        .and_then(|active| active.task.as_ref())
        .map(|task| task.turn_context.sub_id.clone());
    assert_eq!(active_id, Some(context.sub_id.clone()));
    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}

#[tokio::test]
async fn closed_command_waiter_does_not_interrupt_active_turn() {
    let (session, context, _events) = make_session_and_context_with_rx().await;
    session
        .spawn_task(
            Arc::clone(&context),
            Vec::new(),
            NeverEndingTask {
                kind: TaskKind::Regular,
                listen_to_cancellation_token: true,
            },
        )
        .await;
    let (tx, rx) = tokio::sync::oneshot::channel();
    session
        .register_pending_approval(
            "closed-command".to_string(),
            context.sub_id.clone(),
            ApprovalAbortBehavior::InterruptTurn,
            tx,
        )
        .await;
    drop(rx);
    assert!(
        !session
            .notify_approval(
                "closed-command",
                Some(&context.sub_id),
                ReviewDecision::Abort,
            )
            .await
    );
    let active_id = session
        .active_turn
        .lock()
        .await
        .as_ref()
        .and_then(|active| active.task.as_ref())
        .map(|task| task.turn_context.sub_id.clone());
    assert_eq!(active_id, Some(context.sub_id.clone()));
    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}
