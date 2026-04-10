use std::future::Future;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
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
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
pub(crate) struct OracleBrokerResponse {
    #[serde(rename = "sessionId")]
    pub(crate) session_id: Option<String>,
    pub(crate) output: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OracleSessionMeta {
    id: String,
    status: Option<String>,
    response: Option<OracleSessionMetaResponse>,
}

#[derive(Debug, Deserialize)]
struct OracleSessionMetaResponse {
    #[serde(rename = "assistantOutput")]
    assistant_output: Option<String>,
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
}

#[derive(Debug, Deserialize)]
struct OracleBrokerThreadOpenEnvelope {
    #[serde(rename = "sessionId")]
    session_id: Option<String>,
    thread: OracleBrokerThreadOpenResponse,
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
    let mut child = Command::new("pnpm");
    child
        .arg("exec")
        .arg("tsx")
        .arg("bin/oracle-supervisor-broker.ts")
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

    let (tx, rx) = mpsc::channel(8);
    let stderr_task = tokio::spawn(async move {
        let mut lines = BufReader::new(stderr).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "codex_tui::oracle_broker", %line, "oracle broker stderr");
        }
    });
    let broker_task = tokio::spawn(run_broker_loop(
        child,
        stdin,
        BufReader::new(stdout).lines(),
        rx,
        stderr_task,
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
            let value = recv.await.map_err(|_| {
                self.mark_transport_unavailable();
                "Oracle broker closed before replying".to_string()
            })??;
            parse_success_response::<OracleBrokerResponse>(value)
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
        let value = recv.await.map_err(|_| {
            self.mark_transport_unavailable();
            "Oracle broker closed before replying".to_string()
        })??;
        parse_success_field_response::<Vec<OracleBrokerThreadEntry>>(value, "threads")
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
        let value = recv.await.map_err(|_| {
            self.mark_transport_unavailable();
            "Oracle broker closed before replying".to_string()
        })??;
        let mut response = parse_success_response::<OracleBrokerThreadOpenEnvelope>(value)?;
        response.thread.session_id = response.session_id.take();
        Ok(response.thread)
    }

    pub(crate) async fn attach_thread(
        &self,
        conversation_id: impl Into<String>,
        followup_session: Option<String>,
    ) -> Result<OracleBrokerThreadOpenResponse, String> {
        let (reply, recv) = oneshot::channel();
        let payload = serde_json::to_value(OracleBrokerThreadAttachWireRequest {
            action: "attach_thread",
            followup_session,
            conversation_id: conversation_id.into(),
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
        let value = recv.await.map_err(|_| {
            self.mark_transport_unavailable();
            "Oracle broker closed before replying".to_string()
        })??;
        let mut response = parse_success_response::<OracleBrokerThreadOpenEnvelope>(value)?;
        response.thread.session_id = response.session_id.take();
        Ok(response.thread)
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

async fn run_broker_loop(
    mut child: Child,
    mut stdin: ChildStdin,
    mut stdout_lines: tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    mut rx: mpsc::Receiver<BrokerCommand>,
    stderr_task: tokio::task::JoinHandle<()>,
) {
    while let Some(command) = rx.recv().await {
        match command {
            BrokerCommand::Request { payload, reply } => {
                let result = send_and_read(&mut stdin, &mut stdout_lines, payload).await;
                let _ = reply.send(result);
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
    stdin: &mut ChildStdin,
    stdout_lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
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
        .ok_or_else(|| "Oracle broker exited without replying".to_string())?;
    serde_json::from_str(&line)
        .map_err(|error| format!("failed to parse Oracle broker response: {error}"))
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

async fn read_recoverable_supervisor_response_from_dir(
    sessions_dir: &Path,
    session_slug: &str,
    followup_session: Option<&str>,
) -> Option<OracleBrokerResponse> {
    let minimum_ordinal = followup_session
        .and_then(|session_id| oracle_session_chain_ordinal(session_id, session_slug))
        .unwrap_or(0);
    let mut entries = fs::read_dir(sessions_dir).await.ok()?;
    let mut recovered: Option<(u32, OracleBrokerResponse)> = None;
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
        let response = OracleBrokerResponse {
            session_id: Some(meta.id),
            output: Some(output),
        };
        let replace = recovered
            .as_ref()
            .is_none_or(|(best_ordinal, _)| ordinal > *best_ordinal);
        if replace {
            recovered = Some((ordinal, response));
        }
    }
    recovered.map(|(_, response)| response)
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
    use super::oracle_session_chain_ordinal;
    use super::prefer_recovered_supervisor_response_with_timeout;
    use super::read_recoverable_supervisor_response_from_dir;
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
    async fn prefer_recovered_supervisor_response_replaces_transport_reply_with_completed_output() {
        let recovered = prefer_recovered_supervisor_response_with_timeout(
            Ok(OracleBrokerResponse {
                session_id: Some("oracle-root-2".to_string()),
                output: Some("intermediate".to_string()),
            }),
            "oracle-root",
            async {
                OracleBrokerResponse {
                    session_id: Some("oracle-root-2".to_string()),
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
                output: Some("transport".to_string()),
            }),
            "oracle-root",
            async {
                sleep(Duration::from_millis(20)).await;
                OracleBrokerResponse {
                    session_id: Some("oracle-root-2".to_string()),
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
                output: Some("transport".to_string()),
            }),
            "oracle-root",
            async {
                OracleBrokerResponse {
                    session_id: Some("oracle-root-2".to_string()),
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
    async fn abort_marks_broker_client_unusable() {
        let broker = OracleBrokerClient::new_test_client();
        assert!(broker.is_usable());

        broker.abort();

        assert!(!broker.is_usable());
    }
}
