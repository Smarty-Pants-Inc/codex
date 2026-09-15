//! Exercise the real sampler with the installed goal backend, as in keep_working_tests.
use codex_core::TurnInputRequest;
use codex_features::Feature;
use codex_goal_extension::GoalObjectiveUpdate;
use codex_goal_extension::GoalSetRequest;
use codex_goal_extension::GoalTokenBudgetUpdate;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::protocol::ThreadGoalStatus;
use codex_protocol::user_input::UserInput;
use codex_utils_absolute_path::test_support::PathExt;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tempfile::TempDir;
use test_case::test_case;

#[test_case(false, true; "active_recovers")]
#[test_case(false, false; "active_exhausts")]
#[test_case(true, true; "paused_recovers")]
#[test_case(true, false; "paused_exhausts")]
#[tokio::test(flavor = "current_thread")]
async fn capacity_retry_preserves_goal_status_until_real_settlement(
    pause: bool,
    recover: bool,
) -> anyhow::Result<()> {
    let server = responses::start_mock_server().await;
    let capacity = responses::sse_failed("capacity", "slow_down", "capacity");
    let mut replies = vec![
        responses::sse(vec![
            responses::ev_function_call(
                "goal",
                "create_goal",
                r#"{"objective":"finish capacity fixture"}"#,
            ),
            responses::ev_completed("initial"),
        ]),
        capacity.clone(),
    ];
    if recover {
        let mut events = Vec::new();
        if !pause {
            events.push(responses::ev_function_call(
                "complete-goal",
                "update_goal",
                r#"{"status":"complete"}"#,
            ));
        }
        events.push(responses::ev_completed("recovered"));
        replies.push(responses::sse(events));
        if !pause {
            replies.push(responses::sse_completed("done"));
        }
    } else {
        replies.push(capacity);
    }
    let mock = responses::mount_sse_sequence(&server, replies).await;
    let home = Arc::new(TempDir::new()?);
    let state = codex_state::StateRuntime::init(
        codex_state::SqliteConfig::new_for_testing(home.path().abs()),
        "openai".to_string(),
    )
    .await?;
    let goals = Arc::new(codex_goal_extension::GoalService::new());
    let test = test_codex()
        .with_home(Arc::clone(&home))
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
            config.features.disable(Feature::CodeMode);
            config.features.disable(Feature::CodeModeOnly);
        })
        .with_extensions_factory({
            let state = Arc::clone(&state);
            let goals = Arc::clone(&goals);
            move |manager| {
                let mut registry = codex_extension_api::ExtensionRegistryBuilder::new();
                codex_goal_extension::install_with_backend(
                    &mut registry,
                    Arc::clone(&state),
                    codex_analytics::AnalyticsEventsClient::disabled(),
                    /*metrics_client*/ None,
                    manager,
                    Arc::clone(&goals),
                    |_config: &codex_core::config::Config| {
                        codex_goal_extension::GoalExtensionConfig {
                            enabled: true,
                            max_goal_token_budget: None,
                        }
                    },
                );
                Arc::new(registry.build())
            }
        })
        .build_with_auto_env(&server)
        .await?;
    let id = test.session_configured.thread_id;
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "finish capacity fixture".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    let (mut starts, mut retries, mut errors) = (0, 0, 0);
    let mut goal_id = None;
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::TurnStarted(_) => starts += 1,
            EventMsg::StreamError(error) => {
                retries += 1;
                assert_eq!(
                    error.codex_error_info,
                    Some(CodexErrorInfo::ServerOverloaded)
                );
                let goal = state
                    .thread_goals()
                    .get_thread_goal(id)
                    .await?
                    .expect("persisted goal");
                assert_eq!(goal.status, codex_state::ThreadGoalStatus::Active);
                goal_id = Some(goal.goal_id);
                if pause {
                    let outcome = goals
                        .set_thread_goal(
                            &state,
                            GoalSetRequest {
                                thread_id: id,
                                objective: GoalObjectiveUpdate::Keep,
                                status: Some(ThreadGoalStatus::Paused),
                                token_budget: GoalTokenBudgetUpdate::Keep,
                                max_goal_token_budget: None,
                            },
                        )
                        .await?;
                    outcome.apply_runtime_effects(&goals).await;
                }
            }
            EventMsg::Error(error) => {
                errors += 1;
                assert_eq!(
                    error.codex_error_info,
                    Some(CodexErrorInfo::ServerOverloaded)
                );
            }
            EventMsg::TurnAborted(event) => panic!("Pause must not abort sampling: {event:?}"),
            EventMsg::TurnComplete(event) => {
                assert_eq!(
                    event.error.and_then(|error| error.codex_error_info),
                    if recover {
                        None
                    } else {
                        Some(CodexErrorInfo::ServerOverloaded)
                    }
                );
                break;
            }
            _ => {}
        }
    }
    assert_eq!((starts, retries, errors), (1, 1, usize::from(!recover)));
    let goal = state
        .thread_goals()
        .get_thread_goal(id)
        .await?
        .expect("same persisted goal");
    assert_eq!(Some(goal.goal_id), goal_id);
    assert_eq!(
        goal.status,
        if pause {
            codex_state::ThreadGoalStatus::Paused
        } else if recover {
            codex_state::ThreadGoalStatus::Complete
        } else {
            codex_state::ThreadGoalStatus::Blocked
        }
    );
    // Complete, Paused and Blocked goals cannot continue. Keep the native-loop
    // shutdown barrier and reject any successor, without enabling keep_working.
    test.codex.submit(Op::Shutdown).await?;
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::TurnStarted(event) => panic!("unexpected successor: {event:?}"),
            EventMsg::ShutdownComplete => break,
            _ => {}
        }
    }
    let requests = mock.requests();
    assert_eq!(requests.len(), if recover && !pause { 4 } else { 3 });
    for request in &requests[1..] {
        assert_eq!(
            request.body_json()["client_metadata"]["turn_id"],
            requests[0].body_json()["client_metadata"]["turn_id"]
        );
        assert_eq!(
            request.function_call_output("goal"),
            requests[1].function_call_output("goal")
        );
    }
    Ok(())
}
