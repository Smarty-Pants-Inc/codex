//! Reads registered agent identity, status and settings without restoring a runtime.
//! Known unloaded agents stay distinct from missing identities and backend failures.

use super::LocalAgentControl;
use crate::agent::api::AgentInfo;
use crate::agent::types::LiveAgent;
use crate::codex_thread::ThreadConfigSnapshot;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErrorDetails;
use codex_protocol::error::Result as CodexResult;

impl LocalAgentControl {
    pub(super) async fn inspect_agent(&self, thread_id: ThreadId) -> CodexResult<AgentInfo> {
        let manager = self.runtime.upgrade()?;
        let thread = match manager.get_thread(thread_id).await {
            Ok(thread) => thread,
            Err(err) if matches!(err.details(), CodexErrorDetails::ThreadNotFound(_)) => {
                return Ok(AgentInfo::Unloaded(
                    self.runtime.ensure_agent_known(thread_id)?,
                ));
            }
            Err(err) => return Err(err),
        };
        Ok(AgentInfo::Loaded {
            agent: LiveAgent {
                thread_id,
                metadata: self.get_agent_metadata(thread_id).unwrap_or_default(),
                status: thread.agent_status().await,
                multi_agent_version: thread.multi_agent_version(),
            },
            config: Box::new(thread.config_snapshot().await),
        })
    }

    pub(crate) async fn get_agent_config_snapshot(
        &self,
        agent_id: ThreadId,
    ) -> Option<ThreadConfigSnapshot> {
        match self.inspect_agent(agent_id).await.ok()? {
            AgentInfo::Loaded { config, .. } => Some(*config),
            AgentInfo::Unloaded(_) => None,
        }
    }
}
