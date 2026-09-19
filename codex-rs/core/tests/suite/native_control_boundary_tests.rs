use codex_core::StartThreadOptions;
use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_protocol::dynamic_tools::DynamicToolCallOutputContentItem;
use codex_protocol::dynamic_tools::DynamicToolFunctionSpec;
use codex_protocol::dynamic_tools::DynamicToolResponse;
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
async fn dynamic_tool_response_remains_bound_to_its_original_native_turn() -> anyhow::Result<()> {
    let server = responses::start_mock_server().await;
    let mock = responses::mount_sse_sequence(
        &server,
        vec![
            responses::sse(vec![
                responses::ev_response_created("call"),
                responses::ev_function_call("reused-call", "sense", "{}"),
                responses::ev_completed("call"),
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
                name: "sense".into(),
                description: "Synthetic native control boundary".into(),
                input_schema: json!({"type": "object", "properties": {}}),
                defer_loading: false,
            })],
            ..StartThreadOptions::new(base.config.clone())
        })
        .await?
        .thread;
    let TurnInputSubmission::Started { turn_id } = thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "Call sense".into(),
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
    for (response_turn, text) in [("stale-turn".to_owned(), "stale"), (turn_id, "accepted")] {
        thread
            .submit(Op::DynamicToolResponseForTurn {
                turn_id: response_turn,
                id: "reused-call".into(),
                response: DynamicToolResponse {
                    content_items: vec![DynamicToolCallOutputContentItem::InputText {
                        text: text.into(),
                    }],
                    success: true,
                },
            })
            .await?;
    }
    wait_for_event(&thread, |event| matches!(event, EventMsg::TurnComplete(_))).await;
    let requests = mock.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        serde_json::from_value::<FunctionCallOutputPayload>(
            requests[1].function_call_output("reused-call")["output"].clone(),
        )?,
        FunctionCallOutputPayload::from_text("accepted".into()),
    );
    Ok(())
}
