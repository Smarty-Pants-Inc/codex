use codex_app_server_protocol::DynamicToolCallOutputContentItem;
use codex_app_server_protocol::DynamicToolCallResponse;
use codex_core::StartThreadOptions;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolSpec;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::Op;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;

#[tokio::test]
async fn delivered_response_worker_keeps_its_originating_turn() -> anyhow::Result<()> {
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("first-call"),
                responses::ev_function_call("reused-call", "demo", "{}"),
                responses::ev_completed("first-call"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("second-call"),
                responses::ev_function_call("reused-call", "demo", "{}"),
                responses::ev_completed("second-call"),
            ]),
            responses::sse(vec![
                responses::ev_response_created("settled"),
                responses::ev_completed("settled"),
            ]),
        ],
    )
    .await;
    let base = test_codex().build_with_auto_env(&server).await?;
    let thread = base
        .thread_manager
        .start_thread(StartThreadOptions {
            dynamic_tools: vec![DynamicToolSpec::Function(DynamicToolFunctionSpec {
                name: "demo".into(),
                description: "Test asynchronous tool replies".into(),
                input_schema: json!({"type": "object", "properties": {}}),
                defer_loading: false,
            })],
            ..StartThreadOptions::new(base.config.clone())
        })
        .await?
        .thread;
    let mut first_turn_id = None;
    for text in ["Start the first call", "Start the second call"] {
        let TurnInputSubmission::Started { turn_id } = thread
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: text.into(),
                text_elements: Vec::new(),
            }]))
            .await?
        else {
            anyhow::bail!("expected a new turn")
        };
        wait_for_event(&thread, |event| {
            matches!(event, EventMsg::DynamicToolCallRequest(_))
        })
        .await;
        if let Some((previous_turn_id, delivered_receiver)) = first_turn_id.take() {
            assert_ne!(previous_turn_id, turn_id);
            super::on_call_response(
                previous_turn_id,
                "reused-call".into(),
                delivered_receiver,
                thread.clone(),
            )
            .await;
            let (sender, receiver) = tokio::sync::oneshot::channel();
            sender
                .send(Ok(serde_json::to_value(DynamicToolCallResponse {
                    content_items: vec![DynamicToolCallOutputContentItem::InputText {
                        text: "accepted".into(),
                    }],
                    success: true,
                })?))
                .expect("deliver current response");
            super::on_call_response(turn_id, "reused-call".into(), receiver, thread.clone()).await;
        } else {
            // Deliver before interruption but resume the worker only after B starts.
            let (sender, receiver) = tokio::sync::oneshot::channel();
            sender
                .send(Ok(serde_json::to_value(DynamicToolCallResponse {
                    content_items: vec![DynamicToolCallOutputContentItem::InputText {
                        text: "stale".into(),
                    }],
                    success: true,
                })?))
                .expect("deliver old response");
            thread.submit(Op::Interrupt).await?;
            wait_for_event(&thread, |event| matches!(event, EventMsg::TurnAborted(_))).await;
            first_turn_id = Some((turn_id, receiver));
        }
    }
    wait_for_event(&thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 3);
    let outputs = requests[2]
        .input()
        .into_iter()
        .filter(|item| item["type"] == "function_call_output" && item["call_id"] == "reused-call")
        .map(|item| serde_json::from_value::<FunctionCallOutputPayload>(item["output"].clone()))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        outputs.last(),
        Some(&FunctionCallOutputPayload::from_text("accepted".into())),
    );
    assert!(
        !outputs.contains(&FunctionCallOutputPayload::from_text("stale".into())),
        "a late reply must not enter the replacement turn's history"
    );
    Ok(())
}
