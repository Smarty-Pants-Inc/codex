use super::*;
use codex_protocol::protocol::TurnAbortReason;

#[tokio::test]
async fn command_approval_abort_interrupts_without_resampling_or_execution() -> Result<()> {
    let outside = tempfile::tempdir_in(std::env::current_dir()?)?;
    let output = outside.path().join("must-not-exist");
    let command = format!("printf unexpected > {output:?}");
    let policy = AskForApproval::OnRequest;
    let Some((server, test)) = build_unified_exec_zsh_fork_test_or_skip(
        "unified-exec zsh-fork command cancellation test",
        policy,
        restrictive_workspace_write_profile(),
        |_home| {},
    )
    .await?
    else {
        return Ok(());
    };
    let mock = mount_sse_once(
        &server,
        sse(vec![
            ev_response_created("cancel-command"),
            exec_command_event(
                "cancel-command-call",
                &command,
                Some(30_000),
                SandboxPermissions::RequireEscalated,
                "exercise command cancellation",
            )?,
            ev_completed("cancel-command"),
        ]),
    )
    .await;
    submit_turn_with_session_permissions(
        &test,
        "request the command approval",
        policy,
        ApprovalsReviewer::User,
    )
    .await?;
    let approval = expect_exec_approval(&test, &command).await;
    assert_eq!(approval.approval_id, None);
    test.codex
        .submit(Op::ExecApproval {
            id: approval.effective_approval_id(),
            turn_id: Some(approval.turn_id.clone()),
            decision: ReviewDecision::Abort,
        })
        .await?;
    let event = wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_) | EventMsg::TurnComplete(_))
    })
    .await;
    let EventMsg::TurnAborted(aborted) = event else {
        panic!("command cancellation must interrupt the originating turn");
    };
    assert_eq!(
        (aborted.turn_id, aborted.reason),
        (Some(approval.turn_id), TurnAbortReason::Interrupted)
    );
    mock.single_request();
    let requests = server.received_requests().await.expect("recorded requests");
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST" && request.url.path().ends_with("/responses"))
            .count(),
        1,
        "cancellation must not issue even an unmatched follow-up request"
    );
    assert!(!output.exists(), "cancelled command must not execute");
    Ok(())
}
