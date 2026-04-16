use std::fs::File;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::PathBuf;
use std::sync::LazyLock;
use std::sync::Mutex;
use std::sync::OnceLock;

use crate::app_command::AppCommand;
use crate::legacy_core::config::Config;
use serde::Serialize;
use serde_json::json;

use crate::app_event::AppEvent;

static LOGGER: LazyLock<SessionLogger> = LazyLock::new(SessionLogger::new);

struct SessionLogger {
    file: OnceLock<Mutex<File>>,
}

impl SessionLogger {
    fn new() -> Self {
        Self {
            file: OnceLock::new(),
        }
    }

    fn open(&self, path: PathBuf) -> std::io::Result<()> {
        let mut opts = OpenOptions::new();
        opts.create(true).truncate(true).write(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }

        let file = opts.open(path)?;
        self.file.get_or_init(|| Mutex::new(file));
        Ok(())
    }

    fn write_json_line(&self, value: serde_json::Value) {
        let Some(mutex) = self.file.get() else {
            return;
        };
        let mut guard = match mutex.lock() {
            Ok(g) => g,
            Err(poisoned) => poisoned.into_inner(),
        };
        match serde_json::to_string(&value) {
            Ok(serialized) => {
                if let Err(e) = guard.write_all(serialized.as_bytes()) {
                    tracing::warn!("session log write error: {}", e);
                    return;
                }
                if let Err(e) = guard.write_all(b"\n") {
                    tracing::warn!("session log write error: {}", e);
                    return;
                }
                if let Err(e) = guard.flush() {
                    tracing::warn!("session log flush error: {}", e);
                }
            }
            Err(e) => tracing::warn!("session log serialize error: {}", e),
        }
    }

    fn is_enabled(&self) -> bool {
        self.file.get().is_some()
    }
}

fn now_ts() -> String {
    // RFC3339 for readability; consumers can parse as needed.
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn oracle_control_payload(
    directive: Option<&crate::oracle_supervisor::OracleControlDirective>,
) -> serde_json::Value {
    let Some(directive) = directive else {
        return serde_json::Value::Null;
    };
    json!({
        "schema_version": directive.schema_version,
        "op_id": directive.op_id,
        "idempotency_key": directive.idempotency_key,
        "op": directive.op.map(|value| format!("{value:?}")),
        "workflow_id": directive.workflow_id,
        "workflow_version": directive.workflow_version,
        "objective": directive.objective,
        "summary": directive.summary,
        "status": directive.status,
    })
}

fn oracle_control_issue_payload(
    issue: Option<&crate::oracle_supervisor::OracleControlIssue>,
) -> serde_json::Value {
    let Some(issue) = issue else {
        return serde_json::Value::Null;
    };
    json!({
        "kind": format!("{:?}", issue.kind),
        "message": issue.message,
    })
}

fn oracle_run_completed_value(
    result: &crate::oracle_supervisor::OracleRunResult,
) -> serde_json::Value {
    json!({
        "ts": now_ts(),
        "dir": "to_tui",
        "kind": "oracle_run_completed",
        "payload": {
            "run_id": result.run_id,
            "oracle_thread_id": result.oracle_thread_id.to_string(),
            "kind": format!("{:?}", result.kind),
            "requested_slug": result.requested_slug,
            "session_id": result.session_id,
            "source_user_text": result.source_user_text,
            "files": result.files,
            "requires_control": result.requires_control,
            "repair_attempt": result.repair_attempt,
            "action": format!("{:?}", result.response.action),
            "message_for_user": result.response.message_for_user,
            "task_for_orchestrator": result.response.task_for_orchestrator,
            "context_requests": result.response.context_requests,
            "directive": oracle_control_payload(result.response.directive.as_ref()),
            "control_issue": oracle_control_issue_payload(result.response.control_issue.as_ref()),
        }
    })
}

fn oracle_history_window_value(
    history_window: Option<&crate::oracle_broker::OracleBrokerThreadHistoryWindow>,
) -> serde_json::Value {
    let Some(history_window) = history_window else {
        return serde_json::Value::Null;
    };
    json!({
        "limit": history_window.limit,
        "returned_count": history_window.returned_count,
        "total_count": history_window.total_count,
        "truncated": history_window.truncated,
    })
}

fn oracle_run_failed_value(
    run_id: &str,
    visible_thread_id: codex_protocol::ThreadId,
    kind: crate::oracle_supervisor::OracleRequestKind,
    session_slug: &str,
    error: &str,
) -> serde_json::Value {
    json!({
        "ts": now_ts(),
        "dir": "to_tui",
        "kind": "oracle_run_failed",
        "payload": {
            "run_id": run_id,
            "visible_thread_id": visible_thread_id.to_string(),
            "kind": format!("{kind:?}"),
            "session_slug": session_slug,
            "error": error,
        }
    })
}

fn inbound_app_event_value(event: &AppEvent) -> Option<serde_json::Value> {
    match event {
        AppEvent::NewSession => Some(json!({
            "ts": now_ts(),
            "dir": "to_tui",
            "kind": "new_session",
        })),
        AppEvent::ClearUi => Some(json!({
            "ts": now_ts(),
            "dir": "to_tui",
            "kind": "clear_ui",
        })),
        AppEvent::InsertHistoryCell(cell) => Some(json!({
            "ts": now_ts(),
            "dir": "to_tui",
            "kind": "insert_history_cell",
            "lines": cell.transcript_lines(u16::MAX).len(),
        })),
        AppEvent::StartFileSearch(query) => Some(json!({
            "ts": now_ts(),
            "dir": "to_tui",
            "kind": "file_search_start",
            "query": query,
        })),
        AppEvent::FileSearchResult { query, matches } => Some(json!({
            "ts": now_ts(),
            "dir": "to_tui",
            "kind": "file_search_result",
            "query": query,
            "matches": matches.len(),
        })),
        AppEvent::ConfigureOracleMode { raw_command } => Some(json!({
            "ts": now_ts(),
            "dir": "to_tui",
            "kind": "app_event",
            "variant": "ConfigureOracleMode",
            "payload": {
                "raw_command": raw_command,
                "conversation_id": raw_command
                    .split_whitespace()
                    .find_map(|token| token.split("/c/").nth(1))
                    .map(|value| value.trim_end_matches('/').to_string()),
                "import_history": raw_command
                    .split_whitespace()
                    .any(|token| token == "--import-history"),
            },
        })),
        AppEvent::OracleRunCompleted { .. } | AppEvent::OracleRunFailed { .. } => None,
        AppEvent::OracleCheckpoint {
            thread_id,
            workflow_version,
        } => Some(json!({
            "ts": now_ts(),
            "dir": "to_tui",
            "kind": "oracle_checkpoint",
            "payload": {
                "thread_id": thread_id.to_string(),
                "workflow_version": workflow_version,
            }
        })),
        other => Some(json!({
            "ts": now_ts(),
            "dir": "to_tui",
            "kind": "app_event",
            "variant": format!("{other:?}").split('(').next().unwrap_or("app_event"),
        })),
    }
}

pub(crate) fn maybe_init(config: &Config) {
    let enabled = std::env::var("CODEX_TUI_RECORD_SESSION")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false);
    if !enabled {
        return;
    }

    let path = if let Ok(path) = std::env::var("CODEX_TUI_SESSION_LOG_PATH") {
        PathBuf::from(path)
    } else {
        let mut p = match crate::legacy_core::config::log_dir(config) {
            Ok(dir) => dir,
            Err(_) => std::env::temp_dir(),
        };
        let filename = format!(
            "session-{}.jsonl",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ")
        );
        p.push(filename);
        p
    };

    if let Err(e) = LOGGER.open(path.clone()) {
        tracing::error!("failed to open session log {:?}: {}", path, e);
        return;
    }

    // Write a header record so we can attach context.
    let header = json!({
        "ts": now_ts(),
        "dir": "meta",
        "kind": "session_start",
        "cwd": config.cwd,
        "model": config.model,
        "model_provider_id": config.model_provider_id,
        "model_provider_name": config.model_provider.name,
    });
    LOGGER.write_json_line(header);
}

pub(crate) fn log_inbound_app_event(event: &AppEvent) {
    // Log only if enabled
    if !LOGGER.is_enabled() {
        return;
    }

    if let Some(value) = inbound_app_event_value(event) {
        LOGGER.write_json_line(value);
    }
}

pub(crate) fn log_oracle_run_completed(result: &crate::oracle_supervisor::OracleRunResult) {
    if !LOGGER.is_enabled() {
        return;
    }
    LOGGER.write_json_line(oracle_run_completed_value(result));
}

pub(crate) fn log_oracle_history_import(
    thread_id: codex_protocol::ThreadId,
    conversation_id: &str,
    outcome: &str,
    message_count: Option<usize>,
    history_window: Option<&crate::oracle_broker::OracleBrokerThreadHistoryWindow>,
) {
    if !LOGGER.is_enabled() {
        return;
    }
    let value = json!({
        "ts": now_ts(),
        "dir": "to_tui",
        "kind": "oracle_history_import",
        "payload": {
            "thread_id": thread_id.to_string(),
            "conversation_id": conversation_id,
            "outcome": outcome,
            "message_count": message_count,
            "history_window": oracle_history_window_value(history_window),
        }
    });
    LOGGER.write_json_line(value);
}

pub(crate) fn log_oracle_run_failed(
    run_id: &str,
    visible_thread_id: codex_protocol::ThreadId,
    kind: crate::oracle_supervisor::OracleRequestKind,
    session_slug: &str,
    error: &str,
) {
    if !LOGGER.is_enabled() {
        return;
    }
    LOGGER.write_json_line(oracle_run_failed_value(
        run_id,
        visible_thread_id,
        kind,
        session_slug,
        error,
    ));
}

pub(crate) fn log_oracle_run_interrupted(
    thread_id: codex_protocol::ThreadId,
    run_id: &str,
    kind: crate::oracle_supervisor::OracleRequestKind,
) {
    if !LOGGER.is_enabled() {
        return;
    }
    let value = json!({
        "ts": now_ts(),
        "dir": "to_tui",
        "kind": "oracle_run_interrupted",
        "payload": {
            "thread_id": thread_id.to_string(),
            "run_id": run_id,
            "request_kind": format!("{kind:?}"),
        }
    });
    LOGGER.write_json_line(value);
}

pub(crate) fn log_outbound_op(op: &AppCommand) {
    if !LOGGER.is_enabled() {
        return;
    }
    write_record("from_tui", "op", op);
}

pub(crate) fn log_session_end() {
    if !LOGGER.is_enabled() {
        return;
    }
    let value = json!({
        "ts": now_ts(),
        "dir": "meta",
        "kind": "session_end",
    });
    LOGGER.write_json_line(value);
}

fn write_record<T>(dir: &str, kind: &str, obj: &T)
where
    T: Serialize,
{
    let value = json!({
        "ts": now_ts(),
        "dir": dir,
        "kind": kind,
        "payload": obj,
    });
    LOGGER.write_json_line(value);
}

#[cfg(test)]
mod tests {
    use super::inbound_app_event_value;
    use crate::app_event::AppEvent;
    use crate::oracle_supervisor::OracleAction;
    use crate::oracle_supervisor::OracleRequestKind;
    use crate::oracle_supervisor::OracleResponse;
    use crate::oracle_supervisor::OracleRunResult;
    use codex_protocol::ThreadId;

    fn test_thread_id() -> ThreadId {
        ThreadId::from_string("00000000-0000-0000-0000-000000000999").expect("valid thread id")
    }

    fn test_oracle_result() -> OracleRunResult {
        OracleRunResult {
            run_id: "oracle-run-test".to_string(),
            oracle_thread_id: test_thread_id(),
            kind: OracleRequestKind::UserTurn,
            requested_slug: "codex-oracle-test".to_string(),
            session_id: "codex-oracle-test-1".to_string(),
            requested_prompt: "prompt".to_string(),
            source_user_text: Some("user text".to_string()),
            files: vec!["file.txt".to_string()],
            requires_control: false,
            repair_attempt: 0,
            response: OracleResponse {
                action: OracleAction::Reply,
                message_for_user: "reply".to_string(),
                task_for_orchestrator: None,
                context_requests: Vec::new(),
                directive: None,
                control_issue: None,
                raw_output: "reply".to_string(),
            },
        }
    }

    #[test]
    fn inbound_event_logging_skips_oracle_completion_until_accepted() {
        let event = AppEvent::OracleRunCompleted {
            result: test_oracle_result(),
        };
        assert!(inbound_app_event_value(&event).is_none());
    }

    #[test]
    fn inbound_event_logging_skips_oracle_failures_until_accepted() {
        let event = AppEvent::OracleRunFailed {
            run_id: "oracle-run-test".to_string(),
            visible_thread_id: test_thread_id(),
            kind: OracleRequestKind::UserTurn,
            session_slug: "codex-oracle-test".to_string(),
            error: "boom".to_string(),
        };
        assert!(inbound_app_event_value(&event).is_none());
    }
}
