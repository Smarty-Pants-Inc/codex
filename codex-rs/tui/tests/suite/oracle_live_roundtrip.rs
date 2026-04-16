use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use std::time::SystemTime;

use anyhow::Context;
use anyhow::Result;
use codex_ansi_escape::ansi_escape_line;
use serde_json::Value as JsonValue;
use serial_test::serial;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use tokio::select;
use tokio::sync::mpsc::Sender;
use tokio::time::sleep;
use tokio::time::timeout;

const ORACLE_SUPERVISOR_THROTTLE_FILE_ENV: &str = "ORACLE_SUPERVISOR_THROTTLE_FILE";
const CODEX_ORACLE_THREAD_HISTORY_LIMIT_ENV: &str = "CODEX_ORACLE_THREAD_HISTORY_LIMIT";
const ORACLE_NATIVE_HISTORY_THREAD_URL_ENV: &str = "ORACLE_NATIVE_HISTORY_THREAD_URL";
const ORACLE_NATIVE_HISTORY_CONVERSATION_ID_ENV: &str = "ORACLE_NATIVE_HISTORY_CONVERSATION_ID";
const ORACLE_SUPERVISOR_PRO_MAX_REQUESTS_ENV: &str =
    "ORACLE_SUPERVISOR_PRO_MAX_REQUESTS_PER_WINDOW";
const ORACLE_SUPERVISOR_DEFAULT_MAX_REQUESTS_ENV: &str =
    "ORACLE_SUPERVISOR_DEFAULT_MAX_REQUESTS_PER_WINDOW";
const ORACLE_SUPERVISOR_THROTTLE_WINDOW_SECS: i64 = 30 * 60;
const ORACLE_SUPERVISOR_THROTTLE_MIN_GAP_SECS: i64 = 30;
const ORACLE_SUPERVISOR_THROTTLE_MAX_REQUESTS: usize = 24;
const ORACLE_LIVE_QUEUE_BUFFER_SECS: u64 = 120;
const ORACLE_LIVE_HISTORY_PROOF_LIMIT: usize = 20;

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

fn oracle_session_matches_with_model(
    meta_path: &Path,
    thread_url: &str,
    conversation_id: &str,
    token: &str,
    expected_model: &str,
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
    let selected_model = meta.get("model").and_then(JsonValue::as_str);
    runtime
        .and_then(|value| value.get("conversationId"))
        .and_then(JsonValue::as_str)
        == Some(conversation_id)
        && runtime
            .and_then(|value| value.get("tabUrl"))
            .and_then(JsonValue::as_str)
            .is_some_and(|value| normalize_url(value) == normalize_url(thread_url))
        && response.contains(token)
        && selected_model == Some(expected_model)
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

fn parse_positive_usize_env(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
}

fn oracle_supervisor_throttle_max_requests() -> usize {
    [
        ORACLE_SUPERVISOR_DEFAULT_MAX_REQUESTS_ENV,
        ORACLE_SUPERVISOR_PRO_MAX_REQUESTS_ENV,
    ]
    .into_iter()
    .filter_map(parse_positive_usize_env)
    .min()
    .unwrap_or(ORACLE_SUPERVISOR_THROTTLE_MAX_REQUESTS)
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
    let max_requests = oracle_supervisor_throttle_max_requests();
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
        let window_ready = if request_times.len() >= max_requests {
            request_times[request_times.len() - max_requests] + window
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
#[serial(oracle_live_browser)]
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
    let throttle_file_path = codex_home_path.join("oracle-supervisor-throttle.json");
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
    env.insert(
        "ORACLE_SUPERVISOR_THROTTLE_FILE".to_string(),
        throttle_file_path.display().to_string(),
    );
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
#[serial(oracle_live_browser)]
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
    let throttle_file_path = codex_home_path.join("oracle-supervisor-throttle.json");
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
    env.insert(
        "ORACLE_SUPERVISOR_THROTTLE_FILE".to_string(),
        throttle_file_path.display().to_string(),
    );
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

struct OracleHistoryProbe {
    history: Vec<JsonValue>,
    returned_count: usize,
    total_count: usize,
    truncated: bool,
}

struct OracleHistoryImportLogProof {
    attach_index: usize,
    import_index: usize,
    user_turn_index: usize,
    imported_cell_count_before_user_turn: usize,
    outcome: String,
    message_count: Option<usize>,
    returned_count: usize,
    total_count: usize,
    truncated: bool,
}

async fn query_oracle_history_probe(
    smarty_root: &Path,
    project_url: &str,
    conversation_id: &str,
    history_limit: usize,
) -> Result<OracleHistoryProbe> {
    let oracle_root = smarty_root.join("forks").join("oracle");
    let request = serde_json::json!({
        "action": "thread_history",
        "conversationId": conversation_id,
        "historyLimit": history_limit,
    });
    let mut child = Command::new("pnpm")
        .arg("exec")
        .arg("tsx")
        .arg("bin/oracle-supervisor-broker.ts")
        .current_dir(&oracle_root)
        .env("ORACLE_ALLOW_VISIBLE_CHROME", "0")
        .env("ORACLE_CHATGPT_PROJECT_URL", project_url)
        .env("ORACLE_SUPERVISOR_CHATGPT_URL", project_url)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to start Oracle supervisor broker in {}",
                oracle_root.display()
            )
        })?;
    {
        let stdin = child
            .stdin
            .as_mut()
            .context("Oracle supervisor broker stdin was unavailable")?;
        stdin.write_all(request.to_string().as_bytes()).await?;
        stdin.write_all(b"\n").await?;
    }
    let output = timeout(Duration::from_secs(180), child.wait_with_output())
        .await
        .context("timed out waiting for Oracle supervisor broker history probe")??;
    if !output.status.success() {
        anyhow::bail!(
            "Oracle supervisor broker history probe failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let response_line = stdout
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty())
        .context("Oracle supervisor broker emitted no JSON response")?;
    let response: JsonValue = serde_json::from_str(response_line).with_context(|| {
        format!("failed to parse Oracle supervisor broker response: {response_line}")
    })?;
    if response.get("ok").and_then(JsonValue::as_bool) != Some(true) {
        anyhow::bail!(
            "Oracle supervisor broker returned failure: {}",
            response
                .get("error")
                .and_then(JsonValue::as_str)
                .unwrap_or(response_line)
        );
    }

    let history = response
        .get("history")
        .and_then(JsonValue::as_array)
        .cloned()
        .context("Oracle supervisor broker response omitted history")?;
    let history_window = response
        .get("historyWindow")
        .context("Oracle supervisor broker response omitted historyWindow")?;
    let returned_count = history_window
        .get("returnedCount")
        .and_then(JsonValue::as_u64)
        .context("Oracle supervisor broker response omitted returnedCount")?
        as usize;
    let total_count = history_window
        .get("totalCount")
        .and_then(JsonValue::as_u64)
        .context("Oracle supervisor broker response omitted totalCount")?
        as usize;
    let truncated = history_window
        .get("truncated")
        .and_then(JsonValue::as_bool)
        .context("Oracle supervisor broker response omitted truncated")?;

    Ok(OracleHistoryProbe {
        history,
        returned_count,
        total_count,
        truncated,
    })
}

fn session_log_entries(path: &Path) -> Result<Vec<JsonValue>> {
    fs::read_to_string(path)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .enumerate()
        .map(|(index, line)| {
            serde_json::from_str::<JsonValue>(line).with_context(|| {
                format!(
                    "failed to parse session log line {} from {}: {}",
                    index + 1,
                    path.display(),
                    line
                )
            })
        })
        .collect()
}

fn oracle_attach_import_indices(entries: &[JsonValue], conversation_id: &str) -> Vec<usize> {
    entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let is_structured_match = entry.get("kind").and_then(JsonValue::as_str)
                == Some("app_event")
                && entry.get("variant").and_then(JsonValue::as_str) == Some("ConfigureOracleMode")
                && entry
                    .get("payload")
                    .and_then(|value| value.get("conversation_id"))
                    .and_then(JsonValue::as_str)
                    == Some(conversation_id)
                && entry
                    .get("payload")
                    .and_then(|value| value.get("import_history"))
                    .and_then(JsonValue::as_bool)
                    == Some(true);
            let is_legacy_match = entry.get("kind").and_then(JsonValue::as_str)
                == Some("app_event")
                && entry
                    .get("variant")
                    .and_then(JsonValue::as_str)
                    .is_some_and(|variant| {
                        variant.contains("ConfigureOracleMode")
                            && variant.contains("attach ")
                            && variant.contains("--import-history")
                            && variant.contains(conversation_id)
                    });
            (is_structured_match || is_legacy_match).then_some(index)
        })
        .collect()
}

fn first_user_turn_index_after(
    entries: &[JsonValue],
    start_index: usize,
    token: &str,
) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .skip(start_index + 1)
        .find_map(|(index, entry)| {
            (entry.get("dir").and_then(JsonValue::as_str) == Some("from_tui")
                && entry.get("kind").and_then(JsonValue::as_str) == Some("op")
                && entry
                    .get("payload")
                    .and_then(|value| value.get("type"))
                    .and_then(JsonValue::as_str)
                    == Some("user_turn")
                && entry
                    .get("payload")
                    .is_some_and(|value| value.to_string().contains(token)))
            .then_some(index)
        })
}

fn oracle_history_import_log_proof(
    path: &Path,
    conversation_id: &str,
    token: &str,
) -> Result<OracleHistoryImportLogProof> {
    let entries = session_log_entries(path)?;
    let import_index = entries
        .iter()
        .enumerate()
        .find_map(|(index, entry)| {
            (entry.get("kind").and_then(JsonValue::as_str) == Some("oracle_history_import")
                && entry
                    .get("payload")
                    .and_then(|value| value.get("conversation_id"))
                    .and_then(JsonValue::as_str)
                    == Some(conversation_id))
            .then_some(index)
        })
        .context("session log did not record oracle_history_import")?;
    let attach_index = oracle_attach_import_indices(&entries, conversation_id)
        .into_iter()
        .filter(|index| *index < import_index)
        .next_back()
        .context("session log did not record ConfigureOracleMode attach --import-history before oracle_history_import")?;
    let user_turn_index = first_user_turn_index_after(&entries, import_index, token).context(
        "session log did not record the token-bearing user_turn after oracle_history_import",
    )?;
    let import_payload = entries[import_index]
        .get("payload")
        .context("oracle_history_import omitted payload")?;
    let history_window = import_payload
        .get("history_window")
        .context("oracle_history_import omitted history_window")?;
    let imported_cell_count_before_user_turn = entries[attach_index + 1..user_turn_index]
        .iter()
        .filter(|entry| {
            entry.get("kind").and_then(JsonValue::as_str) == Some("insert_history_cell")
        })
        .count();

    Ok(OracleHistoryImportLogProof {
        attach_index,
        import_index,
        user_turn_index,
        imported_cell_count_before_user_turn,
        outcome: import_payload
            .get("outcome")
            .and_then(JsonValue::as_str)
            .map(ToOwned::to_owned)
            .context("oracle_history_import omitted outcome")?,
        message_count: import_payload
            .get("message_count")
            .and_then(JsonValue::as_u64)
            .map(|value| value as usize),
        returned_count: history_window
            .get("returned_count")
            .and_then(JsonValue::as_u64)
            .map(|value| value as usize)
            .context("oracle_history_import omitted history_window.returned_count")?,
        total_count: history_window
            .get("total_count")
            .and_then(JsonValue::as_u64)
            .map(|value| value as usize)
            .context("oracle_history_import omitted history_window.total_count")?,
        truncated: history_window
            .get("truncated")
            .and_then(JsonValue::as_bool)
            .context("oracle_history_import omitted history_window.truncated")?,
    })
}

#[tokio::test]
#[serial(oracle_live_browser)]
#[ignore = "live native Oracle PTY proof; requires ORACLE_NATIVE_THREAD_URL"]
async fn native_oracle_attach_with_import_history_roundtrip() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let thread_url = match std::env::var(ORACLE_NATIVE_HISTORY_THREAD_URL_ENV) {
        Ok(value) => value,
        Err(_) => match std::env::var("ORACLE_NATIVE_THREAD_URL") {
            Ok(value) => value,
            Err(_) => {
                eprintln!(
                    "skipping live oracle import-history proof: ORACLE_NATIVE_THREAD_URL is unset"
                );
                return Ok(());
            }
        },
    };
    let conversation_id = std::env::var(ORACLE_NATIVE_HISTORY_CONVERSATION_ID_ENV)
        .ok()
        .or_else(|| std::env::var("ORACLE_NATIVE_CONVERSATION_ID").ok())
        .or_else(|| parse_conversation_id(&thread_url))
        .context("missing conversation id for oracle history import thread")?;
    let project_url = std::env::var("ORACLE_SUPERVISOR_CHATGPT_URL")
        .context("ORACLE_SUPERVISOR_CHATGPT_URL must point at the hidden Oracle ChatGPT project")?;
    let normalized_thread_url = normalize_url(&thread_url);
    if !normalized_thread_url.contains("/c/") {
        anyhow::bail!(
            "oracle history import proof requires a concrete ChatGPT thread URL containing /c/, got {normalized_thread_url}"
        );
    }
    let codex_repo_root = codex_utils_cargo_bin::repo_root()?;
    let smarty_root = find_smarty_root(&codex_repo_root)?;
    let full_history =
        query_oracle_history_probe(&smarty_root, &project_url, &conversation_id, 200).await?;
    let limited_history = query_oracle_history_probe(
        &smarty_root,
        &project_url,
        &conversation_id,
        ORACLE_LIVE_HISTORY_PROOF_LIMIT,
    )
    .await?;
    if full_history.total_count <= ORACLE_LIVE_HISTORY_PROOF_LIMIT {
        anyhow::bail!(
            "history proof requires a seeded thread with more than {} messages; {} only has {}",
            ORACLE_LIVE_HISTORY_PROOF_LIMIT,
            conversation_id,
            full_history.total_count
        );
    }
    if !limited_history.truncated {
        anyhow::bail!(
            "expected truncated Oracle history window for {} with limit {} (total_count={})",
            conversation_id,
            ORACLE_LIVE_HISTORY_PROOF_LIMIT,
            limited_history.total_count
        );
    }
    if full_history.returned_count != full_history.history.len() {
        anyhow::bail!(
            "full Oracle history probe count mismatch for {}: returned_count={} history_len={}",
            conversation_id,
            full_history.returned_count,
            full_history.history.len()
        );
    }
    if limited_history.returned_count != limited_history.history.len() {
        anyhow::bail!(
            "limited Oracle history probe count mismatch for {}: returned_count={} history_len={}",
            conversation_id,
            limited_history.returned_count,
            limited_history.history.len()
        );
    }
    if full_history.total_count != limited_history.total_count {
        anyhow::bail!(
            "Oracle history probe total_count mismatch for {}: full={} limited={}",
            conversation_id,
            full_history.total_count,
            limited_history.total_count
        );
    }
    let launcher = smarty_root.join("bin").join("smarty-codex");
    let workspace = smarty_root.clone();
    let sessions_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".oracle").join("sessions"))
        .context("HOME is unset; cannot inspect Oracle sessions")?;
    let token = format!(
        "CODEX_IMPORT_HISTORY_PROOF_{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis()
    );
    let test_started_at = SystemTime::now();
    let codex_home = tempfile::tempdir()?;
    let codex_home_path = codex_home.path().to_path_buf();
    let session_log_path = codex_home_path.join("native-oracle-import-history.jsonl");
    let throttle_file_path = codex_home_path.join("oracle-supervisor-throttle.json");
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
    env.insert(
        "ORACLE_SUPERVISOR_THROTTLE_FILE".to_string(),
        throttle_file_path.display().to_string(),
    );
    env.insert(
        CODEX_ORACLE_THREAD_HISTORY_LIMIT_ENV.to_string(),
        ORACLE_LIVE_HISTORY_PROOF_LIMIT.to_string(),
    );
    env.insert(
        "CODEX_ORACLE_MODEL_PRESET".to_string(),
        "thinking".to_string(),
    );
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
    let mut saw_import_history_log = false;
    let mut last_nux_seen_at: Option<tokio::time::Instant> = None;
    let mut last_nux_ack_at: Option<tokio::time::Instant> = None;
    let mut startup_seen_at: Option<tokio::time::Instant> = None;
    let mut sent_attach = false;
    let mut sent_prompt = false;
    let mut import_history_ready_at: Option<tokio::time::Instant> = None;

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
                        if recorded_failure.is_none()
                            && (sanitized_output.contains("failed to fetch remote history")
                                || sanitized_output.contains("failed to import remote history"))
                        {
                            recorded_failure = Some(format!(
                                "oracle attach --import-history reported a remote history error in {session_log_path:?}"
                            ));
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
                        let now = tokio::time::Instant::now();
                        if startup_seen_at.is_none()
                            && (log.contains("\"type\":\"list_skills\"")
                                || log.contains("\"variant\":\"PluginMentionsLoaded"))
                        {
                            startup_seen_at = Some(now);
                        }
                        if !saw_import_history_log
                            && log.contains("\"kind\":\"oracle_history_import\"")
                        {
                            saw_import_history_log = true;
                            import_history_ready_at = Some(now);
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
                            format!("/oracle attach {thread_url} --import-history").as_str(),
                        )
                        .await;
                        continue;
                    }

                    if !sent_prompt
                        && import_history_ready_at
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
                "timed out waiting for native oracle import-history roundtrip\n{}",
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
    if !saw_import_history_log {
        anyhow::bail!(
            "expected oracle_history_import in {session_log_path:?}\n{}",
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
            oracle_session_matches_with_model(
                &path,
                &thread_url,
                &conversation_id,
                &token,
                "gpt-5.4",
            )
        });
    if !matched {
        anyhow::bail!(
            "expected a matching Oracle import-history session for {conversation_id} containing {token} on gpt-5.4\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    let import_proof = oracle_history_import_log_proof(&session_log_path, &conversation_id, &token)
        .with_context(|| {
            format!(
                "failed to prove bounded Oracle import-history replay from {}",
                session_log_path.display()
            )
        })?;
    if import_proof.outcome != "imported" {
        anyhow::bail!(
            "expected oracle_history_import outcome imported, got {}\n{}",
            import_proof.outcome,
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if import_proof.message_count != Some(limited_history.returned_count) {
        anyhow::bail!(
            "expected oracle_history_import message_count {} but got {:?}\n{}",
            limited_history.returned_count,
            import_proof.message_count,
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if import_proof.returned_count != limited_history.returned_count
        || import_proof.total_count != limited_history.total_count
        || !import_proof.truncated
    {
        anyhow::bail!(
            "oracle_history_import window mismatch: expected returned_count={} total_count={} truncated=true, got returned_count={} total_count={} truncated={}\n{}",
            limited_history.returned_count,
            limited_history.total_count,
            import_proof.returned_count,
            import_proof.total_count,
            import_proof.truncated,
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if import_proof.imported_cell_count_before_user_turn < limited_history.returned_count {
        anyhow::bail!(
            "expected at least {} insert_history_cell events before the next user_turn after attach, saw {}\n{}",
            limited_history.returned_count,
            import_proof.imported_cell_count_before_user_turn,
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if !(import_proof.attach_index < import_proof.import_index
        && import_proof.import_index < import_proof.user_turn_index)
    {
        anyhow::bail!(
            "oracle_history_import ordering was invalid: attach_index={} import_index={} user_turn_index={}\n{}",
            import_proof.attach_index,
            import_proof.import_index,
            import_proof.user_turn_index,
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    Ok(())
}

#[tokio::test]
#[serial(oracle_live_browser)]
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
    let throttle_file_path = codex_home_path.join("oracle-supervisor-throttle.json");
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
    env.insert(
        "ORACLE_SUPERVISOR_THROTTLE_FILE".to_string(),
        throttle_file_path.display().to_string(),
    );
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
#[serial(oracle_live_browser)]
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
    let throttle_file_path = codex_home_path.join("oracle-supervisor-throttle.json");
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
    env.insert(
        "ORACLE_SUPERVISOR_THROTTLE_FILE".to_string(),
        throttle_file_path.display().to_string(),
    );
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
