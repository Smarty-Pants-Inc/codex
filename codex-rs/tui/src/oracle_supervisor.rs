use std::collections::HashMap;
use std::collections::HashSet;
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
use url::Url;

use crate::history_cell::PlainHistoryCell;

const ORACLE_CONTEXT_FILE_LIMIT: usize = 4;
const ORACLE_CONTEXT_CHAR_LIMIT: usize = 12_000;
pub(crate) const ORACLE_CONTROL_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OracleCommand {
    Browse,
    NewThread,
    AttachThread {
        conversation_id: String,
        import_history: bool,
    },
    On,
    Off,
    Status,
    Model(Option<OracleModelPreset>),
}

impl OracleCommand {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        const USAGE: &str = "Usage: /oracle [browse|new|attach <conversation_id|chatgpt_thread_url> [history|--import-history]|on|off|info|model [pro [standard|extended]|thinking]]";
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
            "attach" if parts.len() >= 2 && !parts[1].trim().is_empty() => {
                let import_history =
                    match parts.get(2).map(|value| value.trim().to_ascii_lowercase()) {
                        None => false,
                        Some(flag)
                            if flag == "--import-history"
                                || flag == "import-history"
                                || flag == "history" =>
                        {
                            true
                        }
                        _ => return Err(USAGE.to_string()),
                    };
                if parts.len() > 3 {
                    return Err(USAGE.to_string());
                }
                Ok(Self::AttachThread {
                    conversation_id: normalize_oracle_conversation_target(parts[1])
                        .ok_or_else(|| USAGE.to_string())?,
                    import_history,
                })
            }
            "on" if parts.len() == 1 => Ok(Self::On),
            "off" if parts.len() == 1 => Ok(Self::Off),
            "status" | "info" if parts.len() == 1 => Ok(Self::Status),
            "model" if parts.len() == 1 => Ok(Self::Model(None)),
            "model" => OracleModelPreset::parse_parts(&parts[1..])
                .map(|model| Self::Model(Some(model)))
                .ok_or_else(|| USAGE.to_string()),
            _ => Err(USAGE.to_string()),
        }
    }
}

fn normalize_oracle_conversation_target(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if is_oracle_conversation_id(trimmed) {
        return Some(trimmed.to_string());
    }
    let parsed = Url::parse(trimmed).ok()?;
    if parsed.scheme() != "https" {
        return None;
    }
    let host = parsed.host_str()?.to_ascii_lowercase();
    if host != "chatgpt.com" && host != "chat.openai.com" {
        return None;
    }
    let segments = parsed
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    let conversation_id = match segments.as_slice() {
        ["c", conversation_id]
        | ["g", _, "c", conversation_id]
        | ["g", _, "project", "c", conversation_id] => conversation_id,
        _ => return None,
    };
    is_oracle_conversation_id(conversation_id).then(|| conversation_id.to_string())
}

fn is_oracle_conversation_id(raw: &str) -> bool {
    !raw.is_empty()
        && raw
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OracleModelPreset {
    Pro,
    ProExtended,
    Thinking,
}

impl OracleModelPreset {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "pro" | "pro-standard" | "pro_standard" | "gpt-5.4-pro" => Some(Self::Pro),
            "pro-extended" | "pro_extended" => Some(Self::ProExtended),
            "thinking" | "gpt-5.4" => Some(Self::Thinking),
            _ => None,
        }
    }

    fn parse_parts(parts: &[&str]) -> Option<Self> {
        match parts {
            [] => None,
            [model] => Self::parse(model),
            [model, level] => match model.trim().to_ascii_lowercase().as_str() {
                "pro" | "gpt-5.4-pro" => match level.trim().to_ascii_lowercase().as_str() {
                    "standard" => Some(Self::Pro),
                    "extended" => Some(Self::ProExtended),
                    _ => None,
                },
                _ => None,
            },
            _ => None,
        }
    }

    pub(crate) fn model_id(self) -> &'static str {
        match self {
            Self::Pro | Self::ProExtended => "gpt-5.4-pro",
            Self::Thinking => "gpt-5.4",
        }
    }

    pub(crate) fn browser_label(self) -> &'static str {
        match self {
            Self::Pro | Self::ProExtended => "GPT-5.4 Pro",
            Self::Thinking => "Thinking 5.4",
        }
    }

    pub(crate) fn picker_label(self) -> &'static str {
        match self {
            Self::Pro => "GPT-5.4 Pro (Standard)",
            Self::ProExtended => "GPT-5.4 Pro (Extended)",
            Self::Thinking => "Thinking 5.4",
        }
    }

    pub(crate) fn display_name(self) -> &'static str {
        match self {
            Self::Pro => "gpt-5.4-pro (Standard)",
            Self::ProExtended => "gpt-5.4-pro (Extended)",
            Self::Thinking => "gpt-5.4 (Thinking 5.4)",
        }
    }

    pub(crate) fn browser_thinking_time(self) -> Option<&'static str> {
        match self {
            Self::Pro => Some("standard"),
            Self::ProExtended => Some("extended"),
            Self::Thinking => None,
        }
    }

    pub(crate) fn is_pro(self) -> bool {
        matches!(self, Self::Pro | Self::ProExtended)
    }

    pub(crate) fn toggle_family(self) -> Self {
        match self {
            Self::Pro | Self::ProExtended => Self::Thinking,
            Self::Thinking => Self::Pro,
        }
    }

    fn resolve_default(raw: Option<&str>) -> Self {
        raw.and_then(Self::parse).unwrap_or(Self::Pro)
    }
}

impl Default for OracleModelPreset {
    fn default() -> Self {
        Self::resolve_default(env::var("CODEX_ORACLE_MODEL_PRESET").ok().as_deref())
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
    Checkpoint,
    Finish,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OracleControlOp {
    Reply,
    Handoff,
    Checkpoint,
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
            "checkpoint" => Some(Self::Checkpoint),
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
    pub(crate) schema_version: Option<u64>,
    pub(crate) op_id: Option<String>,
    pub(crate) idempotency_key: Option<String>,
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
    pub(crate) pending_op_ids: HashSet<String>,
    pub(crate) pending_idempotency_keys: HashSet<String>,
    pub(crate) applied_op_ids: HashSet<String>,
    pub(crate) applied_idempotency_keys: HashSet<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleRunRequest {
    pub(crate) oracle_thread_id: ThreadId,
    pub(crate) kind: OracleRequestKind,
    pub(crate) session_slug: String,
    pub(crate) prompt: String,
    pub(crate) requested_prompt: String,
    pub(crate) source_user_text: Option<String>,
    pub(crate) files: Vec<String>,
    pub(crate) workspace_cwd: PathBuf,
    pub(crate) oracle_repo: PathBuf,
    pub(crate) followup_session: Option<String>,
    pub(crate) model: OracleModelPreset,
    pub(crate) browser_model_strategy: String,
    pub(crate) browser_model_label: Option<String>,
    pub(crate) browser_thinking_time: Option<String>,
    pub(crate) requires_control: bool,
    pub(crate) repair_attempt: u8,
    pub(crate) transport_retry_attempt: u8,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleRunResult {
    pub(crate) run_id: String,
    pub(crate) oracle_thread_id: ThreadId,
    pub(crate) kind: OracleRequestKind,
    pub(crate) requested_slug: String,
    pub(crate) session_id: String,
    pub(crate) requested_prompt: String,
    pub(crate) source_user_text: Option<String>,
    pub(crate) files: Vec<String>,
    pub(crate) requires_control: bool,
    pub(crate) repair_attempt: u8,
    pub(crate) response: OracleResponse,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleResponse {
    pub(crate) action: OracleAction,
    pub(crate) message_for_user: String,
    pub(crate) task_for_orchestrator: Option<String>,
    pub(crate) context_requests: Vec<String>,
    pub(crate) directive: Option<OracleControlDirective>,
    pub(crate) control_issue: Option<OracleControlIssue>,
    pub(crate) raw_output: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OracleSessionOwnership {
    BrokerThread,
    SessionRootSlug(String),
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OracleThreadBinding {
    pub(crate) session_root_slug: Option<String>,
    pub(crate) current_session_id: Option<String>,
    pub(crate) current_session_ownership: Option<OracleSessionOwnership>,
    pub(crate) current_session_verified: bool,
    pub(crate) orchestrator_thread_id: Option<ThreadId>,
    pub(crate) workflow: Option<OracleWorkflowBinding>,
    pub(crate) phase: OracleSupervisorPhase,
    pub(crate) active_run_id: Option<String>,
    pub(crate) aborted_run_id: Option<String>,
    pub(crate) last_status: Option<String>,
    pub(crate) last_orchestrator_task: Option<String>,
    pub(crate) pending_turn_id: Option<String>,
    pub(crate) automatic_context_followups: u8,
    pub(crate) accept_user_turn_workflow_replacement: bool,
    pub(crate) conversation_id: Option<String>,
    pub(crate) remote_title: Option<String>,
    pub(crate) participants: HashMap<String, OracleRoutingParticipant>,
    pub(crate) pending_checkpoint_threads: Vec<ThreadId>,
    pub(crate) pending_checkpoint_versions: HashMap<ThreadId, u64>,
    pub(crate) pending_checkpoint_turn_ids: HashMap<ThreadId, String>,
    pub(crate) last_checkpoint_turn_ids: HashMap<ThreadId, String>,
    pub(crate) active_checkpoint_thread_id: Option<ThreadId>,
    pub(crate) active_checkpoint_turn_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OracleSupervisorState {
    /// Active visible Oracle thread (legacy working view for in-flight workflows).
    pub(crate) oracle_thread_id: Option<ThreadId>,
    /// Legacy working view fields mirrored from `bindings[oracle_thread_id]`.
    pub(crate) session_root_slug: Option<String>,
    pub(crate) current_session_id: Option<String>,
    pub(crate) current_session_ownership: Option<OracleSessionOwnership>,
    pub(crate) orchestrator_thread_id: Option<ThreadId>,
    pub(crate) workflow: Option<OracleWorkflowBinding>,
    pub(crate) phase: OracleSupervisorPhase,
    pub(crate) active_run_id: Option<String>,
    pub(crate) aborted_run_id: Option<String>,
    pub(crate) model: OracleModelPreset,
    pub(crate) last_status: Option<String>,
    pub(crate) last_orchestrator_task: Option<String>,
    pub(crate) pending_turn_id: Option<String>,
    pub(crate) automatic_context_followups: u8,
    pub(crate) accept_user_turn_workflow_replacement: bool,
    pub(crate) active_checkpoint_thread_id: Option<ThreadId>,
    pub(crate) active_checkpoint_turn_id: Option<String>,
    /// Canonical Oracle browser runs keyed by visible Oracle thread.
    pub(crate) inflight_runs: HashMap<ThreadId, String>,
    /// Original Oracle requests keyed by in-flight run id so transport failures can be retried safely.
    pub(crate) inflight_run_requests: HashMap<String, OracleRunRequest>,
    /// Aborted Oracle run ids that may still report back asynchronously.
    pub(crate) aborted_runs: HashSet<String>,
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
        let active_session = self
            .current_session_id
            .clone()
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
            "Oracle mode: {enabled}\nRequested model: {}\nBrowser strategy: select\nActive oracle thread: {active}\nActive session: {active_session}\nTracked oracle threads: {}\nWorkflow: {workflow}\nState: {}\nLast status: {last}",
            self.model.display_name(),
            self.bindings.len(),
            self.phase.description()
        )
    }
}

#[derive(Debug, Clone, Deserialize)]
struct OracleControlJson {
    schema_version: Option<u64>,
    op_id: Option<String>,
    idempotency_key: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum OracleControlIssueKind {
    MultipleBlocks,
    TrailingAfterBlock,
    MalformedJson,
    InvalidSchema,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct OracleControlIssue {
    pub(crate) kind: OracleControlIssueKind,
    pub(crate) message: String,
}

impl OracleControlIssue {
    fn new(kind: OracleControlIssueKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
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
	The final fenced block may use `oracle_control` plus extra fence attributes, but there must be exactly one control block and nothing after it except whitespace.\n\
	Every control block must include `schema_version`, `op_id`, and `idempotency_key`.\n\
	When continuing an attached workflow, every control block must also echo the current `workflow_id` and `workflow_version`.\n\
	When starting a new workflow, including the first workflow or one that follows a terminal completion or failure, choose a fresh explicit `workflow_id` and a positive `workflow_version` starting at 1 instead of relying on Codex to synthesize them.\n\
	Use `finish` only when the workflow is truly terminal. Use `checkpoint` when you need to surface a completed milestone but the workflow continues.\n\
	Preferred schema:\n\
	{\"schema_version\":1,\"op_id\":\"unique-per-control-op\",\"idempotency_key\":\"stable-for-safe-retries\",\"op\":\"reply|handoff|checkpoint|request_context|finish\",\"message\":\"optional\",\"message_for_user\":\"optional\",\"context_requests\":[\"optional\"],\"workflow_id\":\"optional\",\"workflow_version\":1,\"objective\":\"optional\",\"summary\":\"optional\",\"status\":\"optional\"}\n\
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
- In this Oracle thread you cannot read the local filesystem, run commands, or verify paths yourself. Use `handoff` for execution and `request_context` for targeted repository state.\n\
- If you need execution, use `handoff` to the orchestrator. Workers are private orchestrator details, not Oracle-managed peers.\n\
- Use `checkpoint` for a non-terminal milestone update that should surface to the human while the workflow continues. Use `finish` only for terminal completion or terminal failure.\n\
- If a turn explicitly asks you to use the orchestrator, emit `handoff` in that same turn instead of only saying you will do it later.\n\
- If you need more repository state, use `request_context`.\n\
- Every control block must include `schema_version`, `op_id`, and `idempotency_key`.\n\
- If a workflow is attached, every control block must echo the active `workflow_id` and `workflow_version` exactly.\n\
- If you are starting a new workflow, including the first workflow or a workflow after terminal completion, use a fresh explicit `workflow_id` and `workflow_version` instead of relying on Codex to synthesize them.\n\
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
    let mut prompt_state = state.clone();
    if mode == "direct_chat" {
        prompt_state.workflow = None;
    }
    let mut parts = vec![
        "You are Oracle, a remote reasoning collaborator connected to a Codex harness.".to_string(),
        format!("Mode: {mode}"),
        intro.trim().to_string(),
        oracle_control_schema_help().to_string(),
        oracle_workflow_snapshot(&prompt_state),
    ];
    for (label, body) in sections {
        let body = body.trim();
        if !body.is_empty() {
            parts.push(format!("{label}:\n{body}"));
        }
    }
    parts.join("\n\n")
}

pub(crate) fn build_user_turn_prompt(
    state: &OracleSupervisorState,
    user_text: &str,
    requires_control: bool,
) -> String {
    let mode = if state.workflow.as_ref().is_some_and(|workflow| {
        !matches!(
            workflow.status,
            OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed
        )
    }) {
        "workflow_supervision"
    } else {
        "direct_chat"
    };
    let intro = if requires_control {
        "This turn explicitly requires structured machine control. Do not answer with prose alone. Append one final `oracle_control` block. Use `handoff` when the orchestrator should do work, `checkpoint` for a non-terminal milestone update, `request_context` for targeted repository state, or `reply` only for a concrete blocker that prevents starting work. Do not claim work has started until you emit the control block."
    } else {
        "Make the best useful progress from the current context. Use prose only when the human needs prose. If you are handing work to the orchestrator, keep visible text minimal and put the real instruction in one final `oracle_control` block. When the human explicitly asks you to use the orchestrator, prefer `handoff` in that same turn."
    };
    build_oracle_prompt(
        mode,
        intro,
        state,
        vec![("Human message", user_text.trim().to_string())],
    )
}

pub(crate) fn build_control_repair_prompt(
    state: &OracleSupervisorState,
    kind: OracleRequestKind,
    original_context: &str,
    invalid_reply: &str,
    failure_reason: &str,
) -> String {
    let (mode, intro, original_label) = match kind {
        OracleRequestKind::UserTurn => (
            if state.workflow.as_ref().is_some_and(|workflow| {
                !matches!(
                    workflow.status,
                    OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed
                )
            }) {
                "workflow_supervision"
            } else {
                "direct_chat"
            },
            "Your previous reply was invalid for this harness because the turn explicitly required structured machine control. Do not answer with prose alone. Append one final `oracle_control` block. Use `handoff` for execution, `checkpoint` for a non-terminal milestone update, `request_context` for targeted repository state, or `reply` only if you are surfacing a concrete blocker instead of claiming work has started.",
            "Original human message",
        ),
        OracleRequestKind::Checkpoint => (
            "checkpoint_review",
            "Your previous checkpoint review was invalid for this harness because checkpoint reviews require structured machine control. Do not answer with prose alone. Append one final `oracle_control` block. Use `handoff` if the orchestrator should continue execution, `checkpoint` for a verified non-terminal milestone that should surface to the human, `reply` for a verified blocker, `request_context` for targeted repository state, or `finish` only when the workflow is terminally complete or terminally failed.",
            "Original checkpoint review prompt",
        ),
    };
    build_oracle_prompt(
        mode,
        intro,
        state,
        vec![
            (original_label, original_context.trim().to_string()),
            ("Validation failure", failure_reason.trim().to_string()),
            ("Previous invalid reply", invalid_reply.trim().to_string()),
        ],
    )
}

pub(crate) fn user_turn_requires_orchestrator_control(user_text: &str) -> bool {
    let normalized = user_text.to_ascii_lowercase();
    let mentions_orchestrator = normalized.contains("orchestrator");
    let mentions_explicit_control =
        normalized.contains("oracle_control") || normalized.contains("control block");
    let explicit_handoff = [
        "use the orchestrator for",
        "use the orchestrator to",
        "tell the orchestrator",
        "ask the orchestrator",
        "send the orchestrator",
        "send it a simple task",
        "ask it to do",
        "hand this to the orchestrator",
        "handoff to the orchestrator",
        "hand off to the orchestrator",
        "delegate to the orchestrator",
        "control an orchestrator",
        "control the orchestrator",
        "have the orchestrator",
        "instruct the orchestrator",
        "direct the orchestrator",
        "by sending the orchestrator",
        "handoff block",
        "oracle_control handoff",
        "single oracle_control",
        "single `oracle_control`",
        "control block only",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase));
    let informational_question = [
        "what is the orchestrator",
        "what's the orchestrator",
        "what does the orchestrator",
        "how does the orchestrator",
        "how to use the orchestrator",
        "how do i use the orchestrator",
        "can you explain how to use the orchestrator",
        "whether an orchestrator would help",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase));
    if mentions_orchestrator
        && !mentions_explicit_control
        && !explicit_handoff
        && informational_question
    {
        return false;
    }
    if explicit_handoff {
        return true;
    }
    if !mentions_orchestrator && !mentions_explicit_control {
        return false;
    }
    let tasking_language = [
        "task",
        "simple task",
        "compute",
        "reply with",
        "report back",
        "return the result",
        "result of",
        "round-trip",
        "loop works",
        "completed",
        "completes",
        "done",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase));
    let imperative_tasking = [
        "orchestrator to ",
        "orchestrator agent to ",
        "orchestrator agent,",
        "orchestrator agent and",
        "orchestrator agent by",
        "use it to ",
        "ask it to ",
        "tell it to ",
        "send it ",
        "have it ",
        "make it ",
        "delegate it ",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase));
    if mentions_orchestrator && tasking_language && imperative_tasking {
        return true;
    }
    let implicit_delegate = [
        "ask it to",
        "tell it to",
        "send it ",
        "have it ",
        "make it ",
        "hand it ",
        "delegate it ",
        "report back when it",
        "report back once it",
        "report back after it",
        "when it has completed",
        "when it completes",
        "once it has completed",
        "once it completes",
        "once it is done",
        "when it is done",
    ]
    .iter()
    .any(|phrase| normalized.contains(phrase));
    mentions_orchestrator && implicit_delegate
}

pub(crate) fn build_checkpoint_prompt(
    state: &OracleSupervisorState,
    thread: &Thread,
    git_status: &str,
    diff_stat: &str,
) -> String {
    build_oracle_prompt(
        "checkpoint_review",
        "You are continuing the same long-lived collaboration. This turn requires structured machine control. Review the orchestrator workflow checkpoint, keep Oracle turns sparse, and only escalate when the milestone needs new direction, more context, or completion. Always append one final `oracle_control` block. Use `handoff` if the orchestrator should continue execution, `checkpoint` for a verified non-terminal milestone that should surface to the human, `request_context` if you need targeted repository state, `reply` for a verified blocker, and `finish` only when the workflow is terminally complete or terminally failed. Treat the checkpoint delta as the proof surface: do not mark a file-editing or test-running objective complete unless the checkpoint artifacts/tests/commands show concrete evidence that the requested side effect actually happened. If the evidence is missing, keep the workflow running with `handoff`, request the missing context, or surface a blocker instead of claiming completion. If you reply to the human, include the concrete orchestrator result inline in `message_for_user`; do not say the result is \"above\" or \"below\", and do not rely on transcript position.",
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
    let mode = if state.workflow.as_ref().is_some_and(|workflow| {
        !matches!(
            workflow.status,
            OracleWorkflowStatus::Complete | OracleWorkflowStatus::Failed
        )
    }) {
        "workflow_supervision"
    } else {
        "direct_chat"
    };
    build_oracle_prompt(
        mode,
        "You asked for additional repository context. Continue from this delta context without resetting the conversation. This follow-up still requires structured machine control. Append one final `oracle_control` block. Use `handoff` for execution, `checkpoint` for a non-terminal milestone update, `request_context` only for another targeted delta, `reply` for a concrete blocker, and `finish` only when the workflow is terminally complete or terminally failed.",
        state,
        vec![
            ("Requested context", requests.join("\n")),
            ("Resolved context", context.trim().to_string()),
        ],
    )
}

fn proof_output_priority(command: &str) -> usize {
    let normalized = command.to_ascii_lowercase();
    if [
        "wc -c", "xxd", "hexdump", "sha", "md5", "shasum", "stat ", "realpath", "readlink", "cmp ",
        "diff ", "cat ",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
    {
        3
    } else if ["test ", "ls ", "find ", "printf "]
        .iter()
        .any(|needle| normalized.contains(needle))
    {
        2
    } else {
        1
    }
}

pub(crate) fn summarize_thread(thread: &Thread) -> String {
    let Some(turn) = thread.turns.last() else {
        return "No orchestrator turns available yet.".to_string();
    };
    let mut commands = Vec::new();
    let mut tests_run = Vec::new();
    let mut changed_files = Vec::new();
    let mut assistant_summary = None;
    let mut blockers = Vec::new();
    let mut proof_outputs = Vec::new();

    let summarize = |text: &str, limit: usize| {
        let trimmed = text.trim();
        if trimmed.chars().count() <= limit {
            trimmed.to_string()
        } else {
            let clipped = trimmed.chars().take(limit).collect::<String>();
            format!("{clipped}...[truncated]")
        }
    };
    let summarize_output = |text: &str| {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(summarize(trimmed, 500))
        }
    };
    if let Some(error) = &turn.error {
        blockers.push(error.message.trim().to_string());
    }
    for item in &turn.items {
        match item {
            ThreadItem::UserMessage { content, .. } => {
                commands.push(format!("user_input_items={}", content.len()));
            }
            ThreadItem::AgentMessage { text, .. } => {
                if assistant_summary.is_none() {
                    assistant_summary = Some(summarize(text, 500));
                }
            }
            ThreadItem::CommandExecution {
                command,
                aggregated_output,
                exit_code,
                ..
            } => {
                let summary = format!("{} (exit={exit_code:?})", command.trim());
                let normalized = command.to_ascii_lowercase();
                if normalized.contains(" test")
                    || normalized.starts_with("test ")
                    || normalized.contains("cargo test")
                    || normalized.contains("pnpm test")
                    || normalized.contains("pytest")
                    || normalized.contains("vitest")
                {
                    tests_run.push(summary);
                } else {
                    commands.push(summary);
                }
                if let Some(output) = aggregated_output.as_deref().and_then(summarize_output) {
                    proof_outputs.push((
                        proof_output_priority(command),
                        proof_outputs.len(),
                        summarize(command.trim(), 240),
                        output,
                    ));
                }
            }
            ThreadItem::FileChange { changes, .. } => {
                changed_files.extend(changes.iter().map(|change| change.path.clone()));
            }
            ThreadItem::CollabAgentToolCall {
                tool,
                receiver_thread_ids,
                ..
            } => {
                commands.push(format!(
                    "collab: {:?} -> {}",
                    tool,
                    receiver_thread_ids.len()
                ));
            }
            _ => {}
        }
    }
    changed_files.sort();
    changed_files.dedup();
    let changed_files_summary = if changed_files.is_empty() {
        "none".to_string()
    } else {
        changed_files
            .iter()
            .take(8)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ")
    };
    let tests_summary = if tests_run.is_empty() {
        "none".to_string()
    } else {
        tests_run
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let command_summary = if commands.is_empty() {
        "none".to_string()
    } else {
        commands
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let blocker_summary = if blockers.is_empty() {
        "none".to_string()
    } else {
        blockers
            .iter()
            .take(4)
            .map(|value| summarize(value, 160))
            .collect::<Vec<_>>()
            .join(" | ")
    };
    let proof_output_summary = if proof_outputs.is_empty() {
        "none".to_string()
    } else {
        proof_outputs.sort_by(|left, right| right.0.cmp(&left.0).then(right.1.cmp(&left.1)));
        let mut prioritized_outputs = proof_outputs.into_iter().take(3).collect::<Vec<_>>();
        prioritized_outputs.sort_by_key(|snippet| snippet.1);
        prioritized_outputs
            .iter()
            .enumerate()
            .map(|(idx, (_, _, command, output))| {
                format!(
                    "proof_output[{idx}].command: {command}\nproof_output[{idx}].stdout:\n```text\n{output}\n```"
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    };
    let next_action = if !blockers.is_empty() {
        "Resolve the blocker or request targeted context before continuing."
    } else if matches!(
        turn.status,
        codex_app_server_protocol::TurnStatus::Completed
    ) {
        "Decide whether to continue execution, request targeted context, surface a checkpoint, or finish."
    } else {
        "Wait for the orchestrator turn to finish or request only the missing delta."
    };
    [
        format!("turn_status: {:?}", turn.status),
        "assistant_summary_is_non_authoritative: true".to_string(),
        format!(
            "assistant_summary: {}",
            assistant_summary.unwrap_or_else(|| "none".to_string())
        ),
        format!("artifacts: {changed_files_summary}"),
        format!("tests_run: {tests_summary}"),
        format!("commands: {command_summary}"),
        format!("proof_outputs: {proof_output_summary}"),
        format!("blockers: {blocker_summary}"),
        format!("next_recommended_action: {next_action}"),
    ]
    .join("\n")
}

pub(crate) fn orchestrator_developer_instructions() -> String {
    "You are the hidden orchestrator operating under Oracle supervision. Oracle turns are expensive and slow, so do substantial work before escalating. Break work into milestones, use parallel workers for independent subproblems, keep worker topology private, and only surface a milestone, blocker, or final completion when it materially changes the workflow. When the task requires file edits, commands, or tests, actually perform them and verify the result locally before you report completion. When Oracle specifies an exact path, use that same path consistently in the side effect and every verification command so the proof surface is unambiguous. If Oracle asks for literal stdout, exact proof tokens, or concrete verification lines, include them verbatim in the checkpoint or completion instead of paraphrasing them. Do not claim a side effect happened unless the thread evidence will show it through file changes, command output, or explicit blockers. End checkpoints with outcome, files changed, tests run, and unresolved blockers.".to_string()
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
        "checkpoint" => Some(OracleAction::Checkpoint),
        "finish" => Some(OracleAction::Finish),
        "reply" => Some(OracleAction::Reply),
        _ => None,
    }
}

fn parse_oracle_control_json(raw: &str) -> Result<Option<OracleControlJson>, OracleControlIssue> {
    let candidate = match extract_oracle_control_json(raw)? {
        Some(candidate) => candidate,
        None => return Ok(None),
    };
    serde_json::from_str::<OracleControlJson>(&candidate)
        .map(Some)
        .map_err(|error| {
            OracleControlIssue::new(
                OracleControlIssueKind::MalformedJson,
                format!("Oracle control JSON was malformed: {error}"),
            )
        })
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

pub(crate) fn parse_oracle_control(
    raw: &str,
) -> Result<Option<OracleControlDirective>, OracleControlIssue> {
    let Some(control) = parse_oracle_control_json(raw)? else {
        return Ok(None);
    };
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
    if op.is_none() && action_hint.is_none() {
        return Err(OracleControlIssue::new(
            OracleControlIssueKind::InvalidSchema,
            "Oracle control block is missing a supported `op` or `action`.",
        ));
    }
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
    let directive = OracleControlDirective {
        schema_version: control.schema_version,
        op_id: normalize_non_empty_option(control.op_id.clone()),
        idempotency_key: normalize_non_empty_option(control.idempotency_key.clone()),
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
    };
    if matches!(directive.workflow_version, Some(0)) {
        return Err(OracleControlIssue::new(
            OracleControlIssueKind::InvalidSchema,
            "Oracle control block must include a positive `workflow_version` starting at 1.",
        ));
    }
    if directive.action_hint.is_some() && directive.op.is_some() {
        let mut canonical = directive.clone();
        canonical.action_hint = None;
        let action_from_op = collapse_oracle_action(&canonical);
        if directive.action_hint != Some(action_from_op) {
            return Err(OracleControlIssue::new(
                OracleControlIssueKind::InvalidSchema,
                "Oracle control block has conflicting `op` and legacy `action` values.",
            ));
        }
    }
    Ok(Some(directive))
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
                    .map(|target| target.eq_ignore_ascii_case("orchestrator"))
                    .unwrap_or_else(|| {
                        directive.participants.iter().any(|participant| {
                            participant.address.eq_ignore_ascii_case("orchestrator")
                                || matches!(participant.kind.as_deref(), Some("orchestrator"))
                        })
                    })
        }
        Some(OracleControlOp::List) | Some(OracleControlOp::Search) => false,
        _ => false,
    }
}

fn workflow_handoff_detail(directive: &OracleControlDirective) -> Option<String> {
    match (
        normalize_non_empty_option(directive.objective.clone()),
        normalize_non_empty_option(directive.summary.clone()),
    ) {
        (Some(objective), Some(summary)) if objective != summary => {
            Some(format!("{objective}\n\nWorkflow summary: {summary}"))
        }
        (Some(objective), _) => Some(objective),
        (None, Some(summary)) => Some(summary),
        (None, None) => None,
    }
}

pub(crate) fn oracle_delegate_task_from_directive(
    directive: &OracleControlDirective,
) -> Option<String> {
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
        .or_else(|| directive.query.clone())
        .or_else(|| workflow_handoff_detail(directive))?;
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
        Some(OracleControlOp::Checkpoint) => OracleAction::Checkpoint,
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
                "Legacy Oracle routing beyond the orchestrator is no longer supported. Use `handoff` for orchestrator work or `reply` for the human."
                    .to_string(),
            )
        }
        _ => None,
    }
}

pub(crate) fn parse_oracle_response(raw: &str) -> OracleResponse {
    let raw_output = raw.trim().to_string();
    match parse_oracle_control(raw) {
        Ok(Some(directive)) => {
            let action = collapse_oracle_action(&directive);
            let rejection_message = legacy_control_rejection_message(&directive);
            let message_for_user = rejection_message.unwrap_or_else(|| {
                directive
                    .message_for_user
                    .clone()
                    .unwrap_or_else(|| match action {
                        OracleAction::Reply
                        | OracleAction::AskUser
                        | OracleAction::Checkpoint
                        | OracleAction::Finish => directive
                            .message
                            .clone()
                            .or_else(|| directive.query.clone())
                            .unwrap_or_else(|| raw.trim().to_string()),
                        _ => String::new(),
                    })
            });
            let context_requests = directive.context_requests.clone();
            OracleResponse {
                action,
                message_for_user,
                task_for_orchestrator: if matches!(action, OracleAction::Delegate) {
                    oracle_delegate_task_from_directive(&directive)
                } else {
                    directive.task_for_orchestrator.clone()
                },
                context_requests,
                directive: Some(directive),
                control_issue: None,
                raw_output,
            }
        }
        Ok(None) => OracleResponse {
            action: OracleAction::Reply,
            message_for_user: raw_output.clone(),
            task_for_orchestrator: None,
            context_requests: Vec::new(),
            directive: None,
            control_issue: None,
            raw_output,
        },
        Err(issue) => OracleResponse {
            action: OracleAction::Reply,
            message_for_user: raw_output.clone(),
            task_for_orchestrator: None,
            context_requests: Vec::new(),
            directive: None,
            control_issue: Some(issue),
            raw_output,
        },
    }
}

fn is_oracle_control_info_string(info: &str) -> bool {
    let Some(suffix) = info.strip_prefix("oracle_control") else {
        return false;
    };
    suffix.is_empty()
        || suffix.chars().next().is_some_and(|ch| {
            ch.is_whitespace() || !matches!(ch, 'A'..='Z' | 'a'..='z' | '0'..='9' | '_' | '-')
        })
}

fn is_markdown_closing_fence(content: &str) -> bool {
    content.starts_with("```") && content[3..].trim().is_empty()
}

fn extract_oracle_control_json(raw: &str) -> Result<Option<String>, OracleControlIssue> {
    let mut cursor = 0usize;
    let mut current_block_start = None;
    let mut control_blocks = Vec::new();

    for line in raw.split_inclusive('\n') {
        let trimmed_line = line.trim_end_matches(['\n', '\r']);
        let content = trimmed_line.trim_start_matches([' ', '\t']);
        if let Some(body_start) = current_block_start {
            if is_markdown_closing_fence(content) {
                let body_end = cursor;
                let close_end = cursor + line.len();
                control_blocks.push((body_start, body_end, close_end));
                current_block_start = None;
            }
        } else if let Some(info) = content.strip_prefix("```") {
            let info = info.trim();
            if is_oracle_control_info_string(info) {
                current_block_start = Some(cursor + line.len());
            }
        }
        cursor += line.len();
    }

    if current_block_start.is_some() {
        return Err(OracleControlIssue::new(
            OracleControlIssueKind::MalformedJson,
            "Oracle control block was not closed before the reply ended.",
        ));
    }
    if control_blocks.len() > 1 {
        return Err(OracleControlIssue::new(
            OracleControlIssueKind::MultipleBlocks,
            "Oracle reply contained multiple `oracle_control` blocks. Emit exactly one final control block.",
        ));
    }
    let Some((body_start, body_end, close_end)) = control_blocks.first().copied() else {
        return Ok(None);
    };
    if !raw[close_end..].trim().is_empty() {
        return Err(OracleControlIssue::new(
            OracleControlIssueKind::TrailingAfterBlock,
            "Oracle control block must be the final fenced block in the reply with no trailing prose.",
        ));
    }
    let body = raw[body_start..body_end].trim();
    Ok((!body.is_empty()).then(|| body.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::ffi::OsString;

    fn fenced_oracle_control(json: &str) -> String {
        format!("```oracle_control\n{json}\n```")
    }

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
            Ok(OracleCommand::AttachThread {
                conversation_id: "abc-123".to_string(),
                import_history: false,
            })
        );
        assert_eq!(
            OracleCommand::parse("attach abc-123 --import-history"),
            Ok(OracleCommand::AttachThread {
                conversation_id: "abc-123".to_string(),
                import_history: true,
            })
        );
        assert_eq!(
            OracleCommand::parse("attach abc-123 history"),
            Ok(OracleCommand::AttachThread {
                conversation_id: "abc-123".to_string(),
                import_history: true,
            })
        );
        assert_eq!(
            OracleCommand::parse("attach https://chatgpt.com/c/abc-123"),
            Ok(OracleCommand::AttachThread {
                conversation_id: "abc-123".to_string(),
                import_history: false,
            })
        );
        assert_eq!(
            OracleCommand::parse("attach https://chat.openai.com/c/abc-123/"),
            Ok(OracleCommand::AttachThread {
                conversation_id: "abc-123".to_string(),
                import_history: false,
            })
        );
        assert_eq!(
            OracleCommand::parse(
                "attach https://chatgpt.com/g/g-p-example/project/c/abc-123?foo=bar#frag"
            ),
            Ok(OracleCommand::AttachThread {
                conversation_id: "abc-123".to_string(),
                import_history: false,
            })
        );
        assert!(OracleCommand::parse("attach https://chatgpt.com/c/").is_err());
        assert!(OracleCommand::parse("attach https://example.com/c/abc-123").is_err());
        assert!(OracleCommand::parse("attach https://chatgpt.com/share/abc-123").is_err());
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
            OracleCommand::parse("model pro standard"),
            Ok(OracleCommand::Model(Some(OracleModelPreset::Pro)))
        );
        assert_eq!(
            OracleCommand::parse("model pro extended"),
            Ok(OracleCommand::Model(Some(OracleModelPreset::ProExtended)))
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
        assert_eq!(parsed.control_issue, None);
    }

    #[test]
    fn parse_oracle_response_reads_json_payload() {
        let parsed = parse_oracle_response(
            "```oracle_control\n{\"schema_version\":1,\"op_id\":\"op-json\",\"idempotency_key\":\"idem-json\",\"op\":\"handoff\",\"message_for_user\":\"Working.\",\"task_for_orchestrator\":\"Ship it\"}\n```",
        );
        assert_eq!(parsed.action, OracleAction::Delegate);
        assert_eq!(parsed.task_for_orchestrator.as_deref(), Some("Ship it"));
    }

    #[test]
    fn parse_oracle_response_ignores_trailing_bare_json_after_prose() {
        let parsed = parse_oracle_response(
            "Starting the orchestrator test now.\n\n{\"op\":\"handoff\",\"message\":\"Compute 2+2 and report back.\",\"message_for_user\":\"Starting the orchestrator test now.\"}",
        );
        assert_eq!(parsed.action, OracleAction::Reply);
        assert_eq!(
            parsed.message_for_user,
            "Starting the orchestrator test now.\n\n{\"op\":\"handoff\",\"message\":\"Compute 2+2 and report back.\",\"message_for_user\":\"Starting the orchestrator test now.\"}"
        );
    }

    #[test]
    fn parse_oracle_response_ignores_non_actionable_control_block() {
        let raw = "```oracle_control\n{\"workflow_id\":\"oracle-routing\"}\n```";
        let parsed = parse_oracle_response(raw);

        assert_eq!(parsed.action, OracleAction::Reply);
        assert_eq!(parsed.message_for_user, raw);
        assert!(parsed.directive.is_none());
        assert_eq!(
            parsed.control_issue.map(|issue| issue.kind),
            Some(OracleControlIssueKind::InvalidSchema)
        );
    }

    #[test]
    fn parse_oracle_control_reads_workflow_handoff_directive() {
        let parsed = parse_oracle_control(
            &fenced_oracle_control(
                r#"{"op":"handoff","message":"Audit the diff","workflow_id":"oracle-routing","workflow_version":3,"objective":"Ship the Oracle routing patch","summary":"Diff review milestone","status":"running"}"#,
            ),
        )
        .expect("parsed control")
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
    fn parse_oracle_control_rejects_trailing_prose_after_fenced_block() {
        let raw = "```oracle_control\n{\"op\":\"handoff\",\"message\":\"Audit the diff\"}\n```\nTrailing prose.";

        assert!(parse_oracle_control(raw).is_err());
        let parsed = parse_oracle_response(raw);
        assert_eq!(parsed.action, OracleAction::Reply);
        assert!(parsed.directive.is_none());
        assert!(parsed.control_issue.is_some());
        assert_eq!(parsed.message_for_user, raw);
    }

    #[test]
    fn parse_oracle_response_collapses_addressed_send_to_delegate() {
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"op":"send","to":"orchestrator","message":"Implement milestone A"}"#,
        ));

        assert_eq!(parsed.action, OracleAction::Delegate);
        assert_eq!(
            parsed.task_for_orchestrator.as_deref(),
            Some("Implement milestone A")
        );
        assert!(parsed.message_for_user.is_empty());
    }

    #[test]
    fn parse_oracle_response_collapses_human_send_to_ask_user() {
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"op":"send","to":"human","message":"Need clarification on the rollout plan."}"#,
        ));

        assert_eq!(parsed.action, OracleAction::AskUser);
        assert_eq!(
            parsed.message_for_user,
            "Need clarification on the rollout plan."
        );
        assert_eq!(parsed.task_for_orchestrator, None);
    }

    #[test]
    fn parse_oracle_response_reads_context_requests() {
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"action":"request_context","message_for_user":"Need files.","context_requests":["git_diff","file:src/main.rs"]}"#,
        ));
        assert_eq!(parsed.action, OracleAction::RequestContext);
        assert_eq!(
            parsed.context_requests,
            vec!["git_diff".to_string(), "file:src/main.rs".to_string()]
        );
    }

    #[test]
    fn parse_oracle_response_maps_handoff_to_delegate_with_workflow_metadata() {
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"op":"handoff","message":"Implement milestone A","workflow_id":"oracle-routing","workflow_version":7,"objective":"Ship the workflow refactor","summary":"Ready for implementation","status":"running"}"#,
        ));

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
    fn parse_oracle_response_synthesizes_handoff_task_from_workflow_metadata() {
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"op":"handoff","workflow_id":"oracle-routing","workflow_version":7,"objective":"Verify the Codex harness loop end-to-end by delegating one trivial task to the orchestrator. Ask the orchestrator to compute 2+2 and return the result to Oracle.","summary":"User requested a full harness-loop verification via orchestrator delegation. Required task: orchestrator computes 2+2 and returns the result here; only then report back to the human.","status":"awaiting_orchestrator_result"}"#,
        ));

        assert_eq!(parsed.action, OracleAction::Delegate);
        assert_eq!(
            parsed.task_for_orchestrator.as_deref(),
            Some(
                "Verify the Codex harness loop end-to-end by delegating one trivial task to the orchestrator. Ask the orchestrator to compute 2+2 and return the result to Oracle.\n\nWorkflow summary: User requested a full harness-loop verification via orchestrator delegation. Required task: orchestrator computes 2+2 and returns the result here; only then report back to the human."
            )
        );
    }

    #[test]
    fn parse_oracle_response_rejects_legacy_search_ops() {
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"op":"search","query":"find oracle delegate callers"}"#,
        ));

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
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"op":"send","message":"Do the work quietly"}"#,
        ));

        assert_eq!(parsed.action, OracleAction::Reply);
        assert!(parsed.task_for_orchestrator.is_none());
        assert!(
            parsed
                .message_for_user
                .contains("routing beyond the orchestrator is no longer supported")
        );
    }

    #[test]
    fn parse_oracle_response_maps_search_into_delegate_task() {
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"op":"search","to":"worker:searcher","query":"find oracle delegate callers"}"#,
        ));

        assert_eq!(parsed.action, OracleAction::Reply);
        assert!(parsed.task_for_orchestrator.is_none());
        assert!(
            parsed
                .message_for_user
                .contains("routing discovery is no longer supported")
        );
    }

    #[test]
    fn parse_oracle_response_rejects_worker_send_routing() {
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"op":"send","to":"worker:searcher","message":"Do the work quietly"}"#,
        ));

        assert_eq!(parsed.action, OracleAction::Reply);
        assert!(parsed.task_for_orchestrator.is_none());
        assert!(
            parsed
                .message_for_user
                .contains("routing beyond the orchestrator is no longer supported")
        );
    }

    #[test]
    fn parse_oracle_response_rejects_worker_spawn_routing() {
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"op":"spawn","participants":["worker:searcher"],"message":"Do the work quietly"}"#,
        ));

        assert_eq!(parsed.action, OracleAction::Reply);
        assert!(parsed.task_for_orchestrator.is_none());
        assert!(
            parsed
                .message_for_user
                .contains("routing beyond the orchestrator is no longer supported")
        );
    }

    #[test]
    fn parse_oracle_response_keeps_unaddressed_search_as_reply() {
        let parsed = parse_oracle_response(&fenced_oracle_control(
            r#"{"op":"search","query":"find oracle delegate callers"}"#,
        ));

        assert_eq!(parsed.action, OracleAction::Reply);
        assert_eq!(parsed.task_for_orchestrator, None);
        assert!(
            parsed
                .message_for_user
                .contains("routing discovery is no longer supported")
        );
    }

    #[test]
    fn parse_oracle_response_reads_fenced_control_with_attributes() {
        let parsed = parse_oracle_response(
            "Visible prose.\n```json\n{\"note\":\"ok\"}\n```\n```oracle_control id=\"abc\"\n{\"schema_version\":1,\"op_id\":\"op-1\",\"idempotency_key\":\"idem-1\",\"op\":\"handoff\",\"message\":\"Do work\"}\n```",
        );

        assert_eq!(parsed.action, OracleAction::Delegate);
        assert_eq!(parsed.task_for_orchestrator.as_deref(), Some("Do work"));
        assert_eq!(parsed.control_issue, None);
    }

    #[test]
    fn parse_oracle_response_reads_fenced_control_with_inline_attribute_delimiters() {
        for raw in [
            "```oracle_control,id=\"abc\"\n{\"schema_version\":1,\"op_id\":\"op-1\",\"idempotency_key\":\"idem-1\",\"op\":\"handoff\",\"message\":\"Do work\"}\n```",
            "```oracle_control{id=\"abc\"}\n{\"schema_version\":1,\"op_id\":\"op-2\",\"idempotency_key\":\"idem-2\",\"op\":\"handoff\",\"message\":\"Do work\"}\n```",
            "```oracle_control id=\"abc\"\n{\"schema_version\":1,\"op_id\":\"op-3\",\"idempotency_key\":\"idem-3\",\"op\":\"handoff\",\"message\":\"Do work\"}\n```   \n",
        ] {
            let parsed = parse_oracle_response(raw);
            assert_eq!(parsed.action, OracleAction::Delegate);
            assert_eq!(parsed.task_for_orchestrator.as_deref(), Some("Do work"));
            assert_eq!(parsed.control_issue, None);
        }
    }

    #[test]
    fn parse_oracle_response_rejects_duplicate_control_blocks() {
        let parsed = parse_oracle_response(
            "```oracle_control\n{\"schema_version\":1,\"op_id\":\"op-1\",\"idempotency_key\":\"idem-1\",\"op\":\"reply\",\"message\":\"one\"}\n```\n```oracle_control\n{\"schema_version\":1,\"op_id\":\"op-2\",\"idempotency_key\":\"idem-2\",\"op\":\"reply\",\"message\":\"two\"}\n```",
        );

        assert!(parsed.directive.is_none());
        assert_eq!(
            parsed.control_issue.map(|issue| issue.kind),
            Some(OracleControlIssueKind::MultipleBlocks)
        );
    }

    #[test]
    fn parse_oracle_response_rejects_malformed_control_json() {
        let parsed = parse_oracle_response(
            "```oracle_control\n{\"schema_version\":1,\"op\":\"handoff\",}\n```",
        );

        assert!(parsed.directive.is_none());
        assert_eq!(
            parsed.control_issue.map(|issue| issue.kind),
            Some(OracleControlIssueKind::MalformedJson)
        );
    }

    #[test]
    fn parse_oracle_response_rejects_unfenced_whole_json_control_payload() {
        let raw = r#"{"schema_version":1,"op_id":"op-raw","idempotency_key":"idem-raw","op":"handoff","message":"Do work"}"#;
        let parsed = parse_oracle_response(raw);

        assert_eq!(parsed.action, OracleAction::Reply);
        assert_eq!(parsed.message_for_user, raw);
        assert!(parsed.directive.is_none());
        assert_eq!(parsed.control_issue, None);
    }

    #[test]
    fn parse_oracle_response_rejects_schema_invalid_control_json() {
        let parsed = parse_oracle_response("```oracle_control\n{\"schema_version\":1}\n```");

        assert!(parsed.directive.is_none());
        assert_eq!(
            parsed.control_issue.map(|issue| issue.kind),
            Some(OracleControlIssueKind::InvalidSchema)
        );
    }

    #[test]
    fn parse_oracle_response_rejects_zero_workflow_version() {
        let parsed = parse_oracle_response(
            "```oracle_control\n{\"schema_version\":1,\"op_id\":\"op-zero-version\",\"idempotency_key\":\"idem-zero-version\",\"op\":\"handoff\",\"workflow_id\":\"oracle-routing\",\"workflow_version\":0,\"message\":\"Do work\"}\n```",
        );

        assert!(parsed.directive.is_none());
        assert_eq!(
            parsed.control_issue.map(|issue| issue.message),
            Some(
                "Oracle control block must include a positive `workflow_version` starting at 1."
                    .to_string()
            )
        );
    }

    #[test]
    fn parse_oracle_response_rejects_conflicting_op_and_legacy_action() {
        let parsed = parse_oracle_response(
            "```oracle_control\n{\"schema_version\":1,\"op_id\":\"op-conflict\",\"idempotency_key\":\"idem-conflict\",\"op\":\"handoff\",\"action\":\"reply\",\"message\":\"Do work\"}\n```",
        );

        assert!(parsed.directive.is_none());
        assert_eq!(
            parsed.control_issue.map(|issue| issue.kind),
            Some(OracleControlIssueKind::InvalidSchema)
        );
    }

    #[test]
    fn parse_oracle_response_ignores_non_control_fence_prefixes() {
        let raw = "```oracle_control_example\n{\"schema_version\":1,\"op\":\"reply\",\"message_for_user\":\"sample only\"}\n```";
        let parsed = parse_oracle_response(raw);

        assert_eq!(parsed.action, OracleAction::Reply);
        assert_eq!(parsed.message_for_user, raw);
        assert!(parsed.directive.is_none());
        assert_eq!(parsed.control_issue, None);
    }

    #[test]
    fn parse_oracle_response_allows_backticks_inside_control_json_strings() {
        let parsed = parse_oracle_response(
            "```oracle_control\n{\"schema_version\":1,\"op_id\":\"op-1\",\"idempotency_key\":\"idem-1\",\"op\":\"reply\",\"message_for_user\":\"Fence sample: ```bash\\\\necho hi\\\\n```\"}\n```",
        );

        assert_eq!(parsed.action, OracleAction::Reply);
        assert_eq!(
            parsed.message_for_user,
            "Fence sample: ```bash\\necho hi\\n```"
        );
        assert_eq!(parsed.control_issue, None);
    }

    #[test]
    fn oracle_protocol_golden_examples_remain_conformant() {
        let direct = parse_oracle_response(
            "```oracle_control\n{\"schema_version\":1,\"op_id\":\"op-direct\",\"idempotency_key\":\"idem-direct\",\"op\":\"reply\",\"message_for_user\":\"Need one clarification.\"}\n```",
        );
        assert_eq!(direct.action, OracleAction::Reply);
        assert_eq!(direct.message_for_user, "Need one clarification.");

        let workflow = parse_oracle_response(
            "```oracle_control\n{\"schema_version\":1,\"op_id\":\"op-hand\",\"idempotency_key\":\"idem-hand\",\"op\":\"handoff\",\"workflow_id\":\"oracle-routing\",\"workflow_version\":7,\"message\":\"Run the implementation pass.\",\"summary\":\"Implementation milestone ready.\",\"status\":\"running\"}\n```",
        );
        assert_eq!(workflow.action, OracleAction::Delegate);
        assert_eq!(
            workflow.task_for_orchestrator.as_deref(),
            Some("Run the implementation pass.")
        );

        let checkpoint = parse_oracle_response(
            "```oracle_control\n{\"schema_version\":1,\"op_id\":\"op-check\",\"idempotency_key\":\"idem-check\",\"op\":\"checkpoint\",\"workflow_id\":\"oracle-routing\",\"workflow_version\":7,\"message_for_user\":\"Milestone complete; continuing.\",\"summary\":\"Parser hardening complete.\",\"status\":\"running\"}\n```",
        );
        assert_eq!(checkpoint.action, OracleAction::Checkpoint);
        assert_eq!(
            checkpoint.message_for_user,
            "Milestone complete; continuing."
        );

        let malformed = parse_oracle_response(
            "```oracle_control\n{\"schema_version\":1,\"op\":\"handoff\",}\n```",
        );
        assert_eq!(
            malformed.control_issue.map(|issue| issue.kind),
            Some(OracleControlIssueKind::MalformedJson)
        );
    }

    #[test]
    fn build_context_prompt_requires_structured_control() {
        let state = OracleSupervisorState {
            workflow: Some(OracleWorkflowBinding {
                workflow_id: "oracle-routing".to_string(),
                status: OracleWorkflowStatus::Running,
                version: 7,
                ..Default::default()
            }),
            ..Default::default()
        };

        let prompt = build_context_prompt(
            &state,
            &["git_diff".to_string()],
            "changed files: src/app.rs",
        );

        assert!(prompt.contains("Mode: workflow_supervision"));
        assert!(prompt.contains("Append one final `oracle_control` block"));
    }

    #[test]
    fn build_user_turn_prompt_includes_workflow_contract_and_schema() {
        let prompt =
            build_user_turn_prompt(&OracleSupervisorState::default(), "Ship the patch.", false);

        assert!(prompt.contains("Workflow contract"));
        assert!(prompt.contains("reply|handoff|checkpoint|request_context|finish"));
        assert!(prompt.contains("exactly one control block"));
        assert!(prompt.contains("chat mode"));
        assert!(prompt.contains("cannot read the local filesystem"));
        assert!(prompt.contains("emit `handoff` in that same turn"));
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
                ..Default::default()
            }),
            ..Default::default()
        };

        let prompt = build_user_turn_prompt(&state, "Ship the patch.", false);

        assert!(prompt.contains("workflow_id: oracle-routing"));
        assert!(prompt.contains("status: running"));
        assert!(prompt.contains("version: 4"));
        assert!(prompt.contains("Ship the Oracle workflow refactor"));
        assert!(prompt.contains("Checkpoint ready for review"));
        assert!(prompt.contains("use a fresh explicit `workflow_id` and `workflow_version`"));
    }

    #[test]
    fn build_user_turn_prompt_hides_terminal_workflow_snapshot() {
        let oracle_thread_id = ThreadId::new();
        let state = OracleSupervisorState {
            oracle_thread_id: Some(oracle_thread_id),
            workflow: Some(OracleWorkflowBinding {
                workflow_id: "oracle-routing".to_string(),
                mode: OracleWorkflowMode::Supervising,
                status: OracleWorkflowStatus::Complete,
                version: 4,
                objective: Some("Finished workflow".to_string()),
                summary: Some("Milestone complete".to_string()),
                ..Default::default()
            }),
            ..Default::default()
        };

        let prompt = build_user_turn_prompt(&state, "Start something new.", false);

        assert!(prompt.contains("Mode: direct_chat"));
        assert!(prompt.contains("No workflow is currently attached."));
        assert!(!prompt.contains("workflow_id: oracle-routing"));
        assert!(prompt.contains("use a fresh explicit `workflow_id` and `workflow_version`"));
    }

    #[test]
    fn build_user_turn_prompt_demands_machine_control_when_required() {
        let prompt = build_user_turn_prompt(
            &OracleSupervisorState::default(),
            "Use the orchestrator for this turn.",
            true,
        );

        assert!(prompt.contains("explicitly requires structured machine control"));
        assert!(prompt.contains("Do not answer with prose alone."));
        assert!(prompt.contains("Append one final `oracle_control` block."));
        assert!(prompt.contains("checkpoint"));
        assert!(prompt.contains("Do not claim work has started until you emit the control block."));
    }

    #[test]
    fn build_control_repair_prompt_demands_machine_control() {
        let prompt = build_control_repair_prompt(
            &OracleSupervisorState::default(),
            OracleRequestKind::UserTurn,
            "Please ask the orchestrator to compute 2+2 and report back.",
            "I'll ask the orchestrator now.",
            "Oracle control block is missing `workflow_version`.",
        );

        assert!(prompt.contains("previous reply was invalid"));
        assert!(prompt.contains("Do not answer with prose alone."));
        assert!(prompt.contains("Append one final `oracle_control` block."));
        assert!(prompt.contains("workflow_version"));
        assert!(prompt.contains("Original human message"));
        assert!(prompt.contains("Previous invalid reply"));
    }

    #[test]
    fn build_checkpoint_control_repair_prompt_demands_machine_control() {
        let prompt = build_control_repair_prompt(
            &OracleSupervisorState::default(),
            OracleRequestKind::Checkpoint,
            "Checkpoint review prompt body.",
            "Proceeding to implementation now.",
            "Oracle control block was malformed JSON.",
        );

        assert!(prompt.contains("checkpoint review was invalid"));
        assert!(prompt.contains("checkpoint reviews require structured machine control"));
        assert!(prompt.contains("malformed JSON"));
        assert!(prompt.contains("Original checkpoint review prompt"));
        assert!(prompt.contains("Proceeding to implementation now."));
    }

    #[test]
    fn user_turn_requires_orchestrator_control_only_for_explicit_handoffs() {
        assert!(user_turn_requires_orchestrator_control(
            "Please tell the orchestrator to compute 2+2 and report back."
        ));
        assert!(user_turn_requires_orchestrator_control(
            "Use the orchestrator for this smoke test."
        ));
        assert!(user_turn_requires_orchestrator_control(
            "I am testing your ability to use the orchestrator agent. Please send it a simple task, and report back when it has completed successfully."
        ));
        assert!(user_turn_requires_orchestrator_control(
            "Respond with only a single oracle_control handoff block for the orchestrator."
        ));
        assert!(user_turn_requires_orchestrator_control(
            "Please ask it to do some simple task through the orchestrator agent, then report back once it completes."
        ));
        assert!(user_turn_requires_orchestrator_control(
            "Please test your ability to control an orchestrator agent by sending it a simple task and then report back here once the complete loop works."
        ));
        assert!(user_turn_requires_orchestrator_control(
            "Drive the orchestrator through a trivial round-trip: ask it to compute 2+2, then return the result here."
        ));
        assert!(!user_turn_requires_orchestrator_control(
            "What is the orchestrator responsible for in this design?"
        ));
        assert!(!user_turn_requires_orchestrator_control(
            "Can you explain how to use the orchestrator agent?"
        ));
        assert!(!user_turn_requires_orchestrator_control(
            "Reply directly and mention whether an orchestrator would help."
        ));
        assert!(user_turn_requires_orchestrator_control(
            "Can you explain how to use the orchestrator agent, then send it a simple task and report back once it completes?"
        ));
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
            forked_from_id: None,
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

        for prompt in [&checkpoint_prompt, &context_prompt] {
            assert!(prompt.contains("workflow_id: oracle-routing"));
            assert!(prompt.contains("version: 5"));
            assert!(prompt.contains("Ship the workflow refactor"));
        }
        assert!(checkpoint_prompt.contains("include the concrete orchestrator result inline"));
        assert!(checkpoint_prompt.contains("do not say the result is \"above\" or \"below\""));
        assert!(checkpoint_prompt.contains("Treat the checkpoint delta as the proof surface"));
        assert!(
            checkpoint_prompt
                .contains("checkpoint artifacts/tests/commands show concrete evidence")
        );
        let orchestrator_instructions = orchestrator_developer_instructions();
        assert!(
            orchestrator_instructions
                .contains("actually perform them and verify the result locally")
        );
        assert!(orchestrator_instructions.contains("same path consistently"));
        assert!(orchestrator_instructions.contains("include them verbatim"));
        assert!(orchestrator_instructions.contains("Do not claim a side effect happened"));
    }

    #[test]
    fn summarize_thread_preserves_exact_workspace_paths_and_small_proof_outputs() {
        let workspace = PathBuf::from("/tmp/oracle-workspace");
        let thread = Thread {
            id: "thread-1".to_string(),
            forked_from_id: None,
            preview: "preview".to_string(),
            ephemeral: false,
            model_provider: "provider".to_string(),
            created_at: 0,
            updated_at: 0,
            status: codex_app_server_protocol::ThreadStatus::Idle,
            path: None,
            cwd: workspace.clone(),
            cli_version: "0.0.0".to_string(),
            source: codex_app_server_protocol::SessionSource::Unknown,
            agent_nickname: Some("Oracle Orchestrator".to_string()),
            agent_role: Some("orchestrator".to_string()),
            git_info: None,
            name: Some("Oracle Orchestrator".to_string()),
            turns: vec![codex_app_server_protocol::Turn {
                id: "turn-1".to_string(),
                items: vec![
                    ThreadItem::CommandExecution {
                        id: "cmd-1".to_string(),
                        command: "mkdir -p /tmp/oracle-workspace/tmp/proof && printf P1 > tmp/proof/out.txt && wc -c < /tmp/oracle-workspace/tmp/proof/out.txt".to_string(),
                        cwd: workspace.clone(),
                        process_id: None,
                        source: codex_app_server_protocol::CommandExecutionSource::Agent,
                        status: codex_app_server_protocol::CommandExecutionStatus::Completed,
                        command_actions: Vec::new(),
                        aggregated_output: Some(
                            "/tmp/oracle-workspace/tmp/proof/out.txt\n2\n5031\n".to_string(),
                        ),
                        exit_code: Some(0),
                        duration_ms: Some(1),
                    },
                    ThreadItem::FileChange {
                        id: "file-1".to_string(),
                        changes: vec![codex_app_server_protocol::FileUpdateChange {
                            path: "/tmp/oracle-workspace/tmp/proof/out.txt".to_string(),
                            kind: codex_app_server_protocol::PatchChangeKind::Add,
                            diff: String::new(),
                        }],
                        status: codex_app_server_protocol::PatchApplyStatus::Completed,
                    },
                ],
                status: codex_app_server_protocol::TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            }],
        };

        let summary = summarize_thread(&thread);
        assert!(summary.contains("artifacts: /tmp/oracle-workspace/tmp/proof/out.txt"));
        assert!(summary.contains("mkdir -p /tmp/oracle-workspace/tmp/proof"));
        assert!(summary.contains("wc -c < /tmp/oracle-workspace/tmp/proof/out.txt"));
        assert!(summary.contains("assistant_summary_is_non_authoritative: true"));
        assert!(summary.contains("proof_output[0].command:"));
        assert!(
            summary.contains(
                "proof_output[0].stdout:\n```text\n/tmp/oracle-workspace/tmp/proof/out.txt\n2\n5031\n```"
            )
        );
    }

    #[test]
    fn summarize_thread_prioritizes_literal_verification_outputs_over_setup_noise() {
        let workspace = PathBuf::from("/tmp/oracle-workspace");
        let thread = Thread {
            id: "thread-1".to_string(),
            forked_from_id: None,
            preview: "preview".to_string(),
            ephemeral: false,
            model_provider: "provider".to_string(),
            created_at: 0,
            updated_at: 0,
            status: codex_app_server_protocol::ThreadStatus::Idle,
            path: None,
            cwd: workspace.clone(),
            cli_version: "0.0.0".to_string(),
            source: codex_app_server_protocol::SessionSource::Unknown,
            agent_nickname: Some("Oracle Orchestrator".to_string()),
            agent_role: Some("orchestrator".to_string()),
            git_info: None,
            name: Some("Oracle Orchestrator".to_string()),
            turns: vec![codex_app_server_protocol::Turn {
                id: "turn-1".to_string(),
                items: vec![
                    ThreadItem::CommandExecution {
                        id: "cmd-1".to_string(),
                        command: "printf preparing".to_string(),
                        cwd: workspace.clone(),
                        process_id: None,
                        source: codex_app_server_protocol::CommandExecutionSource::Agent,
                        status: codex_app_server_protocol::CommandExecutionStatus::Completed,
                        command_actions: Vec::new(),
                        aggregated_output: Some("preparing".to_string()),
                        exit_code: Some(0),
                        duration_ms: Some(1),
                    },
                    ThreadItem::CommandExecution {
                        id: "cmd-2".to_string(),
                        command: "test -f /tmp/oracle-workspace/tmp/proof/out.txt".to_string(),
                        cwd: workspace.clone(),
                        process_id: None,
                        source: codex_app_server_protocol::CommandExecutionSource::Agent,
                        status: codex_app_server_protocol::CommandExecutionStatus::Completed,
                        command_actions: Vec::new(),
                        aggregated_output: Some("exists".to_string()),
                        exit_code: Some(0),
                        duration_ms: Some(1),
                    },
                    ThreadItem::CommandExecution {
                        id: "cmd-3".to_string(),
                        command: "wc -c < /tmp/oracle-workspace/tmp/proof/out.txt".to_string(),
                        cwd: workspace.clone(),
                        process_id: None,
                        source: codex_app_server_protocol::CommandExecutionSource::Agent,
                        status: codex_app_server_protocol::CommandExecutionStatus::Completed,
                        command_actions: Vec::new(),
                        aggregated_output: Some("13".to_string()),
                        exit_code: Some(0),
                        duration_ms: Some(1),
                    },
                    ThreadItem::CommandExecution {
                        id: "cmd-4".to_string(),
                        command:
                            "printf '/tmp/oracle-workspace/tmp/proof/out.txt\\n13\\n50315f31\\n'"
                                .to_string(),
                        cwd: workspace.clone(),
                        process_id: None,
                        source: codex_app_server_protocol::CommandExecutionSource::Agent,
                        status: codex_app_server_protocol::CommandExecutionStatus::Completed,
                        command_actions: Vec::new(),
                        aggregated_output: Some(
                            "/tmp/oracle-workspace/tmp/proof/out.txt\n13\n50315f31\n".to_string(),
                        ),
                        exit_code: Some(0),
                        duration_ms: Some(1),
                    },
                ],
                status: codex_app_server_protocol::TurnStatus::Completed,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
            }],
        };

        let summary = summarize_thread(&thread);
        assert!(summary.contains("proof_output[2].command: printf '/tmp/oracle-workspace/tmp/proof/out.txt\\n13\\n50315f31\\n'"));
        assert!(
            summary.contains(
                "proof_output[2].stdout:\n```text\n/tmp/oracle-workspace/tmp/proof/out.txt\n13\n50315f31\n```"
            )
        );
        assert!(
            summary.contains(
                "proof_output[1].command: wc -c < /tmp/oracle-workspace/tmp/proof/out.txt"
            )
        );
        assert!(!summary.contains("proof_output[0].stdout:\n```text\npreparing\n```"));
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
        assert_eq!(
            OracleModelPreset::Pro.browser_thinking_time(),
            Some("standard")
        );
        assert_eq!(OracleModelPreset::ProExtended.model_id(), "gpt-5.4-pro");
        assert_eq!(
            OracleModelPreset::ProExtended.browser_label(),
            "GPT-5.4 Pro"
        );
        assert_eq!(
            OracleModelPreset::ProExtended.browser_thinking_time(),
            Some("extended")
        );
        assert_eq!(OracleModelPreset::Thinking.model_id(), "gpt-5.4");
        assert_eq!(OracleModelPreset::Thinking.browser_label(), "Thinking 5.4");
        assert_eq!(OracleModelPreset::Thinking.browser_thinking_time(), None);
    }

    #[test]
    fn oracle_model_preset_resolves_default_from_optional_text() {
        assert_eq!(
            OracleModelPreset::resolve_default(Some("thinking")),
            OracleModelPreset::Thinking
        );
        assert_eq!(
            OracleModelPreset::resolve_default(Some("gpt-5.4-pro")),
            OracleModelPreset::Pro
        );
        assert_eq!(
            OracleModelPreset::resolve_default(Some("pro-extended")),
            OracleModelPreset::ProExtended
        );
        assert_eq!(
            OracleModelPreset::resolve_default(Some("bogus")),
            OracleModelPreset::Pro
        );
        assert_eq!(
            OracleModelPreset::resolve_default(None),
            OracleModelPreset::Pro
        );
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
