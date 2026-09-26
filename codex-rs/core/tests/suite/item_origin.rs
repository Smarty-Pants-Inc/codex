use codex_core::TurnInputRequest;
use codex_core::TurnInputSubmission;
use codex_core::TurnStartOptions;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::ItemOrigin;
use codex_protocol::items::TurnItem;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ev_assistant_message;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_message_item_added;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::sse;
use core_test_support::streaming_sse::StreamingSseChunk;
use core_test_support::streaming_sse::start_streaming_sse_server;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::sync::oneshot;

fn text_input(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

/// Collects completed assistant messages as (text, origin) pairs.
fn record_agent_message(event: &EventMsg, messages: &mut Vec<(String, Option<ItemOrigin>)>) {
    if let EventMsg::ItemCompleted(completed) = event
        && let TurnItem::AgentMessage(item) = &completed.item
    {
        let text = item
            .content
            .iter()
            .map(|AgentMessageContent::Text { text }| text.as_str())
            .collect::<String>();
        messages.push((text, item.origin));
    }
}

/// A voice-delegated turn stamps its commentary as voice, a typed steer
/// switches later items to typed, and a typed-only turn records no origin.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn realtime_turn_items_record_voice_then_typed_origin() -> anyhow::Result<()> {
    let (release_voice_tx, release_voice_rx) = oneshot::channel();
    let voice_sample = vec![
        StreamingSseChunk {
            gate: None,
            body: sse(vec![
                ev_response_created("resp-voice"),
                ev_message_item_added("msg-voice", ""),
            ]),
        },
        StreamingSseChunk {
            gate: Some(release_voice_rx),
            body: sse(vec![
                json!({
                    "type": "response.output_item.done",
                    "item": {
                        "type": "message",
                        "role": "assistant",
                        "id": "msg-voice",
                        "content": [{"type": "output_text", "text": "voice commentary"}],
                        "phase": "commentary",
                    }
                }),
                ev_completed("resp-voice"),
            ]),
        },
    ];
    let steered_sample = vec![StreamingSseChunk {
        gate: None,
        body: sse(vec![
            ev_response_created("resp-typed"),
            ev_assistant_message("msg-typed", "typed answer"),
            ev_completed("resp-typed"),
        ]),
    }];
    let typed_only_sample = vec![StreamingSseChunk {
        gate: None,
        body: sse(vec![
            ev_response_created("resp-plain"),
            ev_assistant_message("msg-plain", "plain answer"),
            ev_completed("resp-plain"),
        ]),
    }];
    let (server, _completions) =
        start_streaming_sse_server(vec![voice_sample, steered_sample, typed_only_sample]).await;
    let codex = test_codex()
        .with_model("gpt-5.4")
        .build_with_streaming_server(&server)
        .await?
        .codex;
    let mut messages = Vec::new();

    // Same shape as `Session::route_realtime_text_input` submits for a voice handoff.
    codex
        .start_or_steer_turn(
            TurnInputRequest::developer_input(text_input(
                "<realtime_delegation>\n  <input>voice question</input>\n</realtime_delegation>",
            ))
            .on_start(TurnStartOptions {
                turn_trigger: Some("realtime".to_string()),
                ..Default::default()
            }),
        )
        .await?;
    wait_for_event(&codex, |event| {
        record_agent_message(event, &mut messages);
        matches!(
            event,
            EventMsg::ItemStarted(started) if matches!(&started.item, TurnItem::AgentMessage(_))
        )
    })
    .await;

    // The steer is queued while the voice commentary streams and is recorded
    // before the second sample.
    let steer = codex
        .start_or_steer_turn(TurnInputRequest::user_input(text_input("typed steer")))
        .await?;
    assert!(matches!(steer, TurnInputSubmission::Steered { .. }));
    let _ = release_voice_tx.send(());
    wait_for_event(&codex, |event| {
        record_agent_message(event, &mut messages);
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    codex
        .start_or_steer_turn(TurnInputRequest::user_input(text_input("typed only")))
        .await?;
    wait_for_event(&codex, |event| {
        record_agent_message(event, &mut messages);
        matches!(event, EventMsg::TurnComplete(_))
    })
    .await;

    assert_eq!(
        messages,
        vec![
            ("voice commentary".to_string(), Some(ItemOrigin::Voice)),
            ("typed answer".to_string(), Some(ItemOrigin::Typed)),
            ("plain answer".to_string(), None),
        ]
    );
    assert_eq!(server.requests().await.len(), 3);

    server.shutdown().await;
    Ok(())
}
