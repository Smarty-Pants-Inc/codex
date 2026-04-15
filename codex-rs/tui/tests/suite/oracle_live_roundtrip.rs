use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::Context;
use anyhow::Result;
use codex_ansi_escape::ansi_escape_line;
use serde_json::Value as JsonValue;
use tokio::select;
use tokio::sync::mpsc::Sender;
use tokio::time::sleep;
use tokio::time::timeout;

const ORACLE_SUPERVISOR_THROTTLE_FILE_ENV: &str = "ORACLE_SUPERVISOR_THROTTLE_FILE";
const ORACLE_SUPERVISOR_THROTTLE_WINDOW_SECS: i64 = 30 * 60;
const ORACLE_SUPERVISOR_THROTTLE_MIN_GAP_SECS: i64 = 30;
const ORACLE_SUPERVISOR_THROTTLE_MAX_REQUESTS: usize = 6;
const ORACLE_LIVE_QUEUE_BUFFER_SECS: u64 = 120;

fn normalize_url(value: &str) -> String {
    value.trim_end_matches('/').to_string()
}

fn parse_conversation_id(thread_url: &str) -> Option<String> {
    thread_url
        .trim_end_matches('/')
        .rsplit("/c/")
        .next()
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn find_smarty_root(codex_repo_root: &Path) -> Result<PathBuf> {
    codex_repo_root
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .context("failed to resolve smarty-code root from codex repo root")
}

fn oracle_session_matches(
    meta_path: &Path,
    thread_url: &str,
    conversation_id: &str,
    token: &str,
) -> bool {
    let Ok(raw) = fs::read_to_string(meta_path) else {
        return false;
    };
    let Ok(meta) = serde_json::from_str::<JsonValue>(&raw) else {
        return false;
    };
    let runtime = meta.get("browser").and_then(|value| value.get("runtime"));
    let response = meta
        .get("response")
        .and_then(|value| value.get("assistantOutput"))
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    runtime
        .and_then(|value| value.get("conversationId"))
        .and_then(JsonValue::as_str)
        == Some(conversation_id)
        && runtime
            .and_then(|value| value.get("tabUrl"))
            .and_then(JsonValue::as_str)
            .is_some_and(|value| normalize_url(value) == normalize_url(thread_url))
        && response.contains(token)
}

fn oracle_session_targets_thread(
    meta_path: &Path,
    thread_url: &str,
    conversation_id: &str,
) -> bool {
    let Ok(raw) = fs::read_to_string(meta_path) else {
        return false;
    };
    let Ok(meta) = serde_json::from_str::<JsonValue>(&raw) else {
        return false;
    };
    let runtime = meta.get("browser").and_then(|value| value.get("runtime"));
    runtime
        .and_then(|value| value.get("conversationId"))
        .and_then(JsonValue::as_str)
        == Some(conversation_id)
        && runtime
            .and_then(|value| value.get("tabUrl"))
            .and_then(JsonValue::as_str)
            .is_some_and(|value| normalize_url(value) == normalize_url(thread_url))
}

fn oracle_session_matches_with_file(
    meta_path: &Path,
    thread_url: &str,
    conversation_id: &str,
    token: &str,
    expected_file: &Path,
) -> bool {
    let Ok(raw) = fs::read_to_string(meta_path) else {
        return false;
    };
    let Ok(meta) = serde_json::from_str::<JsonValue>(&raw) else {
        return false;
    };
    let runtime = meta.get("browser").and_then(|value| value.get("runtime"));
    let response = meta
        .get("response")
        .and_then(|value| value.get("assistantOutput"))
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let files = meta
        .get("options")
        .and_then(|value| value.get("file"))
        .and_then(JsonValue::as_array);
    runtime
        .and_then(|value| value.get("conversationId"))
        .and_then(JsonValue::as_str)
        == Some(conversation_id)
        && runtime
            .and_then(|value| value.get("tabUrl"))
            .and_then(JsonValue::as_str)
            .is_some_and(|value| normalize_url(value) == normalize_url(thread_url))
        && response.contains(token)
        && files.is_some_and(|entries| {
            entries.iter().any(|entry| {
                entry
                    .as_str()
                    .is_some_and(|value| Path::new(value) == expected_file)
            })
        })
}

fn tail_lines(path: &Path, count: usize) -> String {
    fs::read_to_string(path)
        .map(|text| {
            let mut lines = text.lines().rev().take(count).collect::<Vec<_>>();
            lines.reverse();
            lines.join("\n")
        })
        .unwrap_or_else(|err| format!("<unavailable: {err}>"))
}

fn sanitized_output_text(output: &[u8]) -> String {
    String::from_utf8_lossy(output)
        .lines()
        .map(|line| ansi_escape_line(line).to_string())
        .collect::<Vec<_>>()
        .join("\n")
}

fn failure_artifacts(codex_home_path: &Path, session_log_path: &Path, output: &[u8]) -> String {
    let output_text = String::from_utf8_lossy(output);
    let sanitized_output = sanitized_output_text(output);
    let codex_log_path = codex_home_path.join("log").join("codex-tui.log");
    format!(
        "CODEX_HOME={}\nSESSION_LOG={}\nCODEX_LOG={}\nORACLE_THROTTLE={}\n\nSession log tail:\n{}\n\nCodex log tail:\n{}\n\nPTY output (sanitized):\n{}\n\nPTY output (raw):\n{}",
        codex_home_path.display(),
        session_log_path.display(),
        codex_log_path.display(),
        oracle_supervisor_throttle_snapshot(),
        tail_lines(session_log_path, 80),
        tail_lines(&codex_log_path, 80),
        sanitized_output,
        output_text,
    )
}

fn oracle_supervisor_throttle_path() -> Option<PathBuf> {
    if let Some(path) = env::var_os(ORACLE_SUPERVISOR_THROTTLE_FILE_ENV) {
        return Some(PathBuf::from(path));
    }
    env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join(".oracle")
            .join("supervisor-browser-throttle.json")
    })
}

fn oracle_supervisor_hidden_profile_key() -> Option<String> {
    env::var_os("HOME").map(PathBuf::from).map(|home| {
        home.join(".oracle")
            .join("browser-profile-hidden")
            .display()
            .to_string()
    })
}

fn oracle_supervisor_recent_request_times() -> Vec<chrono::DateTime<chrono::Utc>> {
    let Some(path) = oracle_supervisor_throttle_path() else {
        return Vec::new();
    };
    let Some(profile_key) = oracle_supervisor_hidden_profile_key() else {
        return Vec::new();
    };
    let Ok(raw) = fs::read_to_string(path) else {
        return Vec::new();
    };
    let Ok(state) = serde_json::from_str::<JsonValue>(&raw) else {
        return Vec::new();
    };
    let Some(entries) = state
        .get("profiles")
        .and_then(|value| value.get(profile_key.as_str()))
        .and_then(|value| value.get("requestStartedAt"))
        .and_then(JsonValue::as_array)
    else {
        return Vec::new();
    };
    let mut timestamps = entries
        .iter()
        .filter_map(JsonValue::as_str)
        .filter_map(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&chrono::Utc))
        .collect::<Vec<_>>();
    timestamps.sort_unstable();
    timestamps
}

fn oracle_supervisor_queue_delay(requests_needed: usize) -> Duration {
    if requests_needed == 0 {
        return Duration::ZERO;
    }
    let window = chrono::Duration::seconds(ORACLE_SUPERVISOR_THROTTLE_WINDOW_SECS);
    let min_gap = chrono::Duration::seconds(ORACLE_SUPERVISOR_THROTTLE_MIN_GAP_SECS);
    let mut now = chrono::Utc::now();
    let mut request_times = oracle_supervisor_recent_request_times();
    request_times.retain(|timestamp| *timestamp + window > now);
    request_times.sort_unstable();

    for _ in 0..requests_needed {
        request_times.retain(|timestamp| *timestamp + window > now);
        let min_gap_ready = request_times
            .last()
            .map(|last| *last + min_gap)
            .unwrap_or(now);
        let window_ready = if request_times.len() >= ORACLE_SUPERVISOR_THROTTLE_MAX_REQUESTS {
            request_times[request_times.len() - ORACLE_SUPERVISOR_THROTTLE_MAX_REQUESTS] + window
        } else {
            now
        };
        now = now.max(min_gap_ready).max(window_ready);
        request_times.push(now);
        request_times.sort_unstable();
    }

    now.signed_duration_since(chrono::Utc::now())
        .to_std()
        .unwrap_or(Duration::ZERO)
}

fn oracle_live_timeout(base: Duration, requests_needed: usize) -> Duration {
    base + oracle_supervisor_queue_delay(requests_needed)
        + Duration::from_secs(ORACLE_LIVE_QUEUE_BUFFER_SECS)
}

fn oracle_supervisor_throttle_snapshot() -> String {
    let Some(path) = oracle_supervisor_throttle_path() else {
        return "<unavailable: throttle path unresolved>".to_string();
    };
    match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) => format!("<unavailable: {err}>"),
    }
}

async fn type_humanlike_with_delay(writer_tx: &Sender<Vec<u8>>, text: &str, delay: Duration) {
    for byte in text.bytes() {
        let _ = writer_tx.send(vec![byte]).await;
        sleep(delay).await;
    }
}

async fn submit_humanlike(writer_tx: &Sender<Vec<u8>>, text: &str) {
    submit_humanlike_with_delays(
        writer_tx,
        text,
        Duration::from_millis(15),
        Duration::from_millis(120),
    )
    .await;
}

async fn submit_humanlike_with_delays(
    writer_tx: &Sender<Vec<u8>>,
    text: &str,
    char_delay: Duration,
    enter_delay: Duration,
) {
    type_humanlike_with_delay(writer_tx, text, char_delay).await;
    sleep(enter_delay).await;
    let _ = writer_tx.send(b"\r".to_vec()).await;
}

#[tokio::test]
#[ignore = "live native Oracle PTY proof; requires ORACLE_NATIVE_THREAD_URL"]
async fn native_oracle_attach_and_pro_extended_roundtrip() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let thread_url = match std::env::var("ORACLE_NATIVE_THREAD_URL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping live oracle roundtrip: ORACLE_NATIVE_THREAD_URL is unset");
            return Ok(());
        }
    };
    let conversation_id = std::env::var("ORACLE_NATIVE_CONVERSATION_ID")
        .ok()
        .or_else(|| parse_conversation_id(&thread_url))
        .context("missing conversation id for ORACLE_NATIVE_THREAD_URL")?;
    let project_url = std::env::var("ORACLE_SUPERVISOR_CHATGPT_URL")
        .context("ORACLE_SUPERVISOR_CHATGPT_URL must point at the hidden Oracle ChatGPT project")?;
    let codex_repo_root = codex_utils_cargo_bin::repo_root()?;
    let smarty_root = find_smarty_root(&codex_repo_root)?;
    let launcher = smarty_root.join("bin").join("smarty-codex");
    let workspace = smarty_root.clone();
    let sessions_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".oracle").join("sessions"))
        .context("HOME is unset; cannot inspect Oracle sessions")?;
    let token = format!(
        "CODEX_NATIVE_PROOF_{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis()
    );
    let codex_home = tempfile::tempdir()?;
    let codex_home_path = codex_home.path().to_path_buf();
    let session_log_path = codex_home_path.join("native-oracle-roundtrip.jsonl");
    fs::write(
        codex_home_path.join("config.toml"),
        format!(
            "model_provider = \"ollama\"\ncheck_for_update_on_startup = false\n\n[tui]\nshow_tooltips = false\n\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            workspace.display()
        ),
    )?;

    let args = vec![
        "--no-alt-screen".to_string(),
        "-C".to_string(),
        workspace.display().to_string(),
        "-c".to_string(),
        "analytics.enabled=false".to_string(),
    ];
    let mut env = HashMap::new();
    env.insert(
        "CODEX_HOME".to_string(),
        codex_home_path.display().to_string(),
    );
    env.insert("SMARTY_NO_UPSTREAM_CHECK".to_string(), "1".to_string());
    env.insert(
        "SMARTY_CODEX_NO_DANGEROUS_DEFAULT".to_string(),
        "1".to_string(),
    );
    for key in ["PATH", "HOME", "SHELL", "TMPDIR"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    env.insert("ORACLE_ALLOW_VISIBLE_CHROME".to_string(), "0".to_string());
    env.insert(
        "ORACLE_CHATGPT_PROJECT_URL".to_string(),
        project_url.clone(),
    );
    env.insert("ORACLE_SUPERVISOR_CHATGPT_URL".to_string(), project_url);
    env.insert("CODEX_TUI_RECORD_SESSION".to_string(), "1".to_string());
    env.insert(
        "CODEX_TUI_SESSION_LOG_PATH".to_string(),
        session_log_path.display().to_string(),
    );

    let spawned = codex_utils_pty::spawn_pty_process(
        launcher.to_string_lossy().as_ref(),
        &args,
        &workspace,
        &env,
        &None,
        codex_utils_pty::TerminalSize::default(),
    )
    .await?;
    let codex_utils_pty::SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    } = spawned;
    let mut output = Vec::new();
    let mut output_rx = codex_utils_pty::combine_output_receivers(stdout_rx, stderr_rx);
    let mut exit_rx = exit_rx;
    let writer_tx = session.writer_sender();
    let interrupt_writer = writer_tx.clone();
    let mut answered_cursor_query = false;
    let test_started_at = SystemTime::now();
    let mut saw_completion_log = false;
    let mut recorded_failure: Option<String> = None;
    let mut last_nux_seen_at: Option<tokio::time::Instant> = None;
    let mut last_nux_ack_at: Option<tokio::time::Instant> = None;
    let mut startup_seen_at: Option<tokio::time::Instant> = None;
    let mut sent_attach = false;
    let mut sent_prompt = false;
    let mut attach_ready_at: Option<tokio::time::Instant> = None;
    let mut extended_ready_at: Option<tokio::time::Instant> = None;
    let mut oracle_model_ack_at: Option<tokio::time::Instant> = None;
    let mut last_model_attempt_at: Option<tokio::time::Instant> = None;
    let mut saw_extended_model = false;

    let exit_code = match timeout(oracle_live_timeout(Duration::from_secs(420), 2), async {
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        if chunk.windows(4).any(|window| window == b"\x1b[6n") {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                            answered_cursor_query = true;
                        }
                        let chunk_text = String::from_utf8_lossy(&chunk);
                        if chunk_text.contains("Use ↑/↓ to move, press enter to confirm") {
                            last_nux_seen_at = Some(tokio::time::Instant::now());
                            if last_nux_ack_at
                                .map_or(true, |instant| instant.elapsed() >= Duration::from_millis(500))
                            {
                                last_nux_ack_at = Some(tokio::time::Instant::now());
                                let _ = writer_tx.send(b"\r".to_vec()).await;
                            }
                        }
                        output.extend_from_slice(&chunk);
                        let sanitized_output = sanitized_output_text(&output);
                        let now = tokio::time::Instant::now();

                        if attach_ready_at.is_none()
                            && sanitized_output.contains("Attached Oracle thread on ")
                        {
                            attach_ready_at = Some(now);
                        }
                        if sanitized_output.contains("requested gpt-5.4-pro (Extended)") {
                            saw_extended_model = true;
                            if extended_ready_at.is_none() {
                                extended_ready_at = Some(now);
                            }
                        }
                        if oracle_model_ack_at.is_none()
                            && sanitized_output.contains(
                                "Oracle model preference set to gpt-5.4-pro (Extended).",
                            )
                        {
                            oracle_model_ack_at = Some(now);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break exit_rx.await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                },
                _ = sleep(Duration::from_millis(250)) => {
                    if session_log_path.is_file()
                        && let Ok(log) = fs::read_to_string(&session_log_path)
                    {
                        let now = tokio::time::Instant::now();
                        if startup_seen_at.is_none()
                            && (log.contains("\"type\":\"list_skills\"")
                                || log.contains("\"variant\":\"PluginMentionsLoaded"))
                        {
                            startup_seen_at = Some(now);
                        }
                        if log.contains("\"kind\":\"oracle_run_failed\"") {
                            recorded_failure = Some(format!(
                                "oracle_run_failed recorded in {session_log_path:?}"
                            ));
                            for _ in 0..3 {
                                let _ = interrupt_writer.send(vec![3]).await;
                                sleep(Duration::from_millis(200)).await;
                            }
                        }
                        if log.contains("\"kind\":\"oracle_run_completed\"") && !saw_completion_log {
                            saw_completion_log = true;
                            for _ in 0..3 {
                                let _ = interrupt_writer.send(vec![3]).await;
                                sleep(Duration::from_millis(200)).await;
                            }
                        }
                    }

                    let now = tokio::time::Instant::now();
                    let startup_nux_settled = last_nux_seen_at
                        .map_or(true, |instant| instant.elapsed() >= Duration::from_secs(1));

                    if !sent_attach
                        && startup_nux_settled
                        && startup_seen_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        sent_attach = true;
                        submit_humanlike(
                            &writer_tx,
                            format!("/oracle attach {thread_url}").as_str(),
                        )
                        .await;
                        continue;
                    }

                    if attach_ready_at
                        .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                        && !saw_extended_model
                        && last_model_attempt_at
                            .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(8))
                    {
                        last_model_attempt_at = Some(now);
                        submit_humanlike(&writer_tx, "/oracle model pro extended").await;
                        continue;
                    }

                    if !sent_prompt
                        && oracle_model_ack_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        sent_prompt = true;
                        submit_humanlike_with_delays(
                            &writer_tx,
                            format!("Reply with exactly {token} and nothing else.").as_str(),
                            Duration::from_millis(35),
                            Duration::from_millis(350),
                        )
                        .await;
                    }
                }
                result = &mut exit_rx => break result,
            }
        }
    })
    .await {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => {
            session.terminate();
            while let Ok(chunk) = output_rx.try_recv() {
                output.extend_from_slice(&chunk);
            }
            anyhow::bail!(
                "timed out waiting for native oracle roundtrip\n{}",
                failure_artifacts(&codex_home_path, &session_log_path, &output)
            );
        }
    };

    if let Some(recorded_failure) = recorded_failure {
        anyhow::bail!(
            "{recorded_failure}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if !saw_completion_log {
        anyhow::bail!(
            "expected oracle_run_completed in {session_log_path:?}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if !(exit_code == 0 || exit_code == 130 || exit_code == 1) {
        anyhow::bail!(
            "unexpected exit code {exit_code}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if !saw_extended_model {
        anyhow::bail!(
            "expected PTY proof that Pro Extended was selected\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    let matched = fs::read_dir(&sessions_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("meta.json"))
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let modified = path.metadata().ok()?.modified().ok()?;
            (modified >= test_started_at).then_some(path)
        })
        .any(|path| oracle_session_matches(&path, &thread_url, &conversation_id, &token));
    if !matched {
        anyhow::bail!(
            "expected a matching Oracle session for {conversation_id} containing {token}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    Ok(())
}

#[tokio::test]
#[ignore = "live native Oracle PTY proof; requires ORACLE_NATIVE_THREAD_URL"]
async fn native_oracle_attach_fail_closed_for_missing_project_thread() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let known_thread_url = match std::env::var("ORACLE_NATIVE_THREAD_URL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping live oracle fail-closed proof: ORACLE_NATIVE_THREAD_URL is unset");
            return Ok(());
        }
    };
    let bogus_conversation_id = format!(
        "missing-proof-{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis()
    );
    let bogus_thread_url = known_thread_url
        .rsplit_once("/c/")
        .map(|(prefix, _)| format!("{prefix}/c/{bogus_conversation_id}"))
        .context("ORACLE_NATIVE_THREAD_URL must be a concrete ChatGPT thread url")?;
    let project_url = std::env::var("ORACLE_SUPERVISOR_CHATGPT_URL")
        .context("ORACLE_SUPERVISOR_CHATGPT_URL must point at the hidden Oracle ChatGPT project")?;
    let codex_repo_root = codex_utils_cargo_bin::repo_root()?;
    let smarty_root = find_smarty_root(&codex_repo_root)?;
    let launcher = smarty_root.join("bin").join("smarty-codex");
    let workspace = smarty_root.clone();
    let sessions_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".oracle").join("sessions"))
        .context("HOME is unset; cannot inspect Oracle sessions")?;
    let test_started_at = SystemTime::now();
    let codex_home = tempfile::tempdir()?;
    let codex_home_path = codex_home.path().to_path_buf();
    let session_log_path = codex_home_path.join("native-oracle-fail-closed.jsonl");
    fs::write(
        codex_home_path.join("config.toml"),
        format!(
            "model_provider = \"ollama\"\ncheck_for_update_on_startup = false\n\n[tui]\nshow_tooltips = false\n\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            workspace.display()
        ),
    )?;

    let args = vec![
        "--no-alt-screen".to_string(),
        "-C".to_string(),
        workspace.display().to_string(),
        "-c".to_string(),
        "analytics.enabled=false".to_string(),
    ];
    let mut env = HashMap::new();
    env.insert(
        "CODEX_HOME".to_string(),
        codex_home_path.display().to_string(),
    );
    env.insert("SMARTY_NO_UPSTREAM_CHECK".to_string(), "1".to_string());
    env.insert(
        "SMARTY_CODEX_NO_DANGEROUS_DEFAULT".to_string(),
        "1".to_string(),
    );
    for key in ["PATH", "HOME", "SHELL", "TMPDIR"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    env.insert("ORACLE_ALLOW_VISIBLE_CHROME".to_string(), "0".to_string());
    env.insert(
        "ORACLE_CHATGPT_PROJECT_URL".to_string(),
        project_url.clone(),
    );
    env.insert("ORACLE_SUPERVISOR_CHATGPT_URL".to_string(), project_url);
    env.insert("CODEX_TUI_RECORD_SESSION".to_string(), "1".to_string());
    env.insert(
        "CODEX_TUI_SESSION_LOG_PATH".to_string(),
        session_log_path.display().to_string(),
    );

    let spawned = codex_utils_pty::spawn_pty_process(
        launcher.to_string_lossy().as_ref(),
        &args,
        &workspace,
        &env,
        &None,
        codex_utils_pty::TerminalSize::default(),
    )
    .await?;
    let codex_utils_pty::SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    } = spawned;
    let mut output = Vec::new();
    let mut output_rx = codex_utils_pty::combine_output_receivers(stdout_rx, stderr_rx);
    let mut exit_rx = exit_rx;
    let writer_tx = session.writer_sender();
    let interrupt_writer = writer_tx.clone();
    let mut answered_cursor_query = false;
    let mut last_nux_seen_at: Option<tokio::time::Instant> = None;
    let mut last_nux_ack_at: Option<tokio::time::Instant> = None;
    let mut startup_seen_at: Option<tokio::time::Instant> = None;
    let mut sent_attach = false;
    let mut saw_attach_success = false;
    let mut saw_attach_failure = false;

    let exit_code = match timeout(oracle_live_timeout(Duration::from_secs(180), 1), async {
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        if chunk.windows(4).any(|window| window == b"\x1b[6n") {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                            answered_cursor_query = true;
                        }
                        let chunk_text = String::from_utf8_lossy(&chunk);
                        if chunk_text.contains("Use ↑/↓ to move, press enter to confirm") {
                            last_nux_seen_at = Some(tokio::time::Instant::now());
                            if last_nux_ack_at
                                .map_or(true, |instant| instant.elapsed() >= Duration::from_millis(500))
                            {
                                last_nux_ack_at = Some(tokio::time::Instant::now());
                                let _ = writer_tx.send(b"\r".to_vec()).await;
                            }
                        }
                        output.extend_from_slice(&chunk);
                        let sanitized_output = sanitized_output_text(&output);
                        if sanitized_output.contains("Attached Oracle thread on ") {
                            saw_attach_success = true;
                        }
                        if sanitized_output.contains(
                            format!("Failed to attach Oracle thread {bogus_conversation_id}:").as_str(),
                        ) {
                            saw_attach_failure = true;
                            for _ in 0..3 {
                                let _ = interrupt_writer.send(vec![3]).await;
                                sleep(Duration::from_millis(200)).await;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break exit_rx.await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                },
                _ = sleep(Duration::from_millis(250)) => {
                    if session_log_path.is_file()
                        && let Ok(log) = fs::read_to_string(&session_log_path)
                    {
                        if startup_seen_at.is_none()
                            && (log.contains("\"type\":\"list_skills\"")
                                || log.contains("\"variant\":\"PluginMentionsLoaded"))
                        {
                            startup_seen_at = Some(tokio::time::Instant::now());
                        }
                    }

                    let startup_nux_settled = last_nux_seen_at
                        .map_or(true, |instant| instant.elapsed() >= Duration::from_secs(1));

                    if !sent_attach
                        && answered_cursor_query
                        && startup_nux_settled
                        && startup_seen_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        sent_attach = true;
                        submit_humanlike(
                            &writer_tx,
                            format!("/oracle attach {bogus_thread_url}").as_str(),
                        )
                        .await;
                    }
                }
                result = &mut exit_rx => break result,
            }
        }
    })
    .await {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => {
            session.terminate();
            while let Ok(chunk) = output_rx.try_recv() {
                output.extend_from_slice(&chunk);
            }
            anyhow::bail!(
                "timed out waiting for native oracle fail-closed proof\n{}",
                failure_artifacts(&codex_home_path, &session_log_path, &output)
            );
        }
    };

    if !saw_attach_failure {
        anyhow::bail!(
            "expected attach failure for bogus Oracle thread {bogus_conversation_id}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if saw_attach_success {
        anyhow::bail!(
            "unexpected attach success while proving fail-closed behavior for {bogus_conversation_id}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if !(exit_code == 0 || exit_code == 130 || exit_code == 1) {
        anyhow::bail!(
            "unexpected exit code {exit_code}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    let matched = fs::read_dir(&sessions_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("meta.json"))
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let modified = path.metadata().ok()?.modified().ok()?;
            (modified >= test_started_at).then_some(path)
        })
        .any(|path| {
            oracle_session_targets_thread(&path, &bogus_thread_url, &bogus_conversation_id)
        });
    if matched {
        anyhow::bail!(
            "unexpected Oracle session persisted for bogus conversation {bogus_conversation_id}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    Ok(())
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

#[tokio::test]
#[ignore = "live native Oracle PTY proof; requires ORACLE_NATIVE_THREAD_URL"]
async fn native_oracle_model_matrix_roundtrip() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let thread_url = match std::env::var("ORACLE_NATIVE_THREAD_URL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping live oracle model matrix: ORACLE_NATIVE_THREAD_URL is unset");
            return Ok(());
        }
    };
    let conversation_id = std::env::var("ORACLE_NATIVE_CONVERSATION_ID")
        .ok()
        .or_else(|| parse_conversation_id(&thread_url))
        .context("missing conversation id for ORACLE_NATIVE_THREAD_URL")?;
    let project_url = std::env::var("ORACLE_SUPERVISOR_CHATGPT_URL")
        .context("ORACLE_SUPERVISOR_CHATGPT_URL must point at the hidden Oracle ChatGPT project")?;
    let codex_repo_root = codex_utils_cargo_bin::repo_root()?;
    let smarty_root = find_smarty_root(&codex_repo_root)?;
    let launcher = smarty_root.join("bin").join("smarty-codex");
    let workspace = smarty_root.clone();
    let sessions_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".oracle").join("sessions"))
        .context("HOME is unset; cannot inspect Oracle sessions")?;
    let test_started_at = SystemTime::now();
    let standard_token = format!(
        "CODEX_STD_PROOF_{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis()
    );
    let extended_token = format!("{standard_token}_EXT");
    let thinking_token = format!("{standard_token}_THK");
    let codex_home = tempfile::tempdir()?;
    let codex_home_path = codex_home.path().to_path_buf();
    let session_log_path = codex_home_path.join("native-oracle-model-matrix.jsonl");
    fs::write(
        codex_home_path.join("config.toml"),
        format!(
            "model_provider = \"ollama\"\ncheck_for_update_on_startup = false\n\n[tui]\nshow_tooltips = false\n\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            workspace.display()
        ),
    )?;

    let args = vec![
        "--no-alt-screen".to_string(),
        "-C".to_string(),
        workspace.display().to_string(),
        "-c".to_string(),
        "analytics.enabled=false".to_string(),
    ];
    let mut env = HashMap::new();
    env.insert(
        "CODEX_HOME".to_string(),
        codex_home_path.display().to_string(),
    );
    env.insert("SMARTY_NO_UPSTREAM_CHECK".to_string(), "1".to_string());
    env.insert(
        "SMARTY_CODEX_NO_DANGEROUS_DEFAULT".to_string(),
        "1".to_string(),
    );
    for key in ["PATH", "HOME", "SHELL", "TMPDIR"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    env.insert("ORACLE_ALLOW_VISIBLE_CHROME".to_string(), "0".to_string());
    env.insert(
        "ORACLE_CHATGPT_PROJECT_URL".to_string(),
        project_url.clone(),
    );
    env.insert("ORACLE_SUPERVISOR_CHATGPT_URL".to_string(), project_url);
    env.insert("CODEX_TUI_RECORD_SESSION".to_string(), "1".to_string());
    env.insert(
        "CODEX_TUI_SESSION_LOG_PATH".to_string(),
        session_log_path.display().to_string(),
    );

    let spawned = codex_utils_pty::spawn_pty_process(
        launcher.to_string_lossy().as_ref(),
        &args,
        &workspace,
        &env,
        &None,
        codex_utils_pty::TerminalSize::default(),
    )
    .await?;
    let codex_utils_pty::SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    } = spawned;
    let mut output = Vec::new();
    let mut output_rx = codex_utils_pty::combine_output_receivers(stdout_rx, stderr_rx);
    let mut exit_rx = exit_rx;
    let writer_tx = session.writer_sender();
    let interrupt_writer = writer_tx.clone();
    let mut answered_cursor_query = false;
    let mut recorded_failure: Option<String> = None;
    let mut last_nux_seen_at: Option<tokio::time::Instant> = None;
    let mut last_nux_ack_at: Option<tokio::time::Instant> = None;
    let mut startup_seen_at: Option<tokio::time::Instant> = None;
    let mut attach_ready_at: Option<tokio::time::Instant> = None;
    let mut saw_standard_model = false;
    let mut saw_extended_model = false;
    let mut saw_thinking_model = false;
    let mut standard_ack_at: Option<tokio::time::Instant> = None;
    let mut extended_ack_at: Option<tokio::time::Instant> = None;
    let mut thinking_ack_at: Option<tokio::time::Instant> = None;
    let mut completion_count = 0usize;
    let mut completion_changed_at: Option<tokio::time::Instant> = None;

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Phase {
        AwaitAttach,
        AwaitStandardAck,
        AwaitStandardCompletion,
        AwaitExtendedAck,
        AwaitExtendedCompletion,
        AwaitThinkingAck,
        AwaitThinkingCompletion,
        ExitRequested,
    }

    let mut phase = Phase::AwaitAttach;
    let exit_code = match timeout(oracle_live_timeout(Duration::from_secs(540), 3), async {
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        if chunk.windows(4).any(|window| window == b"\x1b[6n") {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                            answered_cursor_query = true;
                        }
                        let chunk_text = String::from_utf8_lossy(&chunk);
                        if chunk_text.contains("Use ↑/↓ to move, press enter to confirm") {
                            last_nux_seen_at = Some(tokio::time::Instant::now());
                            if last_nux_ack_at
                                .map_or(true, |instant| instant.elapsed() >= Duration::from_millis(500))
                            {
                                last_nux_ack_at = Some(tokio::time::Instant::now());
                                let _ = writer_tx.send(b"\r".to_vec()).await;
                            }
                        }
                        output.extend_from_slice(&chunk);
                        let sanitized_output = sanitized_output_text(&output);
                        let now = tokio::time::Instant::now();

                        if attach_ready_at.is_none()
                            && sanitized_output.contains("Attached Oracle thread on ")
                        {
                            attach_ready_at = Some(now);
                        }
                        if sanitized_output.contains("requested gpt-5.4-pro (Standard)") {
                            saw_standard_model = true;
                        }
                        if sanitized_output.contains("requested gpt-5.4-pro (Extended)") {
                            saw_extended_model = true;
                        }
                        if sanitized_output.contains("requested gpt-5.4 (Thinking 5.4)") {
                            saw_thinking_model = true;
                        }
                        if standard_ack_at.is_none()
                            && sanitized_output.contains(
                                "Oracle model preference set to gpt-5.4-pro (Standard).",
                            )
                        {
                            standard_ack_at = Some(now);
                        }
                        if extended_ack_at.is_none()
                            && sanitized_output.contains(
                                "Oracle model preference set to gpt-5.4-pro (Extended).",
                            )
                        {
                            extended_ack_at = Some(now);
                        }
                        if thinking_ack_at.is_none()
                            && sanitized_output.contains(
                                "Oracle model preference set to gpt-5.4 (Thinking 5.4).",
                            )
                        {
                            thinking_ack_at = Some(now);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break exit_rx.await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                },
                _ = sleep(Duration::from_millis(250)) => {
                    if session_log_path.is_file()
                        && let Ok(log) = fs::read_to_string(&session_log_path)
                    {
                        let now = tokio::time::Instant::now();
                        if startup_seen_at.is_none()
                            && (log.contains("\"type\":\"list_skills\"")
                                || log.contains("\"variant\":\"PluginMentionsLoaded"))
                        {
                            startup_seen_at = Some(now);
                        }
                        if log.contains("\"kind\":\"oracle_run_failed\"") {
                            recorded_failure = Some(format!(
                                "oracle_run_failed recorded in {session_log_path:?}"
                            ));
                            for _ in 0..3 {
                                let _ = interrupt_writer.send(vec![3]).await;
                                sleep(Duration::from_millis(200)).await;
                            }
                        }
                        let observed = count_occurrences(&log, "\"kind\":\"oracle_run_completed\"");
                        if observed > completion_count {
                            completion_count = observed;
                            completion_changed_at = Some(now);
                        }
                    }

                    let startup_nux_settled = last_nux_seen_at
                        .map_or(true, |instant| instant.elapsed() >= Duration::from_secs(1));
                    if !answered_cursor_query
                        || !startup_nux_settled
                        || !startup_seen_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        continue;
                    }

                    match phase {
                        Phase::AwaitAttach => {
                            if attach_ready_at.is_none() {
                                submit_humanlike(
                                    &writer_tx,
                                    format!("/oracle attach {thread_url}").as_str(),
                                )
                                .await;
                            } else if attach_ready_at
                                .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                            {
                                phase = Phase::AwaitStandardAck;
                                submit_humanlike(&writer_tx, "/oracle model pro standard").await;
                            }
                        }
                        Phase::AwaitStandardAck => {
                            if standard_ack_at
                                .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                            {
                                phase = Phase::AwaitStandardCompletion;
                                submit_humanlike_with_delays(
                                    &writer_tx,
                                    format!("Reply with exactly {standard_token} and nothing else.").as_str(),
                                    Duration::from_millis(35),
                                    Duration::from_millis(350),
                                )
                                .await;
                            }
                        }
                        Phase::AwaitStandardCompletion => {
                            if completion_count >= 1
                                && completion_changed_at
                                    .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                            {
                                phase = Phase::AwaitExtendedAck;
                                submit_humanlike(&writer_tx, "/oracle model pro extended").await;
                            }
                        }
                        Phase::AwaitExtendedAck => {
                            if extended_ack_at
                                .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                            {
                                phase = Phase::AwaitExtendedCompletion;
                                submit_humanlike_with_delays(
                                    &writer_tx,
                                    format!("Reply with exactly {extended_token} and nothing else.").as_str(),
                                    Duration::from_millis(35),
                                    Duration::from_millis(350),
                                )
                                .await;
                            }
                        }
                        Phase::AwaitExtendedCompletion => {
                            if completion_count >= 2
                                && completion_changed_at
                                    .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                            {
                                phase = Phase::AwaitThinkingAck;
                                submit_humanlike(&writer_tx, "/oracle model thinking").await;
                            }
                        }
                        Phase::AwaitThinkingAck => {
                            if thinking_ack_at
                                .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                            {
                                phase = Phase::AwaitThinkingCompletion;
                                submit_humanlike_with_delays(
                                    &writer_tx,
                                    format!("Reply with exactly {thinking_token} and nothing else.").as_str(),
                                    Duration::from_millis(35),
                                    Duration::from_millis(350),
                                )
                                .await;
                            }
                        }
                        Phase::AwaitThinkingCompletion => {
                            if completion_count >= 3
                                && completion_changed_at
                                    .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                            {
                                phase = Phase::ExitRequested;
                                for _ in 0..3 {
                                    let _ = interrupt_writer.send(vec![3]).await;
                                    sleep(Duration::from_millis(200)).await;
                                }
                            }
                        }
                        Phase::ExitRequested => {}
                    }
                }
                result = &mut exit_rx => break result,
            }
        }
    })
    .await {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => {
            session.terminate();
            while let Ok(chunk) = output_rx.try_recv() {
                output.extend_from_slice(&chunk);
            }
            anyhow::bail!(
                "timed out waiting for native oracle model matrix\n{}",
                failure_artifacts(&codex_home_path, &session_log_path, &output)
            );
        }
    };

    if let Some(recorded_failure) = recorded_failure {
        anyhow::bail!(
            "{recorded_failure}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if !(exit_code == 0 || exit_code == 130 || exit_code == 1) {
        anyhow::bail!(
            "unexpected exit code {exit_code}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if !(saw_standard_model && saw_extended_model && saw_thinking_model) {
        anyhow::bail!(
            "expected live PTY proof for pro standard, pro extended, and thinking\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    let log = fs::read_to_string(&session_log_path).unwrap_or_default();
    if count_occurrences(&log, "\"kind\":\"oracle_run_completed\"") < 3 {
        anyhow::bail!(
            "expected three oracle_run_completed entries in {session_log_path:?}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    let recent_meta_paths = fs::read_dir(&sessions_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("meta.json"))
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let modified = path.metadata().ok()?.modified().ok()?;
            (modified >= test_started_at).then_some(path)
        })
        .collect::<Vec<_>>();
    for token in [&standard_token, &extended_token, &thinking_token] {
        let matched = recent_meta_paths
            .iter()
            .any(|path| oracle_session_matches(path, &thread_url, &conversation_id, token));
        if !matched {
            anyhow::bail!(
                "expected a matching Oracle session for {conversation_id} containing {token}\n{}",
                failure_artifacts(&codex_home_path, &session_log_path, &output)
            );
        }
    }

    Ok(())
}

#[tokio::test]
#[ignore = "live native Oracle PTY proof; requires ORACLE_NATIVE_THREAD_URL"]
async fn native_oracle_file_upload_roundtrip() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let thread_url = match std::env::var("ORACLE_NATIVE_THREAD_URL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping live oracle file upload: ORACLE_NATIVE_THREAD_URL is unset");
            return Ok(());
        }
    };
    let conversation_id = std::env::var("ORACLE_NATIVE_CONVERSATION_ID")
        .ok()
        .or_else(|| parse_conversation_id(&thread_url))
        .context("missing conversation id for ORACLE_NATIVE_THREAD_URL")?;
    let project_url = std::env::var("ORACLE_SUPERVISOR_CHATGPT_URL")
        .context("ORACLE_SUPERVISOR_CHATGPT_URL must point at the hidden Oracle ChatGPT project")?;
    let codex_repo_root = codex_utils_cargo_bin::repo_root()?;
    let smarty_root = find_smarty_root(&codex_repo_root)?;
    let launcher = smarty_root.join("bin").join("smarty-codex");
    let workspace = smarty_root.clone();
    let sessions_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".oracle").join("sessions"))
        .context("HOME is unset; cannot inspect Oracle sessions")?;
    let test_started_at = SystemTime::now();
    let upload_token = format!(
        "CODEX_UPLOAD_PROOF_{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis()
    );
    let upload_dir = workspace.join("tmp");
    fs::create_dir_all(&upload_dir)?;
    let upload_file = tempfile::Builder::new()
        .prefix("oracle-live-upload-")
        .suffix(".txt")
        .tempfile_in(&upload_dir)?;
    fs::write(upload_file.path(), format!("{upload_token}\n"))?;
    let relative_upload_path = upload_file
        .path()
        .strip_prefix(&workspace)
        .context("upload proof file escaped workspace")?
        .to_string_lossy()
        .to_string();
    let codex_home = tempfile::tempdir()?;
    let codex_home_path = codex_home.path().to_path_buf();
    let session_log_path = codex_home_path.join("native-oracle-upload.jsonl");
    fs::write(
        codex_home_path.join("config.toml"),
        format!(
            "model_provider = \"ollama\"\ncheck_for_update_on_startup = false\n\n[tui]\nshow_tooltips = false\n\n[projects.\"{}\"]\ntrust_level = \"trusted\"\n",
            workspace.display()
        ),
    )?;

    let args = vec![
        "--no-alt-screen".to_string(),
        "-C".to_string(),
        workspace.display().to_string(),
        "-c".to_string(),
        "analytics.enabled=false".to_string(),
    ];
    let mut env = HashMap::new();
    env.insert(
        "CODEX_HOME".to_string(),
        codex_home_path.display().to_string(),
    );
    env.insert("SMARTY_NO_UPSTREAM_CHECK".to_string(), "1".to_string());
    env.insert(
        "SMARTY_CODEX_NO_DANGEROUS_DEFAULT".to_string(),
        "1".to_string(),
    );
    for key in ["PATH", "HOME", "SHELL", "TMPDIR"] {
        if let Ok(value) = std::env::var(key) {
            env.insert(key.to_string(), value);
        }
    }
    env.insert("ORACLE_ALLOW_VISIBLE_CHROME".to_string(), "0".to_string());
    env.insert(
        "ORACLE_CHATGPT_PROJECT_URL".to_string(),
        project_url.clone(),
    );
    env.insert("ORACLE_SUPERVISOR_CHATGPT_URL".to_string(), project_url);
    env.insert("CODEX_TUI_RECORD_SESSION".to_string(), "1".to_string());
    env.insert(
        "CODEX_TUI_SESSION_LOG_PATH".to_string(),
        session_log_path.display().to_string(),
    );

    let spawned = codex_utils_pty::spawn_pty_process(
        launcher.to_string_lossy().as_ref(),
        &args,
        &workspace,
        &env,
        &None,
        codex_utils_pty::TerminalSize::default(),
    )
    .await?;
    let codex_utils_pty::SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    } = spawned;
    let mut output = Vec::new();
    let mut output_rx = codex_utils_pty::combine_output_receivers(stdout_rx, stderr_rx);
    let mut exit_rx = exit_rx;
    let writer_tx = session.writer_sender();
    let interrupt_writer = writer_tx.clone();
    let mut answered_cursor_query = false;
    let mut recorded_failure: Option<String> = None;
    let mut saw_completion_log = false;
    let mut last_nux_seen_at: Option<tokio::time::Instant> = None;
    let mut last_nux_ack_at: Option<tokio::time::Instant> = None;
    let mut startup_seen_at: Option<tokio::time::Instant> = None;
    let mut attach_ready_at: Option<tokio::time::Instant> = None;
    let mut sent_attach = false;
    let mut sent_prompt = false;

    let exit_code = match timeout(oracle_live_timeout(Duration::from_secs(420), 1), async {
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        if chunk.windows(4).any(|window| window == b"\x1b[6n") {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                            answered_cursor_query = true;
                        }
                        let chunk_text = String::from_utf8_lossy(&chunk);
                        if chunk_text.contains("Use ↑/↓ to move, press enter to confirm") {
                            last_nux_seen_at = Some(tokio::time::Instant::now());
                            if last_nux_ack_at
                                .map_or(true, |instant| instant.elapsed() >= Duration::from_millis(500))
                            {
                                last_nux_ack_at = Some(tokio::time::Instant::now());
                                let _ = writer_tx.send(b"\r".to_vec()).await;
                            }
                        }
                        output.extend_from_slice(&chunk);
                        let sanitized_output = sanitized_output_text(&output);
                        let now = tokio::time::Instant::now();
                        if attach_ready_at.is_none()
                            && sanitized_output.contains("Attached Oracle thread on ")
                        {
                            attach_ready_at = Some(now);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break exit_rx.await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                },
                _ = sleep(Duration::from_millis(250)) => {
                    if session_log_path.is_file()
                        && let Ok(log) = fs::read_to_string(&session_log_path)
                    {
                        let now = tokio::time::Instant::now();
                        if startup_seen_at.is_none()
                            && (log.contains("\"type\":\"list_skills\"")
                                || log.contains("\"variant\":\"PluginMentionsLoaded"))
                        {
                            startup_seen_at = Some(now);
                        }
                        if log.contains("\"kind\":\"oracle_run_failed\"") {
                            recorded_failure = Some(format!(
                                "oracle_run_failed recorded in {session_log_path:?}"
                            ));
                            for _ in 0..3 {
                                let _ = interrupt_writer.send(vec![3]).await;
                                sleep(Duration::from_millis(200)).await;
                            }
                        }
                        if log.contains("\"kind\":\"oracle_run_completed\"") && !saw_completion_log {
                            saw_completion_log = true;
                            for _ in 0..3 {
                                let _ = interrupt_writer.send(vec![3]).await;
                                sleep(Duration::from_millis(200)).await;
                            }
                        }
                    }

                    let startup_nux_settled = last_nux_seen_at
                        .map_or(true, |instant| instant.elapsed() >= Duration::from_secs(1));

                    if !sent_attach
                        && answered_cursor_query
                        && startup_nux_settled
                        && startup_seen_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        sent_attach = true;
                        submit_humanlike(
                            &writer_tx,
                            format!("/oracle attach {thread_url}").as_str(),
                        )
                        .await;
                        continue;
                    }

                    if !sent_prompt
                        && attach_ready_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        sent_prompt = true;
                        submit_humanlike_with_delays(
                            &writer_tx,
                            format!(
                                "Read the attached file and reply with exactly its full contents and nothing else.\n\nfile: \"{relative_upload_path}\""
                            )
                            .as_str(),
                            Duration::from_millis(25),
                            Duration::from_millis(350),
                        )
                        .await;
                    }
                }
                result = &mut exit_rx => break result,
            }
        }
    })
    .await {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => {
            session.terminate();
            while let Ok(chunk) = output_rx.try_recv() {
                output.extend_from_slice(&chunk);
            }
            anyhow::bail!(
                "timed out waiting for native oracle upload roundtrip\n{}",
                failure_artifacts(&codex_home_path, &session_log_path, &output)
            );
        }
    };

    if let Some(recorded_failure) = recorded_failure {
        anyhow::bail!(
            "{recorded_failure}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if !saw_completion_log {
        anyhow::bail!(
            "expected oracle_run_completed in {session_log_path:?}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if !(exit_code == 0 || exit_code == 130 || exit_code == 1) {
        anyhow::bail!(
            "unexpected exit code {exit_code}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    let matched = fs::read_dir(&sessions_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("meta.json"))
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let modified = path.metadata().ok()?.modified().ok()?;
            (modified >= test_started_at).then_some(path)
        })
        .any(|path| {
            oracle_session_matches_with_file(
                &path,
                &thread_url,
                &conversation_id,
                &upload_token,
                upload_file.path(),
            )
        });
    if !matched {
        anyhow::bail!(
            "expected a matching Oracle upload session for {conversation_id} containing {upload_token}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    Ok(())
}
