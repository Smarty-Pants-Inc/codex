use std::env;
use std::collections::HashMap;
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
            "status" if parts.len() == 1 => Ok(Self::Status),
            "model" if parts.len() == 1 => Ok(Self::Model(None)),
            "model" if parts.len() == 2 => OracleModelPreset::parse(parts[1])
                .map(|model| Self::Model(Some(model)))
                .ok_or_else(|| {
                    "Usage: /oracle [browse|new|attach <conversation_id>|on|off|status|model [pro|thinking]]".to_string()
                }),
            _ => Err(
                "Usage: /oracle [browse|new|attach <conversation_id>|on|off|status|model [pro|thinking]]"
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
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OracleThreadBinding {
    pub(crate) session_root_slug: Option<String>,
    pub(crate) current_session_id: Option<String>,
    pub(crate) orchestrator_thread_id: Option<ThreadId>,
    pub(crate) phase: OracleSupervisorPhase,
    pub(crate) last_status: Option<String>,
    pub(crate) last_orchestrator_task: Option<String>,
    pub(crate) pending_turn_id: Option<String>,
    pub(crate) automatic_context_followups: u8,
    pub(crate) conversation_id: Option<String>,
    pub(crate) remote_title: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct OracleSupervisorState {
    /// Active visible Oracle thread (legacy working view for in-flight workflows).
    pub(crate) oracle_thread_id: Option<ThreadId>,
    /// Legacy working view fields mirrored from `bindings[oracle_thread_id]`.
    pub(crate) session_root_slug: Option<String>,
    pub(crate) current_session_id: Option<String>,
    pub(crate) orchestrator_thread_id: Option<ThreadId>,
    pub(crate) phase: OracleSupervisorPhase,
    pub(crate) model: OracleModelPreset,
    pub(crate) last_status: Option<String>,
    pub(crate) last_orchestrator_task: Option<String>,
    pub(crate) pending_turn_id: Option<String>,
    pub(crate) automatic_context_followups: u8,
    /// Per-visible-thread Oracle session state.
    pub(crate) bindings: HashMap<ThreadId, OracleThreadBinding>,
    /// Hidden orchestrator thread -> visible Oracle thread.
    pub(crate) orchestrator_owner: HashMap<ThreadId, ThreadId>,
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
        format!(
            "Oracle mode: {enabled}\nRequested model: {}\nBrowser strategy: select\nActive oracle thread: {active}\nTracked oracle threads: {}\nState: {}\nLast status: {last}",
            self.model.display_name(),
            self.bindings.len(),
            self.phase.description()
        )
    }
}

#[derive(Debug, Deserialize)]
struct OracleJson {
    action: Option<String>,
    message_for_user: Option<String>,
    task_for_orchestrator: Option<String>,
    context_requests: Option<Vec<String>>,
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

pub(crate) fn build_user_turn_prompt(state: &OracleSupervisorState, user_text: &str) -> String {
    format!(
        "You are Oracle, a remote reasoning collaborator connected to a Codex harness.\n\
Mode: direct_human\n\
Make the best useful progress from the current context. Do not require unnecessary clarifying turns.\n\
Use prose by default. If you need explicit machine control, append one final fenced block with language `oracle_control` and JSON body using this schema:\n\
{{\"action\":\"reply|delegate|request_context|ask_user|finish\",\"message_for_user\":\"...\",\"task_for_orchestrator\":\"optional\",\"context_requests\":[\"optional\"]}}\n\
When requesting context, ask for targeted delta context only (git_status, git_diff_stat, git_diff, orchestrator_summary, file:relative/path, glob:pattern).\n\
Current harness state:\n\
- orchestrator_thread_id: {}\n\
- last_orchestrator_task: {}\n\
Human message:\n{}\n",
        state
            .orchestrator_thread_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string()),
        state.last_orchestrator_task.as_deref().unwrap_or("none"),
        user_text.trim()
    )
}

pub(crate) fn build_checkpoint_prompt(
    state: &OracleSupervisorState,
    thread: &Thread,
    git_status: &str,
    diff_stat: &str,
) -> String {
    format!(
        "You are Oracle continuing the same long-lived collaboration.\n\
Mode: checkpoint_review\n\
Use prose by default. If you need machine control, append one final fenced `oracle_control` JSON block.\n\
When requesting context, ask only for targeted deltas.\n\
Current harness state:\n\
- orchestrator_thread_id: {}\n\
- last_orchestrator_task: {}\n\
Orchestrator checkpoint:\n{}\n\
Git status:\n{}\n\
Git diff stat:\n{}\n",
        thread.id,
        state.last_orchestrator_task.as_deref().unwrap_or("none"),
        summarize_thread(thread),
        git_status.trim(),
        diff_stat.trim()
    )
}

pub(crate) fn build_context_prompt(
    state: &OracleSupervisorState,
    requests: &[String],
    context: &str,
) -> String {
    format!(
        "You are Oracle continuing the same long-lived collaboration.\n\
Mode: planner_review\n\
You asked for additional repository context. Continue from this delta context without resetting the conversation.\n\
Use prose by default. If needed, append one final fenced `oracle_control` JSON block.\n\
Current harness state:\n\
- orchestrator_thread_id: {}\n\
- last_orchestrator_task: {}\n\
Requested context:\n\
{}\n\
Resolved context:\n\
{}\n",
        state
            .orchestrator_thread_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "none".to_string()),
        state.last_orchestrator_task.as_deref().unwrap_or("none"),
        requests.join("\n"),
        context.trim()
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
    "You are the hidden orchestrator operating with Oracle as a remote reasoning collaborator. Oracle turns are expensive and slow, so do substantial multi-turn work before escalating. Break work into milestones, run safe parallel workers for independent subproblems, and only escalate at major milestones, blockers, or true human-level clarification needs. End checkpoints with outcome, files changed, tests run, and unresolved blockers.".to_string()
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

pub(crate) fn parse_oracle_response(raw: &str) -> OracleResponse {
    let candidate = extract_oracle_control_json(raw)
        .or_else(|| extract_json(raw))
        .unwrap_or_else(|| raw.trim().to_string());
    if let Ok(parsed) = serde_json::from_str::<OracleJson>(&candidate) {
        let action = match parsed
            .action
            .as_deref()
            .unwrap_or("reply")
            .trim()
            .to_ascii_lowercase()
            .as_str()
        {
            "delegate" => OracleAction::Delegate,
            "request_context" => OracleAction::RequestContext,
            "ask_user" => OracleAction::AskUser,
            "finish" => OracleAction::Finish,
            _ => OracleAction::Reply,
        };
        return OracleResponse {
            action,
            message_for_user: parsed
                .message_for_user
                .unwrap_or_else(|| raw.trim().to_string()),
            task_for_orchestrator: parsed.task_for_orchestrator,
            context_requests: parsed.context_requests.unwrap_or_default(),
        };
    }
    OracleResponse {
        action: OracleAction::Reply,
        message_for_user: raw.trim().to_string(),
        task_for_orchestrator: None,
        context_requests: Vec::new(),
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
