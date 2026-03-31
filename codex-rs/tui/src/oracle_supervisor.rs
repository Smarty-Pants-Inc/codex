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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OracleCommand {
    On,
    Off,
    Status,
    Model(Option<OracleModelPreset>),
}

impl OracleCommand {
    pub(crate) fn parse(raw: &str) -> Result<Self, String> {
        let normalized = raw.trim().to_ascii_lowercase();
        let parts = normalized.split_whitespace().collect::<Vec<_>>();
        match parts.as_slice() {
            ["on"] => Ok(Self::On),
            ["off"] => Ok(Self::Off),
            ["status"] => Ok(Self::Status),
            ["model"] => Ok(Self::Model(None)),
            ["model", value] => OracleModelPreset::parse(value)
                .map(|model| Self::Model(Some(model)))
                .ok_or_else(|| "Usage: /oracle [on|off|status|model [pro|thinking]]".to_string()),
            _ => Err("Usage: /oracle [on|off|status|model [pro|thinking]]".to_string()),
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
pub(crate) struct OracleSupervisorState {
    pub(crate) oracle_thread_id: Option<ThreadId>,
    pub(crate) session_root_slug: Option<String>,
    pub(crate) current_session_id: Option<String>,
    pub(crate) orchestrator_thread_id: Option<ThreadId>,
    pub(crate) phase: OracleSupervisorPhase,
    pub(crate) model: OracleModelPreset,
    pub(crate) last_status: Option<String>,
    pub(crate) last_orchestrator_task: Option<String>,
    pub(crate) pending_turn_id: Option<String>,
    pub(crate) automatic_context_followups: u8,
}

impl OracleSupervisorState {
    pub(crate) fn intercepts(&self, thread_id: ThreadId) -> bool {
        self.oracle_thread_id == Some(thread_id)
    }

    pub(crate) fn status_message(&self) -> String {
        let enabled = self
            .oracle_thread_id
            .map_or("off".to_string(), |id| format!("on for {id}"));
        let root_slug = self
            .session_root_slug
            .clone()
            .unwrap_or_else(|| "not started".to_string());
        let current_session = self
            .current_session_id
            .clone()
            .unwrap_or_else(|| "not started".to_string());
        let orchestrator = self
            .orchestrator_thread_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| "not created".to_string());
        let last = self
            .last_status
            .clone()
            .unwrap_or_else(|| "no activity yet".to_string());
        format!(
            "Oracle mode: {enabled}\nRequested model: {}\nBrowser strategy: select\nSession root slug: {root_slug}\nCurrent session: {current_session}\nOrchestrator: {orchestrator}\nState: {}\nLast status: {last}",
            self.model.display_name(),
            self.phase.description(),
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
        "You are Oracle, the slow and expensive master supervisor over a Codex orchestrator.\n\
Optimize for high-leverage planning, not micromanagement. The hierarchy is human <-> oracle <-> orchestrator <-> many parallel workers <-> potentially more subagents.\n\
Default behavior: clarify with the human until the major feature is well specified, then delegate substantial work to the orchestrator. Re-engage only for milestones, blockers, elevated risk, or true HITL questions.\n\
If you need more repository context, return action=request_context with explicit machine-readable requests only: git_status, git_diff_stat, git_diff, orchestrator_summary, file:relative/path, or glob:pattern.\n\
Return JSON only with this schema:\n\
{{\"action\":\"reply|delegate|request_context|ask_user|finish\",\"message_for_user\":\"...\",\"task_for_orchestrator\":\"optional\",\"context_requests\":[\"optional\"]}}\n\
Current controller state:\n\
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
        "You are Oracle continuing the same long-lived supervisor thread.\n\
Decide whether to delegate more work, ask the human for clarification, or finish the milestone.\n\
If you need more repository context, return action=request_context with explicit machine-readable requests only: git_status, git_diff_stat, git_diff, orchestrator_summary, file:relative/path, or glob:pattern.\n\
Return JSON only with this schema:\n\
{{\"action\":\"reply|delegate|request_context|ask_user|finish\",\"message_for_user\":\"...\",\"task_for_orchestrator\":\"optional\",\"context_requests\":[\"optional\"]}}\n\
Current controller state:\n\
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
        "You are Oracle continuing the same long-lived supervisor thread.\n\
You asked for more repository context. Use the context below to continue supervising the Codex orchestrator.\n\
Return JSON only with this schema:\n\
{{\"action\":\"reply|delegate|request_context|ask_user|finish\",\"message_for_user\":\"...\",\"task_for_orchestrator\":\"optional\",\"context_requests\":[\"optional\"]}}\n\
Current controller state:\n\
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
    "You are the hidden orchestrator operating under an Oracle supervisor. Oracle is slow and expensive, so do substantial work before escalating. Break the task into milestones, use parallel worker agents when subproblems are independent, and only stop when the milestone is complete or you are blocked on a human-level clarification. End with a concise checkpoint covering outcome, files changed, tests run, and remaining blockers.".to_string()
}

pub(crate) fn find_oracle_repo(start: &Path) -> Option<PathBuf> {
    start.ancestors().find_map(|dir| {
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
    let candidate = extract_json(raw).unwrap_or_else(|| raw.trim().to_string());
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

    #[test]
    fn parse_oracle_command_accepts_fast_style_args() {
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
}
