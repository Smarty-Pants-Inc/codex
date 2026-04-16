use std::collections::BTreeSet;
use std::collections::VecDeque;
use std::env;
use std::future::Future;
use std::io;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio::fs;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::ChildStdin;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::sleep;
use tokio::time::timeout;

const SUPERVISOR_RECOVERY_GRACE_PERIOD: Duration = Duration::from_secs(10);
const SUPERVISOR_FOLLOWUP_RECOVERY_GRACE_PERIOD: Duration = Duration::from_secs(90);
const ORACLE_BROKER_ABORT_GRACE_PERIOD: Duration = Duration::from_secs(2);
const ORACLE_BROKER_REQUEST_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const ORACLE_BROKER_STDERR_TAIL_LIMIT: usize = 20;
#[cfg(not(windows))]
const NODE_FALLBACK_PATHS: &[&str] = &[
    "/opt/homebrew/bin/node",
    "/usr/local/bin/node",
    "/usr/bin/node",
];
#[cfg(windows)]
const NODE_FALLBACK_PATHS: &[&str] = &[];
#[cfg(not(windows))]
const PNPM_FALLBACK_PATHS: &[&str] = &["/opt/homebrew/bin/pnpm", "/usr/local/bin/pnpm"];
#[cfg(windows)]
const PNPM_FALLBACK_PATHS: &[&str] = &[];

fn resolve_executable(program: &str, fallbacks: &[&str]) -> PathBuf {
    if let Some(path) = env::var_os("PATH").and_then(|search_path| {
        env::split_paths(&search_path).find_map(|dir| {
            let candidate = dir.join(program);
            candidate.is_file().then_some(candidate)
        })
    }) {
        return path;
    }
    fallbacks
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(program))
}

fn oracle_broker_command(oracle_repo: &Path) -> (PathBuf, Vec<String>) {
    let dist_broker = oracle_repo
        .join("dist")
        .join("bin")
        .join("oracle-supervisor-broker.js");
    if dist_broker.is_file() {
        return (
            resolve_executable("node", NODE_FALLBACK_PATHS),
            vec![dist_broker.display().to_string()],
        );
    }

    let tsx_cli = oracle_repo
        .join("node_modules")
        .join("tsx")
        .join("dist")
        .join("cli.mjs");
    if tsx_cli.is_file() {
        return (
            resolve_executable("node", NODE_FALLBACK_PATHS),
            vec![
                tsx_cli.display().to_string(),
                "bin/oracle-supervisor-broker.ts".to_string(),
            ],
        );
    }

    (
        resolve_executable("pnpm", PNPM_FALLBACK_PATHS),
        vec![
            "exec".to_string(),
            "tsx".to_string(),
            "bin/oracle-supervisor-broker.ts".to_string(),
        ],
    )
}

#[cfg(unix)]
fn terminate_process_group(process_group_id: u32) -> io::Result<()> {
    let result = unsafe { libc::killpg(process_group_id as libc::pid_t, libc::SIGTERM) };
    if result == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::NotFound {
            return Err(err);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn terminate_process_group(_process_group_id: u32) -> io::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn kill_process_group(process_group_id: u32) -> io::Result<()> {
    let result = unsafe { libc::killpg(process_group_id as libc::pid_t, libc::SIGKILL) };
    if result == -1 {
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::NotFound {
            return Err(err);
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn kill_process_group(_process_group_id: u32) -> io::Result<()> {
    Ok(())
}

fn kill_child_process_group(child: &mut Child) -> io::Result<()> {
    if let Some(pid) = child.id() {
        kill_process_group(pid)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct OracleBrokerClient {
    tx: mpsc::Sender<BrokerCommand>,
    abort_handle: tokio::task::AbortHandle,
    transport_alive: Arc<AtomicBool>,
    process_group_id: Option<u32>,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleBrokerRequest {
    pub(crate) prompt: String,
    pub(crate) session_slug: String,
    pub(crate) followup_session: Option<String>,
    pub(crate) files: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) model: String,
    pub(crate) browser_model_strategy: String,
    pub(crate) browser_model_label: Option<String>,
    pub(crate) browser_thinking_time: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct OracleBrokerResponse {
    #[serde(rename = "sessionId")]
    pub(crate) session_id: Option<String>,
    #[serde(rename = "conversationId")]
    pub(crate) conversation_id: Option<String>,
    pub(crate) title: Option<String>,
    pub(crate) output: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OracleSessionMeta {
    id: String,
    #[serde(rename = "promptPreview")]
    prompt_preview: Option<String>,
    status: Option<String>,
    browser: Option<OracleSessionMetaBrowser>,
    response: Option<OracleSessionMetaResponse>,
}

#[derive(Debug, Deserialize)]
struct OracleSessionMetaResponse {
    #[serde(rename = "assistantOutput")]
    assistant_output: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OracleSessionMetaBrowser {
    runtime: Option<OracleSessionMetaBrowserRuntime>,
}

#[derive(Debug, Deserialize)]
struct OracleSessionMetaBrowserRuntime {
    #[serde(rename = "conversationId")]
    conversation_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct OracleBrokerThreadEntry {
    pub(crate) title: String,
    #[serde(rename = "conversationId")]
    pub(crate) conversation_id: String,
    pub(crate) url: Option<String>,
    #[serde(rename = "isCurrent", default)]
    pub(crate) is_current: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct OracleBrokerThreadOpenResponse {
    #[serde(rename = "sessionId")]
    pub(crate) session_id: Option<String>,
    pub(crate) title: String,
    #[serde(rename = "conversationId")]
    pub(crate) conversation_id: Option<String>,
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) history: Vec<OracleBrokerThreadHistoryEntry>,
}

#[derive(Debug, Deserialize)]
struct OracleBrokerThreadOpenEnvelope {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    thread: OracleBrokerThreadOpenResponse,
    #[serde(default)]
    history: Vec<OracleBrokerThreadHistoryEntry>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct OracleBrokerThreadHistoryEntry {
    pub(crate) role: String,
    pub(crate) text: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct OracleBrokerThreadHistoryWindow {
    pub(crate) limit: usize,
    #[serde(rename = "returnedCount")]
    pub(crate) returned_count: usize,
    #[serde(rename = "totalCount")]
    pub(crate) total_count: usize,
    pub(crate) truncated: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct OracleBrokerThreadHistoryResponse {
    #[serde(rename = "sessionId")]
    pub(crate) session_id: Option<String>,
    pub(crate) title: String,
    #[serde(rename = "conversationId")]
    pub(crate) conversation_id: Option<String>,
    pub(crate) url: Option<String>,
    #[serde(default)]
    pub(crate) history: Vec<OracleBrokerThreadHistoryEntry>,
    #[serde(rename = "historyWindow")]
    pub(crate) history_window: Option<OracleBrokerThreadHistoryWindow>,
}

#[derive(Debug, Deserialize)]
struct OracleBrokerThreadHistoryEnvelope {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    thread: OracleBrokerThreadOpenResponse,
    #[serde(default)]
    history: Vec<OracleBrokerThreadHistoryEntry>,
    #[serde(rename = "historyWindow")]
    history_window: Option<OracleBrokerThreadHistoryWindow>,
}

#[derive(Debug, Serialize)]
struct OracleBrokerWireRequest {
    action: &'static str,
    prompt: String,
    #[serde(rename = "sessionSlug")]
    session_slug: String,
    #[serde(rename = "followupSession", skip_serializing_if = "Option::is_none")]
    followup_session: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    files: Vec<String>,
    cwd: String,
    model: String,
    #[serde(rename = "browserModelStrategy")]
    browser_model_strategy: String,
    #[serde(rename = "browserModelLabel", skip_serializing_if = "Option::is_none")]
    browser_model_label: Option<String>,
    #[serde(
        rename = "browserThinkingTime",
        skip_serializing_if = "Option::is_none"
    )]
    browser_thinking_time: Option<String>,
}

#[derive(Debug, Serialize)]
struct OracleBrokerThreadListWireRequest {
    action: &'static str,
    #[serde(rename = "followupSession", skip_serializing_if = "Option::is_none")]
    followup_session: Option<String>,
}

#[derive(Debug, Serialize)]
struct OracleBrokerThreadNewWireRequest {
    action: &'static str,
    #[serde(rename = "followupSession", skip_serializing_if = "Option::is_none")]
    followup_session: Option<String>,
}

#[derive(Debug, Serialize)]
struct OracleBrokerThreadAttachWireRequest {
    action: &'static str,
    #[serde(rename = "followupSession", skip_serializing_if = "Option::is_none")]
    followup_session: Option<String>,
    #[serde(rename = "conversationId")]
    conversation_id: String,
    #[serde(rename = "threadUrl", skip_serializing_if = "Option::is_none")]
    thread_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct OracleBrokerThreadHistoryWireRequest {
    action: &'static str,
    #[serde(rename = "followupSession", skip_serializing_if = "Option::is_none")]
    followup_session: Option<String>,
    #[serde(rename = "conversationId")]
    conversation_id: String,
    #[serde(rename = "historyLimit", skip_serializing_if = "Option::is_none")]
    history_limit: Option<usize>,
}

enum BrokerCommand {
    Request {
        payload: Value,
        reply: oneshot::Sender<Result<Value, String>>,
    },
    Shutdown {
        reply: oneshot::Sender<()>,
    },
}

pub(crate) fn spawn_oracle_broker(oracle_repo: &Path) -> Result<OracleBrokerClient, String> {
    let (program, args) = oracle_broker_command(oracle_repo);
    let mut child = Command::new(program);
    child.args(args);
    child
        .current_dir(oracle_repo)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    child.kill_on_drop(true);
    #[cfg(unix)]
    child.process_group(0);
    let mut child = child
        .spawn()
        .map_err(|error| format!("failed to start Oracle broker: {error}"))?;
    let process_group_id = child.id();
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| "Oracle broker stdin was unavailable".to_string())?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "Oracle broker stdout was unavailable".to_string())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "Oracle broker stderr was unavailable".to_string())?;

    let stderr_tail = Arc::new(Mutex::new(VecDeque::with_capacity(
        ORACLE_BROKER_STDERR_TAIL_LIMIT,
    )));
    let stderr_tail_for_task = Arc::clone(&stderr_tail);
    let (tx, rx) = mpsc::channel(8);
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "codex_tui::oracle_broker", %line, "oracle broker stderr");
            if let Ok(mut tail) = stderr_tail_for_task.lock() {
                if tail.len() == ORACLE_BROKER_STDERR_TAIL_LIMIT {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        }
    });
    let broker_task = tokio::spawn(run_broker_loop(
        child,
        stdin,
        BufReader::new(stdout).lines(),
        rx,
        stderr_task,
        stderr_tail,
    ));
    Ok(OracleBrokerClient {
        tx,
        abort_handle: broker_task.abort_handle(),
        transport_alive: Arc::new(AtomicBool::new(true)),
        process_group_id,
    })
}

impl OracleBrokerClient {
    fn mark_transport_unavailable(&self) {
        self.transport_alive.store(false, Ordering::Release);
    }

    pub(crate) fn is_usable(&self) -> bool {
        self.transport_alive.load(Ordering::Acquire)
            && !self.tx.is_closed()
            && !self.abort_handle.is_finished()
    }

    #[cfg(test)]
    pub(crate) fn new_test_client() -> Self {
        let (tx, mut rx) = mpsc::channel(1);
        let broker_task = tokio::spawn(async move { while rx.recv().await.is_some() {} });
        Self {
            tx,
            abort_handle: broker_task.abort_handle(),
            transport_alive: Arc::new(AtomicBool::new(true)),
            process_group_id: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_hanging_test_client() -> Self {
        let (tx, mut rx) = mpsc::channel(1);
        let broker_task = tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                match command {
                    BrokerCommand::Request { .. } => std::future::pending::<()>().await,
                    BrokerCommand::Shutdown { reply } => {
                        let _ = reply.send(());
                        break;
                    }
                }
            }
        });
        Self {
            tx,
            abort_handle: broker_task.abort_handle(),
            transport_alive: Arc::new(AtomicBool::new(true)),
            process_group_id: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn new_error_test_client(error: &str) -> Self {
        let (tx, mut rx) = mpsc::channel(1);
        let error = error.to_string();
        let broker_task = tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                match command {
                    BrokerCommand::Request { reply, .. } => {
                        let _ = reply.send(Err(error.clone()));
                    }
                    BrokerCommand::Shutdown { reply } => {
                        let _ = reply.send(());
                        break;
                    }
                }
            }
        });
        Self {
            tx,
            abort_handle: broker_task.abort_handle(),
            transport_alive: Arc::new(AtomicBool::new(true)),
            process_group_id: None,
        }
    }

    pub(crate) async fn request(
        &self,
        request: OracleBrokerRequest,
    ) -> Result<OracleBrokerResponse, String> {
        let (reply, recv) = oneshot::channel();
        let recovery_session_slug = request.session_slug.clone();
        let recovery_followup_session = request.followup_session.clone();
        let payload = serde_json::to_value(OracleBrokerWireRequest {
            action: "run_prompt",
            prompt: request.prompt,
            session_slug: request.session_slug,
            followup_session: request.followup_session,
            files: request.files,
            cwd: request.cwd.display().to_string(),
            model: request.model,
            browser_model_strategy: request.browser_model_strategy,
            browser_model_label: request.browser_model_label,
            browser_thinking_time: request.browser_thinking_time,
        })
        .map_err(|error| format!("failed to serialize Oracle broker request: {error}"))?;
        if self
            .tx
            .send(BrokerCommand::Request { payload, reply })
            .await
            .is_err()
        {
            self.mark_transport_unavailable();
            return Err("Oracle broker is unavailable".to_string());
        }
        let recv_result = async {
            let value = match recv.await {
                Ok(Ok(value)) => value,
                Ok(Err(error)) => {
                    self.mark_transport_unavailable();
                    return Err(error);
                }
                Err(_) => {
                    self.mark_transport_unavailable();
                    return Err("Oracle broker closed before replying".to_string());
                }
            };
            parse_success_response::<OracleBrokerResponse>(value).inspect_err(|_| {
                self.mark_transport_unavailable();
            })
        };
        let recovery_result = await_recoverable_supervisor_response(
            recovery_session_slug.as_str(),
            recovery_followup_session.as_deref(),
        );
        let recovery_grace_period = if recovery_followup_session.is_some() {
            SUPERVISOR_FOLLOWUP_RECOVERY_GRACE_PERIOD
        } else {
            SUPERVISOR_RECOVERY_GRACE_PERIOD
        };
        tokio::pin!(recv_result);
        tokio::pin!(recovery_result);
        let request_result = tokio::time::timeout(oracle_broker_request_timeout(), async {
            tokio::select! {
                result = &mut recv_result => {
                    prefer_recovered_supervisor_response_with_timeout(
                        result,
                        recovery_session_slug.as_str(),
                        await_recoverable_supervisor_response(
                            recovery_session_slug.as_str(),
                            recovery_followup_session.as_deref(),
                        ),
                        recovery_grace_period,
                    )
                    .await
                },
                recovered = &mut recovery_result => {
                    tracing::warn!(
                        session_slug = %recovery_session_slug,
                        followup_session = recovery_followup_session.as_deref(),
                        "recovered Oracle broker response from persisted session metadata; resetting broker transport"
                    );
                    self.abort();
                    Ok(recovered)
                }
            }
        })
        .await;
        match request_result {
            Ok(result) => result,
            Err(_) => {
                let timeout = oracle_broker_request_timeout();
                self.abort();
                Err(format!(
                    "Oracle supervisor prompt timed out after {} without a response.",
                    format_duration(timeout)
                ))
            }
        }
    }

    pub(crate) async fn list_threads(
        &self,
        followup_session: Option<String>,
    ) -> Result<Vec<OracleBrokerThreadEntry>, String> {
        let (reply, recv) = oneshot::channel();
        let payload = serde_json::to_value(OracleBrokerThreadListWireRequest {
            action: "list_threads",
            followup_session,
        })
        .map_err(|error| {
            format!("failed to serialize Oracle broker thread list request: {error}")
        })?;
        if self
            .tx
            .send(BrokerCommand::Request { payload, reply })
            .await
            .is_err()
        {
            self.mark_transport_unavailable();
            return Err("Oracle broker is unavailable".to_string());
        }
        let value = match recv.await {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                self.mark_transport_unavailable();
                return Err(error);
            }
            Err(_) => {
                self.mark_transport_unavailable();
                return Err("Oracle broker closed before replying".to_string());
            }
        };
        parse_success_field_response::<Vec<OracleBrokerThreadEntry>>(value, "threads").inspect_err(
            |_| {
                self.mark_transport_unavailable();
            },
        )
    }

    pub(crate) async fn new_thread(
        &self,
        followup_session: Option<String>,
    ) -> Result<OracleBrokerThreadOpenResponse, String> {
        let (reply, recv) = oneshot::channel();
        let payload = serde_json::to_value(OracleBrokerThreadNewWireRequest {
            action: "new_thread",
            followup_session,
        })
        .map_err(|error| {
            format!("failed to serialize Oracle broker thread new request: {error}")
        })?;
        if self
            .tx
            .send(BrokerCommand::Request { payload, reply })
            .await
            .is_err()
        {
            self.mark_transport_unavailable();
            return Err("Oracle broker is unavailable".to_string());
        }
        let value = match recv.await {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                self.mark_transport_unavailable();
                return Err(error);
            }
            Err(_) => {
                self.mark_transport_unavailable();
                return Err("Oracle broker closed before replying".to_string());
            }
        };
        decode_thread_open_envelope(value).inspect_err(|_| {
            self.mark_transport_unavailable();
        })
    }

    pub(crate) async fn attach_thread(
        &self,
        conversation_id: impl Into<String>,
        thread_url: Option<String>,
        followup_session: Option<String>,
    ) -> Result<OracleBrokerThreadOpenResponse, String> {
        let (reply, recv) = oneshot::channel();
        let payload = serde_json::to_value(OracleBrokerThreadAttachWireRequest {
            action: "attach_thread",
            followup_session,
            conversation_id: conversation_id.into(),
            thread_url,
        })
        .map_err(|error| {
            format!("failed to serialize Oracle broker thread attach request: {error}")
        })?;
        if self
            .tx
            .send(BrokerCommand::Request { payload, reply })
            .await
            .is_err()
        {
            self.mark_transport_unavailable();
            return Err("Oracle broker is unavailable".to_string());
        }
        let value = match recv.await {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                self.mark_transport_unavailable();
                return Err(error);
            }
            Err(_) => {
                self.mark_transport_unavailable();
                return Err("Oracle broker closed before replying".to_string());
            }
        };
        decode_thread_open_envelope(value).inspect_err(|_| {
            self.mark_transport_unavailable();
        })
    }

    pub(crate) async fn thread_history(
        &self,
        followup_session: Option<String>,
        conversation_id: String,
        history_limit: Option<usize>,
    ) -> Result<OracleBrokerThreadHistoryResponse, String> {
        let (reply, recv) = oneshot::channel();
        let payload = serde_json::to_value(OracleBrokerThreadHistoryWireRequest {
            action: "thread_history",
            followup_session,
            conversation_id,
            history_limit,
        })
        .map_err(|error| {
            format!("failed to serialize Oracle broker thread history request: {error}")
        })?;
        if self
            .tx
            .send(BrokerCommand::Request { payload, reply })
            .await
            .is_err()
        {
            self.mark_transport_unavailable();
            return Err("Oracle broker is unavailable".to_string());
        }
        let value = match recv.await {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => {
                self.mark_transport_unavailable();
                return Err(error);
            }
            Err(_) => {
                self.mark_transport_unavailable();
                return Err("Oracle broker closed before replying".to_string());
            }
        };
        decode_thread_history_envelope(value).inspect_err(|_| {
            self.mark_transport_unavailable();
        })
    }

    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        let (reply, recv) = oneshot::channel();
        self.mark_transport_unavailable();
        if self
            .tx
            .send(BrokerCommand::Shutdown { reply })
            .await
            .is_err()
        {
            self.abort_handle.abort();
            return Ok(());
        }
        let _ = recv.await;
        Ok(())
    }

    pub(crate) fn abort(&self) {
        self.mark_transport_unavailable();
        let abort_handle = self.abort_handle.clone();
        let process_group_id = self.process_group_id;
        tokio::spawn(async move {
            if let Some(process_group_id) = process_group_id {
                if let Err(error) = terminate_process_group(process_group_id) {
                    tracing::warn!(
                        process_group_id,
                        %error,
                        "failed to terminate Oracle broker process group"
                    );
                }
                sleep(ORACLE_BROKER_ABORT_GRACE_PERIOD).await;
                if let Err(error) = kill_process_group(process_group_id) {
                    tracing::warn!(
                        process_group_id,
                        %error,
                        "failed to kill Oracle broker process group"
                    );
                }
            }
            abort_handle.abort();
        });
    }
}

fn decode_thread_open_envelope(value: Value) -> Result<OracleBrokerThreadOpenResponse, String> {
    let mut response = parse_success_response::<OracleBrokerThreadOpenEnvelope>(value)?;
    response.thread.session_id = response.session_id.take();
    if response.thread.history.is_empty() && !response.history.is_empty() {
        response.thread.history = response.history;
    }
    Ok(response.thread)
}

fn decode_thread_history_envelope(
    value: Value,
) -> Result<OracleBrokerThreadHistoryResponse, String> {
    let response = parse_success_response::<OracleBrokerThreadHistoryEnvelope>(value)?;
    let history = if response.history.is_empty() {
        response.thread.history
    } else {
        response.history
    };
    Ok(OracleBrokerThreadHistoryResponse {
        session_id: response.session_id,
        title: response.thread.title,
        conversation_id: response.thread.conversation_id,
        url: response.thread.url,
        history,
        history_window: response.history_window,
    })
}

async fn prefer_recovered_supervisor_response_with_timeout<F>(
    transport_result: Result<OracleBrokerResponse, String>,
    session_slug: &str,
    recovery_result: F,
    grace_period: Duration,
) -> Result<OracleBrokerResponse, String>
where
    F: Future<Output = OracleBrokerResponse>,
{
    let transport = match transport_result {
        Ok(transport) => transport,
        Err(error) => {
            return match timeout(grace_period, recovery_result).await {
                Ok(recovered) => Ok(recovered),
                Err(_) => Err(error),
            };
        }
    };
    if transport.session_id.is_none() {
        return Ok(transport);
    }
    match timeout(grace_period, recovery_result).await {
        Ok(recovered)
            if recovered != transport
                && should_prefer_recovered_supervisor_response(
                    &transport,
                    &recovered,
                    session_slug,
                ) =>
        {
            Ok(recovered)
        }
        _ => Ok(transport),
    }
}

fn should_prefer_recovered_supervisor_response(
    transport: &OracleBrokerResponse,
    recovered: &OracleBrokerResponse,
    session_slug: &str,
) -> bool {
    if !oracle_conversations_compatible(
        transport.conversation_id.as_deref(),
        recovered.conversation_id.as_deref(),
    ) {
        return false;
    }
    if transport.session_id == recovered.session_id {
        return true;
    }
    match (
        transport
            .session_id
            .as_deref()
            .and_then(|session_id| oracle_session_chain_ordinal(session_id, session_slug)),
        recovered
            .session_id
            .as_deref()
            .and_then(|session_id| oracle_session_chain_ordinal(session_id, session_slug)),
    ) {
        (Some(transport_ordinal), Some(recovered_ordinal)) => {
            recovered_ordinal >= transport_ordinal
        }
        _ => false,
    }
}

fn oracle_conversations_compatible(
    transport_conversation_id: Option<&str>,
    recovered_conversation_id: Option<&str>,
) -> bool {
    match (transport_conversation_id, recovered_conversation_id) {
        (Some(transport), Some(recovered)) => transport == recovered,
        (Some(_), None) => false,
        _ => true,
    }
}

async fn run_broker_loop(
    mut child: Child,
    mut stdin: ChildStdin,
    mut stdout_lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    mut rx: mpsc::Receiver<BrokerCommand>,
    stderr_task: tokio::task::JoinHandle<()>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
) {
    while let Some(command) = rx.recv().await {
        match command {
            BrokerCommand::Request { payload, reply } => {
                let result = send_and_read(
                    &mut child,
                    &mut stdin,
                    &mut stdout_lines,
                    &stderr_tail,
                    payload,
                )
                .await;
                let fatal_error = result.as_ref().err().cloned();
                let _ = reply.send(result);
                if let Some(error) = fatal_error {
                    while let Some(queued) = rx.recv().await {
                        match queued {
                            BrokerCommand::Request { reply, .. } => {
                                let _ = reply.send(Err(error.clone()));
                            }
                            BrokerCommand::Shutdown { reply } => {
                                let _ = reply.send(());
                                break;
                            }
                        }
                    }
                    break;
                }
            }
            BrokerCommand::Shutdown { reply } => {
                let _ = send_shutdown(&mut stdin).await;
                let _ = reply.send(());
                break;
            }
        }
    }
    let _ = stdin.shutdown().await;
    if timeout(Duration::from_secs(5), child.wait()).await.is_err() {
        let _ = kill_child_process_group(&mut child);
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    stderr_task.abort();
}

async fn send_and_read(
    child: &mut Child,
    stdin: &mut ChildStdin,
    stdout_lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    stderr_tail: &Arc<Mutex<VecDeque<String>>>,
    payload: Value,
) -> Result<Value, String> {
    let payload = serde_json::to_string(&payload)
        .map_err(|error| format!("failed to serialize Oracle broker request: {error}"))?;
    stdin
        .write_all(payload.as_bytes())
        .await
        .map_err(|error| format!("failed to write Oracle broker request: {error}"))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|error| format!("failed to finalize Oracle broker request: {error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("failed to flush Oracle broker request: {error}"))?;

    let line = stdout_lines
        .next_line()
        .await
        .map_err(|error| format!("failed to read Oracle broker response: {error}"))?
        .ok_or_else(|| format_oracle_broker_unexpected_exit_message(child, stderr_tail))?;
    serde_json::from_str(&line)
        .map_err(|error| format!("failed to parse Oracle broker response: {error}"))
}

fn format_oracle_broker_unexpected_exit_message(
    child: &mut Child,
    stderr_tail: &Arc<Mutex<VecDeque<String>>>,
) -> String {
    let exit_status = child
        .try_wait()
        .ok()
        .flatten()
        .map(format_oracle_broker_exit_status);
    let stderr_excerpt = stderr_tail
        .lock()
        .ok()
        .map(|lines| lines.iter().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    format_oracle_broker_unexpected_exit_message_parts(exit_status, &stderr_excerpt)
}

fn format_oracle_broker_exit_status(status: std::process::ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit code {code}");
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return format!("signal {signal}");
    }
    "unknown status".to_string()
}

fn format_oracle_broker_unexpected_exit_message_parts(
    exit_status: Option<String>,
    stderr_tail: &[String],
) -> String {
    let mut details = Vec::new();
    if let Some(status) = exit_status.filter(|status| !status.is_empty()) {
        details.push(status);
    }
    if !stderr_tail.is_empty() {
        details.push(format!("stderr tail: {}", stderr_tail.join(" | ")));
    }
    if details.is_empty() {
        "Oracle broker exited without replying".to_string()
    } else {
        format!(
            "Oracle broker exited without replying ({})",
            details.join("; ")
        )
    }
}

async fn send_shutdown(stdin: &mut ChildStdin) -> Result<(), String> {
    let payload = serde_json::to_string(&serde_json::json!({ "shutdown": true }))
        .map_err(|error| format!("failed to serialize Oracle broker shutdown request: {error}"))?;
    stdin
        .write_all(payload.as_bytes())
        .await
        .map_err(|error| format!("failed to write Oracle broker shutdown request: {error}"))?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|error| format!("failed to finalize Oracle broker shutdown request: {error}"))?;
    stdin
        .flush()
        .await
        .map_err(|error| format!("failed to flush Oracle broker shutdown request: {error}"))?;
    Ok(())
}

fn parse_success_response<T>(value: Value) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    ensure_success_response(&value)?;
    serde_json::from_value(value)
        .map_err(|error| format!("failed to decode Oracle broker success payload: {error}"))
}

fn parse_success_field_response<T>(value: Value, field: &str) -> Result<T, String>
where
    T: for<'de> Deserialize<'de>,
{
    ensure_success_response(&value)?;
    let field_value = value
        .get(field)
        .cloned()
        .ok_or_else(|| format!("Oracle broker success response was missing `{field}`."))?;
    serde_json::from_value(field_value)
        .map_err(|error| format!("failed to decode Oracle broker field `{field}`: {error}"))
}

fn ensure_success_response(value: &Value) -> Result<(), String> {
    let ok = value
        .get("ok")
        .and_then(Value::as_bool)
        .ok_or_else(|| "Oracle broker response was missing `ok`.".to_string())?;
    if ok {
        return Ok(());
    }
    let error = value
        .get("error")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "Oracle broker returned an unknown error".to_string());
    let session_id = value
        .get("sessionId")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    Err(if let Some(session_id) = session_id {
        format!("{error} (session: {session_id})")
    } else {
        error
    })
}

fn oracle_sessions_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".oracle").join("sessions"))
}

fn oracle_session_chain_ordinal(session_id: &str, session_slug: &str) -> Option<u32> {
    if session_id == session_slug {
        return Some(1);
    }
    let suffix = session_id.strip_prefix(session_slug)?.strip_prefix('-')?;
    suffix.parse::<u32>().ok()
}

fn oracle_broker_request_timeout() -> Duration {
    if cfg!(test) {
        Duration::from_millis(50)
    } else {
        ORACLE_BROKER_REQUEST_TIMEOUT
    }
}

fn format_duration(duration: Duration) -> String {
    if duration.as_secs() > 0 {
        format!("{} seconds", duration.as_secs())
    } else {
        format!("{} ms", duration.as_millis())
    }
}

async fn read_recoverable_supervisor_response_from_dir(
    sessions_dir: &Path,
    session_slug: &str,
    followup_session: Option<&str>,
) -> Option<OracleBrokerResponse> {
    let expected_followup_conversation_id =
        read_followup_session_conversation_id(sessions_dir, followup_session).await;
    let minimum_ordinal = followup_session
        .and_then(|session_id| oracle_session_chain_ordinal(session_id, session_slug))
        .unwrap_or(0);
    let mut entries = fs::read_dir(sessions_dir).await.ok()?;
    let mut recovered: Vec<(u32, Option<String>, OracleBrokerResponse)> = Vec::new();
    loop {
        let entry = match entries.next_entry().await {
            Ok(Some(entry)) => entry,
            Ok(None) => break,
            Err(_) => continue,
        };
        let entry_path = entry.path();
        if !entry_path.is_dir() {
            continue;
        }
        let Some(session_id) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let Some(ordinal) = oracle_session_chain_ordinal(session_id.as_str(), session_slug) else {
            continue;
        };
        if ordinal <= minimum_ordinal {
            continue;
        }
        let meta_path = entry_path.join("meta.json");
        let Ok(meta_raw) = fs::read_to_string(meta_path).await else {
            continue;
        };
        let Ok(meta) = serde_json::from_str::<OracleSessionMeta>(&meta_raw) else {
            continue;
        };
        if meta.status.as_deref() != Some("completed") {
            continue;
        }
        let Some(output) = meta
            .response
            .and_then(|response| response.assistant_output)
            .filter(|value| !value.trim().is_empty())
        else {
            continue;
        };
        let conversation_id = meta
            .browser
            .as_ref()
            .and_then(|browser| browser.runtime.as_ref())
            .and_then(|runtime| runtime.conversation_id.as_deref())
            .filter(|value| !value.trim().is_empty())
            .map(str::to_owned);
        if let Some(expected_followup_conversation_id) =
            expected_followup_conversation_id.as_deref()
            && conversation_id.as_deref() != Some(expected_followup_conversation_id)
        {
            continue;
        }
        let response = OracleBrokerResponse {
            session_id: Some(meta.id),
            conversation_id: conversation_id.clone(),
            title: meta.prompt_preview,
            output: Some(output),
        };
        recovered.push((ordinal, conversation_id, response));
    }
    if followup_session.is_some() && expected_followup_conversation_id.is_none() {
        let recovered_conversation_ids = recovered
            .iter()
            .filter_map(|(_, conversation_id, _)| conversation_id.clone())
            .collect::<BTreeSet<_>>();
        if recovered_conversation_ids.len() > 1 {
            return None;
        }
        if let Some(required_conversation_id) = recovered_conversation_ids.iter().next() {
            recovered.retain(|(_, conversation_id, _)| {
                conversation_id.as_deref() == Some(required_conversation_id.as_str())
            });
        }
    }
    recovered
        .into_iter()
        .max_by_key(|(ordinal, _, _)| *ordinal)
        .map(|(_, _, response)| response)
}

async fn read_followup_session_conversation_id(
    sessions_dir: &Path,
    followup_session: Option<&str>,
) -> Option<String> {
    let followup_session = followup_session?;
    let meta_path = sessions_dir.join(followup_session).join("meta.json");
    let meta_raw = fs::read_to_string(meta_path).await.ok()?;
    let meta = serde_json::from_str::<OracleSessionMeta>(&meta_raw).ok()?;
    meta.browser
        .and_then(|browser| browser.runtime)
        .and_then(|runtime| runtime.conversation_id)
        .filter(|conversation_id| !conversation_id.trim().is_empty())
}

async fn await_recoverable_supervisor_response(
    session_slug: &str,
    followup_session: Option<&str>,
) -> OracleBrokerResponse {
    loop {
        if let Some(sessions_dir) = oracle_sessions_dir()
            && let Some(response) = read_recoverable_supervisor_response_from_dir(
                sessions_dir.as_path(),
                session_slug,
                followup_session,
            )
            .await
        {
            return response;
        }
        sleep(Duration::from_millis(500)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::OracleBrokerClient;
    use super::OracleBrokerResponse;
    use super::OracleBrokerThreadHistoryEntry;
    use super::OracleBrokerThreadHistoryWindow;
    use super::OracleBrokerThreadOpenResponse;
    use super::decode_thread_history_envelope;
    use super::decode_thread_open_envelope;
    use super::format_oracle_broker_unexpected_exit_message_parts;
    use super::oracle_broker_command;
    use super::oracle_session_chain_ordinal;
    use super::prefer_recovered_supervisor_response_with_timeout;
    use super::read_recoverable_supervisor_response_from_dir;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::time::Duration;
    use tokio::time::sleep;

    fn write_meta(dir: &std::path::Path, session_id: &str, body: &str) {
        let session_dir = dir.join(session_id);
        std::fs::create_dir_all(&session_dir).expect("session dir");
        std::fs::write(session_dir.join("meta.json"), body).expect("meta");
    }

    #[test]
    fn oracle_session_chain_ordinal_parses_root_and_followup_ids() {
        assert_eq!(
            oracle_session_chain_ordinal("oracle-root", "oracle-root"),
            Some(1)
        );
        assert_eq!(
            oracle_session_chain_ordinal("oracle-root-7", "oracle-root"),
            Some(7)
        );
        assert_eq!(
            oracle_session_chain_ordinal("other-root-2", "oracle-root"),
            None
        );
        assert_eq!(
            oracle_session_chain_ordinal("oracle-root-bad", "oracle-root"),
            None
        );
    }

    #[test]
    fn decode_thread_open_envelope_accepts_top_level_history() {
        let response = decode_thread_open_envelope(json!({
            "ok": true,
            "sessionId": "runtime-1",
            "thread": {
                "title": "Attached Thread",
                "conversationId": "attached-1",
                "url": "https://chatgpt.com/c/attached-1"
            },
            "history": [
                { "role": "user", "text": "hello" },
                { "role": "assistant", "text": "world" }
            ]
        }))
        .expect("thread open envelope");

        assert_eq!(
            response,
            OracleBrokerThreadOpenResponse {
                session_id: Some("runtime-1".to_string()),
                title: "Attached Thread".to_string(),
                conversation_id: Some("attached-1".to_string()),
                url: Some("https://chatgpt.com/c/attached-1".to_string()),
                history: vec![
                    OracleBrokerThreadHistoryEntry {
                        role: "user".to_string(),
                        text: "hello".to_string(),
                    },
                    OracleBrokerThreadHistoryEntry {
                        role: "assistant".to_string(),
                        text: "world".to_string(),
                    },
                ],
            }
        );
    }

    #[test]
    fn decode_thread_open_envelope_preserves_nested_history() {
        let response = decode_thread_open_envelope(json!({
            "ok": true,
            "sessionId": "runtime-1",
            "thread": {
                "title": "Attached Thread",
                "conversationId": "attached-1",
                "url": "https://chatgpt.com/c/attached-1",
                "history": [
                    { "role": "assistant", "text": "already nested" }
                ]
            },
            "history": [
                { "role": "user", "text": "should not overwrite nested" }
            ]
        }))
        .expect("thread open envelope");

        assert_eq!(
            response.history,
            vec![OracleBrokerThreadHistoryEntry {
                role: "assistant".to_string(),
                text: "already nested".to_string(),
            }]
        );
    }

    #[test]
    fn decode_thread_history_envelope_accepts_nested_history() {
        let response = decode_thread_history_envelope(json!({
            "ok": true,
            "sessionId": "runtime-1",
            "thread": {
                "title": "Attached Thread",
                "conversationId": "attached-1",
                "url": "https://chatgpt.com/c/attached-1",
                "history": [
                    { "role": "assistant", "text": "already nested" }
                ]
            },
            "history": []
        }))
        .expect("thread history envelope");

        assert_eq!(
            response.history,
            vec![OracleBrokerThreadHistoryEntry {
                role: "assistant".to_string(),
                text: "already nested".to_string(),
            }]
        );
        assert_eq!(response.history_window, None);
    }

    #[test]
    fn decode_thread_history_envelope_preserves_history_window_metadata() {
        let response = decode_thread_history_envelope(json!({
            "ok": true,
            "sessionId": "runtime-1",
            "thread": {
                "title": "Attached Thread",
                "conversationId": "attached-1",
                "url": "https://chatgpt.com/c/attached-1"
            },
            "history": [
                { "role": "assistant", "text": "latest" }
            ],
            "historyWindow": {
                "limit": 100,
                "returnedCount": 1,
                "totalCount": 135,
                "truncated": true
            }
        }))
        .expect("thread history envelope");

        assert_eq!(
            response.history_window,
            Some(OracleBrokerThreadHistoryWindow {
                limit: 100,
                returned_count: 1,
                total_count: 135,
                truncated: true,
            })
        );
    }

    #[tokio::test]
    async fn recoverable_supervisor_response_prefers_newer_completed_followup() {
        let temp = tempdir().expect("tempdir");
        write_meta(
            temp.path(),
            "oracle-root-2",
            r#"{"id":"oracle-root-2","status":"completed","response":{"assistantOutput":"old"}} "#,
        );
        write_meta(
            temp.path(),
            "oracle-root-3",
            r#"{"id":"oracle-root-3","status":"completed","response":{"assistantOutput":"new"}} "#,
        );

        let recovered = read_recoverable_supervisor_response_from_dir(
            temp.path(),
            "oracle-root",
            Some("oracle-root-2"),
        )
        .await;

        let recovered = recovered.expect("expected recovered response");
        assert_eq!(recovered.session_id.as_deref(), Some("oracle-root-3"));
        assert_eq!(recovered.output.as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn recoverable_supervisor_response_skips_malformed_entries() {
        let temp = tempdir().expect("tempdir");
        write_meta(temp.path(), "oracle-root-2", "{not-json");
        write_meta(
            temp.path(),
            "oracle-root-3",
            r#"{"id":"oracle-root-3","status":"completed","response":{"assistantOutput":"new"}} "#,
        );

        let recovered =
            read_recoverable_supervisor_response_from_dir(temp.path(), "oracle-root", None).await;

        let recovered = recovered.expect("expected recovered response");
        assert_eq!(recovered.session_id.as_deref(), Some("oracle-root-3"));
        assert_eq!(recovered.output.as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn recoverable_supervisor_response_requires_matching_followup_conversation() {
        let temp = tempdir().expect("tempdir");
        write_meta(
            temp.path(),
            "oracle-root-2",
            r#"{"id":"oracle-root-2","status":"completed","browser":{"runtime":{"conversationId":"attached-1"}},"response":{"assistantOutput":"old"}} "#,
        );
        write_meta(
            temp.path(),
            "oracle-root-3",
            r#"{"id":"oracle-root-3","status":"completed","browser":{"runtime":{"conversationId":"wrong-9"}},"response":{"assistantOutput":"new"}} "#,
        );

        let recovered = read_recoverable_supervisor_response_from_dir(
            temp.path(),
            "oracle-root",
            Some("oracle-root-2"),
        )
        .await;

        assert!(recovered.is_none());
    }

    #[tokio::test]
    async fn recoverable_supervisor_response_recovers_unique_descendant_conversation_when_followup_metadata_is_missing()
     {
        let temp = tempdir().expect("tempdir");
        write_meta(
            temp.path(),
            "oracle-root-2",
            r#"{"id":"oracle-root-2","status":"completed","browser":{"runtime":{"tabUrl":"https://chatgpt.com/g/g-p-example/project"}},"response":{"assistantOutput":"old"}} "#,
        );
        write_meta(
            temp.path(),
            "oracle-root-3",
            r#"{"id":"oracle-root-3","status":"completed","browser":{"runtime":{"conversationId":"attached-1"}},"response":{"assistantOutput":"new"}} "#,
        );

        let recovered = read_recoverable_supervisor_response_from_dir(
            temp.path(),
            "oracle-root",
            Some("oracle-root-2"),
        )
        .await
        .expect("expected recovered response");

        assert_eq!(recovered.session_id.as_deref(), Some("oracle-root-3"));
        assert_eq!(recovered.conversation_id.as_deref(), Some("attached-1"));
        assert_eq!(recovered.output.as_deref(), Some("new"));
    }

    #[tokio::test]
    async fn recoverable_supervisor_response_rejects_ambiguous_descendant_conversations_when_followup_metadata_is_missing()
     {
        let temp = tempdir().expect("tempdir");
        write_meta(
            temp.path(),
            "oracle-root-2",
            r#"{"id":"oracle-root-2","status":"completed","browser":{"runtime":{"tabUrl":"https://chatgpt.com/g/g-p-example/project"}},"response":{"assistantOutput":"old"}} "#,
        );
        write_meta(
            temp.path(),
            "oracle-root-3",
            r#"{"id":"oracle-root-3","status":"completed","browser":{"runtime":{"conversationId":"attached-1"}},"response":{"assistantOutput":"new"}} "#,
        );
        write_meta(
            temp.path(),
            "oracle-root-4",
            r#"{"id":"oracle-root-4","status":"completed","browser":{"runtime":{"conversationId":"attached-2"}},"response":{"assistantOutput":"other"}} "#,
        );

        let recovered = read_recoverable_supervisor_response_from_dir(
            temp.path(),
            "oracle-root",
            Some("oracle-root-2"),
        )
        .await;

        assert!(recovered.is_none());
    }

    #[tokio::test]
    async fn recoverable_supervisor_response_prefers_latest_matching_followup_conversation() {
        let temp = tempdir().expect("tempdir");
        write_meta(
            temp.path(),
            "oracle-root-2",
            r#"{"id":"oracle-root-2","status":"completed","browser":{"runtime":{"conversationId":"attached-1"}},"response":{"assistantOutput":"old"}} "#,
        );
        write_meta(
            temp.path(),
            "oracle-root-3",
            r#"{"id":"oracle-root-3","status":"completed","browser":{"runtime":{"conversationId":"wrong-9"}},"response":{"assistantOutput":"wrong"}} "#,
        );
        write_meta(
            temp.path(),
            "oracle-root-4",
            r#"{"id":"oracle-root-4","status":"completed","browser":{"runtime":{"conversationId":"attached-1"}},"response":{"assistantOutput":"correct"}} "#,
        );

        let recovered = read_recoverable_supervisor_response_from_dir(
            temp.path(),
            "oracle-root",
            Some("oracle-root-2"),
        )
        .await
        .expect("expected recovered response");

        assert_eq!(recovered.session_id.as_deref(), Some("oracle-root-4"));
        assert_eq!(recovered.output.as_deref(), Some("correct"));
    }

    #[tokio::test]
    async fn prefer_recovered_supervisor_response_replaces_transport_reply_with_completed_output() {
        let recovered = prefer_recovered_supervisor_response_with_timeout(
            Ok(OracleBrokerResponse {
                session_id: Some("oracle-root-2".to_string()),
                conversation_id: None,
                title: None,
                output: Some("intermediate".to_string()),
            }),
            "oracle-root",
            async {
                OracleBrokerResponse {
                    session_id: Some("oracle-root-2".to_string()),
                    conversation_id: None,
                    title: None,
                    output: Some("final".to_string()),
                }
            },
            Duration::from_millis(10),
        )
        .await
        .expect("response");

        assert_eq!(recovered.output.as_deref(), Some("final"));
    }

    #[tokio::test]
    async fn prefer_recovered_supervisor_response_times_out_to_transport_reply() {
        let recovered = prefer_recovered_supervisor_response_with_timeout(
            Ok(OracleBrokerResponse {
                session_id: Some("oracle-root-2".to_string()),
                conversation_id: None,
                title: None,
                output: Some("transport".to_string()),
            }),
            "oracle-root",
            async {
                sleep(Duration::from_millis(20)).await;
                OracleBrokerResponse {
                    session_id: Some("oracle-root-2".to_string()),
                    conversation_id: None,
                    title: None,
                    output: Some("final".to_string()),
                }
            },
            Duration::from_millis(5),
        )
        .await
        .expect("response");

        assert_eq!(recovered.output.as_deref(), Some("transport"));
    }

    #[tokio::test]
    async fn prefer_recovered_supervisor_response_falls_back_from_transport_error() {
        let recovered = prefer_recovered_supervisor_response_with_timeout(
            Err("Oracle broker closed before replying".to_string()),
            "oracle-root",
            async {
                sleep(Duration::from_millis(2)).await;
                OracleBrokerResponse {
                    session_id: Some("oracle-root-2".to_string()),
                    conversation_id: None,
                    title: None,
                    output: Some("final".to_string()),
                }
            },
            Duration::from_millis(20),
        )
        .await
        .expect("response");

        assert_eq!(recovered.session_id.as_deref(), Some("oracle-root-2"));
        assert_eq!(recovered.output.as_deref(), Some("final"));
    }

    #[tokio::test]
    async fn prefer_recovered_supervisor_response_waits_for_delayed_followup_completion() {
        let recovered = prefer_recovered_supervisor_response_with_timeout(
            Err("Oracle broker exited without replying".to_string()),
            "oracle-root",
            async {
                sleep(Duration::from_millis(40)).await;
                OracleBrokerResponse {
                    session_id: Some("oracle-root-6".to_string()),
                    conversation_id: None,
                    title: None,
                    output: Some("late-final".to_string()),
                }
            },
            Duration::from_millis(80),
        )
        .await
        .expect("response");

        assert_eq!(recovered.session_id.as_deref(), Some("oracle-root-6"));
        assert_eq!(recovered.output.as_deref(), Some("late-final"));
    }

    #[tokio::test]
    async fn prefer_recovered_supervisor_response_keeps_newer_transport_session() {
        let recovered = prefer_recovered_supervisor_response_with_timeout(
            Ok(OracleBrokerResponse {
                session_id: Some("oracle-root-3".to_string()),
                conversation_id: None,
                title: None,
                output: Some("transport".to_string()),
            }),
            "oracle-root",
            async {
                OracleBrokerResponse {
                    session_id: Some("oracle-root-2".to_string()),
                    conversation_id: None,
                    title: None,
                    output: Some("older".to_string()),
                }
            },
            Duration::from_millis(10),
        )
        .await
        .expect("response");

        assert_eq!(recovered.session_id.as_deref(), Some("oracle-root-3"));
        assert_eq!(recovered.output.as_deref(), Some("transport"));
    }

    #[tokio::test]
    async fn prefer_recovered_supervisor_response_rejects_conversation_identity_drift() {
        let recovered = prefer_recovered_supervisor_response_with_timeout(
            Ok(OracleBrokerResponse {
                session_id: Some("oracle-root-2".to_string()),
                conversation_id: Some("attached-1".to_string()),
                title: None,
                output: Some("transport".to_string()),
            }),
            "oracle-root",
            async {
                OracleBrokerResponse {
                    session_id: Some("oracle-root-3".to_string()),
                    conversation_id: Some("wrong-9".to_string()),
                    title: None,
                    output: Some("drifted".to_string()),
                }
            },
            Duration::from_millis(10),
        )
        .await
        .expect("response");

        assert_eq!(recovered.session_id.as_deref(), Some("oracle-root-2"));
        assert_eq!(recovered.conversation_id.as_deref(), Some("attached-1"));
        assert_eq!(recovered.output.as_deref(), Some("transport"));
    }

    #[tokio::test]
    async fn prefer_recovered_supervisor_response_rejects_missing_conversation_identity() {
        let recovered = prefer_recovered_supervisor_response_with_timeout(
            Ok(OracleBrokerResponse {
                session_id: Some("oracle-root-2".to_string()),
                conversation_id: Some("attached-1".to_string()),
                title: None,
                output: Some("transport".to_string()),
            }),
            "oracle-root",
            async {
                OracleBrokerResponse {
                    session_id: Some("oracle-root-3".to_string()),
                    conversation_id: None,
                    title: None,
                    output: Some("missing".to_string()),
                }
            },
            Duration::from_millis(10),
        )
        .await
        .expect("response");

        assert_eq!(recovered.session_id.as_deref(), Some("oracle-root-2"));
        assert_eq!(recovered.conversation_id.as_deref(), Some("attached-1"));
        assert_eq!(recovered.output.as_deref(), Some("transport"));
    }

    #[tokio::test]
    async fn prefer_recovered_supervisor_response_accepts_recovered_conversation_identity() {
        let recovered = prefer_recovered_supervisor_response_with_timeout(
            Ok(OracleBrokerResponse {
                session_id: Some("oracle-root-2".to_string()),
                conversation_id: None,
                title: None,
                output: Some("transport".to_string()),
            }),
            "oracle-root",
            async {
                OracleBrokerResponse {
                    session_id: Some("oracle-root-3".to_string()),
                    conversation_id: Some("attached-1".to_string()),
                    title: None,
                    output: Some("recovered".to_string()),
                }
            },
            Duration::from_millis(10),
        )
        .await
        .expect("response");

        assert_eq!(recovered.session_id.as_deref(), Some("oracle-root-3"));
        assert_eq!(recovered.conversation_id.as_deref(), Some("attached-1"));
        assert_eq!(recovered.output.as_deref(), Some("recovered"));
    }

    #[tokio::test]
    async fn abort_marks_broker_client_unusable() {
        let broker = OracleBrokerClient::new_test_client();
        assert!(broker.is_usable());

        broker.abort();

        assert!(!broker.is_usable());
    }

    #[tokio::test]
    async fn request_times_out_when_supervisor_broker_never_replies() {
        let broker = OracleBrokerClient::new_hanging_test_client();

        let err = broker
            .request(super::OracleBrokerRequest {
                prompt: "hello".to_string(),
                session_slug: "oracle-root".to_string(),
                followup_session: None,
                files: Vec::new(),
                cwd: std::env::temp_dir(),
                model: "gpt-5.4".to_string(),
                browser_model_strategy: "select".to_string(),
                browser_model_label: Some("Thinking 5.4".to_string()),
                browser_thinking_time: None,
            })
            .await
            .expect_err("hanging broker should time out");

        assert!(err.contains("timed out"));
        assert!(!broker.is_usable());
    }

    #[test]
    fn broker_unexpected_exit_message_includes_exit_status_and_stderr_tail() {
        let message = format_oracle_broker_unexpected_exit_message_parts(
            Some("exit code 2".to_string()),
            &["fatal: boom".to_string(), "second line".to_string()],
        );

        assert!(message.contains("Oracle broker exited without replying"));
        assert!(message.contains("exit code 2"));
        assert!(message.contains("fatal: boom | second line"));
    }

    #[test]
    fn oracle_broker_command_prefers_built_js_broker() {
        let temp = tempdir().expect("tempdir");
        let dist_bin = temp.path().join("dist").join("bin");
        std::fs::create_dir_all(&dist_bin).expect("dist/bin");
        std::fs::write(
            dist_bin.join("oracle-supervisor-broker.js"),
            "console.log('broker');",
        )
        .expect("built broker");

        let (program, args) = oracle_broker_command(temp.path());

        assert_eq!(
            program.file_name().and_then(|name| name.to_str()),
            Some("node")
        );
        assert_eq!(args.len(), 1);
        assert!(args[0].ends_with("dist/bin/oracle-supervisor-broker.js"));
    }

    #[test]
    fn oracle_broker_command_falls_back_to_local_tsx_cli_when_no_build_exists() {
        let temp = tempdir().expect("tempdir");
        let tsx_dir = temp.path().join("node_modules").join("tsx").join("dist");
        std::fs::create_dir_all(&tsx_dir).expect("tsx dir");
        std::fs::write(tsx_dir.join("cli.mjs"), "console.log('tsx');").expect("tsx cli");

        let (program, args) = oracle_broker_command(temp.path());

        assert_eq!(
            program.file_name().and_then(|name| name.to_str()),
            Some("node")
        );
        assert_eq!(args.len(), 2);
        assert!(args[0].ends_with("node_modules/tsx/dist/cli.mjs"));
        assert_eq!(args[1], "bin/oracle-supervisor-broker.ts");
    }

    #[test]
    fn oracle_broker_command_falls_back_to_pnpm_exec_tsx_without_local_assets() {
        let temp = tempdir().expect("tempdir");

        let (program, args) = oracle_broker_command(temp.path());

        assert_eq!(
            program.file_name().and_then(|name| name.to_str()),
            Some("pnpm")
        );
        assert_eq!(args, vec!["exec", "tsx", "bin/oracle-supervisor-broker.ts"]);
    }

    #[tokio::test]
    async fn broker_declared_error_marks_client_unusable() {
        let broker = OracleBrokerClient::new_error_test_client("Oracle broker exploded");

        let err = broker
            .list_threads(None)
            .await
            .expect_err("broker error should bubble up");

        assert!(err.contains("Oracle broker exploded"));
        assert!(!broker.is_usable());
    }
}
