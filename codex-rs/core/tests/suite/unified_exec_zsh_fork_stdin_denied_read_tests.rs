use super::*;
use pretty_assertions::assert_eq;

/// A persistent terminal launched outside the sandbox cannot enforce denied reads that the
/// current turn adds, so its stdin is rejected before any review instead of being approved.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn current_turn_denied_read_rejects_stdin_to_earlier_escalated_terminal() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let approval_policy = AskForApproval::OnRequest;
    let outside_dir = tempfile::tempdir_in(std::env::current_dir()?)?;
    let outside_path = outside_dir.path().join("must-not-exist.txt");
    let Some((server, test)) = build_unified_exec_zsh_fork_test_or_skip(
        "unified-exec zsh-fork current-turn denied-read stdin test",
        approval_policy,
        restrictive_workspace_write_profile(),
        |_home| {},
    )
    .await?
    else {
        return Ok(());
    };

    let open_call_id = "uexec-zsh-fork-denied-read-open";
    let open_command = "while IFS= read -r command; do eval \"$command\"; done";
    let open_args = json!({
        "cmd": open_command,
        "yield_time_ms": 250,
        "tty": true,
        "sandbox_permissions": SandboxPermissions::RequireEscalated,
        "justification": "start an interactive terminal for the next turn",
    });
    let write_call_id = "uexec-zsh-fork-denied-read-write";
    let write_args = json!({
        "chars": format!("touch {outside_path:?}\nexit\n"),
        "session_id": 1000,
        "yield_time_ms": 5_000,
    });
    let responses = mount_sse_sequence(
        &server,
        vec![
            sse(vec![
                ev_response_created("resp-denied-read-open"),
                ev_function_call(
                    open_call_id,
                    "exec_command",
                    &serde_json::to_string(&open_args)?,
                ),
                ev_completed("resp-denied-read-open"),
            ]),
            sse(vec![
                ev_response_created("resp-denied-read-first-done"),
                ev_assistant_message("msg-denied-read-first-done", "terminal is running"),
                ev_completed("resp-denied-read-first-done"),
            ]),
            sse(vec![
                ev_response_created("resp-denied-read-write"),
                ev_function_call(
                    write_call_id,
                    "write_stdin",
                    &serde_json::to_string(&write_args)?,
                ),
                ev_completed("resp-denied-read-write"),
            ]),
            sse(vec![
                ev_response_created("resp-denied-read-second-done"),
                ev_assistant_message("msg-denied-read-second-done", "done"),
                ev_completed("resp-denied-read-second-done"),
            ]),
        ],
    )
    .await;

    submit_turn_with_session_permissions(
        &test,
        "start a persistent terminal with user approvals",
        approval_policy,
        ApprovalsReviewer::User,
    )
    .await?;
    approve_expected_exec(&test, open_command).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    let next_cwd = test.config.cwd.join("next-turn");
    fs::create_dir(&next_cwd)?;
    let (sandbox_policy, permission_profile) = turn_permission_fields(
        denied_read_permission_profile(next_cwd.join("next-environment-private").as_path())?,
        next_cwd.as_path(),
    );
    test.codex
        .start_or_steer_turn(
            TurnInputRequest::user_input(vec![UserInput::Text {
                text: "write to the persistent terminal under the new denied read".into(),
                text_elements: Vec::new(),
            }])
            .with_thread_settings(ThreadSettingsOverrides {
                environments: Some(local_selections(next_cwd)),
                approvals_reviewer: Some(ApprovalsReviewer::AutoReview),
                sandbox_policy: Some(sandbox_policy),
                permission_profile,
                ..Default::default()
            }),
        )
        .await?;
    loop {
        let event = tokio::time::timeout(Duration::from_secs(30), test.codex.next_event())
            .await
            .context("timed out waiting for the denied-read turn to complete")??;
        match event.msg {
            EventMsg::GuardianAssessment(_) | EventMsg::ExecApprovalRequest(_) => {
                panic!("stdin to an unenforceable terminal must be rejected before review")
            }
            EventMsg::TurnComplete(_) => break,
            _ => {}
        }
    }

    let requests = responses.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[3].function_call_output_text(write_call_id),
        Some(
            "write_stdin rejected: this terminal cannot enforce the current denied-read restrictions; start a new terminal"
                .to_string()
        )
    );
    assert!(!outside_path.exists());

    Ok(())
}
