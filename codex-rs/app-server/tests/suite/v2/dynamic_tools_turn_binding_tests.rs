use super::*;
use codex_app_server_protocol::TurnCompletedNotification;
use codex_app_server_protocol::TurnInterruptParams;
use codex_app_server_protocol::TurnStatus;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn interrupted_dynamic_call_cannot_resolve_reused_call_in_next_turn() -> Result<()> {
    let call_id = "reused-call";
    let calls = ["first", "second"].map(|id| {
        responses::sse(vec![
            responses::ev_response_created(id),
            responses::ev_function_call(call_id, "demo_tool", r#"{"city":"Paris"}"#),
            responses::ev_completed(id),
        ])
    });
    let PendingDynamicToolCall {
        mut mcp,
        server,
        request_id,
        params,
    } = start_function_dynamic_tool_call_with_responses(
        call_id,
        vec![
            calls[0].clone(),
            calls[1].clone(),
            create_final_assistant_message_sse_response("Done")?,
        ],
    )
    .await?;
    let interrupt = mcp
        .send_turn_interrupt_request(TurnInterruptParams {
            thread_id: params.thread_id.clone(),
            turn_id: params.turn_id.clone(),
        })
        .await?;
    timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(interrupt)),
    )
    .await??;
    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let interrupted: TurnCompletedNotification =
        serde_json::from_value(notification.params.context("turn completion params")?)?;
    assert_eq!(
        (
            interrupted.thread_id,
            interrupted.turn.id,
            interrupted.turn.status
        ),
        (
            params.thread_id.clone(),
            params.turn_id.clone(),
            TurnStatus::Interrupted
        )
    );

    let start = mcp
        .send_turn_start_request(TurnStartParams {
            thread_id: params.thread_id.clone(),
            input: vec![V2UserInput::Text {
                text: "Run the tool again".into(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    let response = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(start)),
    )
    .await??;
    let TurnStartResponse { turn } = to_response::<TurnStartResponse>(response)?;
    assert_ne!(turn.id, params.turn_id);
    let started = wait_for_dynamic_tool_started(&mut mcp, call_id).await?;
    assert_eq!(
        (started.thread_id, started.turn_id),
        (params.thread_id.clone(), turn.id.clone())
    );
    let request = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_request_message(),
    )
    .await??;
    let ServerRequest::DynamicToolCall {
        request_id: current_request_id,
        params: current_params,
    } = request
    else {
        anyhow::bail!("expected replacement dynamic tool request");
    };
    assert_ne!(current_request_id, request_id);
    assert_eq!(
        current_params,
        DynamicToolCallParams {
            turn_id: turn.id.clone(),
            ..params.clone()
        }
    );
    for (id, text) in [(request_id, "stale"), (current_request_id, "accepted")] {
        mcp.send_response(
            id,
            serde_json::to_value(DynamicToolCallResponse {
                content_items: vec![DynamicToolCallOutputContentItem::InputText {
                    text: text.into(),
                }],
                success: true,
            })?,
        )
        .await?;
    }
    let completed = wait_for_dynamic_tool_completed(&mut mcp, call_id).await?;
    assert_eq!(
        (completed.thread_id, completed.turn_id),
        (params.thread_id.clone(), turn.id.clone())
    );
    let ThreadItem::DynamicToolCall {
        status,
        content_items,
        success,
        ..
    } = completed.item
    else {
        anyhow::bail!("expected replacement dynamic tool completion");
    };
    assert_eq!(
        (status, content_items, success),
        (
            DynamicToolCallStatus::Completed,
            Some(vec![DynamicToolCallOutputContentItem::InputText {
                text: "accepted".into()
            }]),
            Some(true)
        )
    );
    let notification = timeout(
        DEFAULT_READ_TIMEOUT,
        mcp.read_stream_until_notification_message("turn/completed"),
    )
    .await??;
    let completed: TurnCompletedNotification =
        serde_json::from_value(notification.params.context("turn completion params")?)?;
    assert_eq!(
        (
            completed.thread_id,
            completed.turn.id,
            completed.turn.status
        ),
        (params.thread_id, turn.id, TurnStatus::Completed)
    );
    let bodies = responses_bodies(&server).await?;
    assert_eq!(bodies.len(), 3);
    let outputs = bodies[2]["input"]
        .as_array()
        .context("model input")?
        .iter()
        .filter(|item| item["type"] == "function_call_output" && item["call_id"] == call_id)
        .map(|item| serde_json::from_value::<FunctionCallOutputPayload>(item["output"].clone()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    assert_eq!(
        outputs.last(),
        Some(&FunctionCallOutputPayload::from_text("accepted".into()))
    );
    assert!(!outputs.contains(&FunctionCallOutputPayload::from_text("stale".into())));
    Ok(())
}
