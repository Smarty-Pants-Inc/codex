use codex_core::NotSubmittedReason;
use codex_core::StartIfIdleSubmission;
use codex_core::StartThreadOptions;
use codex_core::TurnInput;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolResponse;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::turn_input::TurnStartGuard;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_function_call;
use core_test_support::responses::ev_response_created;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use test_case::test_case;
use tokio::time::timeout;

#[tokio::test]
async fn turn_bound_response_and_idle_guard_preserve_native_admission() -> anyhow::Result<()> {
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                ev_response_created("call"),
                ev_function_call("reused-call", "sense", "{}"),
                ev_completed("call"),
            ]),
            responses::sse(vec![
                ev_response_created("settled"),
                ev_completed("settled"),
            ]),
            responses::sse(vec![
                ev_response_created("automatic"),
                ev_function_call("accepted-work", "sense", "{}"),
                ev_completed("automatic"),
            ]),
            responses::sse(vec![
                ev_response_created("retired"),
                ev_completed("retired"),
            ]),
        ],
    )
    .await;
    let base = test_codex().build_with_auto_env(&server).await?;
    let thread = base
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: "sense".to_string(),
                description: "Synthetic native control boundary".to_string(),
                input_schema: json!({"type": "object", "properties": {}}),
                defer_loading: false,
            })],
            ..StartThreadOptions::new(base.config.clone())
        })
        .await?
        .thread;
    let guard = TurnStartGuard::default();
    let TurnInputSubmission::Started { turn_id } = thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Call sense".to_string(),
            text_elements: Vec::new(),
        }]))
        .await?
    else {
        anyhow::bail!("expected a new turn");
    };
    wait_for_event(&thread, |event| {
        matches!(event, EventMsg::DynamicToolCallRequest(_))
    })
    .await;

    // The guard was captured while idle, but admission happens in Core after
    // another turn became busy. A JS observation of idle is not authority.
    let automatic = TurnInputRequest::new(TurnInput::ResponseItem(responses::user_message_item(
        "guarded automatic input",
    )))
    .with_idle_start_guard(guard.clone());
    let settings = thread.thread_settings_snapshot().await;
    assert_eq!(
        thread.start_turn_if_idle(automatic.clone()).await?,
        StartIfIdleSubmission::NotSubmitted {
            reason: NotSubmittedReason::NotIdle
        },
    );
    for (response_turn, text) in [("stale-turn".to_string(), "stale"), (turn_id, "accepted")] {
        thread
            .submit(Op::DynamicToolResponseForTurn {
                turn_id: response_turn,
                id: "reused-call".to_string(),
                response: DynamicToolResponse {
                    content_items: vec![DynamicToolCallOutputContentItem::InputText {
                        text: text.to_string(),
                    }],
                    success: true,
                },
            })
            .await?;
    }
    wait_for_event(&thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    let output = mock.requests()[1].function_call_output("reused-call")["output"].clone();
    assert_eq!(
        serde_json::from_value::<FunctionCallOutputPayload>(output)?,
        FunctionCallOutputPayload::from_text("accepted".to_string()),
    );
    // TurnComplete is sent before Core releases its active-turn reservation.
    // Observe actual idle so the revoked admission cannot pass as merely busy.
    timeout(Duration::from_secs(/*secs*/ 10), async {
        while thread.has_active_turn().await {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("completed turn must release its reservation");
    assert!(!thread.has_active_turn().await);
    guard.revoke();
    assert_eq!(
        thread.start_turn_if_idle(automatic).await?,
        StartIfIdleSubmission::NotSubmitted {
            reason: NotSubmittedReason::NotIdle
        },
    );
    assert_eq!(thread.thread_settings_snapshot().await, settings);
    assert_eq!(mock.requests().len(), 2);

    let admitted = TurnStartGuard::default();
    let StartIfIdleSubmission::Started { turn_id } = thread
        .start_turn_if_idle(
            TurnInputRequest::new(TurnInput::ResponseItem(responses::user_message_item(
                "fresh automatic input",
            )))
            .with_idle_start_guard(admitted.clone()),
        )
        .await?
    else {
        anyhow::bail!("expected automatic admission");
    };
    wait_for_event(&thread, |event| {
        matches!(event, EventMsg::DynamicToolCallRequest(_))
    })
    .await;
    // A pending tool is a deterministic barrier: accepted work has not settled
    // when the owner revokes future admission. Its original turn must still join it.
    admitted.revoke();
    assert!(thread.has_active_turn().await);
    thread
        .submit(Op::DynamicToolResponseForTurn {
            turn_id,
            id: "accepted-work".to_string(),
            response: DynamicToolResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: "settled-after-revoke".to_string(),
                }],
                success: true,
            },
        })
        .await?;
    wait_for_event(&thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 4);
    assert_eq!(
        serde_json::from_value::<FunctionCallOutputPayload>(
            requests[3].function_call_output("accepted-work")["output"].clone(),
        )?,
        FunctionCallOutputPayload::from_text("settled-after-revoke".to_string()),
    );
    assert!(requests[2].body_contains_text("fresh automatic input"));
    assert!(!requests[2].body_contains_text("guarded automatic input"));
    Ok(())
}

#[derive(Clone, Copy)]
enum GuardedRoute {
    HumanIdleStart,
    StartOrSteer,
    Steer,
}

#[test_case(GuardedRoute::HumanIdleStart; "guard cannot become human idle input")]
#[test_case(GuardedRoute::StartOrSteer; "guard cannot bypass idle admission")]
#[test_case(GuardedRoute::Steer; "guard cannot become steering")]
#[tokio::test]
async fn guarded_input_cannot_bypass_automatic_admission(
    route: GuardedRoute,
) -> anyhow::Result<()> {
    let server = responses::start_mock_server().await;
    let test = test_codex().build_with_auto_env(&server).await?;
    let settings = test.codex.thread_settings_snapshot().await;
    let guard = TurnStartGuard::default();
    guard.revoke();
    let request = TurnInputRequest::user_input(vec![UserInput::Text {
        text: "must not run".to_string(),
        text_elements: Vec::new(),
    }])
    .with_idle_start_guard(guard);
    let error = match route {
        GuardedRoute::HumanIdleStart => test.codex.start_turn_if_idle(request).await.unwrap_err(),
        GuardedRoute::StartOrSteer => test.codex.start_or_steer_turn(request).await.unwrap_err(),
        GuardedRoute::Steer => test
            .codex
            .steer_turn(request, "stale-turn".to_string())
            .await
            .unwrap_err(),
    };
    assert!(
        error
            .to_string()
            .contains("automatic turn authority requires start-if-idle with automatic input")
    );
    assert_eq!(test.codex.thread_settings_snapshot().await, settings);
    assert!(!test.codex.has_active_turn().await);
    assert!(
        server
            .received_requests()
            .await
            .expect("request recording")
            .is_empty()
    );
    Ok(())
}
