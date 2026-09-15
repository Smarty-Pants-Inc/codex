//! Ordinary continuation authority is local to the current input and runtime.
//! The persisted setting is not restored authority; legacy goal state is separate.

use super::ContinuityBoundary;
use super::GoalRuntimeHandle;
use super::keep_working::StopScope;
use codex_extension_api::ThreadIdleCause;
use codex_protocol::protocol::AgentStatus;
use std::sync::atomic::Ordering;

impl GoalRuntimeHandle {
    pub(crate) fn is_retired(&self) -> bool {
        self.inner.keep_working.retired.load(Ordering::SeqCst)
    }

    pub(crate) fn invalidate_continuity(&self, boundary: ContinuityBoundary<'_>) {
        self.inner.keep_working.invalidate(boundary);
    }

    pub(crate) async fn stop_keep_working_for_turn(&self, turn_id: &str) -> Result<(), String> {
        self.inner
            .keep_working
            .stop(
                self.inner.state_dbs.thread_goals(),
                self.thread_id(),
                StopScope::Turn(turn_id),
            )
            .await
    }

    pub(super) async fn stop_keep_working_if_idle(
        &self,
        cause: ThreadIdleCause,
    ) -> Result<(), String> {
        // Capture before host awaits so intervening input revokes this stop.
        let scope = self.inner.keep_working.idle_stop_scope();
        if let Some(manager) = self.inner.thread_manager.upgrade() {
            let Ok(thread) = manager.get_thread(self.thread_id()).await else {
                return Ok(());
            };
            if thread.has_active_turn().await
                || !matches!(
                    (cause, thread.agent_status().await),
                    (ThreadIdleCause::Interrupted, AgentStatus::Interrupted)
                        | (ThreadIdleCause::Failed, AgentStatus::Errored(_))
                )
            {
                return Ok(());
            }
        }
        self.inner
            .keep_working
            .stop(self.inner.state_dbs.thread_goals(), self.thread_id(), scope)
            .await
    }
}
