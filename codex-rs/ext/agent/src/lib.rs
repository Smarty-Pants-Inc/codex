use codex_core::CodexThread;
use codex_core::NewThread;
use codex_core::StartIfIdleSubmission;
use codex_core::StartThreadOptions;
use codex_core::ThreadManager;
use codex_core::TurnInputRequest;
use codex_core::config::Config;
use codex_protocol::ThreadId;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::protocol::W3cTraceContext;
use codex_protocol::user_input::UserInput;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Weak;

/// A skill selected by agent discovery, not inferred from developer prompt text.
pub struct AgentSkill {
    pub name: String,
    pub path: PathBuf,
}

/// A fully resolved agent invocation.
///
/// Agent discovery owns rendering `prompt` and resolving its optional skill.
/// The runtime starts developer input in isolated forked context; the selected
/// skill does not confer direct-user authority on the prompt.
pub struct AgentInvocation {
    pub config: Config,
    pub prompt: String,
    pub skill: Option<AgentSkill>,
    pub parent_trace: Option<W3cTraceContext>,
}

/// A spawned agent whose initial turn has been submitted.
pub struct AgentRun {
    pub thread_id: ThreadId,
    pub turn_id: String,
    pub thread: Arc<CodexThread>,
}

/// Runs resolved agents in threads forked by the owning [`ThreadManager`].
#[derive(Clone)]
pub struct AgentRunner {
    thread_manager: Weak<ThreadManager>,
}

impl AgentRunner {
    pub fn new(thread_manager: Weak<ThreadManager>) -> Self {
        Self { thread_manager }
    }

    /// Starts a resolved agent in a fork of `parent_thread_id`.
    pub async fn start(
        &self,
        parent_thread_id: ThreadId,
        invocation: AgentInvocation,
    ) -> CodexResult<AgentRun> {
        let AgentInvocation {
            config,
            prompt,
            skill,
            parent_trace,
        } = invocation;
        if prompt.trim().is_empty() {
            return Err(CodexErr::InvalidRequest(
                "agent prompt must not be empty".to_string(),
            ));
        }

        let thread_manager = self
            .thread_manager
            .upgrade()
            .ok_or_else(|| CodexErr::UnsupportedOperation("thread manager dropped".to_string()))?;
        let NewThread {
            thread_id, thread, ..
        } = thread_manager
            .spawn_subagent(
                parent_thread_id,
                StartThreadOptions {
                    parent_trace: parent_trace.clone(),
                    ..StartThreadOptions::new(config)
                },
            )
            .await?;
        let mut input = vec![UserInput::Text {
            text: prompt,
            text_elements: Vec::new(),
        }];
        if let Some(AgentSkill { name, path }) = skill {
            input.push(UserInput::Skill { name, path });
        }
        let turn_id = match thread
            .start_turn_if_idle(TurnInputRequest::developer_input(input).with_trace(parent_trace))
            .await?
        {
            StartIfIdleSubmission::Started { turn_id } => turn_id,
            StartIfIdleSubmission::NotSubmitted { reason } => {
                return Err(CodexErr::InvalidRequest(format!(
                    "agent prompt was not submitted: {reason:?}"
                )));
            }
        };

        Ok(AgentRun {
            thread_id,
            turn_id,
            thread,
        })
    }
}
