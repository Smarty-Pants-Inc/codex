use std::sync::Arc;

use codex_protocol::protocol::TurnAbortReason;
use pretty_assertions::assert_eq;
use tokio_util::sync::CancellationToken;

use crate::session::TurnInput;
use crate::session::session::Session;
use crate::session::tests::make_session_and_context_with_rx;
use crate::session::turn_context::NewTurnContextOptions;
use crate::session::turn_context::TurnContext;
use crate::state::ActiveTurn;
use crate::state::TaskKind;
use crate::tasks::SessionTask;
use crate::tasks::SessionTaskResult;

struct CancellableTask;

impl SessionTask for CancellableTask {
    fn kind(&self) -> TaskKind {
        TaskKind::Regular
    }

    fn span_name(&self) -> &'static str {
        "session_task.cancellable"
    }

    async fn run(
        self: Arc<Self>,
        _session: Arc<Session>,
        _turn_context: Arc<TurnContext>,
        _input: Vec<TurnInput>,
        cancellation_token: CancellationToken,
    ) -> SessionTaskResult {
        cancellation_token.cancelled().await;
        Ok(None)
    }
}

async fn turn_context_disabling(
    session: &Session,
    sub_id: &str,
    plugin_id: &str,
) -> Arc<TurnContext> {
    session
        .state
        .lock()
        .await
        .session_configuration
        .disabled_plugin_ids = vec![plugin_id.to_string()];
    session
        .new_turn_with_default_settings(sub_id.to_string(), NewTurnContextOptions::default())
        .await
}

#[tokio::test]
async fn rejected_stale_start_keeps_admitted_plugin_selection() {
    let (session, _turn_context, _rx) = make_session_and_context_with_rx().await;
    let stale_turn_context = turn_context_disabling(&session, "stale-turn", "stale-plugin").await;
    // The stale starter's taskless reservation was replaced, as direct user input does.
    let stale_turn_state = Arc::clone(&ActiveTurn::default().turn_state);
    *session.active_turn.lock().await = Some(ActiveTurn::default());
    let admitted_disabled_plugin_ids = session
        .state
        .lock()
        .await
        .active_disabled_plugin_ids
        .clone();
    let admitted_hooks = session.hooks();

    assert!(
        !session
            .start_reserved_task(
                stale_turn_context,
                &mut Vec::new(),
                CancellableTask,
                &stale_turn_state,
            )
            .await
    );

    assert_eq!(
        session.state.lock().await.active_disabled_plugin_ids,
        admitted_disabled_plugin_ids
    );
    assert!(Arc::ptr_eq(&session.hooks(), &admitted_hooks));

    // A start that owns its reservation still activates its selection before it runs.
    let reserved_turn_state = session
        .active_turn
        .lock()
        .await
        .as_ref()
        .map(|turn| Arc::clone(&turn.turn_state))
        .expect("replacement reservation must remain active");
    let admitted_turn_context =
        turn_context_disabling(&session, "admitted-turn", "admitted-plugin").await;
    assert!(
        session
            .start_reserved_task(
                admitted_turn_context,
                &mut Vec::new(),
                CancellableTask,
                &reserved_turn_state,
            )
            .await
    );
    assert_eq!(
        session.state.lock().await.active_disabled_plugin_ids,
        vec!["admitted-plugin".to_string()]
    );

    session.abort_all_tasks(TurnAbortReason::Interrupted).await;
}
