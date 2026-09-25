//! Seals child admission for the whole agent tree once the root turn is suspended.
//! Spawns and restorations hold the read guard; root turn suspension takes the write guard.

use super::LocalAgentRuntime;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use std::sync::Arc;
use tokio::sync::OwnedRwLockWriteGuard;
use tokio::sync::RwLockReadGuard;

/// Holds tree admission closed while root turn suspension persists its state.
pub(crate) struct RootTurnSuspensionAdmission {
    sealed: OwnedRwLockWriteGuard<bool>,
}

impl RootTurnSuspensionAdmission {
    pub(crate) fn seal(mut self) {
        *self.sealed = true;
    }
}

impl LocalAgentRuntime {
    pub(super) async fn acquire_spawn_admission(&self) -> CodexResult<RwLockReadGuard<'_, bool>> {
        let admission = self.spawn_admission_sealed.read().await;
        if *admission {
            return Err(CodexErr::UnsupportedOperation(
                "agent admission is sealed after root turn suspension".to_string(),
            ));
        }
        Ok(admission)
    }

    pub(crate) async fn begin_root_turn_suspension_admission(
        &self,
        root_thread_id: ThreadId,
    ) -> CodexResult<Option<RootTurnSuspensionAdmission>> {
        let admission = Arc::clone(&self.spawn_admission_sealed).write_owned().await;
        if self
            .list_live_agent_subtree_thread_ids(root_thread_id)
            .await?
            .len()
            > 1
        {
            return Ok(None);
        }
        Ok(Some(RootTurnSuspensionAdmission { sealed: admission }))
    }
}
