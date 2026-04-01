use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;
use tokio::process::Child;
use tokio::process::ChildStdin;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use tokio::time::timeout;

#[derive(Debug, Clone)]
pub(crate) struct OracleBrokerClient {
    tx: mpsc::Sender<BrokerCommand>,
    abort_handle: tokio::task::AbortHandle,
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

#[derive(Debug, Deserialize)]
pub(crate) struct OracleBrokerResponse {
    #[serde(rename = "sessionId")]
    pub(crate) session_id: Option<String>,
    pub(crate) output: Option<String>,
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
    let mut child = child
        .spawn()
        .map_err(|error| format!("failed to start Oracle broker: {error}"))?;
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
    })
}

impl OracleBrokerClient {
    pub(crate) async fn request(
        &self,
        request: OracleBrokerRequest,
    ) -> Result<OracleBrokerResponse, String> {
        let (reply, recv) = oneshot::channel();
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
        self.tx
            .send(BrokerCommand::Request { payload, reply })
            .await
            .map_err(|_| "Oracle broker is unavailable".to_string())?;
        let value = recv
            .await
            .map_err(|_| "Oracle broker closed before replying".to_string())??;
        parse_success_response::<OracleBrokerResponse>(value)
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
        self.tx
            .send(BrokerCommand::Request { payload, reply })
            .await
            .map_err(|_| "Oracle broker is unavailable".to_string())?;
        let value = recv
            .await
            .map_err(|_| "Oracle broker closed before replying".to_string())??;
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
        self.tx
            .send(BrokerCommand::Request { payload, reply })
            .await
            .map_err(|_| "Oracle broker is unavailable".to_string())?;
        let value = recv
            .await
            .map_err(|_| "Oracle broker closed before replying".to_string())??;
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
        self.tx
            .send(BrokerCommand::Request { payload, reply })
            .await
            .map_err(|_| "Oracle broker is unavailable".to_string())?;
        let value = recv
            .await
            .map_err(|_| "Oracle broker closed before replying".to_string())??;
        let mut response = parse_success_response::<OracleBrokerThreadOpenEnvelope>(value)?;
        response.thread.session_id = response.session_id.take();
        Ok(response.thread)
    }

    pub(crate) async fn shutdown(&self) -> Result<(), String> {
        let (reply, recv) = oneshot::channel();
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
        self.abort_handle.abort();
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
