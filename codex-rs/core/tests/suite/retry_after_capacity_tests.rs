use super::RetryTelemetryCapture;
use super::RetryTelemetryEvent;
use super::submit_user_input;
use super::wait_for_retry;
use anyhow::Result;
use codex_features::Feature;
use codex_protocol::protocol::CodexErrorInfo;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::turn_input::TurnInputRequest;
use codex_protocol::turn_input::TurnInputSubmission;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::time::Duration;
use test_case::test_case;
use tokio::sync::mpsc;
use wiremock::ResponseTemplate;

#[test_case("server_is_overloaded", true; "overloaded_recovers")]
#[test_case("slow_down", true; "slow_down_recovers")]
#[test_case("slow_down", false; "exhaustion_keeps_steering_unadmitted")]
#[tokio::test(flavor = "current_thread")]
async fn streamed_capacity_preserves_tool_result_and_queued_steering(
    code: &str,
    recover: bool,
) -> Result<()> {
    let mut telemetry = RetryTelemetryCapture::install();
    let server = responses::start_mock_server().await;
    let args = json!({"plan": [{"step": "retain real result", "status": "completed"}]}).to_string();
    let failure = responses::sse(vec![
        responses::ev_response_created("tool-before-capacity"),
        responses::ev_function_call("committed-plan", "update_plan", &args),
    ]) + &responses::sse_failed("tool-before-capacity", code, "capacity");
    let mut replies = vec![failure];
    replies.extend(if recover {
        vec![
            responses::sse_completed("recovered"),
            responses::sse_completed("steering-admitted"),
        ]
    } else {
        vec![responses::sse_failed("exhausted", code, "capacity")]
    });
    let mock = responses::mount_sse_sequence(&server, replies).await;
    let test = test_codex()
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
            config.update_plan_enabled = true;
            config.features.disable(Feature::CodeMode);
            config.features.disable(Feature::CodeModeOnly);
        })
        .build_with_auto_env(&server)
        .await?;
    submit_user_input(&test, "finish the plan").await?;
    let retry = telemetry.next_retry().await;
    assert_eq!(
        retry,
        RetryTelemetryEvent {
            attempt: 1,
            delay: retry.delay,
            layer: "stream".into(),
            operation: "sampling".into(),
        }
    );
    let steered = test
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "CAPACITY_QUEUED_STEERING".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    assert!(matches!(steered, TurnInputSubmission::Steered { .. }));
    let (mut plans, mut retries, mut starts, mut errors) = (0, 0, 0, 0);
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::PlanUpdate(_) => plans += 1,
            EventMsg::StreamError(error) => {
                assert_eq!(
                    error.codex_error_info,
                    Some(CodexErrorInfo::ServerOverloaded)
                );
                retries += 1;
            }
            EventMsg::TurnStarted(_) => starts += 1,
            EventMsg::Error(error) => {
                assert_eq!(
                    error.codex_error_info,
                    Some(CodexErrorInfo::ServerOverloaded)
                );
                errors += 1;
            }
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
    assert_eq!(
        (plans, retries, starts, errors),
        (1, 1, 1, usize::from(!recover))
    );
    let requests = mock.requests();
    assert_eq!(requests.len(), if recover { 3 } else { 2 });
    assert!(!requests[1].body_contains_text("CAPACITY_QUEUED_STEERING"));
    if recover {
        assert_eq!(
            requests[2]
                .message_input_texts("user")
                .iter()
                .filter(|text| text.contains("CAPACITY_QUEUED_STEERING"))
                .count(),
            1
        );
    }
    let tool_items = requests[1]
        .input()
        .into_iter()
        .filter(|item| item["call_id"] == "committed-plan")
        .collect::<Vec<_>>();
    assert_eq!(tool_items.len(), 2);
    assert_eq!(tool_items[0]["arguments"], args);
    assert_eq!(
        requests[1]
            .function_call_output_text("committed-plan")
            .as_deref(),
        Some("Plan updated")
    );
    for request in &requests[1..] {
        assert_eq!(
            request
                .input()
                .into_iter()
                .filter(|item| item["call_id"] == "committed-plan")
                .collect::<Vec<_>>(),
            tool_items
        );
        assert_eq!(
            request.body_json()["model"],
            requests[0].body_json()["model"]
        );
        assert_eq!(
            request.body_json()["client_metadata"]["turn_id"],
            requests[0].body_json()["client_metadata"]["turn_id"]
        );
    }
    assert_eq!(
        telemetry.events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn interrupt_cancels_capacity_backoff_without_another_sample() -> Result<()> {
    let mut telemetry = RetryTelemetryCapture::install();
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse_failed("capacity", "slow_down", "capacity"),
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.model_provider.stream_max_retries = Some(2);
        })
        .build_with_auto_env(&server)
        .await?;
    submit_user_input(&test, "interrupt held retry").await?;
    let retry = telemetry.next_retry().await;
    tokio::time::pause();
    let interrupted = tokio::time::Instant::now();
    test.codex.submit(Op::Interrupt).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::TurnAborted(_))
    })
    .await;
    assert!(
        interrupted.elapsed() < retry.delay,
        "abort must not wait out backoff"
    );
    tokio::time::advance(retry.delay + Duration::from_secs(1)).await;
    test.codex.submit(Op::Shutdown).await?;
    wait_for_event(&test.codex, |event| {
        matches!(event, EventMsg::ShutdownComplete)
    })
    .await;
    tokio::time::resume();
    assert_eq!(mock.requests().len(), 1);
    assert_eq!(
        telemetry.resumptions.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    );
    assert_eq!(
        telemetry.events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    );
    Ok(())
}

#[test_case(0, false; "http_zero")]
#[test_case(2, false; "http_exhaustion")]
#[test_case(0, true; "stream_then_http_zero")]
#[test_case(2, true; "stream_then_http_exhaustion")]
#[tokio::test(flavor = "current_thread")]
async fn http_capacity_never_renews_sampling_allowance(
    request_retries: u64,
    stream_first: bool,
) -> Result<()> {
    let mut telemetry = RetryTelemetryCapture::install();
    let server = responses::start_mock_server().await;
    let mut replies = Vec::new();
    if stream_first {
        replies.push(responses::sse_response(responses::sse_failed(
            "stream",
            "slow_down",
            "capacity",
        )));
    }
    replies.extend((0..=request_retries).map(|_| {
        ResponseTemplate::new(503).set_body_json(json!({"error": {"code": "server_is_overloaded"}}))
    }));
    let mock = responses::mount_response_sequence(&server, replies).await;
    let test = test_codex()
        .with_config(move |config| {
            config.model_provider.request_max_retries = Some(request_retries);
            config.model_provider.stream_max_retries = Some(2);
        })
        .build_with_auto_env(&server)
        .await?;
    submit_user_input(&test, "one retry owner per failure").await?;
    let (mut errors, mut retries) = (0, 0);
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::Error(error) => {
                assert_eq!(
                    error.codex_error_info,
                    Some(CodexErrorInfo::ServerOverloaded)
                );
                errors += 1;
            }
            EventMsg::StreamError(_) => retries += 1,
            EventMsg::TurnComplete(event) => {
                assert_eq!(
                    event.error.and_then(|error| error.codex_error_info),
                    Some(CodexErrorInfo::ServerOverloaded)
                );
                break;
            }
            _ => {}
        }
    }
    assert_eq!((errors, retries), (1, usize::from(stream_first)));
    assert_eq!(
        mock.requests().len(),
        usize::from(stream_first) + request_retries as usize + 1
    );
    if stream_first {
        let retry = telemetry.next_retry().await;
        assert_eq!(
            (
                retry.attempt,
                retry.layer.as_str(),
                retry.operation.as_str()
            ),
            (1, "stream", "sampling")
        );
    }
    for attempt in 1..=request_retries {
        let retry = telemetry.next_retry().await;
        assert_eq!(
            (
                retry.attempt,
                retry.layer.as_str(),
                retry.operation.as_str()
            ),
            (attempt, "http", "request")
        );
    }
    assert_eq!(
        telemetry.events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    );
    Ok(())
}

#[test_case("insufficient_quota", CodexErrorInfo::UsageLimitExceeded; "quota")]
#[test_case("usage_not_included", CodexErrorInfo::UsageLimitExceeded; "billing")]
#[test_case("cyber_policy", CodexErrorInfo::CyberPolicy; "policy")]
#[tokio::test(flavor = "current_thread")]
async fn noncapacity_stream_errors_remain_terminal(
    code: &str,
    expected: CodexErrorInfo,
) -> Result<()> {
    let mut telemetry = RetryTelemetryCapture::install();
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_once(
        &server,
        responses::sse_failed(
            "terminal",
            code,
            "Selected model is at capacity. Please try a different model.",
        ),
    )
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.model_provider.stream_max_retries = Some(2);
        })
        .build_with_auto_env(&server)
        .await?;
    submit_user_input(&test, "do not retry a noncapacity code").await?;
    let mut errors = 0;
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::Error(error) => {
                assert_eq!(error.codex_error_info, Some(expected.clone()));
                errors += 1;
            }
            EventMsg::StreamError(error) => panic!("unexpected retry: {error:?}"),
            EventMsg::TurnComplete(event) => {
                assert_eq!(
                    event.error.and_then(|error| error.codex_error_info),
                    Some(expected)
                );
                break;
            }
            _ => {}
        }
    }
    assert_eq!((errors, mock.requests().len()), (1, 1));
    assert_eq!(
        telemetry.events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    );
    Ok(())
}

#[test_case("server_is_overloaded"; "overloaded")]
#[test_case("slow_down"; "slow_down")]
#[tokio::test(flavor = "current_thread")]
async fn websocket_capacity_recovers_without_http_fallback(code: &str) -> Result<()> {
    let mut telemetry = RetryTelemetryCapture::install();
    let server = responses::start_websocket_server(vec![
        vec![
            vec![
                responses::ev_response_created("prewarm"),
                responses::ev_completed("prewarm"),
            ],
            vec![
                json!({"type":"error", "status":503, "error":{"code":code, "message":"capacity"}}),
            ],
        ],
        vec![vec![
            responses::ev_response_created("recovered"),
            responses::ev_completed("recovered"),
        ]],
    ])
    .await;
    let test = test_codex()
        .with_config(|config| {
            config.model_provider.request_max_retries = Some(0);
            config.model_provider.stream_max_retries = Some(1);
        })
        .build_with_websocket_server(&server)
        .await?;
    submit_user_input(&test, "recover within the same transport").await?;
    let retry = telemetry.next_retry().await;
    assert_eq!(
        (
            retry.attempt,
            retry.layer.as_str(),
            retry.operation.as_str()
        ),
        (1, "stream", "sampling")
    );
    wait_for_retry(&mut telemetry, &retry).await;
    let mut retries = 0;
    loop {
        match wait_for_event(&test.codex, |_| true).await {
            EventMsg::StreamError(error) => {
                assert_eq!(
                    error.codex_error_info,
                    Some(CodexErrorInfo::ServerOverloaded)
                );
                retries += 1;
            }
            EventMsg::Error(error) => panic!("unexpected terminal error: {error:?}"),
            EventMsg::Warning(warning) => {
                assert!(!warning.message.contains("Falling back from WebSockets"))
            }
            EventMsg::TurnComplete(event) => {
                assert_eq!(event.error, None);
                break;
            }
            _ => {}
        }
    }
    assert_eq!(retries, 1);
    assert_eq!(
        server
            .connections()
            .iter()
            .map(Vec::len)
            .collect::<Vec<_>>(),
        vec![2, 1]
    );
    assert_eq!(
        telemetry.events.try_recv(),
        Err(mpsc::error::TryRecvError::Empty)
    );
    server.shutdown().await;
    Ok(())
}
