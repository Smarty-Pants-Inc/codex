use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;

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

#[derive(Debug, Clone)]
pub(crate) struct OracleBrokerClient {
    tx: mpsc::Sender<BrokerCommand>,
}

#[derive(Debug, Clone)]
pub(crate) struct OracleBrokerRequest {
    pub(crate) prompt: String,
    pub(crate) session_slug: String,
    pub(crate) followup_session: Option<String>,
    pub(crate) files: Vec<String>,
    pub(crate) cwd: PathBuf,
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
}

struct BrokerCommand {
    request: OracleBrokerRequest,
    reply: oneshot::Sender<Result<OracleBrokerResponse, String>>,
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
    tokio::spawn(run_broker_loop(
        child,
        stdin,
        BufReader::new(stdout).lines(),
        rx,
        stderr_task,
    ));
    Ok(OracleBrokerClient { tx })
}

impl OracleBrokerClient {
    pub(crate) async fn request(
        &self,
        request: OracleBrokerRequest,
    ) -> Result<OracleBrokerResponse, String> {
        let (reply, recv) = oneshot::channel();
        self.tx
            .send(BrokerCommand { request, reply })
            .await
            .map_err(|_| "Oracle broker is unavailable".to_string())?;
        recv.await
            .map_err(|_| "Oracle broker closed before replying".to_string())?
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
        let result = send_and_read(&mut stdin, &mut stdout_lines, command.request).await;
        let _ = command.reply.send(result);
    }
    let _ = stdin.shutdown().await;
    let _ = child.kill().await;
    let _ = child.wait().await;
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
        Err(response
            .error
            .clone()
            .unwrap_or_else(|| "Oracle broker returned an unknown error".to_string()))
    }
}
