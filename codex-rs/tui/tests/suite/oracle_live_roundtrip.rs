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

fn oracle_session_matches_with_files(
    meta_path: &Path,
    thread_url: &str,
    conversation_id: &str,
    tokens: &[&str],
    expected_files: &[&Path],
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
        && tokens.iter().all(|token| response.contains(token))
        && files.is_some_and(|entries| {
            expected_files.iter().all(|expected_file| {
                entries.iter().any(|entry| {
                    entry
                        .as_str()
                        .is_some_and(|value| Path::new(value) == *expected_file)
                })
            })
        })
}

fn oracle_session_matches_with_file(
    meta_path: &Path,
    thread_url: &str,
    conversation_id: &str,
    token: &str,
    expected_file: &Path,
) -> bool {
    oracle_session_matches_with_files(
        meta_path,
        thread_url,
        conversation_id,
        &[token],
        &[expected_file],
    )
}

fn oracle_session_matches_with_download(
    meta_path: &Path,
    thread_url: &str,
    conversation_id: &str,
    expected_filename: &str,
    expected_download_contents: &str,
) -> bool {
    let Ok(raw) = fs::read_to_string(meta_path) else {
        return false;
    };
    let Ok(meta) = serde_json::from_str::<JsonValue>(&raw) else {
        return false;
    };
    let runtime = meta.get("browser").and_then(|value| value.get("runtime"));
    let expected_link_prefix = format!("[{expected_filename}](sandbox:/mnt/data/");
    let response = meta
        .get("response")
        .and_then(|value| value.get("assistantOutput"))
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let downloads = meta
        .get("response")
        .and_then(|value| value.get("downloads"))
        .and_then(JsonValue::as_array);
    runtime
        .and_then(|value| value.get("conversationId"))
        .and_then(JsonValue::as_str)
        == Some(conversation_id)
        && runtime
            .and_then(|value| value.get("tabUrl"))
            .and_then(JsonValue::as_str)
            .is_some_and(|value| normalize_url(value) == normalize_url(thread_url))
        && response.contains(&expected_link_prefix)
        && downloads.is_some_and(|entries| {
            entries.iter().any(|entry| {
                let Some(path_value) = entry.get("path").and_then(JsonValue::as_str) else {
                    return false;
                };
                let suggested_filename = entry.get("suggestedFilename").and_then(JsonValue::as_str);
                fs::read_to_string(path_value).ok().is_some_and(|contents| {
                    suggested_filename == Some(expected_filename)
                        && contents.replace("\r\n", "\n").trim_end()
                            == expected_download_contents.replace("\r\n", "\n").trim_end()
                })
            })
        })
}

fn recent_oracle_meta_paths(
    sessions_dir: &Path,
    test_started_at: SystemTime,
) -> Result<Vec<PathBuf>> {
    Ok(fs::read_dir(sessions_dir)?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join("meta.json"))
        .filter(|path| path.is_file())
        .filter_map(|path| {
            let modified = path.metadata().ok()?.modified().ok()?;
            (modified >= test_started_at).then_some(path)
        })
        .collect::<Vec<_>>())
}

async fn send_key_burst(writer_tx: &Sender<Vec<u8>>, key: &[u8], repeats: usize, delay: Duration) {
    for _ in 0..repeats {
        let _ = writer_tx.send(key.to_vec()).await;
        sleep(delay).await;
    }
}

async fn send_interrupt_burst(writer_tx: &Sender<Vec<u8>>, repeats: usize, delay: Duration) {
    send_key_burst(writer_tx, b"\x1b[99;5:1u", repeats, delay).await;
}

async fn send_escape_burst(writer_tx: &Sender<Vec<u8>>, repeats: usize, delay: Duration) {
    send_key_burst(writer_tx, b"\x1b[27u", repeats, delay).await;
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for byte in bytes {
        let mut value = (crc ^ u32::from(*byte)) & 0xFF;
        for _ in 0..8 {
            value = if (value & 1) == 1 {
                (value >> 1) ^ 0xEDB8_8320
            } else {
                value >> 1
            };
        }
        crc = (crc >> 8) ^ value;
    }
    !crc
}

fn write_single_entry_zip(path: &Path, entry_name: &str, contents: &[u8]) -> Result<()> {
    let entry_name_bytes = entry_name.as_bytes();
    let entry_name_len =
        u16::try_from(entry_name_bytes.len()).context("zip entry name exceeded u16 length")?;
    let contents_len = u32::try_from(contents.len()).context("zip entry exceeded u32 length")?;
    let checksum = crc32(contents);
    let local_header_len = 30u32 + u32::from(entry_name_len);
    let central_directory_offset = local_header_len + contents_len;
    let central_directory_len = 46u32 + u32::from(entry_name_len);

    let mut zip = Vec::new();

    zip.extend_from_slice(&0x0403_4B50u32.to_le_bytes());
    zip.extend_from_slice(&20u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&checksum.to_le_bytes());
    zip.extend_from_slice(&contents_len.to_le_bytes());
    zip.extend_from_slice(&contents_len.to_le_bytes());
    zip.extend_from_slice(&entry_name_len.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(entry_name_bytes);
    zip.extend_from_slice(contents);

    zip.extend_from_slice(&0x0201_4B50u32.to_le_bytes());
    zip.extend_from_slice(&20u16.to_le_bytes());
    zip.extend_from_slice(&20u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&checksum.to_le_bytes());
    zip.extend_from_slice(&contents_len.to_le_bytes());
    zip.extend_from_slice(&contents_len.to_le_bytes());
    zip.extend_from_slice(&entry_name_len.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u32.to_le_bytes());
    zip.extend_from_slice(&0u32.to_le_bytes());
    zip.extend_from_slice(entry_name_bytes);

    zip.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());
    zip.extend_from_slice(&1u16.to_le_bytes());
    zip.extend_from_slice(&1u16.to_le_bytes());
    zip.extend_from_slice(&central_directory_len.to_le_bytes());
    zip.extend_from_slice(&central_directory_offset.to_le_bytes());
    zip.extend_from_slice(&0u16.to_le_bytes());

    fs::write(path, zip)?;
    Ok(())
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
    let mut loop_tick = tokio::time::interval(Duration::from_millis(250));
    loop_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop_tick.tick().await;

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
                                .is_none_or(|instant| instant.elapsed() >= Duration::from_millis(500))
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
                _ = loop_tick.tick() => {
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
                        .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));

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
    let mut loop_tick = tokio::time::interval(Duration::from_millis(250));
    loop_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop_tick.tick().await;

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
                                .is_none_or(|instant| instant.elapsed() >= Duration::from_millis(500))
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
                _ = loop_tick.tick() => {
                    if session_log_path.is_file()
                        && let Ok(log) = fs::read_to_string(&session_log_path)
                        && startup_seen_at.is_none()
                        && (log.contains("\"type\":\"list_skills\"")
                            || log.contains("\"variant\":\"PluginMentionsLoaded"))
                    {
                        startup_seen_at = Some(tokio::time::Instant::now());
                    }

                    let startup_nux_settled = last_nux_seen_at
                        .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));

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

#[tokio::test]
#[serial(oracle_live_browser)]
#[ignore = "live native Oracle PTY proof; requires ORACLE_NATIVE_THREAD_URL"]
async fn native_oracle_interrupt_recovery_roundtrip() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let thread_url = match std::env::var("ORACLE_NATIVE_THREAD_URL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!(
                "skipping live oracle interrupt/recovery proof: ORACLE_NATIVE_THREAD_URL is unset"
            );
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
    let long_prompt_token = format!(
        "CODEX_INTERRUPT_LONG_{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis()
    );
    let recovery_token = format!("{long_prompt_token}_RECOVERY");
    let codex_home = tempfile::tempdir()?;
    let codex_home_path = codex_home.path().to_path_buf();
    let session_log_path = codex_home_path.join("native-oracle-interrupt-recovery.jsonl");
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
    let mut last_nux_seen_at: Option<tokio::time::Instant> = None;
    let mut last_nux_ack_at: Option<tokio::time::Instant> = None;
    let mut startup_seen_at: Option<tokio::time::Instant> = None;
    let mut attach_ready_at: Option<tokio::time::Instant> = None;
    let mut thinking_ack_at: Option<tokio::time::Instant> = None;
    let mut sent_attach = false;
    let mut model_command_sent_at: Option<tokio::time::Instant> = None;
    let mut long_prompt_sent = false;
    let mut long_prompt_logged = false;
    let mut last_interrupt_attempt_at: Option<tokio::time::Instant> = None;
    let mut saw_interrupt_log = false;
    let mut interrupt_log_seen_at: Option<tokio::time::Instant> = None;
    let mut interrupt_op_count = 0usize;
    let mut interrupt_attempt_count = 0usize;
    let mut recovery_prompt_sent_at: Option<tokio::time::Instant> = None;
    let mut recovery_completion_target: Option<usize> = None;
    let mut recovery_completed_at: Option<tokio::time::Instant> = None;
    let mut completion_count = 0usize;
    let mut completion_changed_at: Option<tokio::time::Instant> = None;
    let mut exit_requested = false;
    let mut loop_tick = tokio::time::interval(Duration::from_millis(250));
    loop_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop_tick.tick().await;

    let exit_code = match timeout(oracle_live_timeout(Duration::from_secs(480), 2), async {
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
                                .is_none_or(|instant| instant.elapsed() >= Duration::from_millis(500))
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
                _ = loop_tick.tick() => {
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
                            send_interrupt_burst(
                                &interrupt_writer,
                                3,
                                Duration::from_millis(200),
                            )
                            .await;
                        }
                        let observed = count_occurrences(&log, "\"kind\":\"oracle_run_completed\"");
                        if observed > completion_count {
                            completion_count = observed;
                            completion_changed_at = Some(now);
                        }
                        let observed_interrupt_ops = count_interrupt_ops(&log);
                        if observed_interrupt_ops > interrupt_op_count {
                            interrupt_op_count = observed_interrupt_ops;
                        }
                        if !long_prompt_logged && log.contains(&long_prompt_token) {
                            long_prompt_logged = true;
                            last_interrupt_attempt_at = None;
                        }
                        if !saw_interrupt_log
                            && log.contains("\"kind\":\"oracle_run_interrupted\"")
                        {
                            saw_interrupt_log = true;
                            interrupt_log_seen_at = Some(now);
                        }
                        if recorded_failure.is_none()
                            && long_prompt_logged
                            && !saw_interrupt_log
                            && completion_count > 0
                        {
                            recorded_failure = Some(format!(
                                "the long interrupt-proof run completed {} in {session_log_path:?}",
                                if interrupt_attempt_count == 0 {
                                    "before the live harness made any interrupt attempt"
                                } else if interrupt_op_count == 0 {
                                    "before any interrupt op was emitted"
                                } else {
                                    "after interrupt was requested but before oracle_run_interrupted was observed"
                                }
                            ));
                            send_interrupt_burst(
                                &interrupt_writer,
                                3,
                                Duration::from_millis(200),
                            )
                            .await;
                        }
                    }

                    let startup_nux_settled = last_nux_seen_at
                        .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));
                    if !answered_cursor_query
                        || !startup_nux_settled
                        || startup_seen_at.is_none_or(|instant| instant.elapsed() < Duration::from_secs(2))
                    {
                        continue;
                    }

                    if !sent_attach {
                        sent_attach = true;
                        submit_humanlike(
                            &writer_tx,
                            format!("/oracle attach {thread_url}").as_str(),
                        )
                        .await;
                        continue;
                    }

                    let now = tokio::time::Instant::now();
                    if model_command_sent_at.is_none()
                        && attach_ready_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        model_command_sent_at = Some(now);
                        submit_humanlike(&writer_tx, "/oracle model thinking").await;
                        continue;
                    }

                    if !long_prompt_sent
                        && model_command_sent_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                        && (thinking_ack_at.is_some()
                            || model_command_sent_at
                                .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(10)))
                    {
                        long_prompt_sent = true;
                        submit_humanlike_with_delays(
                            &writer_tx,
                            format!(
                                "Interrupt-resilience live proof: produce a very long response by printing numbers 1 through 4000, one per line, and then finish with exactly {long_prompt_token}."
                            )
                            .as_str(),
                            Duration::from_millis(25),
                            Duration::from_millis(350),
                        )
                        .await;
                        continue;
                    }

                    if recovery_prompt_sent_at.is_none()
                        && long_prompt_logged
                        && !saw_interrupt_log
                        && completion_count == 0
                        && last_interrupt_attempt_at
                            .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        last_interrupt_attempt_at = Some(now);
                        interrupt_attempt_count += 1;
                        send_escape_burst(
                            &interrupt_writer,
                            1,
                            Duration::from_millis(125),
                        )
                        .await;
                        continue;
                    }

                    if saw_interrupt_log && recovery_prompt_sent_at.is_none() {
                        recovery_prompt_sent_at = Some(now);
                        recovery_completion_target = Some(completion_count + 1);
                        submit_humanlike_with_delays(
                            &writer_tx,
                            format!("Reply with exactly {recovery_token} and nothing else.").as_str(),
                            Duration::from_millis(35),
                            Duration::from_millis(350),
                        )
                        .await;
                        continue;
                    }

                    if recovery_completed_at.is_none()
                        && recovery_completion_target
                            .is_some_and(|target| completion_count >= target)
                        && completion_changed_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        recovery_completed_at = Some(now);
                    }

                    if recovery_completed_at.is_some() && !exit_requested {
                        exit_requested = true;
                        send_interrupt_burst(
                            &interrupt_writer,
                            3,
                            Duration::from_millis(200),
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
                "timed out waiting for native oracle interrupt/recovery proof\n{}",
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
    if !saw_interrupt_log {
        anyhow::bail!(
            "expected oracle_run_interrupted in {session_log_path:?}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if interrupt_attempt_count == 0 || interrupt_op_count == 0 {
        anyhow::bail!(
            "expected a locally emitted interrupt op after {interrupt_attempt_count} interrupt attempts; observed {interrupt_op_count}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    let restart_after_interrupt = match (interrupt_log_seen_at, recovery_prompt_sent_at) {
        (Some(interrupt_seen_at), Some(restart_sent_at)) => {
            restart_sent_at.saturating_duration_since(interrupt_seen_at)
        }
        _ => {
            anyhow::bail!(
                "expected an immediate recovery prompt after oracle_run_interrupted\n{}",
                failure_artifacts(&codex_home_path, &session_log_path, &output)
            );
        }
    };
    if restart_after_interrupt > Duration::from_secs(10) {
        anyhow::bail!(
            "recovery prompt waited {:?} after interrupt (expected <=10s)\n{}",
            restart_after_interrupt,
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    let recovery_completion_delay = match (recovery_prompt_sent_at, recovery_completed_at) {
        (Some(recovery_started), Some(recovery_completed)) => {
            recovery_completed.saturating_duration_since(recovery_started)
        }
        _ => {
            anyhow::bail!(
                "expected recovery run completion after interrupt\n{}",
                failure_artifacts(&codex_home_path, &session_log_path, &output)
            );
        }
    };
    if recovery_completion_delay > Duration::from_secs(180) {
        anyhow::bail!(
            "recovery run took {:?} after restart; stale lease/throttle delay suspected\n{}",
            recovery_completion_delay,
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    if !(exit_code == 0 || exit_code == 130 || exit_code == 1) {
        anyhow::bail!(
            "unexpected exit code {exit_code}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    let matched = recent_oracle_meta_paths(&sessions_dir, test_started_at)?
        .iter()
        .any(|path| {
            oracle_session_matches_with_model(
                path,
                &thread_url,
                &conversation_id,
                &recovery_token,
                "gpt-5.4",
            )
        });
    if !matched {
        anyhow::bail!(
            "expected matching Oracle recovery session on gpt-5.4 for conversation {conversation_id} containing {recovery_token}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    let entries = session_log_entries(&session_log_path)?;
    let interrupted_index = entries
        .iter()
        .enumerate()
        .find_map(|(index, entry)| {
            (entry.get("kind").and_then(JsonValue::as_str) == Some("oracle_run_interrupted"))
                .then_some(index)
        })
        .context("session log did not record oracle_run_interrupted")?;
    let interrupted_ts = entry_timestamp(&entries[interrupted_index])?;
    let interrupted_run_id = entries[interrupted_index]
        .get("payload")
        .and_then(|value| value.get("run_id"))
        .and_then(JsonValue::as_str)
        .context("oracle_run_interrupted payload omitted run_id")?;
    let recovery_user_turn_index =
        first_user_turn_index_after(&entries, interrupted_index, &recovery_token).context(
            "session log did not record the recovery token-bearing user_turn after interruption",
        )?;
    let recovery_user_turn_ts = entry_timestamp(&entries[recovery_user_turn_index])?;
    let recovery_submit_delay = recovery_user_turn_ts
        .signed_duration_since(interrupted_ts)
        .to_std()
        .unwrap_or(Duration::ZERO);
    if recovery_submit_delay > Duration::from_secs(20) {
        anyhow::bail!(
            "recovery user_turn occurred {:?} after oracle_run_interrupted (expected <=20s)\n{}",
            recovery_submit_delay,
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    let (recovery_completion_index, recovery_completion_entry) = entries
        .iter()
        .enumerate()
        .skip(recovery_user_turn_index + 1)
        .find(|(_, entry)| {
            entry.get("kind").and_then(JsonValue::as_str) == Some("oracle_run_completed")
        })
        .context("session log did not record oracle_run_completed after recovery user_turn")?;
    let recovery_completion_ts = entry_timestamp(recovery_completion_entry)?;
    let recovery_run_id = recovery_completion_entry
        .get("payload")
        .and_then(|value| value.get("run_id"))
        .and_then(JsonValue::as_str)
        .context("oracle_run_completed payload omitted run_id for recovery turn")?;
    if recovery_run_id == interrupted_run_id {
        anyhow::bail!(
            "expected recovery completion run_id to differ from interrupted run_id {interrupted_run_id}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }
    let recovery_log_delay = recovery_completion_ts
        .signed_duration_since(recovery_user_turn_ts)
        .to_std()
        .unwrap_or(Duration::ZERO);
    if recovery_log_delay > Duration::from_secs(180) {
        anyhow::bail!(
            "recovery oracle_run_completed at entry {recovery_completion_index} took {:?} after recovery user_turn; stale lease/throttle delay suspected\n{}",
            recovery_log_delay,
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    Ok(())
}

fn count_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

fn count_interrupt_ops(log: &str) -> usize {
    count_occurrences(log, "\"kind\":\"op\",\"payload\":{\"type\":\"interrupt\"")
}

fn entry_timestamp(entry: &JsonValue) -> Result<chrono::DateTime<chrono::Utc>> {
    let raw = entry
        .get("ts")
        .and_then(JsonValue::as_str)
        .context("session log entry omitted ts")?;
    chrono::DateTime::parse_from_rfc3339(raw)
        .map(|value| value.with_timezone(&chrono::Utc))
        .with_context(|| format!("failed to parse session log timestamp {raw}"))
}

struct OracleHistoryProbe {
    history: Vec<JsonValue>,
    returned_count: usize,
    total_count: usize,
    truncated: bool,
}

#[derive(Debug)]
struct OracleHistoryImportLogProof {
    attach_index: usize,
    import_index: usize,
    user_turn_index: usize,
    completion_index: usize,
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

async fn create_fresh_oracle_project_thread(
    smarty_root: &Path,
    project_url: &str,
) -> Result<(String, String)> {
    let oracle_root = smarty_root.join("forks").join("oracle");
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is unset; cannot inspect Oracle sessions")?;
    let slug = format!(
        "codex-oracle-download-seed-{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_secs()
    );
    let meta_path = home
        .join(".oracle")
        .join("sessions")
        .join(&slug)
        .join("meta.json");
    let child = Command::new("pnpm")
        .arg("exec")
        .arg("tsx")
        .arg("bin/oracle-cli.ts")
        .arg("--engine")
        .arg("browser")
        .arg("--model")
        .arg("gpt-5.4")
        .arg("--wait")
        .arg("--heartbeat")
        .arg("0")
        .arg("--timeout")
        .arg("900")
        .arg("--browser-input-timeout")
        .arg("120000")
        .arg("--browser-manual-login")
        .arg("--browser-model-strategy")
        .arg("select")
        .arg("--browser-hide-window")
        .arg("--chatgpt-url")
        .arg(project_url)
        .arg("--slug")
        .arg(&slug)
        .arg("--force")
        .arg("--prompt")
        .arg("Reply with exactly READY and nothing else.")
        .current_dir(&oracle_root)
        .env("ORACLE_ALLOW_VISIBLE_CHROME", "0")
        .env("ORACLE_CHATGPT_PROJECT_URL", project_url)
        .env("ORACLE_SUPERVISOR_CHATGPT_URL", project_url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| {
            format!(
                "failed to start Oracle CLI for a fresh project thread in {}",
                oracle_root.display()
            )
        })?;
    let output = timeout(Duration::from_secs(240), child.wait_with_output())
        .await
        .context("timed out waiting for Oracle CLI to create a fresh project thread")??;
    if !output.status.success() {
        anyhow::bail!(
            "Oracle CLI fresh-thread seed failed with status {}\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let raw = fs::read_to_string(&meta_path).with_context(|| {
        format!(
            "fresh Oracle thread seed did not write session metadata at {}",
            meta_path.display()
        )
    })?;
    let meta: JsonValue = serde_json::from_str(&raw).with_context(|| {
        format!(
            "failed to parse fresh Oracle thread seed metadata at {}",
            meta_path.display()
        )
    })?;
    let assistant_output = meta
        .get("response")
        .and_then(|value| value.get("assistantOutput"))
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .trim();
    if assistant_output != "READY" {
        anyhow::bail!(
            "fresh Oracle thread seed returned unexpected output {assistant_output:?} in {}",
            meta_path.display()
        );
    }

    let thread_url = meta
        .get("browser")
        .and_then(|value| value.get("runtime"))
        .and_then(|value| value.get("tabUrl"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .filter(|value| value.contains("/c/"))
        .context("fresh Oracle thread seed metadata omitted a concrete thread url")?;
    let conversation_id = meta
        .get("browser")
        .and_then(|value| value.get("runtime"))
        .and_then(|value| value.get("conversationId"))
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .or_else(|| parse_conversation_id(&thread_url))
        .context("fresh Oracle thread seed metadata omitted conversation id")?;

    Ok((thread_url, conversation_id))
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

fn first_oracle_run_completed_index_after(
    entries: &[JsonValue],
    start_index: usize,
) -> Option<usize> {
    entries
        .iter()
        .enumerate()
        .skip(start_index + 1)
        .find_map(|(index, entry)| {
            (entry.get("kind").and_then(JsonValue::as_str) == Some("oracle_run_completed"))
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
        .rfind(|index| *index < import_index)
        .context("session log did not record ConfigureOracleMode attach --import-history before oracle_history_import")?;
    let user_turn_index = first_user_turn_index_after(&entries, import_index, token).context(
        "session log did not record the token-bearing user_turn after oracle_history_import",
    )?;
    let completion_index = first_oracle_run_completed_index_after(&entries, user_turn_index)
        .context("session log did not record oracle_run_completed after token-bearing user_turn")?;
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
        completion_index,
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

fn write_temp_session_log(entries: &[JsonValue]) -> Result<PathBuf> {
    let nonce = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos();
    let path = env::temp_dir().join(format!(
        "codex-oracle-import-history-proof-{}-{nonce}.jsonl",
        std::process::id()
    ));
    let mut raw = String::new();
    for entry in entries {
        raw.push_str(&serde_json::to_string(entry)?);
        raw.push('\n');
    }
    fs::write(&path, raw)?;
    Ok(path)
}

#[test]
fn oracle_history_import_log_proof_requires_oracle_run_completed_after_token_user_turn()
-> Result<()> {
    let conversation_id = "conv-123";
    let token = "PROOF_TOKEN";
    let path = write_temp_session_log(&[
        serde_json::json!({
            "kind": "app_event",
            "variant": "ConfigureOracleMode",
            "payload": {
                "conversation_id": conversation_id,
                "import_history": true
            }
        }),
        serde_json::json!({
            "kind": "oracle_history_import",
            "payload": {
                "conversation_id": conversation_id,
                "outcome": "imported",
                "message_count": 2,
                "history_window": {
                    "returned_count": 2,
                    "total_count": 3,
                    "truncated": true
                }
            }
        }),
        serde_json::json!({"kind": "insert_history_cell"}),
        serde_json::json!({
            "dir": "from_tui",
            "kind": "op",
            "payload": {
                "type": "user_turn",
                "items": [{"text": format!("Reply with {token}")}]
            }
        }),
    ])?;

    let err = match oracle_history_import_log_proof(&path, conversation_id, token) {
        Ok(_) => anyhow::bail!("missing completion after token-bearing user turn should fail"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains(
            "session log did not record oracle_run_completed after token-bearing user_turn"
        )
    );

    let _ = fs::remove_file(path);
    Ok(())
}

#[test]
fn oracle_history_import_log_proof_tracks_completion_index_after_user_turn() -> Result<()> {
    let conversation_id = "conv-123";
    let token = "PROOF_TOKEN";
    let path = write_temp_session_log(&[
        serde_json::json!({
            "kind": "app_event",
            "variant": "ConfigureOracleMode",
            "payload": {
                "conversation_id": conversation_id,
                "import_history": true
            }
        }),
        serde_json::json!({
            "kind": "oracle_history_import",
            "payload": {
                "conversation_id": conversation_id,
                "outcome": "imported",
                "message_count": 2,
                "history_window": {
                    "returned_count": 2,
                    "total_count": 3,
                    "truncated": true
                }
            }
        }),
        serde_json::json!({"kind": "insert_history_cell"}),
        serde_json::json!({
            "dir": "from_tui",
            "kind": "op",
            "payload": {
                "type": "user_turn",
                "items": [{"text": format!("Reply with {token}")}]
            }
        }),
        serde_json::json!({"kind": "oracle_run_completed", "payload": {"run_id": "run-1"}}),
    ])?;

    let proof = oracle_history_import_log_proof(&path, conversation_id, token)?;
    assert_eq!(proof.attach_index, 0);
    assert_eq!(proof.import_index, 1);
    assert_eq!(proof.user_turn_index, 3);
    assert_eq!(proof.completion_index, 4);

    let _ = fs::remove_file(path);
    Ok(())
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
    let mut import_replay_ready_at: Option<tokio::time::Instant> = None;
    let mut insert_history_cell_count = 0usize;
    let mut insert_history_cell_baseline_at_attach: Option<usize> = None;
    let mut loop_tick = tokio::time::interval(Duration::from_millis(250));
    loop_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop_tick.tick().await;

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
                                .is_none_or(|instant| instant.elapsed() >= Duration::from_millis(500))
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
                _ = loop_tick.tick() => {
                    if session_log_path.is_file()
                        && let Ok(log) = fs::read_to_string(&session_log_path)
                    {
                        let now = tokio::time::Instant::now();
                        insert_history_cell_count =
                            count_occurrences(&log, "\"kind\":\"insert_history_cell\"");
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
                        if import_replay_ready_at.is_none()
                            && saw_import_history_log
                            && insert_history_cell_baseline_at_attach
                                .is_some_and(|baseline| {
                                    insert_history_cell_count
                                        >= baseline + limited_history.returned_count
                                })
                        {
                            import_replay_ready_at = Some(now);
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
                        .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));

                    if !sent_attach
                        && answered_cursor_query
                        && startup_nux_settled
                        && startup_seen_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        sent_attach = true;
                        insert_history_cell_baseline_at_attach = Some(
                            if session_log_path.is_file() {
                                fs::read_to_string(&session_log_path)
                                    .ok()
                                    .map(|log| {
                                        count_occurrences(&log, "\"kind\":\"insert_history_cell\"")
                                    })
                                    .unwrap_or(insert_history_cell_count)
                            } else {
                                insert_history_cell_count
                            },
                        );
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
                        && import_replay_ready_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(1))
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
        && import_proof.import_index < import_proof.user_turn_index
        && import_proof.user_turn_index < import_proof.completion_index)
    {
        anyhow::bail!(
            "oracle_history_import ordering was invalid: attach_index={} import_index={} user_turn_index={} completion_index={}\n{}",
            import_proof.attach_index,
            import_proof.import_index,
            import_proof.user_turn_index,
            import_proof.completion_index,
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
    let mut sent_attach = false;
    let mut loop_tick = tokio::time::interval(Duration::from_millis(250));
    loop_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop_tick.tick().await;
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
                                .is_none_or(|instant| instant.elapsed() >= Duration::from_millis(500))
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
                _ = loop_tick.tick() => {
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
                        .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));
                    if !answered_cursor_query
                        || !startup_nux_settled
                        || startup_seen_at.is_none_or(|instant| instant.elapsed() < Duration::from_secs(2))
                    {
                        continue;
                    }

                    match phase {
                        Phase::AwaitAttach => {
                            if attach_ready_at.is_none() && !sent_attach {
                                sent_attach = true;
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
    let mut loop_tick = tokio::time::interval(Duration::from_millis(250));
    loop_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop_tick.tick().await;

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
                                .is_none_or(|instant| instant.elapsed() >= Duration::from_millis(500))
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
                _ = loop_tick.tick() => {
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
                        .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));

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

    let matched = recent_oracle_meta_paths(&sessions_dir, test_started_at)?
        .iter()
        .any(|path| {
            oracle_session_matches_with_file(
                path,
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

#[tokio::test]
#[serial(oracle_live_browser)]
#[ignore = "live native Oracle PTY proof; requires ORACLE_NATIVE_THREAD_URL"]
async fn native_oracle_multi_file_upload_roundtrip() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let thread_url = match std::env::var("ORACLE_NATIVE_THREAD_URL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping live oracle multi-file upload: ORACLE_NATIVE_THREAD_URL is unset");
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
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_millis();
    let upload_token_a = format!("CODEX_UPLOAD_MULTI_A_{timestamp}");
    let upload_token_b = format!("CODEX_UPLOAD_MULTI_B_{timestamp}");
    let upload_dir = workspace.join("tmp");
    fs::create_dir_all(&upload_dir)?;
    let upload_file_a = tempfile::Builder::new()
        .prefix("oracle-live-upload-a-")
        .suffix(".txt")
        .tempfile_in(&upload_dir)?;
    let upload_file_b = tempfile::Builder::new()
        .prefix("oracle-live-upload-b-")
        .suffix(".txt")
        .tempfile_in(&upload_dir)?;
    fs::write(upload_file_a.path(), format!("{upload_token_a}\n"))?;
    fs::write(upload_file_b.path(), format!("{upload_token_b}\n"))?;
    let relative_upload_path_a = upload_file_a
        .path()
        .strip_prefix(&workspace)
        .context("upload proof file A escaped workspace")?
        .to_string_lossy()
        .to_string();
    let relative_upload_path_b = upload_file_b
        .path()
        .strip_prefix(&workspace)
        .context("upload proof file B escaped workspace")?
        .to_string_lossy()
        .to_string();
    let codex_home = tempfile::tempdir()?;
    let codex_home_path = codex_home.path().to_path_buf();
    let session_log_path = codex_home_path.join("native-oracle-upload-multi.jsonl");
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
    let mut loop_tick = tokio::time::interval(Duration::from_millis(250));
    loop_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop_tick.tick().await;

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
                                .is_none_or(|instant| instant.elapsed() >= Duration::from_millis(500))
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
                _ = loop_tick.tick() => {
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
                            send_interrupt_burst(
                                &interrupt_writer,
                                3,
                                Duration::from_millis(200),
                            )
                            .await;
                        }
                        if log.contains("\"kind\":\"oracle_run_completed\"") && !saw_completion_log {
                            saw_completion_log = true;
                            send_interrupt_burst(
                                &interrupt_writer,
                                3,
                                Duration::from_millis(200),
                            )
                            .await;
                        }
                    }

                    let startup_nux_settled = last_nux_seen_at
                        .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));

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
                                "Read both attached files and reply with exactly both full contents and nothing else.\n\nfile: \"{relative_upload_path_a}\"\nfile: \"{relative_upload_path_b}\""
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
                "timed out waiting for native oracle multi-file upload roundtrip\n{}",
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

    let matched = recent_oracle_meta_paths(&sessions_dir, test_started_at)?
        .iter()
        .any(|path| {
            oracle_session_matches_with_files(
                path,
                &thread_url,
                &conversation_id,
                &[upload_token_a.as_str(), upload_token_b.as_str()],
                &[upload_file_a.path(), upload_file_b.path()],
            )
        });
    if !matched {
        anyhow::bail!(
            "expected a matching Oracle multi-file upload session for {conversation_id} containing both tokens\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    Ok(())
}

#[tokio::test]
#[serial(oracle_live_browser)]
#[ignore = "live native Oracle PTY proof; requires ORACLE_NATIVE_THREAD_URL"]
async fn native_oracle_zip_upload_roundtrip() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let thread_url = match std::env::var("ORACLE_NATIVE_THREAD_URL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!("skipping live oracle zip upload: ORACLE_NATIVE_THREAD_URL is unset");
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
    let zip_ack_token = format!(
        "CODEX_ZIP_UPLOAD_ACK_{}",
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)?
            .as_millis()
    );
    let upload_dir = workspace.join("tmp");
    fs::create_dir_all(&upload_dir)?;
    let zip_file = tempfile::Builder::new()
        .prefix("oracle-live-upload-")
        .suffix(".zip")
        .tempfile_in(&upload_dir)?;
    write_single_entry_zip(
        zip_file.path(),
        "proof.txt",
        format!("ZIP_TOKEN_{zip_ack_token}\n").as_bytes(),
    )?;
    let relative_zip_path = zip_file
        .path()
        .strip_prefix(&workspace)
        .context("zip proof file escaped workspace")?
        .to_string_lossy()
        .to_string();
    let codex_home = tempfile::tempdir()?;
    let codex_home_path = codex_home.path().to_path_buf();
    let session_log_path = codex_home_path.join("native-oracle-upload-zip.jsonl");
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
    let mut loop_tick = tokio::time::interval(Duration::from_millis(250));
    loop_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop_tick.tick().await;

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
                                .is_none_or(|instant| instant.elapsed() >= Duration::from_millis(500))
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
                _ = loop_tick.tick() => {
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
                            send_interrupt_burst(
                                &interrupt_writer,
                                3,
                                Duration::from_millis(200),
                            )
                            .await;
                        }
                        if log.contains("\"kind\":\"oracle_run_completed\"") && !saw_completion_log {
                            saw_completion_log = true;
                            send_interrupt_burst(
                                &interrupt_writer,
                                3,
                                Duration::from_millis(200),
                            )
                            .await;
                        }
                    }

                    let startup_nux_settled = last_nux_seen_at
                        .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));

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
                                "Acknowledge the attached zip file by replying with exactly {zip_ack_token} and nothing else.\n\nfile: \"{relative_zip_path}\""
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
                "timed out waiting for native oracle zip upload roundtrip\n{}",
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

    let matched = recent_oracle_meta_paths(&sessions_dir, test_started_at)?
        .iter()
        .any(|path| {
            oracle_session_matches_with_file(
                path,
                &thread_url,
                &conversation_id,
                &zip_ack_token,
                zip_file.path(),
            )
        });
    if !matched {
        anyhow::bail!(
            "expected a matching Oracle zip upload session for {conversation_id} containing {zip_ack_token}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    Ok(())
}

#[tokio::test]
#[serial(oracle_live_browser)]
#[ignore = "live native Oracle PTY proof; requires ORACLE_SUPERVISOR_CHATGPT_URL"]
async fn native_oracle_assistant_download_roundtrip() -> Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let project_url = match std::env::var("ORACLE_SUPERVISOR_CHATGPT_URL") {
        Ok(value) => value,
        Err(_) => {
            eprintln!(
                "skipping live oracle assistant download: ORACLE_SUPERVISOR_CHATGPT_URL is unset"
            );
            return Ok(());
        }
    };
    let codex_repo_root = codex_utils_cargo_bin::repo_root()?;
    let smarty_root = find_smarty_root(&codex_repo_root)?;
    // Use a fresh project-scoped thread so long-lived thread state cannot hide download regressions.
    let (thread_url, conversation_id) =
        create_fresh_oracle_project_thread(&smarty_root, &project_url).await?;
    let launcher = smarty_root.join("bin").join("smarty-codex");
    let workspace = smarty_root.clone();
    let sessions_dir = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".oracle").join("sessions"))
        .context("HOME is unset; cannot inspect Oracle sessions")?;
    let test_started_at = SystemTime::now();
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_millis();
    let download_token = format!("CODEX_DOWNLOAD_CONTENT_{timestamp}");
    let expected_download_contents = format!("# Codex Download Proof\n\n{download_token}");
    let codex_home = tempfile::tempdir()?;
    let codex_home_path = codex_home.path().to_path_buf();
    let session_log_path = codex_home_path.join("native-oracle-download.jsonl");
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
    let mut model_command_sent_at: Option<tokio::time::Instant> = None;
    let mut thinking_ack_at: Option<tokio::time::Instant> = None;
    let mut sent_attach = false;
    let mut sent_prompt = false;
    let mut loop_tick = tokio::time::interval(Duration::from_millis(250));
    loop_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop_tick.tick().await;

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
                                .is_none_or(|instant| instant.elapsed() >= Duration::from_millis(500))
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
                _ = loop_tick.tick() => {
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
                            send_interrupt_burst(
                                &interrupt_writer,
                                3,
                                Duration::from_millis(200),
                            )
                            .await;
                        }
                        if log.contains("\"kind\":\"oracle_run_completed\"") && !saw_completion_log {
                            saw_completion_log = true;
                            send_interrupt_burst(
                                &interrupt_writer,
                                3,
                                Duration::from_millis(200),
                            )
                            .await;
                        }
                    }

                    let startup_nux_settled = last_nux_seen_at
                        .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));

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
                        && model_command_sent_at.is_none()
                        && attach_ready_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                    {
                        model_command_sent_at = Some(tokio::time::Instant::now());
                        submit_humanlike(&writer_tx, "/oracle model thinking").await;
                        continue;
                    }

                    if !sent_prompt
                        && model_command_sent_at
                            .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(2))
                        && (thinking_ack_at.is_some()
                            || model_command_sent_at
                                .is_some_and(|instant| instant.elapsed() >= Duration::from_secs(10)))
                    {
                        sent_prompt = true;
                        submit_humanlike_with_delays(
                            &writer_tx,
                            format!(
                                "Generate an example markdown file named codex-download-proof.md whose exact contents are:\n# Codex Download Proof\n\n{download_token}\n\nProvide it in your reply as a clickable markdown hyperlink to the file, with no extra explanation."
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
                "timed out waiting for native oracle assistant download roundtrip\n{}",
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

    let matched = recent_oracle_meta_paths(&sessions_dir, test_started_at)?
        .iter()
        .any(|path| {
            oracle_session_matches_with_download(
                path,
                &thread_url,
                &conversation_id,
                "codex-download-proof.md",
                &expected_download_contents,
            )
        });
    if !matched {
        anyhow::bail!(
            "expected a matching Oracle download session for {conversation_id} containing codex-download-proof.md and a saved file with {download_token}\n{}",
            failure_artifacts(&codex_home_path, &session_log_path, &output)
        );
    }

    Ok(())
}
