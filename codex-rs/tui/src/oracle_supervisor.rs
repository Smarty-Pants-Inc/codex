use std::collections::HashMap;
use std::env;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use codex_app_server_protocol::Thread;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::ThreadId;
use ratatui::style::Stylize;
use ratatui::text::Line;
use serde::Deserialize;
use tokio::fs;
use tokio::process::Command;

use crate::history_cell::PlainHistoryCell;

const ORACLE_CONTEXT_FILE_LIMIT: usize = 4;
const ORACLE_CONTEXT_CHAR_LIMIT: usize = 12_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OracleCommand {
    Browse,
    NewThread,
    AttachThread(String),
    On,
    Off,
    Status,
    Model(Option<OracleModelPreset>),
}

impl OracleCommand {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(Self::Browse);
        }
        let parts = trimmed.split_whitespace().collect::<Vec<_>>();
        let command = parts
            .first()
            .map(|value| value.to_ascii_lowercase())
            .unwrap_or_default();
        match command.as_str() {
            "browse" | "threads" if parts.len() == 1 => Ok(Self::Browse),
            "new" if parts.len() == 1 => Ok(Self::NewThread),
            "attach" if parts.len() == 2 && !parts[1].trim().is_empty() => {
                Ok(Self::AttachThread(parts[1].to_string()))
            }
            "on" if parts.len() == 1 => Ok(Self::On),
            "off" if parts.len() == 1 => Ok(Self::Off),
            "status" | "info" if parts.len() == 1 => Ok(Self::Status),
            "model" if parts.len() == 1 => Ok(Self::Model(None)),
            "model" if parts.len() == 2 => OracleModelPreset::parse(parts[1])
                .map(|model| Self::Model(Some(model)))
                .ok_or_else(|| {
                    "Usage: /oracle [browse|new|attach <conversation_id>|on|off|info|model [pro|thinking]]".to_string()
                }),
            _ => Err(
                "Usage: /oracle [browse|new|attach <conversation_id>|on|off|info|model [pro|thinking]]"
                    .to_string(),
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OracleModelPreset {
    #[default]
    Pro,
    Thinking,
}

impl OracleModelPreset {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "pro" | "gpt-5.4-pro" => Some(Self::Pro),
            "thinking" | "gpt-5.4" => Some(Self::Thinking),
            _ => None,
        }
    }

    pub(crate) fn model_id(self) -> &'static str {
        match self {
            Self::Pro => "gpt-5.4-pro",
            Self::Thinking => "gpt-5.4",
        }
    }

    pub(crate) fn browser_label(self) -> &'static str {
        match self {
            Self::Pro => "GPT-5.4 Pro",
            Self::Thinking => "Thinking 5.4",
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Pro => "gpt-5.4-pro",
            Self::Thinking => "gpt-5.4 (Thinking 5.4)",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OracleRequestKind {
    UserTurn,
    Checkpoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OracleSupervisorPhase {
    #[default]
    Disabled,
    Idle,
    WaitingForOracle(OracleRequestKind),
    WaitingForOrchestrator,
}

impl OracleSupervisorPhase {
    pub(crate) fn description(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Idle => "idle",
            Self::WaitingForOracle(OracleRequestKind::UserTurn) => {
                "waiting for Oracle user-turn reply"
            }
            Self::WaitingForOracle(OracleRequestKind::Checkpoint) => {
                "waiting for Oracle checkpoint review"
            }
            Self::WaitingForOrchestrator => "waiting for orchestrator",
        }
    }

    pub(crate) fn is_busy(self) -> bool {
        matches!(
            self,
            Self::WaitingForOracle(_) | Self::WaitingForOrchestrator
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OracleAction {
    Reply,
    Delegate,
    RequestContext,
    AskUser,
    Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OracleControlOp {
    Reply,
    Handoff,
    Send,
    Spawn,
    List,
    Search,
    RequestContext,
    Finish,
}

impl OracleControlOp {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "reply" => Some(Self::Reply),
            "handoff" => Some(Self::Handoff),
            "send" | "delegate" => Some(Self::Send),
            "spawn" => Some(Self::Spawn),
            "list" => Some(Self::List),
            "search" => Some(Self::Search),
            "request_context" | "context" => Some(Self::RequestContext),
            "finish" => Some(Self::Finish),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OracleControlParticipant {
    pub(crate) address: String,
    pub(crate) kind: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) visibility: Option<String>,
}

impl OracleControlParticipant {
    fn from_address(raw: &str) -> Option<Self> {
        let address = normalize_non_empty(raw)?;
        Some(Self {
            kind: infer_participant_kind(address.as_str()),
            address,
            role: None,
            visibility: None,
        })
    }

    fn from_json(value: OracleControlParticipantValue) -> Option<Self> {
        match value {
            OracleControlParticipantValue::Address(raw) => Self::from_address(raw.as_str()),
            OracleControlParticipantValue::Detailed(json) => {
                let address = normalize_non_empty_option(json.address)
                    .or_else(|| normalize_non_empty_option(json.id))
                    .or_else(|| normalize_non_empty_option(json.target))
                    .or_else(|| normalize_non_empty_option(json.name))
                    .or_else(|| normalize_non_empty_option(json.kind.clone()))?;
                let kind = normalize_non_empty_option(json.kind)
                    .or_else(|| infer_participant_kind(address.as_str()));
                let role = normalize_non_empty_option(json.role);
                let visibility = normalize_non_empty_option(json.visibility);
                Some(Self {
                    address,
                    kind,
                    role,
                    visibility,
                })
            }
        }
    }

    fn display_label(&self) -> String {
        match (&self.kind, &self.role) {
            (Some(kind), Some(role))
                if !self.address.eq_ignore_ascii_case(kind.as_str())
                    && !self
                        .address
                        .to_ascii_lowercase()
                        .starts_with(&format!("{kind}:")) =>
            {
                format!("{kind} {} ({role})", self.address)
            }
            (Some(kind), _)
                if !self.address.eq_ignore_ascii_case(kind.as_str())
                    && !self
                        .address
                        .to_ascii_lowercase()
                        .starts_with(&format!("{kind}:")) =>
            {
                format!("{kind} {}", self.address)
            }
            (_, Some(role)) => format!("{} ({role})", self.address),
            _ => self.address.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct OracleControlDirective {
    pub(crate) op: Option<OracleControlOp>,
    pub(crate) action_hint: Option<OracleAction>,
    pub(crate) to: Option<String>,
    pub(crate) participants: Vec<OracleControlParticipant>,
    pub(crate) message: Option<String>,
    pub(crate) message_for_user: Option<String>,
    pub(crate) task_for_orchestrator: Option<String>,
    pub(crate) context_requests: Vec<String>,
    pub(crate) query: Option<String>,
    pub(crate) workflow_id: Option<String>,
    pub(crate) workflow_version: Option<u64>,
    pub(crate) objective: Option<String>,
    pub(crate) summary: Option<String>,
    pub(crate) status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OracleParticipantVisibility {
    #[default]
    Visible,
    Hidden,
}

impl OracleParticipantVisibility {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Hidden => "hidden",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct OracleRoutingParticipant {
    pub(crate) address: String,
    pub(crate) thread_id: Option<ThreadId>,
    pub(crate) title: Option<String>,
    pub(crate) kind: Option<String>,
    pub(crate) role: Option<String>,
    pub(crate) visibility: OracleParticipantVisibility,
    pub(crate) owned_by_oracle: bool,
    pub(crate) route_completions: bool,
    pub(crate) route_closures: bool,
    pub(crate) last_task: Option<String>,
}

impl OracleRoutingParticipant {
    pub(crate) fn summary_line(&self) -> String {
        let thread = self
            .thread_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "no-thread".to_string());
        let title = self.title.as_deref().unwrap_or("untitled");
        let kind = self.kind.as_deref().unwrap_or("destination");
        let role = self.role.as_deref().unwrap_or(kind);
        let mut summary = format!(
            "- {} [{} {}] -> {} ({title})",
            self.address,
            kind,
            self.visibility.label(),
            thread,
        );
        if !role.eq_ignore_ascii_case(kind) {
            summary.push_str(format!(" role={role}").as_str());
        }
        summary
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OracleRoutedThreadOwner {
    pub(crate) oracle_thread_id: ThreadId,
    pub(crate) address: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OracleWorkflowMode {
    #[default]
    Chat,
    Supervising,
}

impl OracleWorkflowMode {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Supervising => "supervising",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OracleWorkflowStatus {
    #[default]
    Idle,
    Running,
    NeedsHuman,
    Complete,
    Failed,
}

impl OracleWorkflowStatus {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
            Self::NeedsHuman => "needs_human",
            Self::Complete => "complete",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct OracleWorkflowBinding {
    pub(crate) workflow_id: String,
    pub(crate) mode: OracleWorkflowMode,
    pub(crate) status: OracleWorkflowStatus,
    pub(crate) version: u64,
    pub(crate) objective: Option<String>,
    pub(crate) summary: Option<String>,
    pub(crate) last_checkpoint: Option<String>,
    pub(crate) last_blocker: Option<String>,
    pub(crate) orchestrator_thread_id: Option<ThreadId>,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleRunRequest {
    pub(crate) oracle_thread_id: ThreadId,
    pub(crate) kind: OracleRequestKind,
    pub(crate) session_slug: String,
    pub(crate) prompt: String,
    pub(crate) files: Vec<String>,
    pub(crate) workspace_cwd: PathBuf,
    pub(crate) oracle_repo: PathBuf,
    pub(crate) followup_session: Option<String>,
    pub(crate) model: OracleModelPreset,
    pub(crate) browser_model_strategy: String,
    pub(crate) browser_model_label: Option<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleRunResult {
    pub(crate) oracle_thread_id: ThreadId,
    pub(crate) kind: OracleRequestKind,
    pub(crate) requested_slug: String,
    pub(crate) session_id: String,
    pub(crate) response: OracleResponse,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleResponse {
    pub(crate) action: OracleAction,
    pub(crate) message_for_user: String,
    pub(crate) task_for_orchestrator: Option<String>,
    pub(crate) context_requests: Vec<String>,
    pub(crate) directive: Option<OracleControlDirective>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OracleThreadBinding {
    pub(crate) session_root_slug: Option<String>,
    pub(crate) current_session_id: Option<String>,
    pub(crate) orchestrator_thread_id: Option<ThreadId>,
    pub(crate) workflow: Option<OracleWorkflowBinding>,
    pub(crate) phase: OracleSupervisorPhase,
    pub(crate) last_status: Option<String>,
    pub(crate) last_orchestrator_task: Option<String>,
    pub(crate) pending_turn_id: Option<String>,
    pub(crate) automatic_context_followups: u8,
    pub(crate) conversation_id: Option<String>,
    pub(crate) remote_title: Option<String>,
    pub(crate) participants: HashMap<String, OracleRoutingParticipant>,
    pub(crate) pending_checkpoint_threads: Vec<ThreadId>,
    pub(crate) pending_checkpoint_versions: HashMap<ThreadId, u64>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OracleSupervisorState {
    /// Active visible Oracle thread (legacy working view for in-flight workflows).
    pub(crate) oracle_thread_id: Option<ThreadId>,
    /// Legacy working view fields mirrored from `bindings[oracle_thread_id]`.
    pub(crate) session_root_slug: Option<String>,
    pub(crate) current_session_id: Option<String>,
    pub(crate) orchestrator_thread_id: Option<ThreadId>,
    pub(crate) workflow: Option<OracleWorkflowBinding>,
    pub(crate) phase: OracleSupervisorPhase,
    pub(crate) model: OracleModelPreset,
    pub(crate) last_status: Option<String>,
    pub(crate) last_orchestrator_task: Option<String>,
    pub(crate) pending_turn_id: Option<String>,
    pub(crate) automatic_context_followups: u8,
    /// Per-visible-thread Oracle session state.
    pub(crate) bindings: HashMap<ThreadId, OracleThreadBinding>,
    /// Routed child thread -> owning visible Oracle thread + destination address.
    pub(crate) routed_thread_owner: HashMap<ThreadId, OracleRoutedThreadOwner>,
}

impl OracleSupervisorState {
    pub(crate) fn intercepts(&self, thread_id: ThreadId) -> bool {
        self.oracle_thread_id == Some(thread_id) || self.bindings.contains_key(&thread_id)
    }

    pub(crate) fn status_message(&self) -> String {
        let enabled = if self.bindings.is_empty() {
            "off".to_string()
        } else {
            format!("on for {} thread(s)", self.bindings.len())
        };
        let active = self
            .oracle_thread_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string());
        let last = self.last_status.clone().unwrap_or_else(|| {
            self.oracle_thread_id
                .and_then(|thread_id| {
                    self.bindings
                        .get(&thread_id)
                        .and_then(|binding| binding.last_status.clone())
                })
                .unwrap_or_else(|| "no activity yet".to_string())
        });
        let workflow = self.workflow.as_ref().map_or_else(
            || "none".to_string(),
            |workflow| {
                format!(
                    "{} v{} ({})",
                    workflow.workflow_id,
                    workflow.version,
                    workflow.status.label()
                )
            },
        );
        format!(
            "Oracle mode: {enabled}\nRequested model: {}\nBrowser strategy: select\nActive oracle thread: {active}\nTracked oracle threads: {}\nWorkflow: {workflow}\nState: {}\nLast status: {last}",
            self.model.display_name(),
            self.bindings.len(),
            self.phase.description()
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
struct OracleControlJson {
    action: Option<String>,
    op: Option<String>,
    operation: Option<String>,
    to: Option<String>,
    recipient: Option<String>,
    target: Option<String>,
    participant: Option<OracleControlParticipantValue>,
    participants: Option<Vec<OracleControlParticipantValue>>,
    message_for_user: Option<String>,
    message: Option<String>,
    body: Option<String>,
    task_for_orchestrator: Option<String>,
    context_requests: Option<Vec<String>>,
    requests: Option<Vec<String>>,
    query: Option<String>,
    search: Option<String>,
    workflow_id: Option<String>,
    workflow_version: Option<u64>,
    objective: Option<String>,
    summary: Option<String>,
    status: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct OracleControlParticipantJson {
    address: Option<String>,
    id: Option<String>,
    name: Option<String>,
    kind: Option<String>,
    role: Option<String>,
    visibility: Option<String>,
    target: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum OracleControlParticipantValue {
    Address(String),
    Detailed(OracleControlParticipantJson),
}

pub(crate) fn oracle_history_cell(title: &str, body: &str) -> PlainHistoryCell {
    let mut lines = vec![Line::from(title.to_string().cyan().bold())];
    lines.extend(body.lines().map(|line| Line::from(line.to_string())));
    PlainHistoryCell::new(lines)
}

pub(crate) fn generate_session_slug(thread_id: ThreadId) -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis())
        .unwrap_or_default();
    let short: String = thread_id.to_string().chars().take(8).collect();
    format!("codex-oracle-{short}-{millis}")
}

fn oracle_control_schema_help() -> &'static str {
    "If you need machine control, append one final fenced block with language `oracle_control` and JSON body.\n\
Preferred schema:\n\
{\"op\":\"reply|handoff|request_context|finish\",\"message\":\"optional\",\"message_for_user\":\"optional\",\"context_requests\":[\"optional\"],\"workflow_id\":\"optional\",\"workflow_version\":0,\"objective\":\"optional\",\"summary\":\"optional\",\"status\":\"optional\"}\n\
Compatibility: older callers may still send legacy `delegate`/`send`/`spawn`-style payloads. Codex only honors them when they clearly map to a human reply or an orchestrator handoff. Obsolete routing discovery ops such as `list`/`search` may be rejected."
}

fn oracle_workflow_snapshot(state: &OracleSupervisorState) -> String {
    let orchestrator = state
        .orchestrator_thread_id
        .map(|id| format!("thread {id}"))
        .unwrap_or_else(|| "not attached".to_string());
    let last_task = state.last_orchestrator_task.as_deref().unwrap_or("none");
    let workflow = state.workflow.as_ref().map_or_else(
        || "No workflow is currently attached. This conversation is in chat mode until you explicitly hand work off to the orchestrator.".to_string(),
        |workflow| {
            format!(
                "workflow_id: {}\nmode: {}\nstatus: {}\nversion: {}\nobjective: {}\nsummary: {}\nlast_checkpoint: {}\nlast_blocker: {}\norchestrator_cache: {}",
                workflow.workflow_id,
                workflow.mode.label(),
                workflow.status.label(),
                workflow.version,
                workflow.objective.as_deref().unwrap_or("none"),
                workflow.summary.as_deref().unwrap_or("none"),
                workflow.last_checkpoint.as_deref().unwrap_or("none"),
                workflow.last_blocker.as_deref().unwrap_or("none"),
                workflow
                    .orchestrator_thread_id
                    .map(|id| id.to_string())
                    .unwrap_or_else(|| orchestrator.clone())
            )
        },
    );
    format!(
        "Workflow contract:\n\
- Stay conversation-first: the human talks only in this Oracle thread.\n\
- If you need execution, use `handoff` to the orchestrator. Workers are private orchestrator details, not Oracle-managed peers.\n\
- If you need more repository state, use `request_context`.\n\
- Oracle turns are slow and expensive, so only checkpoint at major milestones, blockers, or completion.\n\
- Thread ids are runtime cache only. Workflow continuity comes from snapshot state, not from persistent agent-thread mappings.\n\
Known state:\n\
- orchestrator_cache: {orchestrator}\n\
- last_orchestrator_task: {last_task}\n\
Workflow snapshot:\n\
{workflow}"
    )
}

pub(crate) fn build_oracle_workflow_reminder(
    workflow: &OracleWorkflowBinding,
    oracle_thread_id: ThreadId,
    task: &str,
) -> String {
    let objective = workflow.objective.as_deref().unwrap_or("none");
    format!(
        "Oracle workflow contract:\n\
- Oracle thread: {oracle_thread_id}\n\
- Workflow: `{}` version {}\n\
- Objective: {objective}\n\
- Oracle turns are slow and expensive. Do substantial work before escalating.\n\
- Workers are private implementation details. Report only a major milestone, blocker, or final completion in this orchestrator thread.\n\
\nTask from Oracle:\n{task}",
        workflow.workflow_id, workflow.version,
    )
}

fn build_oracle_prompt(
    mode: &str,
    intro: &str,
    state: &OracleSupervisorState,
    sections: Vec<(&str, String)>,
) -> String {
    let mut parts = vec![
        "You are Oracle, a remote reasoning collaborator connected to a Codex harness.".to_string(),
        format!("Mode: {mode}"),
        intro.trim().to_string(),
        oracle_control_schema_help().to_string(),
        oracle_workflow_snapshot(state),
    ];
    for (label, body) in sections {
        let body = body.trim();
        if !body.is_empty() {
            parts.push(format!("{label}:\n{body}"));
        }
    }
    parts.join("\n\n")
}

pub(crate) fn build_user_turn_prompt(state: &OracleSupervisorState, user_text: &str) -> String {
    build_oracle_prompt(
        if state.workflow.is_some() {
            "workflow_supervision"
        } else {
            "direct_chat"
        },
        "Make the best useful progress from the current context. Prefer direct prose. Only use machine control when you are handing work to the orchestrator, asking for targeted context, or marking a workflow milestone complete.",
        state,
        vec![("Human message", user_text.trim().to_string())],
    )
}

pub(crate) fn build_checkpoint_prompt(
    state: &OracleSupervisorState,
    thread: &Thread,
    git_status: &str,
    diff_stat: &str,
) -> String {
    build_oracle_prompt(
        "checkpoint_review",
        "You are continuing the same long-lived collaboration. Review the orchestrator workflow checkpoint, keep Oracle turns sparse, and only escalate when the milestone needs new direction, more context, or completion.",
        state,
        vec![
            ("Orchestrator thread", thread.id.to_string()),
            ("Orchestrator checkpoint", summarize_thread(thread)),
            ("Git status", git_status.trim().to_string()),
            ("Git diff stat", diff_stat.trim().to_string()),
        ],
    )
}

pub(crate) fn build_context_prompt(
    state: &OracleSupervisorState,
    requests: &[String],
    context: &str,
) -> String {
    build_oracle_prompt(
        "planner_review",
        "You asked for additional repository context. Continue from this delta context without resetting the conversation. When requesting more context, ask only for targeted deltas.",
        state,
        vec![
            ("Requested context", requests.join("\n")),
            ("Resolved context", context.trim().to_string()),
        ],
    )
}

pub(crate) fn summarize_thread(thread: &Thread) -> String {
    let Some(turn) = thread.turns.last() else {
        return "No orchestrator turns available yet.".to_string();
    };
    let mut lines = vec![format!("turn_status: {:?}", turn.status)];
    if let Some(error) = &turn.error {
        lines.push(format!("turn_error: {}", error.message));
    }
    for item in &turn.items {
        match item {
            ThreadItem::UserMessage { content, .. } => {
                lines.push(format!("user_input_items: {}", content.len()));
            }
            ThreadItem::AgentMessage { text, .. } => {
                lines.push(format!("assistant: {}", text.trim()));
            }
            ThreadItem::CommandExecution {
                command, exit_code, ..
            } => {
                lines.push(format!("command: {command} (exit={exit_code:?})"));
            }
            ThreadItem::FileChange { changes, .. } => {
                lines.push(format!("file_changes: {}", changes.len()));
            }
            ThreadItem::CollabAgentToolCall {
                tool,
                receiver_thread_ids,
                ..
            } => {
                lines.push(format!(
                    "collab: {:?} -> {}",
                    tool,
                    receiver_thread_ids.len()
                ));
            }
            _ => {}
        }
    }
    lines.join("\n")
}

pub(crate) fn orchestrator_developer_instructions() -> String {
    "You are the hidden orchestrator operating under Oracle supervision. Oracle turns are expensive and slow, so do substantial work before escalating. Break work into milestones, use parallel workers for independent subproblems, keep worker topology private, and only surface a milestone, blocker, or final completion when it materially changes the workflow. End checkpoints with outcome, files changed, tests run, and unresolved blockers.".to_string()
}

fn search_oracle_repo_from_root(root: &Path) -> Option<PathBuf> {
    root.ancestors().find_map(|dir| {
        ["forks/oracle", "external/oracle"]
            .into_iter()
            .map(|suffix| dir.join(suffix))
            .find(|path| {
                path.join("package.json").exists()
                    && path
                        .join("bin")
                        .join("oracle-supervisor-broker.ts")
                        .exists()
            })
    })
}

pub(crate) fn find_oracle_repo(start: &Path) -> Option<PathBuf> {
    search_oracle_repo_from_root(start)
        .or_else(|| {
            env::var_os("SMARTY_CODE_ROOT")
                .map(PathBuf::from)
                .and_then(|root| search_oracle_repo_from_root(root.as_path()))
        })
        .or_else(|| {
            env::var_os("SMARTY_CODEX_REPO_DIR")
                .map(PathBuf::from)
                .and_then(|root| search_oracle_repo_from_root(root.as_path()))
        })
        .or_else(|| {
            env::current_exe()
                .ok()
                .and_then(|exe| exe.parent().and_then(search_oracle_repo_from_root))
        })
}

fn truncate_context(text: &str) -> String {
    if text.chars().count() <= ORACLE_CONTEXT_CHAR_LIMIT {
        return text.to_string();
    }
    let truncated = text
        .chars()
        .take(ORACLE_CONTEXT_CHAR_LIMIT)
        .collect::<String>();
    format!("{truncated}\n...[truncated for Oracle context budget]")
}

fn format_context_block(label: &str, body: &str) -> String {
    format!("{label}\n{}\n", truncate_context(body.trim()))
}

fn has_glob_magic(text: &str) -> bool {
    text.contains('*') || text.contains('?') || text.contains('[')
}

async fn capture_workspace_command(cwd: &Path, args: &[&str]) -> String {
    match Command::new(args[0])
        .args(&args[1..])
        .current_dir(cwd)
        .output()
        .await
    {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !stdout.is_empty() {
                stdout
            } else {
                String::from_utf8_lossy(&output.stderr).trim().to_string()
            }
        }
        Err(error) => format!("{} failed: {error}", args.join(" ")),
    }
}

async fn read_workspace_file(cwd: &Path, file_spec: &str) -> String {
    let display = file_spec.trim();
    if display.is_empty() {
        return "Requested file path was empty.".to_string();
    }

    if Path::new(display).is_absolute() {
        return format!("FILE {display} was rejected because absolute paths are not allowed.");
    }
    if Path::new(display).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return format!("FILE {display} was rejected because it escapes the workspace.");
    }

    let path = cwd.join(display);
    let workspace_root = match fs::canonicalize(cwd).await {
        Ok(path) => path,
        Err(error) => {
            return format!(
                "Workspace root {} could not be resolved: {error}",
                cwd.display()
            );
        }
    };
    match fs::canonicalize(&path).await {
        Ok(real_path) if !real_path.starts_with(&workspace_root) => {
            return format!(
                "FILE {} was rejected because it resolves outside the workspace.",
                path.display()
            );
        }
        Ok(_) => {}
        Err(_) => {}
    }

    match fs::read_to_string(&path).await {
        Ok(contents) => format_context_block(&format!("FILE {}", path.display()), &contents),
        Err(error) => format!("FILE {} could not be read: {error}", path.display()),
    }
}

async fn read_workspace_glob(cwd: &Path, pattern: &str) -> String {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return "glob: pattern was empty.".to_string();
    }
    if Path::new(pattern).is_absolute() {
        return format!("glob:{pattern} was rejected because absolute paths are not allowed.");
    }
    if Path::new(pattern).components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return format!("glob:{pattern} was rejected because it escapes the workspace.");
    }
    let output = Command::new("rg")
        .arg("--files")
        .arg("-g")
        .arg(pattern)
        .current_dir(cwd)
        .output()
        .await;
    let Ok(output) = output else {
        return format!("glob:{pattern} failed because `rg` could not be started.");
    };
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return if stderr.is_empty() {
            format!("glob:{pattern} matched no files.")
        } else {
            format!("glob:{pattern} failed: {stderr}")
        };
    }
    let matches = stdout.lines().collect::<Vec<_>>();
    let shown = matches
        .iter()
        .take(ORACLE_CONTEXT_FILE_LIMIT)
        .copied()
        .collect::<Vec<_>>();
    let mut sections = vec![format!(
        "GLOB {pattern} matched {} file(s). Showing {}.",
        matches.len(),
        shown.len()
    )];
    for matched in shown {
        sections.push(read_workspace_file(cwd, matched).await);
    }
    sections.join("\n\n")
}

async fn resolve_context_request(
    cwd: &Path,
    request: &str,
    orchestrator_summary: Option<&str>,
) -> String {
    let request = request.trim();
    if request.is_empty() {
        return "Empty Oracle context request.".to_string();
    }
    match request {
        "git_status" => format_context_block(
            "GIT STATUS",
            &capture_workspace_command(cwd, &["git", "status", "--short"]).await,
        ),
        "git_diff_stat" => format_context_block(
            "GIT DIFF STAT",
            &capture_workspace_command(cwd, &["git", "diff", "--stat", "--no-ext-diff"]).await,
        ),
        "git_diff" => format_context_block(
            "GIT DIFF",
            &capture_workspace_command(cwd, &["git", "diff", "--no-ext-diff"]).await,
        ),
        "orchestrator_summary" => format_context_block(
            "ORCHESTRATOR SUMMARY",
            orchestrator_summary.unwrap_or("No orchestrator summary is available yet."),
        ),
        _ => {
            if let Some(path) = request.strip_prefix("file:") {
                return read_workspace_file(cwd, path).await;
            }
            if let Some(pattern) = request.strip_prefix("glob:") {
                return read_workspace_glob(cwd, pattern).await;
            }
            if has_glob_magic(request) {
                return read_workspace_glob(cwd, request).await;
            }
            read_workspace_file(cwd, request).await
        }
    }
}

pub(crate) async fn resolve_context_requests(
    cwd: &Path,
    requests: &[String],
    orchestrator_summary: Option<&str>,
) -> String {
    let mut sections = Vec::new();
    for request in requests {
        sections.push(resolve_context_request(cwd, request, orchestrator_summary).await);
    }
    if sections.is_empty() {
        "Oracle requested more context, but no explicit request keys were supplied.".to_string()
    } else {
        sections.join("\n\n")
    }
}

fn normalize_non_empty(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn normalize_non_empty_option(raw: Option<String>) -> Option<String> {
    raw.and_then(|value| normalize_non_empty(value.as_str()))
}

fn infer_participant_kind(address: &str) -> Option<String> {
    let head = address.split(':').next().unwrap_or(address);
    match head.trim().to_ascii_lowercase().as_str() {
        "human" | "user" => Some("human".to_string()),
        "oracle" => Some("oracle".to_string()),
        "orchestrator" => Some("orchestrator".to_string()),
        "worker" => Some("worker".to_string()),
        "context" => Some("context".to_string()),
        _ => None,
    }
}

fn parse_legacy_action(raw: &str) -> Option<OracleAction> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "delegate" => Some(OracleAction::Delegate),
        "request_context" => Some(OracleAction::RequestContext),
        "ask_user" => Some(OracleAction::AskUser),
        "finish" => Some(OracleAction::Finish),
        "reply" => Some(OracleAction::Reply),
        _ => None,
    }
}

fn parse_oracle_control_json(raw: &str) -> Option<OracleControlJson> {
    let candidate = extract_oracle_control_json(raw)
        .or_else(|| extract_json(raw))
        .unwrap_or_else(|| raw.trim().to_string());
    serde_json::from_str::<OracleControlJson>(&candidate).ok()
}

fn normalize_context_requests(control: &OracleControlJson) -> Vec<String> {
    control
        .context_requests
        .clone()
        .unwrap_or_default()
        .into_iter()
        .chain(control.requests.clone().unwrap_or_default())
        .filter_map(|request| normalize_non_empty(request.as_str()))
        .collect()
}

fn normalize_participants(control: &OracleControlJson) -> Vec<OracleControlParticipant> {
    control
        .participant
        .clone()
        .into_iter()
        .chain(control.participants.clone().unwrap_or_default())
        .filter_map(OracleControlParticipant::from_json)
        .collect()
}

pub(crate) fn parse_oracle_control(raw: &str) -> Option<OracleControlDirective> {
    let control = parse_oracle_control_json(raw)?;
    let op = control
        .op
        .as_ref()
        .as_deref()
        .and_then(|op| OracleControlOp::parse(op))
        .or_else(|| {
            control
                .operation
                .as_ref()
                .as_deref()
                .and_then(|op| OracleControlOp::parse(op))
        })
        .or_else(|| {
            control
                .action
                .as_ref()
                .as_deref()
                .and_then(|op| OracleControlOp::parse(op))
        });
    let action_hint = control
        .action
        .as_ref()
        .as_deref()
        .and_then(|action| parse_legacy_action(action));
    let to = normalize_non_empty_option(control.to.clone())
        .or_else(|| normalize_non_empty_option(control.recipient.clone()))
        .or_else(|| normalize_non_empty_option(control.target.clone()));
    let message = normalize_non_empty_option(control.message.clone())
        .or_else(|| normalize_non_empty_option(control.body.clone()))
        .or_else(|| normalize_non_empty_option(control.task_for_orchestrator.clone()));
    let message_for_user = normalize_non_empty_option(control.message_for_user.clone());
    let task_for_orchestrator = normalize_non_empty_option(control.task_for_orchestrator.clone());
    let query = normalize_non_empty_option(control.query.clone())
        .or_else(|| normalize_non_empty_option(control.search.clone()));
    Some(OracleControlDirective {
        op,
        action_hint,
        to,
        participants: normalize_participants(&control),
        message,
        message_for_user,
        task_for_orchestrator,
        context_requests: normalize_context_requests(&control),
        query,
        workflow_id: normalize_non_empty_option(control.workflow_id.clone()),
        workflow_version: control.workflow_version,
        objective: normalize_non_empty_option(control.objective.clone()),
        summary: normalize_non_empty_option(control.summary.clone()),
        status: normalize_non_empty_option(control.status.clone()),
    })
}

fn directive_targets_human(directive: &OracleControlDirective) -> bool {
    directive
        .to
        .as_deref()
        .is_some_and(|target| matches!(target.to_ascii_lowercase().as_str(), "human" | "user"))
        || directive
            .participants
            .iter()
            .any(|participant| matches!(participant.kind.as_deref(), Some("human") | Some("user")))
}

fn directive_targets_delegate(directive: &OracleControlDirective) -> bool {
    if directive.action_hint == Some(OracleAction::Delegate)
        || directive.op == Some(OracleControlOp::Handoff)
    {
        return true;
    }
    let has_explicit_target = directive.to.is_some() || !directive.participants.is_empty();
    match directive.op {
        Some(OracleControlOp::Spawn) | Some(OracleControlOp::Send) => {
            has_explicit_target
                && directive
                    .to
                    .as_deref()
                    .map(|target| {
                        matches!(
                            target.to_ascii_lowercase().as_str(),
                            "orchestrator" | "worker" | "context"
                        ) || target.to_ascii_lowercase().starts_with("worker:")
                    })
                    .unwrap_or_else(|| {
                        directive.participants.iter().any(|participant| {
                            matches!(
                                participant.kind.as_deref(),
                                Some("orchestrator") | Some("worker") | Some("context")
                            )
                        })
                    })
        }
        Some(OracleControlOp::List) | Some(OracleControlOp::Search) => false,
        _ => false,
    }
}

fn delegate_task_from_directive(directive: &OracleControlDirective) -> Option<String> {
    if let Some(task) = directive.task_for_orchestrator.clone() {
        return Some(task);
    }
    let route = directive.to.clone().or_else(|| {
        directive
            .participants
            .first()
            .map(|participant| participant.display_label())
    });
    let detail = directive
        .message
        .clone()
        .or_else(|| directive.query.clone())?;
    match directive.op {
        Some(OracleControlOp::Handoff) => Some(detail),
        Some(OracleControlOp::Spawn) => Some(match route {
            Some(route) => format!("Spawn or route {route}: {detail}"),
            None => format!("Spawn or route a worker: {detail}"),
        }),
        Some(OracleControlOp::List) => Some(match route {
            Some(route) => format!("List via {route}: {detail}"),
            None => format!("List: {detail}"),
        }),
        Some(OracleControlOp::Search) => Some(match route {
            Some(route) => format!("Search via {route}: {detail}"),
            None => format!("Search: {detail}"),
        }),
        Some(OracleControlOp::Send) => Some(match route {
            Some(route) if !route.eq_ignore_ascii_case("orchestrator") => {
                format!("Route to {route}: {detail}")
            }
            _ => detail,
        }),
        _ => Some(detail),
    }
}

fn collapse_oracle_action(directive: &OracleControlDirective) -> OracleAction {
    directive.action_hint.unwrap_or_else(|| match directive.op {
        Some(OracleControlOp::RequestContext) => OracleAction::RequestContext,
        Some(OracleControlOp::Finish) => OracleAction::Finish,
        Some(OracleControlOp::Reply) => OracleAction::Reply,
        Some(OracleControlOp::Handoff) => OracleAction::Delegate,
        Some(OracleControlOp::Send) | Some(OracleControlOp::Spawn) => {
            if directive_targets_human(directive) {
                OracleAction::AskUser
            } else if directive_targets_delegate(directive) {
                OracleAction::Delegate
            } else {
                OracleAction::Reply
            }
        }
        Some(OracleControlOp::List) | Some(OracleControlOp::Search) => OracleAction::Reply,
        None => OracleAction::Reply,
    })
}

fn legacy_control_rejection_message(directive: &OracleControlDirective) -> Option<String> {
    match directive.op {
        Some(OracleControlOp::List) | Some(OracleControlOp::Search) => Some(
            "Legacy Oracle routing discovery is no longer supported. Use `request_context` for repository state or `handoff` for orchestrator work."
                .to_string(),
        ),
        Some(OracleControlOp::Send) | Some(OracleControlOp::Spawn)
            if !directive_targets_human(directive) && !directive_targets_delegate(directive) =>
        {
            Some(
                "Legacy Oracle routing is missing a supported destination. Use `handoff` for orchestrator work or `reply` for the human."
                    .to_string(),
            )
        }
        _ => None,
    }
}

pub(crate) fn parse_oracle_response(raw: &str) -> OracleResponse {
    if let Some(directive) = parse_oracle_control(raw) {
        let action = collapse_oracle_action(&directive);
        let rejection_message = legacy_control_rejection_message(&directive);
        let message_for_user = rejection_message.unwrap_or_else(|| {
            directive
                .message_for_user
                .clone()
                .unwrap_or_else(|| match action {
                    OracleAction::Reply | OracleAction::AskUser | OracleAction::Finish => directive
                        .message
                        .clone()
                        .or_else(|| directive.query.clone())
                        .unwrap_or_else(|| raw.trim().to_string()),
                    _ => String::new(),
                })
        });
        let context_requests = directive.context_requests.clone();
        return OracleResponse {
            action,
            message_for_user,
            task_for_orchestrator: if matches!(action, OracleAction::Delegate) {
                delegate_task_from_directive(&directive)
            } else {
                directive.task_for_orchestrator.clone()
            },
            context_requests,
            directive: Some(directive),
        };
    }
    OracleResponse {
        action: OracleAction::Reply,
        message_for_user: raw.trim().to_string(),
        task_for_orchestrator: None,
        context_requests: Vec::new(),
        directive: None,
    }
}

fn extract_oracle_control_json(raw: &str) -> Option<String> {
    let marker = "```oracle_control";
    let start = raw.rfind(marker)?;
    let after = &raw[start + marker.len()..];
    let end = after.find("```")?;
    let body = after[..end].trim();
    (!body.is_empty()).then(|| body.to_string())
}

fn extract_json(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.starts_with('{') && trimmed.ends_with('}') {
        return Some(trimmed.to_string());
    }
    let start = trimmed.find('{')?;
    let end = trimmed.rfind('}')?;
    (start < end).then(|| trimmed[start..=end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsString;

    struct EnvVarGuard {
        key: &'static str,
        original: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: &Path) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, original }
        }

        fn remove(key: &'static str) -> Self {
            let original = std::env::var_os(key);
            unsafe {
                std::env::remove_var(key);
            }
            Self { key, original }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.original {
                    Some(value) => std::env::set_var(self.key, value),
                    None => std::env::remove_var(self.key),
                }
            }
        }
    }

    #[test]
    fn parse_oracle_command_accepts_fast_style_args() {
        assert_eq!(OracleCommand::parse(""), Ok(OracleCommand::Browse));
        assert_eq!(OracleCommand::parse("browse"), Ok(OracleCommand::Browse));
        assert_eq!(OracleCommand::parse("new"), Ok(OracleCommand::NewThread));
        assert_eq!(
            OracleCommand::parse("attach abc-123"),
            Ok(OracleCommand::AttachThread("abc-123".to_string()))
        );
        assert_eq!(OracleCommand::parse("on"), Ok(OracleCommand::On));
        assert_eq!(OracleCommand::parse("off"), Ok(OracleCommand::Off));
        assert_eq!(OracleCommand::parse("status"), Ok(OracleCommand::Status));
        assert_eq!(OracleCommand::parse("info"), Ok(OracleCommand::Status));
        assert_eq!(
            OracleCommand::parse("model"),
            Ok(OracleCommand::Model(None))
        );
        assert_eq!(
            OracleCommand::parse("model pro"),
            Ok(OracleCommand::Model(Some(OracleModelPreset::Pro)))
        );
        assert_eq!(
            OracleCommand::parse("model thinking"),
            Ok(OracleCommand::Model(Some(OracleModelPreset::Thinking)))
        );
    }

    #[test]
    fn parse_oracle_response_falls_back_to_plain_reply() {
        let parsed = parse_oracle_response("plain text");
        assert_eq!(parsed.action, OracleAction::Reply);
        assert_eq!(parsed.message_for_user, "plain text");
    }

    #[test]
    fn parse_oracle_response_reads_json_payload() {
        let parsed = parse_oracle_response(
            r#"{"action":"delegate","message_for_user":"Working.","task_for_orchestrator":"Ship it"}"#,
        );
        assert_eq!(parsed.action, OracleAction::Delegate);
        assert_eq!(parsed.task_for_orchestrator.as_deref(), Some("Ship it"));
    }

    #[test]
    fn parse_oracle_control_reads_workflow_handoff_directive() {
        let parsed = parse_oracle_control(
            r#"{"op":"handoff","message":"Audit the diff","workflow_id":"oracle-routing","workflow_version":3,"objective":"Ship the Oracle routing patch","summary":"Diff review milestone","status":"running"}"#,
        )
        .expect("directive");

        assert_eq!(parsed.op, Some(OracleControlOp::Handoff));
        assert_eq!(parsed.message.as_deref(), Some("Audit the diff"));
        assert_eq!(parsed.workflow_id.as_deref(), Some("oracle-routing"));
        assert_eq!(parsed.workflow_version, Some(3));
        assert_eq!(
            parsed.objective.as_deref(),
            Some("Ship the Oracle routing patch")
        );
        assert_eq!(parsed.summary.as_deref(), Some("Diff review milestone"));
        assert_eq!(parsed.status.as_deref(), Some("running"));
    }

    #[test]
    fn parse_oracle_response_collapses_addressed_send_to_delegate() {
        let parsed = parse_oracle_response(
            r#"{"op":"send","to":"orchestrator","message":"Implement milestone A"}"#,
        );

        assert_eq!(parsed.action, OracleAction::Delegate);
        assert_eq!(
            parsed.task_for_orchestrator.as_deref(),
            Some("Implement milestone A")
        );
        assert!(parsed.message_for_user.is_empty());
    }

    #[test]
    fn parse_oracle_response_collapses_human_send_to_ask_user() {
        let parsed = parse_oracle_response(
            r#"{"op":"send","to":"human","message":"Need clarification on the rollout plan."}"#,
        );

        assert_eq!(parsed.action, OracleAction::AskUser);
        assert_eq!(
            parsed.message_for_user,
            "Need clarification on the rollout plan."
        );
        assert_eq!(parsed.task_for_orchestrator, None);
    }

    #[test]
    fn parse_oracle_response_reads_context_requests() {
        let parsed = parse_oracle_response(
            r#"{"action":"request_context","message_for_user":"Need files.","context_requests":["git_diff","file:src/main.rs"]}"#,
        );
        assert_eq!(parsed.action, OracleAction::RequestContext);
        assert_eq!(
            parsed.context_requests,
            vec!["git_diff".to_string(), "file:src/main.rs".to_string()]
        );
    }

    #[test]
    fn parse_oracle_response_maps_handoff_to_delegate_with_workflow_metadata() {
        let parsed = parse_oracle_response(
            r#"{"op":"handoff","message":"Implement milestone A","workflow_id":"oracle-routing","workflow_version":7,"objective":"Ship the workflow refactor","summary":"Ready for implementation","status":"running"}"#,
        );

        assert_eq!(parsed.action, OracleAction::Delegate);
        assert_eq!(
            parsed.task_for_orchestrator.as_deref(),
            Some("Implement milestone A")
        );
        let directive = parsed.directive.expect("directive");
        assert_eq!(directive.workflow_id.as_deref(), Some("oracle-routing"));
        assert_eq!(directive.workflow_version, Some(7));
        assert_eq!(
            directive.objective.as_deref(),
            Some("Ship the workflow refactor")
        );
        assert_eq!(
            directive.summary.as_deref(),
            Some("Ready for implementation")
        );
        assert_eq!(directive.status.as_deref(), Some("running"));
    }

    #[test]
    fn parse_oracle_response_rejects_legacy_search_ops() {
        let parsed =
            parse_oracle_response(r#"{"op":"search","query":"find oracle delegate callers"}"#);

        assert_eq!(parsed.action, OracleAction::Reply);
        assert!(parsed.task_for_orchestrator.is_none());
        assert!(
            parsed
                .message_for_user
                .contains("routing discovery is no longer supported")
        );
    }

    #[test]
    fn parse_oracle_response_rejects_ambiguous_legacy_send_without_destination() {
        let parsed = parse_oracle_response(r#"{"op":"send","message":"Do the work quietly"}"#);

        assert_eq!(parsed.action, OracleAction::Reply);
        assert!(parsed.task_for_orchestrator.is_none());
        assert!(
            parsed
                .message_for_user
                .contains("routing is missing a supported destination")
        );
    }

    #[test]
    fn parse_oracle_response_maps_search_into_delegate_task() {
        let parsed = parse_oracle_response(
            r#"{"op":"search","to":"worker:searcher","query":"find oracle delegate callers"}"#,
        );

        assert_eq!(parsed.action, OracleAction::Reply);
        assert!(parsed.task_for_orchestrator.is_none());
        assert!(
            parsed
                .message_for_user
                .contains("routing discovery is no longer supported")
        );
    }

    #[test]
    fn parse_oracle_response_keeps_unaddressed_search_as_reply() {
        let parsed =
            parse_oracle_response(r#"{"op":"search","query":"find oracle delegate callers"}"#);

        assert_eq!(parsed.action, OracleAction::Reply);
        assert_eq!(parsed.task_for_orchestrator, None);
        assert!(
            parsed
                .message_for_user
                .contains("routing discovery is no longer supported")
        );
    }

    #[test]
    fn build_user_turn_prompt_includes_workflow_contract_and_schema() {
        let prompt = build_user_turn_prompt(&OracleSupervisorState::default(), "Ship the patch.");

        assert!(prompt.contains("Workflow contract"));
        assert!(prompt.contains("reply|handoff|request_context|finish"));
        assert!(prompt.contains("chat mode"));
        assert!(prompt.contains("Ship the patch."));
    }

    #[test]
    fn build_user_turn_prompt_includes_workflow_snapshot_when_supervising() {
        let oracle_thread_id = ThreadId::new();
        let state = OracleSupervisorState {
            oracle_thread_id: Some(oracle_thread_id),
            orchestrator_thread_id: Some(ThreadId::new()),
            workflow: Some(OracleWorkflowBinding {
                workflow_id: "oracle-routing".to_string(),
                mode: OracleWorkflowMode::Supervising,
                status: OracleWorkflowStatus::Running,
                version: 4,
                objective: Some("Ship the Oracle workflow refactor".to_string()),
                summary: Some("Checkpoint ready for review".to_string()),
                last_checkpoint: Some("Tests are green".to_string()),
                last_blocker: None,
                orchestrator_thread_id: None,
            }),
            ..Default::default()
        };

        let prompt = build_user_turn_prompt(&state, "Ship the patch.");

        assert!(prompt.contains("workflow_id: oracle-routing"));
        assert!(prompt.contains("status: running"));
        assert!(prompt.contains("version: 4"));
        assert!(prompt.contains("Ship the Oracle workflow refactor"));
        assert!(prompt.contains("Checkpoint ready for review"));
    }

    #[test]
    fn build_oracle_workflow_reminder_mentions_workflow_thread_and_task() {
        let oracle_thread_id = ThreadId::new();
        let reminder = build_oracle_workflow_reminder(
            &OracleWorkflowBinding {
                workflow_id: "oracle-routing".to_string(),
                mode: OracleWorkflowMode::Supervising,
                status: OracleWorkflowStatus::Running,
                version: 2,
                objective: Some("Break the feature into milestones.".to_string()),
                ..Default::default()
            },
            oracle_thread_id,
            "Break the feature into milestones.",
        );

        assert!(reminder.contains("Oracle workflow contract"));
        assert!(reminder.contains("oracle-routing"));
        assert!(reminder.contains(oracle_thread_id.to_string().as_str()));
        assert!(reminder.contains("slow and expensive"));
        assert!(reminder.contains("Break the feature into milestones."));
    }

    #[test]
    fn build_checkpoint_and_context_prompts_include_workflow_snapshot() {
        let thread_id = ThreadId::new();
        let state = OracleSupervisorState {
            oracle_thread_id: Some(thread_id),
            workflow: Some(OracleWorkflowBinding {
                workflow_id: "oracle-routing".to_string(),
                mode: OracleWorkflowMode::Supervising,
                status: OracleWorkflowStatus::Running,
                version: 5,
                objective: Some("Ship the workflow refactor".to_string()),
                summary: Some("Checkpoint ready".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };
        let thread = Thread {
            id: thread_id.to_string(),
            preview: "preview".to_string(),
            ephemeral: false,
            model_provider: "provider".to_string(),
            created_at: 0,
            updated_at: 0,
            status: codex_app_server_protocol::ThreadStatus::Idle,
            path: None,
            cwd: PathBuf::from("/tmp/oracle"),
            cli_version: "0.0.0".to_string(),
            source: codex_app_server_protocol::SessionSource::Unknown,
            agent_nickname: Some("Oracle Orchestrator".to_string()),
            agent_role: Some("orchestrator".to_string()),
            git_info: None,
            name: Some("Oracle Orchestrator".to_string()),
            turns: Vec::new(),
        };

        let checkpoint_prompt = build_checkpoint_prompt(&state, &thread, "M file.rs", "1 file");
        let context_prompt = build_context_prompt(
            &state,
            &["file:src/app.rs".to_string()],
            "src/app.rs contents",
        );

        for prompt in [checkpoint_prompt, context_prompt] {
            assert!(prompt.contains("workflow_id: oracle-routing"));
            assert!(prompt.contains("version: 5"));
            assert!(prompt.contains("Ship the workflow refactor"));
        }
    }

    #[tokio::test]
    async fn resolve_context_requests_supports_files_globs_and_orchestrator_summary() {
        let temp = tempfile::tempdir().expect("tempdir");
        let src = temp.path().join("src");
        std::fs::create_dir_all(&src).expect("mkdir");
        std::fs::write(src.join("lib.rs"), "fn demo() {}\n").expect("write");

        let resolved = resolve_context_requests(
            temp.path(),
            &[
                "file:src/lib.rs".to_string(),
                "glob:src/**/*.rs".to_string(),
                "orchestrator_summary".to_string(),
            ],
            Some("worker milestone complete"),
        )
        .await;

        assert!(resolved.contains("fn demo() {}"));
        assert!(resolved.contains("worker milestone complete"));
        assert!(resolved.contains("GLOB src/**/*.rs matched 1 file(s)."));
    }

    #[tokio::test]
    async fn resolve_context_requests_rejects_paths_outside_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let absolute_path = temp.path().join("absolute.txt").display().to_string();

        let resolved = resolve_context_requests(
            temp.path(),
            &[
                absolute_path,
                "../secret.txt".to_string(),
                "glob:../**/*.txt".to_string(),
            ],
            /*orchestrator_summary*/ None,
        )
        .await;

        assert!(resolved.contains("absolute paths are not allowed"));
        assert!(resolved.contains("escapes the workspace"));
        assert!(resolved.contains("glob:../**/*.txt was rejected"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn resolve_context_requests_rejects_symlink_escape() {
        let temp = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("outside");
        let outside_file = outside.path().join("secret.txt");
        let linked = temp.path().join("linked.txt");

        std::fs::write(&outside_file, "secret\n").expect("write");
        std::os::unix::fs::symlink(&outside_file, &linked).expect("symlink");

        let resolved = resolve_context_requests(
            temp.path(),
            &["linked.txt".to_string()],
            /*orchestrator_summary*/ None,
        )
        .await;

        assert!(resolved.contains("resolves outside the workspace"));
    }

    #[test]
    fn oracle_model_preset_maps_to_expected_browser_values() {
        assert_eq!(OracleModelPreset::Pro.model_id(), "gpt-5.4-pro");
        assert_eq!(OracleModelPreset::Pro.browser_label(), "GPT-5.4 Pro");
        assert_eq!(OracleModelPreset::Thinking.model_id(), "gpt-5.4");
        assert_eq!(OracleModelPreset::Thinking.browser_label(), "Thinking 5.4");
    }

    #[test]
    fn find_oracle_repo_prefers_forks_checkout() {
        let temp = tempfile::tempdir().expect("tempdir");
        let forks = temp.path().join("forks").join("oracle");
        let external = temp.path().join("external").join("oracle");
        std::fs::create_dir_all(forks.join("bin")).expect("mkdir forks");
        std::fs::create_dir_all(&external).expect("mkdir external");
        std::fs::write(forks.join("package.json"), "{}\n").expect("write forks package");
        std::fs::write(forks.join("bin").join("oracle-supervisor-broker.ts"), "")
            .expect("write forks broker");
        std::fs::write(external.join("package.json"), "{}\n").expect("write external package");

        assert_eq!(
            find_oracle_repo(temp.path()).as_deref(),
            Some(forks.as_path())
        );
    }

    #[test]
    #[serial]
    fn find_oracle_repo_uses_smarty_codex_repo_dir_when_workspace_is_outside_project() {
        let temp = tempfile::tempdir().expect("tempdir");
        let project = temp.path().join("smarty-code");
        let codex = project.join("forks").join("codex");
        let forks = project.join("forks").join("oracle");
        let outside = temp.path().join("workspace");

        std::fs::create_dir_all(codex.join("codex-rs")).expect("mkdir codex");
        std::fs::create_dir_all(forks.join("bin")).expect("mkdir oracle");
        std::fs::create_dir_all(&outside).expect("mkdir workspace");
        std::fs::write(forks.join("package.json"), "{}\n").expect("write oracle package");
        std::fs::write(forks.join("bin").join("oracle-supervisor-broker.ts"), "")
            .expect("write oracle broker");

        let _code_root_guard = EnvVarGuard::remove("SMARTY_CODE_ROOT");
        let _repo_guard = EnvVarGuard::set("SMARTY_CODEX_REPO_DIR", codex.as_path());

        assert_eq!(
            find_oracle_repo(outside.as_path()).as_deref(),
            Some(forks.as_path())
        );
    }
}
