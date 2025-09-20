use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use rand::Rng;
use rand::distr::Alphanumeric;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout};
use tracing::{info, warn};
use url::Url;

use crate::config::Config;
use crate::conversation_manager::RemoteConversationOptions;

const LOOPBACK_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_TOKEN_LEN: usize = 24;

pub struct LoopbackRemote {
    options: RemoteConversationOptions,
    child: Option<Child>,
    stdout_task: Option<JoinHandle<()>>,
    stderr_task: Option<JoinHandle<()>>,
}

impl LoopbackRemote {
    pub async fn start(config: &Config) -> Result<Self> {
        let disable_env = std::env::var("SMARTY_TUI_REMOTE_DISABLE").unwrap_or_default();
        if env_var_truthy(&disable_env) {
            return Err(anyhow!(
                "loopback remote disabled via SMARTY_TUI_REMOTE_DISABLE"
            ));
        }

        let (command, args) = resolve_loopback_command(config)?;
        let token = std::env::var("SMARTY_TUI_LOOPBACK_TOKEN").unwrap_or_else(|_| {
            let mut rng = rand::rng();
            (&mut rng)
                .sample_iter(&Alphanumeric)
                .take(DEFAULT_TOKEN_LEN)
                .map(char::from)
                .collect()
        });

        let mut child = Command::new(&command)
            .args(&args)
            .arg("--ws-bind")
            .arg("127.0.0.1:0")
            .arg("--http-bind")
            .arg("127.0.0.1:0")
            .arg("--token")
            .arg(&token)
            .arg("--root")
            .arg(config.cwd.to_string_lossy().to_string())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("failed to spawn loopback server using {command}"))?;

        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| anyhow!("loopback server stdout not captured"))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| anyhow!("loopback server stderr not captured"))?;

        let mut stdout_reader = BufReader::new(stdout).lines();
        let mut ws_url = None;
        let mut http_url = None;
        let deadline = Instant::now() + LOOPBACK_TIMEOUT;

        while Instant::now() < deadline {
            match timeout(
                deadline.saturating_duration_since(Instant::now()),
                stdout_reader.next_line(),
            )
            .await
            {
                Ok(Ok(Some(line))) => {
                    let trimmed = line.trim();
                    if let Some(rest) = trimmed.strip_prefix("listening ws://") {
                        ws_url = Some(format!("ws://{}", rest.trim()));
                    } else if let Some(rest) = trimmed.strip_prefix("listening http://") {
                        http_url = Some(format!("http://{}", rest.trim()));
                    } else {
                        info!(target = "remote", "loopback: {trimmed}");
                    }

                    if ws_url.is_some() && http_url.is_some() {
                        break;
                    }
                }
                Ok(Ok(None)) => {
                    return Err(anyhow!(
                        "loopback server exited before reporting bind addresses"
                    ));
                }
                Ok(Err(err)) => {
                    return Err(anyhow!("error reading loopback server stdout: {err}"));
                }
                Err(_) => {
                    return Err(anyhow!(
                        "timed out waiting for loopback server to report bind addresses"
                    ));
                }
            }
        }

        let ws_url =
            ws_url.ok_or_else(|| anyhow!("loopback server did not report ws bind address"))?;
        let http_url =
            http_url.ok_or_else(|| anyhow!("loopback server did not report http bind address"))?;

        let remote_url = Url::parse(&ws_url)
            .with_context(|| format!("invalid ws url reported by loopback server: {ws_url}"))?;
        let sse_base_url = Url::parse(&http_url)
            .with_context(|| format!("invalid http url reported by loopback server: {http_url}"))?;

        let stdout_task = tokio::spawn(async move {
            while let Ok(Some(line)) = stdout_reader.next_line().await {
                info!(target = "remote", "loopback: {line}");
            }
        });

        let mut stderr_reader = BufReader::new(stderr).lines();
        let stderr_task = tokio::spawn(async move {
            while let Ok(Some(line)) = stderr_reader.next_line().await {
                warn!(target = "remote", "loopback stderr: {line}");
            }
        });

        let options = RemoteConversationOptions {
            remote_url,
            sse_base_url,
            token: Some(token),
            timeout: Duration::from_secs(45),
            trust_cert: None,
        };

        Ok(Self {
            options,
            child: Some(child),
            stdout_task: Some(stdout_task),
            stderr_task: Some(stderr_task),
        })
    }

    pub fn options(&self) -> &RemoteConversationOptions {
        &self.options
    }
}

impl Drop for LoopbackRemote {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        if let Some(handle) = self.stdout_task.take() {
            handle.abort();
        }
        if let Some(handle) = self.stderr_task.take() {
            handle.abort();
        }
    }
}

fn env_var_truthy(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn resolve_loopback_command(config: &Config) -> Result<(String, Vec<String>)> {
    if let Ok(cmdline) = std::env::var("SMARTY_TUI_LOOPBACK_SERVER_CMD") {
        let mut parts = shlex::split(&cmdline)
            .ok_or_else(|| anyhow!("failed to parse SMARTY_TUI_LOOPBACK_SERVER_CMD: {cmdline}"))?;
        if parts.is_empty() {
            return Err(anyhow!("SMARTY_TUI_LOOPBACK_SERVER_CMD is empty"));
        }
        let program = parts.remove(0);
        return Ok((program, parts));
    }

    let script = locate_repo_script(&config.cwd, "scripts/smarty-tui-server").ok_or_else(|| {
        anyhow!(
            "unable to locate scripts/smarty-tui-server from cwd {}",
            config.cwd.display()
        )
    })?;
    Ok((script.to_string_lossy().to_string(), Vec::new()))
}

fn locate_repo_script(start: &Path, relative: &str) -> Option<PathBuf> {
    let mut dir = start;
    loop {
        let candidate = dir.join(relative);
        if candidate.exists() {
            return Some(candidate);
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => break,
        }
    }
    None
}
