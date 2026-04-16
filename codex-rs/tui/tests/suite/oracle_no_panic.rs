use std::collections::HashMap;
use std::time::Duration;

use tokio::select;
use tokio::time::timeout;

async fn submit_humanlike(writer_tx: &tokio::sync::mpsc::Sender<Vec<u8>>, text: &str) {
    for byte in text.bytes() {
        let _ = writer_tx.send(vec![byte]).await;
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    tokio::time::sleep(Duration::from_millis(120)).await;
    let _ = writer_tx.send(b"\r".to_vec()).await;
}

#[tokio::test]
async fn oracle_info_command_does_not_panic_in_pty() -> anyhow::Result<()> {
    if cfg!(windows) {
        return Ok(());
    }

    let repo_root = codex_utils_cargo_bin::repo_root()?;
    let codex_home = tempfile::tempdir()?;
    let config_contents = format!(
        r#"
model_provider = "ollama"

[projects]
"{cwd}" = {{ trust_level = "trusted" }}
"#,
        cwd = repo_root.display()
    );
    std::fs::write(codex_home.path().join("config.toml"), config_contents)?;

    let codex_cli = codex_utils_cargo_bin::cargo_bin("codex")?;
    let session_log_path = codex_home.path().join("oracle-no-panic.jsonl");
    let args = vec![
        "--no-alt-screen".to_string(),
        "-C".to_string(),
        repo_root.display().to_string(),
        "-c".to_string(),
        "analytics.enabled=false".to_string(),
    ];
    let mut env = HashMap::new();
    env.insert(
        "CODEX_HOME".to_string(),
        codex_home.path().display().to_string(),
    );
    env.insert("CODEX_TUI_RECORD_SESSION".to_string(), "1".to_string());
    env.insert(
        "CODEX_TUI_SESSION_LOG_PATH".to_string(),
        session_log_path.display().to_string(),
    );

    let spawned = codex_utils_pty::spawn_pty_process(
        codex_cli.to_string_lossy().as_ref(),
        &args,
        &repo_root,
        &env,
        &None,
        codex_utils_pty::TerminalSize::default(),
    )
    .await?;

    let mut output = Vec::new();
    let codex_utils_pty::SpawnedProcess {
        session,
        stdout_rx,
        stderr_rx,
        exit_rx,
    } = spawned;
    let mut output_rx = codex_utils_pty::combine_output_receivers(stdout_rx, stderr_rx);
    let mut exit_rx = exit_rx;
    let writer_tx = session.writer_sender();
    let interrupt_writer = writer_tx.clone();
    let mut answered_cursor_query = false;
    let mut sent_command = false;
    let mut resent_command = false;
    let mut saw_oracle_info = false;
    let mut requested_exit = false;
    let mut last_nux_seen_at: Option<tokio::time::Instant> = None;
    let mut last_nux_ack_at: Option<tokio::time::Instant> = None;
    let startup = tokio::time::Instant::now();
    let mut command_sent_at: Option<tokio::time::Instant> = None;

    let exit_code_result = timeout(Duration::from_secs(30), async {
        loop {
            select! {
                result = output_rx.recv() => match result {
                    Ok(chunk) => {
                        let chunk_text = String::from_utf8_lossy(&chunk);
                        let saw_cursor_query = chunk.windows(4).any(|window| window == b"\x1b[6n");
                        if saw_cursor_query {
                            let _ = writer_tx.send(b"\x1b[1;1R".to_vec()).await;
                            answered_cursor_query = true;
                        }
                        output.extend_from_slice(&chunk);
                        let output_text = String::from_utf8_lossy(&output);

                        if chunk_text.contains("Use ↑/↓ to move, press enter to confirm") {
                            last_nux_seen_at = Some(tokio::time::Instant::now());
                            if last_nux_ack_at
                                .is_none_or(|instant| instant.elapsed() >= Duration::from_millis(500))
                            {
                                last_nux_ack_at = Some(tokio::time::Instant::now());
                                let _ = writer_tx.send(b"\r".to_vec()).await;
                            }
                        }

                        let startup_nux_settled = last_nux_seen_at
                            .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));

                        if answered_cursor_query
                            && !sent_command
                            && !saw_cursor_query
                            && startup_nux_settled
                        {
                            sent_command = true;
                            command_sent_at = Some(tokio::time::Instant::now());
                            submit_humanlike(&writer_tx, "/oracle info").await;
                        }

                        if !saw_oracle_info
                            && (output_text.contains("Requested model:")
                                || output_text.contains("Oracle mode:"))
                        {
                            saw_oracle_info = true;
                        }

                        if saw_oracle_info && !requested_exit {
                            requested_exit = true;
                            for _ in 0..3 {
                                let _ = interrupt_writer.send(vec![3]).await;
                                tokio::time::sleep(Duration::from_millis(200)).await;
                            }
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break exit_rx.await,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                },
                _ = tokio::time::sleep(Duration::from_millis(150)) => {
                    let startup_nux_settled = last_nux_seen_at
                        .is_none_or(|instant| instant.elapsed() >= Duration::from_secs(1));

                    if !sent_command
                        && startup.elapsed() >= Duration::from_secs(2)
                        && startup_nux_settled
                    {
                        sent_command = true;
                        command_sent_at = Some(tokio::time::Instant::now());
                        submit_humanlike(&writer_tx, "/oracle info").await;
                    }

                    if sent_command
                        && !saw_oracle_info
                        && !resent_command
                        && command_sent_at
                            .is_some_and(|sent_at| sent_at.elapsed() >= Duration::from_secs(4))
                    {
                        resent_command = true;
                        command_sent_at = Some(tokio::time::Instant::now());
                        submit_humanlike(&writer_tx, "/oracle info").await;
                    }

                    if sent_command
                        && !requested_exit
                        && command_sent_at
                            .is_some_and(|sent_at| sent_at.elapsed() >= Duration::from_secs(10))
                    {
                        requested_exit = true;
                        for _ in 0..3 {
                            let _ = interrupt_writer.send(vec![3]).await;
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                    }
                }
                result = &mut exit_rx => break result,
            }
        }
    })
    .await;

    let exit_code = match exit_code_result {
        Ok(Ok(code)) => code,
        Ok(Err(err)) => return Err(err.into()),
        Err(_) => {
            session.terminate();
            let output_text = String::from_utf8_lossy(&output);
            anyhow::bail!(
                "timed out waiting for codex oracle info PTY regression test: {output_text}"
            );
        }
    };

    let output_text = String::from_utf8_lossy(&output);
    let interrupt_only_output = {
        let trimmed_output = output_text.trim();
        !trimmed_output.is_empty()
            && trimmed_output
                .chars()
                .all(|character| character == '^' || character == 'C' || character.is_whitespace())
    };
    assert!(
        !output_text.contains("The application panicked (crashed)."),
        "unexpected panic output: {output_text}"
    );
    assert!(
        output_text.contains("/oracle"),
        "expected /oracle command input to appear in PTY transcript, got: {output_text}"
    );
    let session_log = std::fs::read_to_string(&session_log_path).unwrap_or_default();
    assert!(
        session_log.contains("ConfigureOracleMode"),
        "expected a real ConfigureOracleMode event, got session log: {session_log}\nPTY: {output_text}"
    );
    assert!(
        exit_code == 0 || exit_code == 130 || (exit_code == 1 && interrupt_only_output),
        "unexpected exit code {exit_code}; output: {output_text}"
    );
    Ok(())
}
