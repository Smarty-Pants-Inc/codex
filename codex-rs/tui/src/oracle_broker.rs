use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
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
    pub(crate) ok: bool,
    #[serde(rename = "sessionId")]
    pub(crate) session_id: Option<String>,
    pub(crate) output: Option<String>,
    pub(crate) error: Option<String>,
}

#[derive(Debug, Serialize)]
struct OracleBrokerWireRequest {
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

enum BrokerCommand {
    Request {
        request: OracleBrokerRequest,
        reply: oneshot::Sender<Result<OracleBrokerResponse, String>>,
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
        self.tx
            .send(BrokerCommand::Request { request, reply })
            .await
            .map_err(|_| "Oracle broker is unavailable".to_string())?;
        recv.await
            .map_err(|_| "Oracle broker closed before replying".to_string())?
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
            BrokerCommand::Request { request, reply } => {
                let result = send_and_read(&mut stdin, &mut stdout_lines, request).await;
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
    request: OracleBrokerRequest,
) -> Result<OracleBrokerResponse, String> {
    let payload = serde_json::to_string(&OracleBrokerWireRequest {
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
    let response: OracleBrokerResponse = serde_json::from_str(&line)
        .map_err(|error| format!("failed to parse Oracle broker response: {error}"))?;
    if response.ok {
        Ok(response)
    } else {
        let error = response
            .error
            .clone()
            .unwrap_or_else(|| "Oracle broker returned an unknown error".to_string());
        if let Some(session_id) = response.session_id.clone() {
            Err(format!("{error} (session: {session_id})"))
        } else {
            Err(error)
        }
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
