use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ThreadForkParams;
use codex_app_server_protocol::ThreadForkResponse;
use codex_app_server_protocol::ThreadGoalSetResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadSettingsUpdateParams;
use codex_app_server_protocol::ThreadSettingsUpdateResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnInterruptResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::TurnStartResponse;
use codex_app_server_protocol::TurnStatus;
use codex_app_server_protocol::UserInput;
use codex_features::Feature;
use codex_protocol::ThreadId;
use codex_protocol::config_types::CollaborationMode;
use codex_protocol::config_types::ModeKind;
use codex_protocol::config_types::Settings;
use codex_utils_absolute_path::test_support::PathExt;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

const TIMEOUT: Duration = Duration::from_secs(60);

fn toggle_response(enabled: bool) -> String {
    responses::sse(vec![
        responses::ev_function_call(
            if enabled { "enable" } else { "disable" },
            "keep_working",
            &json!({"enabled": enabled}).to_string(),
        ),
        responses::ev_completed("toggle-response"),
    ])
}

async fn start(
    config: MockResponsesConfig,
    home: &TempDir,
) -> Result<(TestAppServer, String, Arc<codex_state::StateRuntime>)> {
    config.with_model("gpt-5.2-codex").write(home.path())?;
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_managed_config()
        .build_initialized()
        .await?;
    let id = app
        .send_thread_start_request_with_auto_env(ThreadStartParams::default())
        .await?;
    let response: ThreadStartResponse = timeout(TIMEOUT, app.read_response(id)).await??;
    let state = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        "mock_provider".to_string(),
    )
    .await?;
    Ok((app, response.thread.id, state))
}

fn turn(thread_id: &str, mode: ModeKind) -> TurnStartParams {
    TurnStartParams {
        thread_id: thread_id.to_string(),
        collaboration_mode: Some(CollaborationMode {
            mode,
            settings: Settings {
                model: "gpt-5.2-codex".to_string(),
                reasoning_effort: None,
                developer_instructions: None,
            },
        }),
        input: vec![UserInput::Text {
            text: "Finish the authorized work".to_string(),
            text_elements: Vec::new(),
        }],
        ..Default::default()
    }
}

#[tokio::test]
async fn keep_working_completed_admits_one_continuation_and_off_keeps_final_explanation()
-> Result<()> {
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            toggle_response(/*enabled*/ true),
            responses::sse(vec![
                responses::ev_assistant_message("first", "More work remains"),
                responses::ev_completed("first"),
            ]),
            toggle_response(/*enabled*/ false),
            responses::sse(vec![
                responses::ev_assistant_message("final", "All done"),
                responses::ev_completed("final"),
            ]),
        ],
    )
    .await;
    let home = TempDir::new()?;
    let (mut app, thread_id, state) = start(
        MockResponsesConfig::new(&server.uri()).disable_feature(Feature::Goals),
        &home,
    )
    .await?;
    let first = timeout(
        TIMEOUT,
        app.start_turn_and_wait_for_completion(turn(&thread_id, ModeKind::Default)),
    )
    .await??;
    assert_eq!(first.turn.status, TurnStatus::Completed);
    let notification = timeout(
        TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let second: TurnCompletedNotification =
        serde_json::from_value(notification.params.expect("completion params"))?;
    assert_eq!(second.turn.status, TurnStatus::Completed);
    assert_ne!(first.turn.id, second.turn.id);
    assert!(
        !state
            .thread_goals()
            .keep_working_enabled(ThreadId::from_string(&thread_id)?)
            .await?
    );
    timeout(TIMEOUT, app.shutdown_gracefully()).await??;
    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        requests[1].function_call_output("enable")["output"],
        "{\"enabled\":true}"
    );
    let continuation = requests[2].message_input_texts("user");
    let fragments = continuation
        .iter()
        .filter(|text| text.contains("source=\"keep_working\""))
        .collect::<Vec<_>>();
    assert_eq!(fragments.len(), 1);
    assert!(fragments[0].len() < 512);
    assert_eq!(
        requests[2].body_json()["client_metadata"]["turn_id"],
        second.turn.id
    );
    assert_eq!(
        state
            .thread_goals()
            .get_thread_goal(ThreadId::from_string(&thread_id)?)
            .await?,
        None
    );
    // The second completion includes the response generated after OFF, not an abort.
    assert!(serde_json::to_string(&second.turn.items)?.contains("All done"));
    Ok(())
}

#[tokio::test]
async fn keep_working_plan_rejection_is_not_replayed_on_cold_resume_or_fork() -> Result<()> {
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            toggle_response(/*enabled*/ true),
            responses::sse(vec![responses::ev_completed("settled")]),
        ],
    )
    .await;
    let home = TempDir::new()?;
    let (mut app, thread_id, state) = start(MockResponsesConfig::new(&server.uri()), &home).await?;
    let params = turn(&thread_id, ModeKind::Plan);
    timeout(TIMEOUT, app.start_turn_and_wait_for_completion(params)).await??;
    timeout(TIMEOUT, app.shutdown_gracefully()).await??;
    assert!(
        state
            .thread_goals()
            .keep_working_enabled(ThreadId::from_string(&thread_id)?)
            .await?
    );
    let mut app = TestAppServer::builder()
        .with_codex_home(home.path())
        .without_managed_config()
        .build_initialized()
        .await?;
    let id = app
        .send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.clone(),
            ..Default::default()
        })
        .await?;
    let _: ThreadResumeResponse = timeout(TIMEOUT, app.read_response(id)).await??;
    let id = app
        .send_thread_fork_request(ThreadForkParams {
            thread_id: thread_id.clone(),
            ..Default::default()
        })
        .await?;
    let fork: ThreadForkResponse = timeout(TIMEOUT, app.read_response(id)).await??;
    assert!(
        !state
            .thread_goals()
            .keep_working_enabled(ThreadId::from_string(&fork.thread.id)?)
            .await?
    );
    timeout(TIMEOUT, app.shutdown_gracefully()).await??;
    assert_eq!(mock.requests().len(), 2);
    Ok(())
}

#[tokio::test]
async fn keep_working_idle_interrupt_persists_off_and_explicit_legacy_resume_works() -> Result<()> {
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            toggle_response(/*enabled*/ true),
            responses::sse_completed("settled-in-plan"),
            responses::sse(vec![
                responses::ev_function_call(
                    "complete-goal",
                    "update_goal",
                    r#"{"status":"complete"}"#,
                ),
                responses::ev_completed("goal-tool"),
            ]),
            responses::sse_completed("legacy-complete"),
            responses::sse_completed("explicit-user-work"),
        ],
    )
    .await;
    let home = TempDir::new()?;
    let (mut app, thread_id, state) = start(
        MockResponsesConfig::new(&server.uri()).enable_feature(Feature::Goals),
        &home,
    )
    .await?;
    timeout(
        TIMEOUT,
        app.start_turn_and_wait_for_completion(turn(&thread_id, ModeKind::Plan)),
    )
    .await??;
    let native_id = ThreadId::from_string(&thread_id)?;
    assert!(state.thread_goals().keep_working_enabled(native_id).await?);
    app.clear_message_buffer();
    // The existing empty-turn interrupt is an idle/startup stop, not a new RPC.
    let id = app
        .send_turn_interrupt_request(TurnInterruptParams {
            thread_id: thread_id.clone(),
            turn_id: String::new(),
        })
        .await?;
    let _: TurnInterruptResponse = timeout(TIMEOUT, app.read_response(id)).await??;
    let id = app
        .send_thread_settings_update_request(ThreadSettingsUpdateParams {
            thread_id: thread_id.clone(),
            collaboration_mode: turn(&thread_id, ModeKind::Default).collaboration_mode,
            ..Default::default()
        })
        .await?;
    let _: ThreadSettingsUpdateResponse = timeout(TIMEOUT, app.read_response(id)).await??;
    // The notification (not the RPC's submission acknowledgement) fences the
    // native loop after Interrupt, without starting another turn or clearing stop.
    timeout(
        TIMEOUT,
        app.read_stream_until_notification_message("thread/settings/updated"),
    )
    .await??;
    assert!(!state.thread_goals().keep_working_enabled(native_id).await?);

    let id = app.send_raw_request("thread/goal/set", Some(json!({
        "threadId": thread_id, "objective": "Complete the legacy regression fixture", "status": "active",
    }))).await?;
    let _: ThreadGoalSetResponse = timeout(TIMEOUT, app.read_response(id)).await??;
    let notification = timeout(
        TIMEOUT,
        app.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed: TurnCompletedNotification =
        serde_json::from_value(notification.params.expect("completion params"))?;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    assert_eq!(
        state
            .thread_goals()
            .get_thread_goal(native_id)
            .await?
            .expect("legacy goal")
            .status,
        codex_state::ThreadGoalStatus::Complete
    );
    let explicit = timeout(
        TIMEOUT,
        app.start_turn_and_wait_for_completion(turn(&thread_id, ModeKind::Default)),
    )
    .await??;
    assert_eq!(explicit.turn.status, TurnStatus::Completed);
    assert!(!state.thread_goals().keep_working_enabled(native_id).await?);
    timeout(TIMEOUT, app.shutdown_gracefully()).await??;
    assert_eq!(mock.requests().len(), 5);
    Ok(())
}

#[tokio::test]
async fn keep_working_interrupts_active_work_without_a_continuation() -> Result<()> {
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(&server, vec![
        toggle_response(/*enabled*/ true),
        responses::sse(vec![
            responses::ev_function_call("wait", "request_user_input", &json!({"questions": [{
                "id": "choice", "header": "Choose", "question": "Which approach?",
                "options": [{"label": "First", "description": "First approach"}, {"label": "Second", "description": "Second approach"}]
            }]}).to_string()),
            responses::ev_completed("waiting"),
        ]),
    ]).await;
    let home = TempDir::new()?;
    let (mut app, thread_id, state) = start(MockResponsesConfig::new(&server.uri()), &home).await?;
    let id = app
        .send_turn_start_request(turn(&thread_id, ModeKind::Plan))
        .await?;
    let response: TurnStartResponse = timeout(TIMEOUT, app.read_response(id)).await??;
    timeout(TIMEOUT, app.read_stream_until_request_message()).await??;
    assert!(
        state
            .thread_goals()
            .keep_working_enabled(ThreadId::from_string(&thread_id)?)
            .await?
    );
    app.interrupt_turn_and_wait_for_aborted(thread_id.clone(), response.turn.id, TIMEOUT)
        .await?;
    assert!(
        !state
            .thread_goals()
            .keep_working_enabled(ThreadId::from_string(&thread_id)?)
            .await?
    );
    timeout(TIMEOUT, app.shutdown_gracefully()).await??;
    assert_eq!(mock.requests().len(), 2);
    Ok(())
}

#[tokio::test]
async fn keep_working_terminal_model_and_compaction_errors_disable_continuation() -> Result<()> {
    for total_tokens in [100, 500_000] {
        let server = responses::start_mock_server().await;
        let mock = responses::mount_sse_sequence(
            &server,
            vec![
                responses::sse(vec![
                    responses::ev_function_call("enable", "keep_working", r#"{"enabled":true}"#),
                    responses::ev_completed_with_tokens("enabled", total_tokens),
                ]),
                responses::sse_failed("failed", "insufficient_quota", "quota exhausted"),
            ],
        )
        .await;
        let home = TempDir::new()?;
        let config = MockResponsesConfig::new(&server.uri())
            .with_root_config("model_auto_compact_token_limit = 1000\ncompact_prompt = \"Summarize the conversation.\"");
        let (mut app, thread_id, state) = start(config, &home).await?;
        let completed = timeout(
            TIMEOUT,
            app.start_turn_and_wait_for_completion(turn(&thread_id, ModeKind::Default)),
        )
        .await??;
        assert_eq!(completed.turn.status, TurnStatus::Failed);
        assert!(
            !state
                .thread_goals()
                .keep_working_enabled(ThreadId::from_string(&thread_id)?)
                .await?
        );
        timeout(TIMEOUT, app.shutdown_gracefully()).await??;
        assert_eq!(mock.requests().len(), 2);
        let texts = mock.requests()[1].message_input_texts("user");
        assert_eq!(
            texts
                .iter()
                .any(|text| text.contains("Summarize the conversation.")),
            total_tokens == 500_000
        );
    }
    Ok(())
}
