use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use app_test_support::create_final_assistant_message_sse_response;
use app_test_support::create_mock_responses_server_repeating_assistant;
use app_test_support::create_mock_responses_server_sequence;
use app_test_support::create_request_user_input_sse_response;
use codex_app_server_protocol::AskForApproval;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCError;
use codex_app_server_protocol::JSONRPCMessage;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ServerRequest;
use codex_app_server_protocol::SortDirection;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadGoalSetResponse;
use codex_app_server_protocol::ThreadHistoryMode;
use codex_app_server_protocol::ThreadItemsListParams;
use codex_app_server_protocol::ThreadItemsListResponse;
use codex_app_server_protocol::ThreadQueueAddParams;
use codex_app_server_protocol::ThreadQueueAddResponse;
use codex_app_server_protocol::ThreadQueueChangedNotification;
use codex_app_server_protocol::ThreadQueueDeleteParams;
use codex_app_server_protocol::ThreadQueueDeleteResponse;
use codex_app_server_protocol::ThreadQueueListParams;
use codex_app_server_protocol::ThreadQueueListResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadRevertParams;
use codex_app_server_protocol::ThreadRevertResponse;
use codex_app_server_protocol::ThreadRevertedNotification;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::ThreadTurnsListParams;
use codex_app_server_protocol::ThreadTurnsListResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStartedNotification;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_protocol::openai_models::ReasoningEffort;
use codex_protocol::protocol::EventMsg;
use codex_rollout::RolloutItem;
use codex_rollout::RolloutLine;
use codex_rollout::read_session_meta_line;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test]
async fn thread_revert_preserves_fork_cutoff_after_cold_resume() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    initialize_experimental(&mut mcp).await?;
    let ThreadStartResponse { thread: parent, .. } = mcp
        .start_thread(ThreadStartParams {
            history_mode: Some(ThreadHistoryMode::Paginated),
            ..Default::default()
        })
        .await?;
    let mut parent_turns = Vec::new();
    for text in ["parent first", "parent second"] {
        let completed = mcp
            .start_turn_and_wait_for_completion(TurnStartParams {
                thread_id: parent.id.clone(),
                input: vec![UserInput::Text {
                    text: text.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            })
            .await?;
        parent_turns.push(completed.turn.id);
    }
    let ThreadForkResponse { thread: child, .. } = mcp
        .request(|request_id| ClientRequest::ThreadFork {
            request_id,
            params: ThreadForkParams {
                thread_id: parent.id.clone(),
                ..Default::default()
            },
        })
        .await?;
    let child_meta = read_session_meta_line(child.path.as_ref().expect("child rollout"))
        .await?
        .meta;
    let fork_cutoff = child_meta
        .history_base
        .expect("fork history base")
        .end_ordinal_exclusive;
    assert_eq!(child_meta.forked_from_ordinal_exclusive, Some(fork_cutoff));
    let inherited_revert_cutoff =
        std::fs::read_to_string(parent.path.as_ref().expect("parent rollout"))?
            .lines()
            .map(serde_json::from_str::<RolloutLine>)
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .find_map(|line| match line.item {
                RolloutItem::EventMsg(EventMsg::TurnStarted(turn))
                    if turn.turn_id == parent_turns[1] =>
                {
                    line.ordinal
                }
                _ => None,
            })
            .expect("inherited turn start ordinal");
    let mut child_turns = Vec::new();
    for text in ["child first", "child second"] {
        let completed = mcp
            .start_turn_and_wait_for_completion(TurnStartParams {
                thread_id: child.id.clone(),
                input: vec![UserInput::Text {
                    text: text.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            })
            .await?;
        child_turns.push(completed.turn.id);
    }

    // First revert within the child, then revert into its inherited parent history.
    for (before_turn_id, expected_cutoff) in [
        (child_turns[1].clone(), fork_cutoff),
        (parent_turns[1].clone(), inherited_revert_cutoff),
    ] {
        let ThreadRevertResponse {
            thread: reverted, ..
        } = mcp
            .request(|request_id| ClientRequest::ThreadRevert {
                request_id,
                params: ThreadRevertParams {
                    thread_id: child.id.clone(),
                    before_turn_id,
                },
            })
            .await?;
        let meta = read_session_meta_line(reverted.path.as_ref().expect("reverted rollout"))
            .await?
            .meta;
        assert_eq!(meta.forked_from_ordinal_exclusive, Some(expected_cutoff));
        if expected_cutoff == fork_cutoff {
            assert!(
                meta.history_base
                    .expect("child revert base")
                    .end_ordinal_exclusive
                    > fork_cutoff
            );
        }

        mcp.shutdown_gracefully().await?;
        mcp = TestAppServer::builder()
            .with_codex_home(codex_home.path())
            .build()
            .await?;
        initialize_experimental(&mut mcp).await?;
        let _: ThreadResumeResponse = mcp
            .request(|request_id| ClientRequest::ThreadResume {
                request_id,
                params: ThreadResumeParams {
                    thread_id: child.id.clone(),
                    ..Default::default()
                },
            })
            .await?;
        mcp.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: child.id.clone(),
            input: vec![UserInput::Text {
                text: "continue after revert and cold resume".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
        let requests = server.received_requests().await.expect("response requests");
        let body = requests
            .iter()
            .rev()
            .find(|request| request.url.path().ends_with("/responses"))
            .expect("resumed model request")
            .body_json::<Value>()?;
        let metadata: Value = serde_json::from_str(
            body["client_metadata"]["x-codex-turn-metadata"]
                .as_str()
                .expect("turn metadata"),
        )?;
        assert_eq!(
            (
                metadata["forked_from_thread_id"].as_str(),
                metadata["forked_from_ordinal_exclusive"].as_u64()
            ),
            (Some(parent.id.as_str()), Some(expected_cutoff))
        );
    }
    Ok(())
}

#[tokio::test]
async fn thread_revert_replaces_paginated_history_before_turn() -> Result<()> {
    let server = create_mock_responses_server_repeating_assistant("Done").await;
    let codex_home = TempDir::new()?;
    MockResponsesConfig::new(&server.uri()).write(codex_home.path())?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    initialize_experimental(&mut mcp).await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            history_mode: Some(ThreadHistoryMode::Paginated),
            ..Default::default()
        })
        .await?;
    let stale_rollout_path = thread.path.clone().expect("thread rollout path");
    let mut turn_ids = Vec::new();
    for text in ["first", "second"] {
        let completed = mcp
            .start_turn_and_wait_for_completion(TurnStartParams {
                thread_id: thread.id.clone(),
                input: vec![UserInput::Text {
                    text: text.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            })
            .await?;
        turn_ids.push(completed.turn.id);
    }

    let ThreadRevertResponse {
        thread: reverted_thread,
        turns_backwards_cursor,
        items_backwards_cursor,
    } = mcp
        .request(|request_id| ClientRequest::ThreadRevert {
            request_id,
            params: ThreadRevertParams {
                thread_id: thread.id.clone(),
                before_turn_id: turn_ids[1].clone(),
            },
        })
        .await?;
    let reverted: ThreadRevertedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("thread/reverted"),
    )
    .await??;
    assert_eq!(reverted.thread_id, thread.id);

    assert_eq!(reverted_thread.id, thread.id);
    assert!(reverted_thread.turns.is_empty());
    assert!(items_backwards_cursor.is_some());
    assert_eq!(
        turn_ids_from_cursor(
            &mut mcp,
            &thread.id,
            turns_backwards_cursor,
            /*sort_direction*/ None,
        )
        .await?,
        turn_ids[..1]
    );
    let ThreadItemsListResponse {
        data: reverted_items,
        ..
    } = mcp
        .request(|request_id| ClientRequest::ThreadItemsList {
            request_id,
            params: ThreadItemsListParams {
                thread_id: thread.id.clone(),
                turn_id: None,
                cursor: items_backwards_cursor,
                limit: None,
                sort_direction: None,
            },
        })
        .await?;
    assert!(!reverted_items.is_empty());
    assert!(
        reverted_items
            .iter()
            .all(|item| item.turn_id == turn_ids[0])
    );

    mcp.shutdown_gracefully().await?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(codex_home.path())
        .build()
        .await?;
    initialize_experimental(&mut mcp).await?;
    let stale_resume_id = mcp
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: thread.id.clone(),
            path: Some(stale_rollout_path),
            ..Default::default()
        })
        .await?;
    let stale_resume_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(stale_resume_id)),
    )
    .await??;
    assert!(
        stale_resume_error.error.message.contains("stale path")
            && stale_resume_error
                .error
                .message
                .contains("omit path and resume by thread id"),
        "unexpected resume error: {}",
        stale_resume_error.error.message,
    );
    let resume_id = mcp
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: thread.id.clone(),
            ..Default::default()
        })
        .await?;
    let _: ThreadResumeResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(resume_id)).await??;
    let invalid_revert_id = mcp
        .send_raw_request(
            "thread/revert",
            Some(serde_json::to_value(ThreadRevertParams {
                thread_id: thread.id.clone(),
                before_turn_id: "missing-turn".to_string(),
            })?),
        )
        .await?;
    let invalid_revert_error: JSONRPCError = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_error_message(RequestId::Integer(invalid_revert_id)),
    )
    .await??;
    assert_eq!(
        invalid_revert_error.error.message,
        "turn not found: missing-turn"
    );

    let third_turn = mcp
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "third".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let requests = server.received_requests().await.expect("response requests");
    let model_input = requests
        .iter()
        .rev()
        .find(|request| request.url.path().ends_with("/responses"))
        .expect("third turn response request")
        .body_json::<serde_json::Value>()?["input"]
        .clone();
    let model_input = serde_json::to_string(&model_input)?;
    assert!(model_input.contains("first"));
    assert!(!model_input.contains("second"));
    assert!(model_input.contains("third"));
    assert_eq!(
        turn_ids_from_cursor(
            &mut mcp,
            &thread.id,
            /*cursor*/ None,
            Some(SortDirection::Asc),
        )
        .await?,
        vec![turn_ids[0].clone(), third_turn.turn.id]
    );
    Ok(())
}

#[tokio::test]
async fn thread_revert_interrupts_active_turn_keeps_thread_loaded_and_continues_goal() -> Result<()>
{
    let home = TempDir::new()?;
    let server = create_mock_responses_server_sequence(vec![
        create_final_assistant_message_sse_response("first")?,
        create_request_user_input_sse_response("call_blocked")?,
        create_request_user_input_sse_response("call_restored")?,
        create_final_assistant_message_sse_response("third")?,
    ])
    .await;
    MockResponsesConfig::new(&server.uri()).write(home.path())?;
    let config_path = home.path().join("config.toml");
    let config = std::fs::read_to_string(&config_path)?;
    std::fs::write(
        &config_path,
        config.replace("personality = true\n", "personality = true\ngoals = true\n"),
    )?;
    let mut mcp = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_managed_config()
        .with_goal_auto_continue()
        .build_initialized()
        .await?;

    let ThreadStartResponse { thread, .. } = mcp
        .start_thread(ThreadStartParams {
            history_mode: Some(ThreadHistoryMode::Paginated),
            ..Default::default()
        })
        .await?;
    let first_turn = mcp
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread.id.clone(),
            input: vec![UserInput::Text {
                text: "first".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let first_started: TurnStartedNotification =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_notification("turn/started")).await??;
    assert_eq!(first_started.thread_id, thread.id);
    assert_eq!(first_started.turn.id, first_turn.turn.id);

    let TurnStartResponse { turn: active_turn } = mcp
        .request(|request_id| ClientRequest::TurnStart {
            request_id,
            params: TurnStartParams {
                thread_id: thread.id.clone(),
                input: vec![UserInput::Text {
                    text: "sleep".to_string(),
                    text_elements: Vec::new(),
                }],
                collaboration_mode: Some(CollaborationMode {
                    mode: ModeKind::Plan,
                    settings: Settings {
                        model: "mock-model".to_string(),
                        reasoning_effort: Some(ReasoningEffort::Medium),
                        developer_instructions: None,
                    },
                }),
                approval_policy: Some(AskForApproval::Never),
                ..Default::default()
            },
        })
        .await?;
    let active_started: TurnStartedNotification =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_notification("turn/started")).await??;
    assert_eq!(active_started.thread_id, thread.id);
    assert_eq!(active_started.turn.id, active_turn.id);
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;

    let goal_request_id = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(json!({
                "threadId": thread.id,
                "objective": "continue after the active turn is reverted",
                "status": "active",
            })),
        )
        .await?;
    let _: ThreadGoalSetResponse =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_response(goal_request_id)).await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("thread/goal/updated"),
    )
    .await??;

    let queued: ThreadQueueAddResponse = mcp
        .request(|request_id| ClientRequest::ThreadQueueAdd {
            request_id,
            params: ThreadQueueAddParams {
                thread_id: thread.id.clone(),
                input: vec![UserInput::Text {
                    text: "queued after restore".to_string(),
                    text_elements: Vec::new(),
                }],
                client_user_message_id: "queued-after-restore".to_string(),
            },
        })
        .await?;
    let queued_change: ThreadQueueChangedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("thread/queue/changed"),
    )
    .await??;
    assert_eq!(queued_change.thread_id, thread.id);

    let ThreadRevertResponse {
        thread: reverted_thread,
        turns_backwards_cursor,
        items_backwards_cursor,
    } = mcp
        .request(|request_id| ClientRequest::ThreadRevert {
            request_id,
            params: ThreadRevertParams {
                thread_id: thread.id.clone(),
                before_turn_id: active_turn.id.clone(),
            },
        })
        .await?;
    let reverted: ThreadRevertedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("thread/reverted"),
    )
    .await??;
    assert_eq!(reverted.thread_id, thread.id);
    let completed: TurnCompletedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(completed.thread_id, thread.id);
    assert_eq!(completed.turn.status, TurnStatus::Interrupted);
    let continued: TurnStartedNotification =
        timeout(DEFAULT_READ_TIMEOUT, mcp.read_notification("turn/started")).await??;
    assert_eq!(continued.thread_id, thread.id);
    assert_eq!(continued.turn.status, TurnStatus::InProgress);

    let queued_after_restore: ThreadQueueListResponse = mcp
        .request(|request_id| ClientRequest::ThreadQueueList {
            request_id,
            params: ThreadQueueListParams {
                thread_id: thread.id.clone(),
                cursor: None,
                limit: None,
            },
        })
        .await?;
    assert_eq!(
        queued_after_restore.data,
        vec![queued.queued_submission.clone()]
    );
    let continued_request = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::ToolRequestUserInput {
        request_id: continued_request_id,
        params: continued_request_params,
    } = continued_request
    else {
        anyhow::bail!("restored goal turn did not request user input");
    };
    assert_eq!(continued_request_params.thread_id, thread.id);
    assert_eq!(continued_request_params.turn_id, continued.turn.id);
    assert_eq!(continued_request_params.item_id, "call_restored");
    let requests = server.received_requests().await.expect("response requests");
    let restored_input = requests
        .iter()
        .rev()
        .find(|request| request.url.path().ends_with("/responses"))
        .expect("restored goal model request")
        .body_json::<Value>()?["input"]
        .to_string();
    assert!(restored_input.contains("continue after the active turn is reverted"));
    assert!(!restored_input.contains("queued after restore"));

    let deleted: ThreadQueueDeleteResponse = mcp
        .request(|request_id| ClientRequest::ThreadQueueDelete {
            request_id,
            params: ThreadQueueDeleteParams {
                thread_id: thread.id.clone(),
                queued_submission_id: queued.queued_submission.id,
            },
        })
        .await?;
    assert!(deleted.deleted);
    let deleted_change: ThreadQueueChangedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("thread/queue/changed"),
    )
    .await??;
    assert_eq!(deleted_change.thread_id, thread.id);

    let complete_goal_request_id = mcp
        .send_raw_request(
            "thread/goal/set",
            Some(json!({
                "threadId": thread.id,
                "status": "complete",
            })),
        )
        .await?;
    let _: ThreadGoalSetResponse = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_response(complete_goal_request_id),
    )
    .await??;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("thread/goal/updated"),
    )
    .await??;
    mcp.send_response(
        continued_request_id,
        json!({
            "answers": {
                "confirm_path": { "answers": ["yes"] }
            }
        }),
    )
    .await?;
    let continued_completion: TurnCompletedNotification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_notification("turn/completed"),
    )
    .await??;
    assert_eq!(continued_completion.thread_id, thread.id);
    assert_eq!(continued_completion.turn.id, continued.turn.id);
    assert_eq!(continued_completion.turn.status, TurnStatus::Completed);
    assert!(reverted_thread.turns.is_empty());
    assert!(items_backwards_cursor.is_some());
    assert_eq!(
        turn_ids_from_cursor(
            &mut mcp,
            &thread.id,
            turns_backwards_cursor,
            /*sort_direction*/ None,
        )
        .await?,
        vec![first_turn.turn.id]
    );

    Ok(())
}

async fn turn_ids_from_cursor(
    mcp: &mut TestAppServer,
    thread_id: &str,
    cursor: Option<String>,
    sort_direction: Option<SortDirection>,
) -> Result<Vec<String>> {
    let ThreadTurnsListResponse { data, .. } = mcp
        .request(|request_id| ClientRequest::ThreadTurnsList {
            request_id,
            params: ThreadTurnsListParams {
                thread_id: thread_id.to_string(),
                cursor,
                limit: None,
                sort_direction,
                items_view: None,
            },
        })
        .await?;
    Ok(data.into_iter().map(|turn| turn.id).collect())
}

async fn initialize_experimental(mcp: &mut TestAppServer) -> Result<()> {
    let initialized = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.initialize_with_capabilities(
            ClientInfo {
                name: "test-client".to_string(),
                title: None,
                version: "0.1.0".to_string(),
            },
            Some(InitializeCapabilities {
                experimental_api: true,
                request_attestation: false,
                opt_out_notification_methods: None,
                mcp_server_openai_form_elicitation: false,
                extensions: None,
                goal_auto_continue: false,
            }),
        ),
    )
    .await??;
    assert!(matches!(initialized, JSONRPCMessage::Response(_)));
    Ok(())
}
