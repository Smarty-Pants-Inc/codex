use super::handlers;
use super::session::Session;
use crate::state::TaskKind;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::turn_input::IdleTurnSource;
use codex_protocol::turn_input::SuspendTurnOutcome;
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

pub(super) struct SuspensionResult {
    pub(super) outcome: CodexResult<SuspendTurnOutcome>,
    pub(super) terminate_session: bool,
}

impl SuspensionResult {
    fn recoverable(outcome: CodexResult<SuspendTurnOutcome>) -> Self {
        Self {
            outcome,
            terminate_session: false,
        }
    }

    fn terminal(outcome: CodexResult<SuspendTurnOutcome>) -> Self {
        Self {
            outcome,
            terminate_session: true,
        }
    }
}

pub(super) async fn suspend_turn_and_shutdown(
    session: &Arc<Session>,
    submission_id: String,
) -> SuspensionResult {
    {
        let active = session.active_turn.lock().await;
        let Some(task) = active.as_ref().and_then(|turn| turn.task.as_ref()) else {
            return SuspensionResult::recoverable(Ok(SuspendTurnOutcome::NotActive));
        };
        if task.kind != TaskKind::Regular {
            return SuspensionResult::recoverable(Ok(SuspendTurnOutcome::UnsupportedTask));
        }
    }

    // Refuse obvious descendants before the durability barrier; admission is rechecked and
    // sealed after the flush before handoff can be accepted.
    let live_subtree = match session
        .services
        .agent_control
        .list_live_agent_subtree_thread_ids(session.thread_id)
        .await
    {
        Ok(live_subtree) => live_subtree,
        Err(error) => return SuspensionResult::recoverable(Err(error)),
    };
    if live_subtree.len() > 1 {
        return SuspensionResult::recoverable(Ok(SuspendTurnOutcome::HasLiveDescendants));
    }

    let live_thread = match session.live_thread_for_persistence("suspend an unfinished root turn") {
        Ok(live_thread) => live_thread.clone(),
        Err(error) => {
            return SuspensionResult::recoverable(Err(CodexErr::Fatal(error.to_string())));
        }
    };
    // Flush before canceling execution so a persistence failure leaves the original turn running.
    if let Err(error) = live_thread.flush().await {
        return SuspensionResult::recoverable(Err(CodexErr::Fatal(format!(
            "flush before root turn suspension failed: {error}"
        ))));
    }

    let spawn_admission = match session
        .services
        .agent_control
        .begin_root_turn_suspension_admission(session.thread_id)
        .await
    {
        Ok(Some(spawn_admission)) => spawn_admission,
        Ok(None) => {
            return SuspensionResult::recoverable(Ok(SuspendTurnOutcome::HasLiveDescendants));
        }
        Err(error) => return SuspensionResult::recoverable(Err(error)),
    };

    // Persist extension-owned resumable state while the task is still active. The admission
    // write guard prevents a child spawn from crossing this yielding boundary.
    let turn_context = {
        let active = session.active_turn.lock().await;
        let Some(task) = active.as_ref().and_then(|turn| turn.task.as_ref()) else {
            return SuspensionResult::recoverable(Ok(SuspendTurnOutcome::NotActive));
        };
        if task.kind != TaskKind::Regular {
            return SuspensionResult::recoverable(Ok(SuspendTurnOutcome::UnsupportedTask));
        }
        Arc::clone(&task.turn_context)
    };
    if let Err(error) = session
        .emit_turn_suspend_lifecycle(turn_context.extension_data.as_ref())
        .await
    {
        return SuspensionResult::recoverable(Err(CodexErr::Fatal(format!(
            "persist resumable state before root turn suspension failed: {error}"
        ))));
    }

    // The flush and extension hooks can yield while the active turn completes or changes.
    // Remove only the exact regular turn whose resumable state was persisted.
    let (turn, task) = {
        let mut active = session.active_turn.lock().await;
        let Some(active_turn) = active.as_ref() else {
            return SuspensionResult::recoverable(Ok(SuspendTurnOutcome::NotActive));
        };
        let Some(active_task) = active_turn.task.as_ref() else {
            return SuspensionResult::recoverable(Ok(SuspendTurnOutcome::NotActive));
        };
        if !Arc::ptr_eq(&active_task.turn_context, &turn_context) {
            return SuspensionResult::recoverable(Ok(SuspendTurnOutcome::NotActive));
        }
        if active_task.kind != TaskKind::Regular {
            return SuspensionResult::recoverable(Ok(SuspendTurnOutcome::UnsupportedTask));
        }
        let Some(mut turn) = active.take() else {
            return SuspensionResult::recoverable(Err(CodexErr::Fatal(
                "accepted root turn suspension had no running turn".to_string(),
            )));
        };
        let Some(task) = turn.task.take() else {
            *active = Some(turn);
            return SuspensionResult::recoverable(Err(CodexErr::Fatal(
                "accepted root turn suspension had no running task".to_string(),
            )));
        };
        (turn, task)
    };
    spawn_admission.seal();
    let idle_turn_source = task
        .turn_context
        .extension_data
        .get::<IdleTurnSource>()
        .map_or(IdleTurnSource::Unspecified, |source| *source);
    let turn_id = task.turn_context.sub_id.clone();
    // Normal shutdown records a terminal turn event, preventing another worker from
    // recovering this turn under its original ID. Cancel the task without that event.
    task.cancellation_token.cancel();
    task.turn_context
        .turn_metadata_state
        .cancel_git_enrichment_task();
    let mut task_handle = task.handle.detach();
    match tokio::time::timeout(
        Duration::from_millis(crate::tasks::GRACEFULL_INTERRUPTION_TIMEOUT_MS),
        &mut task_handle,
    )
    .await
    {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            warn!(thread_id = %session.thread_id, %error, "suspended turn task exited abnormally");
        }
        Err(_) => {
            warn!(
                thread_id = %session.thread_id,
                "suspended turn task did not stop gracefully; aborting it"
            );
            task_handle.abort();
            let _ = task_handle.await;
        }
    }
    // Pending accepted input and interactive waiters live only in this process. Handoff
    // intentionally drops that state; persisting or replaying it needs a separate protocol.
    session.input_queue.clear_pending(&turn).await;

    // Stop all producers before flushing their final history and closing its writer. This is
    // past the point of no return: every outcome below terminates the session, and a failed close
    // falls back to discarding the live writer so durable history can be recovered safely.
    handlers::shutdown_session_runtime(session).await;
    let flush_result = live_thread.flush().await;
    let shutdown_result = live_thread.shutdown().await;
    let discard_result = if shutdown_result.is_err() {
        Some(live_thread.discard().await)
    } else {
        None
    };
    // The runtime is terminal even when persistence reports an error, so release extension-owned
    // thread state before the submission loop closes.
    handlers::emit_thread_stop_lifecycle(session.as_ref()).await;
    let outcome = match (flush_result, shutdown_result, discard_result) {
        (Ok(()), Ok(()), None) => Ok(SuspendTurnOutcome::Suspended {
            turn_id,
            idle_turn_source,
        }),
        (flush_result, shutdown_result, discard_result) => {
            let flush_error = flush_result
                .err()
                .map(|error| format!("flush after root turn suspension failed: {error}"));
            let shutdown_error = shutdown_result
                .err()
                .map(|error| format!("close suspended root turn writer failed: {error}"));
            let discard_status = match discard_result {
                Some(Ok(())) => "session was quarantined and its writer was discarded".to_string(),
                Some(Err(error)) => format!(
                    "session was quarantined, but discarding its writer also failed: {error}"
                ),
                None => "session was quarantined after its writer closed".to_string(),
            };
            let message = [flush_error, shutdown_error, Some(discard_status)]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join("; ");
            let error = CodexErr::Fatal(message);
            session
                .send_event_raw(Event {
                    id: submission_id.clone(),
                    msg: EventMsg::Error(error.to_error_event(/*message_prefix*/ None)),
                })
                .await;
            Err(error)
        }
    };
    session
        .deliver_event_raw(Event {
            id: submission_id,
            msg: EventMsg::ShutdownComplete,
        })
        .await;
    SuspensionResult::terminal(outcome)
}
