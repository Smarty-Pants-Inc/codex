//! Exercise durable voice hydration through the native JSON-RPC resume path.

use super::active_reconnect::drain_history;
use super::disconnect::serve_reconnect_requests;
use super::*;
use crate::app_server_session::ResumeModelSettings;
use crate::app_server_session::ThreadParamsMode;
use pretty_assertions::assert_eq;
use serde_json::json;
use tokio::net::TcpListener;

#[tokio::test]
async fn realtime_resume_reads_backwards_pages_and_replays_voice_only_thread_snapshot() -> Result<()>
{
    let (mut app, mut events, mut ops) = make_test_app_with_channels().await;
    let id = ThreadId::new();
    let cwd = app.config.cwd.clone();
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = crate::resolve_remote_addr(&format!("ws://{}", listener.local_addr()?))?;
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await?;
        let mut timeline_reads = 0;
        let mut resumes = 0;
        let earlier_answer =
            json!({"type": "agentMessage", "id": "earlier-answer", "text": "Earlier typed answer"});
        let typed_answer =
            json!({"type": "agentMessage", "id": "typed-answer", "text": "Typed answer"});
        let typed_continuation = json!({"type": "agentMessage", "id": "typed-continuation", "text": "Typed continuation"});
        let last_answer =
            json!({"type": "agentMessage", "id": "last-answer", "text": "Last typed answer"});
        serve_reconnect_requests(tokio_tungstenite::accept_async(stream).await?, |request| {
            let result = match request.method.as_str() {
                "thread/resume" => {
                    resumes += 1;
                    let params = request.params.as_ref().unwrap();
                    assert_eq!(params["threadId"], id.to_string());
                    assert!(params["model"].is_null());
                    assert!(params["cwd"].is_null());
                    json!({"thread": {
                        "id": id, "sessionId": id, "preview": "", "ephemeral": false,
                        "historyMode": "paginated", "modelProvider": "test-provider",
                        "createdAt": 1, "updatedAt": 2, "status": {"type": "idle"},
                        "cwd": cwd, "cliVersion": "0.153.4", "source": "cli", "turns": []
                    }, "model": "gpt-test", "modelProvider": "test-provider", "cwd": cwd,
                    "approvalPolicy": "never", "approvalsReviewer": "user",
                    "sandbox": {"type": "dangerFullAccess"}, "reasoningEffort": null})
                }
                "thread/turns/list" if resumes == 2 => {
                    assert_eq!(request.params.as_ref().unwrap()["sortDirection"], "desc");
                    let turns = [
                        ("earlier-turn", vec![earlier_answer.clone()]),
                        ("ordinary-turn", vec![typed_answer.clone(), typed_continuation.clone()]),
                        ("last-turn", vec![last_answer.clone()]),
                    ].into_iter().rev().map(|(turn_id, items)| json!({
                        "id": turn_id, "status": "completed", "itemsView": "full",
                        "items": items, "error": null,
                        "startedAt": null, "completedAt": null, "durationMs": null
                    })).collect::<Vec<_>>();
                    json!({"data": turns, "nextCursor": null})
                }
                "thread/turns/list" | "thread/items/list" => json!({"data": [], "nextCursor": null}),
                "thread/timeline/list" => {
                    let params = request.params.as_ref().unwrap();
                    assert_eq!(params["threadId"], id.to_string());
                    assert_eq!(params["limit"], 100);
                    timeline_reads += 1;
                    if timeline_reads > 4 {
                        // The third resume encounters a backend that keeps returning cursors.
                        json!({"data": [{"type": "realtime", "position": timeline_reads,
                            "item": {"id": format!("bounded-{timeline_reads}"), "realtimeSessionId": "voice",
                                "type": "transcriptSegment", "role": "assistant", "text": "More speech"}}],
                            "nextCursor": format!("cursor-{timeline_reads}"), "activeRealtimeSessionAtPageStart": "voice"})
                    } else if params["cursor"].is_null() {
                        let mut data = vec![json!({"type": "realtime", "position": 31,
                            "item": {"id": "assistant", "realtimeSessionId": "voice",
                                "type": "transcriptSegment", "role": "assistant", "text": "Spoken answer"}})];
                        if resumes == 2 {
                            data.extend([
                                json!({"type": "item", "position": 42,
                                    "turnId": "ordinary-turn", "item": typed_answer}),
                                json!({"type": "realtime", "position": 43,
                                    "item": {"id": "between-items", "realtimeSessionId": "voice",
                                        "type": "transcriptSegment", "role": "assistant", "text": "Between-item speech"}}),
                                json!({"type": "item", "position": 44,
                                    "turnId": "ordinary-turn", "item": typed_continuation}),
                                json!({"type": "item", "position": 52,
                                    "turnId": "last-turn", "item": last_answer}),
                                json!({"type": "realtime", "position": 58,
                                    "item": {"id": "tail", "realtimeSessionId": "voice",
                                        "type": "transcriptSegment", "role": "assistant", "text": "Later speech"}}),
                                json!({"type": "realtime", "position": 59,
                                    "item": {"id": "tail-user", "realtimeSessionId": "voice",
                                        "type": "transcriptSegment", "role": "user", "text": "Later question"}}),
                            ]);
                        }
                        json!({"data": data, "nextCursor": "older", "activeRealtimeSessionAtPageStart": "voice"})
                    } else {
                        assert_eq!(params["cursor"], "older");
                        let mut data = Vec::new();
                        if resumes == 2 {
                            data.extend([
                                json!({"type": "item", "position": 10,
                                    "turnId": "earlier-turn", "item": earlier_answer}),
                                json!({"type": "turnCompleted", "position": 14,
                                    "turnId": "earlier-turn", "status": "completed"}),
                            ]);
                        }
                        data.push(json!({"type": "realtime", "position": 30,
                            "item": {"id": "user", "realtimeSessionId": "voice",
                                "type": "transcriptSegment", "role": "user", "text": "Spoken question"}}));
                        json!({"data": data, "nextCursor": null, "activeRealtimeSessionAtPageStart": "voice"})
                    }
                }
                method => panic!("unexpected operation during voice replay: {method}"),
            };
            std::future::ready(Some(json!({"result": result})))
        }).await
    });
    let mut session = AppServerSession::new(
        crate::connect_remote_app_server(endpoint).await?,
        ThreadParamsMode::Remote,
    );
    let mut tui = crate::tui::test_support::make_test_tui()?;
    // Initial attachment is voice-only. The second resume interleaves speech with
    // ordinary turns/items while all their consolidations await the real App dispatcher.
    for resume_index in 0..2 {
        let started = session
            .resume_thread(
                app.config.clone(),
                id,
                ResumeModelSettings::PreserveExistingThread,
            )
            .await?;
        assert_eq!(started.turns.is_empty(), resume_index == 0);
        assert!(started.session.realtime_history.notice.is_none());
        let mut snapshot = ThreadEventSnapshot {
            session: Some(started.session),
            turns: started.turns,
            events: Vec::new(),
            input_state: None,
        };
        // A buffered completion can overlap the durable snapshot after attachment.
        snapshot
            .events
            .push(ThreadBufferedEvent::Notification(Box::new(
                ServerNotification::ThreadRealtimeItemCompleted(serde_json::from_value(json!({
                    "threadId": id, "item": {"id": "assistant", "realtimeSessionId": "voice",
                        "type": "transcriptSegment", "role": "assistant", "text": "Spoken answer"}
                }))?),
            )));
        for _ in 0..2 {
            let init = app.chatwidget_init_for_forked_or_resumed_thread(
                &mut tui,
                app.config.clone(),
                /*initial_user_message*/ None,
            );
            app.replace_chat_widget(ChatWidget::new_with_app_event(init));
            app.transcript_cells.clear();
            app.replay_thread_snapshot(snapshot.clone(), /*resume_restored_queue*/ false);
            let rendered = drain_history(&mut app, &mut tui, &mut session, &mut events).await?;
            assert_eq!(rendered.matches("Spoken question").count(), 1);
            assert_eq!(rendered.matches("Spoken answer").count(), 1);
            assert!(rendered.find("Spoken question") < rendered.find("Spoken answer"));
            if resume_index == 1 {
                assert_eq!(rendered.matches("Typed answer").count(), 1);
                assert_eq!(rendered.matches("Later speech").count(), 1);
                assert!(rendered.find("Spoken answer") < rendered.find("Typed answer"));
                assert!(rendered.find("Typed answer") < rendered.find("Later speech"));
                let messages = rendered
                    .lines()
                    .filter(|line| line.starts_with("• ") || line.starts_with("› "))
                    .collect::<Vec<_>>()
                    .join("\n");
                insta::allow_duplicates! {
                    insta::assert_snapshot!(messages, @"
                    • Earlier typed answer
                    › Spoken question
                    • Spoken answer
                    • Typed answer
                    • Between-item speech
                    • Typed continuation
                    • Last typed answer
                    • Later speech
                    › Later question
                    ");
                }
            }
            assert!(ops.try_recv().is_err());
        }
    }
    let bounded = session
        .resume_thread(
            app.config.clone(),
            id,
            ResumeModelSettings::PreserveExistingThread,
        )
        .await?;
    assert_eq!(
        bounded
            .session
            .realtime_history
            .items
            .values()
            .map(Vec::len)
            .sum::<usize>(),
        5
    );
    assert!(bounded.session.realtime_history.notice.is_some());
    session.shutdown().await?;
    let methods = server.await??;
    assert_eq!(
        methods
            .iter()
            .filter(|method| *method == "thread/timeline/list")
            .count(),
        9
    );
    Ok(())
}
