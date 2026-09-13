use super::*;
use crate::realtime_history::RealtimeHistory;
use codex_app_server_protocol::ThreadRealtimeItem;
use codex_app_server_protocol::ThreadRealtimeItemCompletedNotification;
use codex_app_server_protocol::ThreadRealtimeItemContent;
use codex_app_server_protocol::ThreadRealtimeTranscriptRole;
use codex_app_server_protocol::ThreadTimelineEntry;
use pretty_assertions::assert_eq;

const QUESTION: &str =
    "Playback diagnostic. Copper lantern. What is 3 plus 4? Please answer in one short sentence.";
const ANSWER: &str = "Sure thing! Three plus four equals seven.";

fn speech(id: &str, role: ThreadRealtimeTranscriptRole, text: &str) -> ThreadRealtimeItem {
    ThreadRealtimeItem {
        id: id.into(),
        realtime_session_id: "voice-session".into(),
        content: ThreadRealtimeItemContent::TranscriptSegment {
            role,
            text: text.into(),
        },
    }
}

fn deliver(chat: &mut ChatWidget, item: ThreadRealtimeItem) {
    chat.handle_server_notification(
        ServerNotification::ThreadRealtimeItemCompleted(ThreadRealtimeItemCompletedNotification {
            thread_id: chat.thread_id().unwrap().to_string(),
            item,
        }),
        /*replay_kind*/ None,
    );
}

fn transcript(rx: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>) -> Vec<String> {
    let mut cells = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            cells.push(
                cell.display_lines(/*width*/ 120)
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_string(),
            );
        }
    }
    cells
}

#[tokio::test]
async fn realtime_external_speech_uses_native_roles_and_identity_not_text() {
    let (mut chat, mut rx, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.transcript.active_cell = None;
    let user = speech("user-1", ThreadRealtimeTranscriptRole::User, QUESTION);
    let assistant = speech(
        "assistant-1",
        ThreadRealtimeTranscriptRole::Assistant,
        ANSWER,
    );
    deliver(&mut chat, user.clone());
    deliver(&mut chat, assistant.clone());
    deliver(&mut chat, user);
    deliver(&mut chat, assistant.clone());
    deliver(
        &mut chat,
        speech(
            "assistant-2",
            ThreadRealtimeTranscriptRole::Assistant,
            ANSWER,
        ),
    );
    let mut another_session = assistant;
    another_session.realtime_session_id = "another-session".into();
    deliver(&mut chat, another_session);
    chat.handle_server_notification(
        ServerNotification::ThreadRealtimeTranscriptDone(
            codex_app_server_protocol::ThreadRealtimeTranscriptDoneNotification {
                thread_id: chat.thread_id().unwrap().to_string(),
                role: "assistant".into(),
                text: ANSWER.into(),
            },
        ),
        /*replay_kind*/ None,
    );

    insta::assert_snapshot!(transcript(&mut rx).join("\n\n"), @"
    › Playback diagnostic. Copper lantern. What is 3 plus 4? Please answer in one short sentence.

    • Sure thing! Three plus four equals seven.

    • Sure thing! Three plus four equals seven.

    • Sure thing! Three plus four equals seven.
    ");
    assert!(
        ops.try_recv().is_err(),
        "display must not submit any native operation"
    );
    assert!(!chat.bottom_pane.is_task_running());
}

#[tokio::test]
async fn realtime_waits_for_ordinary_stream_consolidation() {
    let (mut chat, mut rx, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.transcript.active_cell = None;
    chat.on_agent_message_delta("Ordinary answer".into());
    deliver(
        &mut chat,
        speech(
            "speech",
            ThreadRealtimeTranscriptRole::User,
            "Spoken question",
        ),
    );
    assert!(
        transcript(&mut rx)
            .iter()
            .all(|cell| !cell.contains("Spoken question"))
    );
    chat.finalize_completed_assistant_message(Some("Ordinary answer"));
    assert!(chat.pending_stream_consolidations > 0);
    assert!(
        transcript(&mut rx)
            .iter()
            .all(|cell| !cell.contains("Spoken question"))
    );
    chat.note_stream_consolidation_completed();
    assert_eq!(transcript(&mut rx), vec!["› Spoken question"]);
    assert!(ops.try_recv().is_err());
}

#[tokio::test]
async fn realtime_replay_restores_durable_speech_in_order_and_deduplicates_live_overlap() {
    let user = speech("user-1", ThreadRealtimeTranscriptRole::User, QUESTION);
    let assistant = speech(
        "assistant-1",
        ThreadRealtimeTranscriptRole::Assistant,
        ANSWER,
    );
    let turn = AppServerTurn {
        items: vec![
            AppServerThreadItem::UserMessage {
                id: "typed-user".into(),
                client_id: None,
                content: vec![AppServerUserInput::Text {
                    text: "Typed question".into(),
                    text_elements: Vec::new(),
                }],
            },
            AppServerThreadItem::AgentMessage {
                id: "typed-answer".into(),
                text: "Typed answer".into(),
                phase: None,
                memory_citation: None,
                delivery: None,
                questions: None,
            },
        ],
        ..app_server_turn(
            "ordinary-turn",
            AppServerTurnStatus::Completed,
            /*duration_ms*/ None,
            /*error*/ None,
        )
    };
    let entries = vec![
        ThreadTimelineEntry::Realtime {
            position: 30,
            item: user.clone(),
        },
        ThreadTimelineEntry::Realtime {
            position: 31,
            item: assistant.clone(),
        },
        ThreadTimelineEntry::Item {
            position: 32,
            turn_id: turn.id.clone(),
            item: Box::new(turn.items[0].clone()),
        },
        ThreadTimelineEntry::Item {
            position: 33,
            turn_id: turn.id.clone(),
            item: Box::new(turn.items[1].clone()),
        },
        ThreadTimelineEntry::Realtime {
            position: 34,
            item: speech(
                "tail",
                ThreadRealtimeTranscriptRole::Assistant,
                "Later speech",
            ),
        },
    ];
    // Reconnect builds a fresh widget with a newly read timeline; thread switching
    // rebuilds from the saved session history and buffered live events.
    for replay_kind in [
        ReplayKind::ResumeInitialMessages,
        ReplayKind::ThreadSnapshot,
    ] {
        let (mut chat, mut rx, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.thread_id = Some(ThreadId::new());
        chat.transcript.active_cell = None;
        chat.transcript.realtime.history = RealtimeHistory::from_timeline(entries.clone());
        chat.replay_thread_turns(vec![turn.clone()], replay_kind);
        deliver(&mut chat, user.clone());
        deliver(&mut chat, assistant.clone());
        // The manual widget has no App loop to acknowledge the ordinary answer's
        // consolidation. Speech must remain deferred until that acknowledgement.
        assert_eq!(chat.pending_stream_consolidations, 1);
        chat.note_stream_consolidation_completed();
        insta::assert_snapshot!(transcript(&mut rx).join("\n\n"), @"
        › Playback diagnostic. Copper lantern. What is 3 plus 4? Please answer in one short sentence.

        • Sure thing! Three plus four equals seven.

        › Typed question

        • Typed answer

        • Later speech
        ");
        assert!(ops.try_recv().is_err());
    }
}

#[tokio::test]
async fn realtime_voice_only_history_replays_without_ordinary_turns() {
    let (mut chat, mut rx, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.transcript.active_cell = None;
    chat.transcript.realtime.history = RealtimeHistory::from_timeline(vec![
        ThreadTimelineEntry::Realtime {
            position: 30,
            item: speech("user", ThreadRealtimeTranscriptRole::User, QUESTION),
        },
        ThreadTimelineEntry::Realtime {
            position: 31,
            item: speech("assistant", ThreadRealtimeTranscriptRole::Assistant, ANSWER),
        },
    ]);
    chat.replay_thread_turns(Vec::new(), ReplayKind::ThreadSnapshot);
    assert_eq!(
        transcript(&mut rx),
        vec![format!("› {QUESTION}"), format!("• {ANSWER}")]
    );
    assert!(ops.try_recv().is_err());
}

#[tokio::test]
async fn realtime_replay_keeps_speech_around_hidden_nested_review_user_items() {
    let (mut chat, mut rx, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.transcript.active_cell = None;
    let review = AppServerTurn {
        items: vec![
            AppServerThreadItem::EnteredReviewMode {
                id: "review-start".into(),
                review: "changes".into(),
            },
            AppServerThreadItem::ExitedReviewMode {
                id: "review-end".into(),
                review: "done".into(),
            },
        ],
        ..app_server_turn(
            "review",
            AppServerTurnStatus::Completed,
            /*duration_ms*/ None,
            /*error*/ None,
        )
    };
    let hidden = AppServerTurn {
        items: ["hidden-one", "hidden-two"]
            .into_iter()
            .map(|id| AppServerThreadItem::UserMessage {
                id: id.into(),
                client_id: None,
                content: vec![AppServerUserInput::Text {
                    text: "Hidden review prompt must stay hidden".into(),
                    text_elements: Vec::new(),
                }],
            })
            .collect(),
        ..app_server_turn(
            "hidden",
            AppServerTurnStatus::Interrupted,
            /*duration_ms*/ None,
            /*error*/ None,
        )
    };
    chat.transcript.realtime.history = RealtimeHistory::from_timeline(vec![
        ThreadTimelineEntry::Realtime {
            position: 2,
            item: speech(
                "before",
                ThreadRealtimeTranscriptRole::User,
                "Before hidden review",
            ),
        },
        ThreadTimelineEntry::Item {
            position: 3,
            turn_id: hidden.id.clone(),
            item: Box::new(hidden.items[0].clone()),
        },
        ThreadTimelineEntry::Item {
            position: 4,
            turn_id: hidden.id.clone(),
            item: Box::new(hidden.items[1].clone()),
        },
        ThreadTimelineEntry::Realtime {
            position: 5,
            item: speech(
                "after",
                ThreadRealtimeTranscriptRole::Assistant,
                "After hidden review",
            ),
        },
    ]);
    chat.replay_thread_turns(vec![review, hidden], ReplayKind::ResumeInitialMessages);
    let cells = transcript(&mut rx);
    assert!(
        !cells
            .iter()
            .any(|text| text.contains("Hidden review prompt"))
    );
    insta::assert_snapshot!(cells.into_iter().filter(|text| text.contains("hidden review")).collect::<Vec<_>>().join("\n\n"), @"
    › Before hidden review

    • After hidden review
    ");
    assert!(ops.try_recv().is_err());
}

#[tokio::test]
async fn realtime_rejects_other_threads_and_ignores_non_transcript_items() {
    let (mut chat, mut rx, mut ops) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.transcript.active_cell = None;
    chat.handle_server_notification(
        ServerNotification::ThreadRealtimeItemCompleted(ThreadRealtimeItemCompletedNotification {
            thread_id: ThreadId::new().to_string(),
            item: speech("other-thread", ThreadRealtimeTranscriptRole::User, QUESTION),
        }),
        /*replay_kind*/ None,
    );
    let mut item = speech("session-start", ThreadRealtimeTranscriptRole::User, "");
    item.content = ThreadRealtimeItemContent::RealtimeSessionStarted;
    deliver(&mut chat, item);
    assert_eq!(transcript(&mut rx), Vec::<String>::new());
    assert!(ops.try_recv().is_err());
}
