//! Servers that submit voice delegations as developer input mark the turn with
//! the `realtime` trigger instead of a `<realtime_delegation>` user item.

use super::super::RealtimeTurnOrigin;
use super::*;
use codex_app_server_protocol::TurnStartedNotification;
use pretty_assertions::assert_eq;

fn start_turn(
    chat: &mut ChatWidget,
    thread_id: ThreadId,
    turn_id: &str,
    turn_trigger: Option<&str>,
) {
    chat.handle_server_notification(
        ServerNotification::TurnStarted(TurnStartedNotification {
            thread_id: thread_id.to_string(),
            turn: Turn {
                id: turn_id.to_string(),
                items: Vec::new(),
                items_view: TurnItemsView::Full,
                status: TurnStatus::InProgress,
                error: None,
                started_at: None,
                completed_at: None,
                duration_ms: None,
                turn_trigger: turn_trigger.map(str::to_string),
            },
        }),
        /*replay_kind*/ None,
    );
}

fn spoken_texts(ops: &mut tokio::sync::mpsc::UnboundedReceiver<AppCommand>) -> Vec<String> {
    let mut spoken = Vec::new();
    while let Ok(op) = ops.try_recv() {
        if let AppCommand::RealtimeConversationSpeech { text, .. } = op {
            spoken.push(text.as_str().to_string());
        }
    }
    spoken
}

fn complete_answer(chat: &mut ChatWidget, thread_id: ThreadId, turn_id: &str) {
    let commentary = agent_item(
        "commentary",
        "[COMMENTARY] private voice work",
        Some(MessagePhase::Commentary),
    );
    start_item(chat, thread_id, turn_id, commentary.clone());
    complete_item(chat, thread_id, turn_id, commentary);
    let answer = agent_item(
        "final-answer",
        "Spoken answer",
        Some(MessagePhase::FinalAnswer),
    );
    start_item(chat, thread_id, turn_id, answer.clone());
    complete_item(chat, thread_id, turn_id, answer.clone());
    finish_turn(
        chat,
        thread_id,
        turn_id,
        vec![answer],
        TurnStatus::Completed,
    );
}

fn history_text(
    chat: &mut ChatWidget,
    events: &mut tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
) -> String {
    commit_realtime_history_events(chat, events);
    let mut lines = Vec::new();
    while let Ok(event) = events.try_recv() {
        if let AppEvent::InsertHistoryCell(cell) = event {
            lines.extend(
                cell.display_lines(/*width*/ 80)
                    .iter()
                    .map(ToString::to_string),
            );
        }
    }
    lines.join("\n")
}

#[tokio::test]
async fn realtime_triggered_turn_is_delegated_and_speaks_its_final_answer() {
    let (mut chat, _sender, mut events, mut ops) = make_chatwidget_manual_with_sender().await;
    let thread_id = activate_voice(&mut chat);
    let turn_id = "triggered-turn";

    start_turn(&mut chat, thread_id, turn_id, Some("realtime"));

    assert!(chat.is_realtime_delegated_reasoning_turn(turn_id));
    assert!(matches!(
        chat.realtime_conversation.turn_origins.get(turn_id),
        Some(RealtimeTurnOrigin::Delegated {
            may_speak: true,
            ..
        })
    ));
    complete_answer(&mut chat, thread_id, turn_id);
    assert_eq!(spoken_texts(&mut ops), vec!["Spoken answer".to_string()]);
    assert!(!history_text(&mut chat, &mut events).contains("private voice work"));
}

#[tokio::test]
async fn realtime_trigger_and_marker_on_one_turn_count_the_delegation_once() {
    let (mut chat, _sender, _events, mut ops) = make_chatwidget_manual_with_sender().await;
    let thread_id = activate_voice(&mut chat);
    let turn_id = "triggered-marker-turn";
    let input_generation = chat.realtime_conversation.input_generation;

    start_turn(&mut chat, thread_id, turn_id, Some("realtime"));
    let marker =
        user_item("<realtime_delegation><input>spoken question</input></realtime_delegation>");
    start_item(&mut chat, thread_id, turn_id, marker.clone());
    complete_item(&mut chat, thread_id, turn_id, marker);

    // A second count would treat the marker as a new delegation and advance the generation.
    assert_eq!(
        chat.realtime_conversation.input_generation,
        input_generation
    );
    assert!(matches!(
        chat.realtime_conversation.turn_origins.get(turn_id),
        Some(RealtimeTurnOrigin::Delegated {
            may_speak: true,
            input_generation: generation,
        }) if *generation == input_generation
    ));
    assert!(chat.realtime_conversation.triggered_turns.is_empty());
    complete_answer(&mut chat, thread_id, turn_id);
    assert_eq!(spoken_texts(&mut ops), vec!["Spoken answer".to_string()]);
}

#[tokio::test]
async fn turn_without_trigger_or_marker_stays_typed() {
    let (mut chat, _sender, mut events, mut ops) = make_chatwidget_manual_with_sender().await;
    let thread_id = activate_voice(&mut chat);
    let turn_id = "plain-turn";

    start_turn(&mut chat, thread_id, turn_id, /*turn_trigger*/ None);

    assert!(!chat.is_realtime_delegated_reasoning_turn(turn_id));
    assert!(
        chat.realtime_conversation
            .turn_origins
            .get(turn_id)
            .is_none()
    );
    complete_answer(&mut chat, thread_id, turn_id);
    assert_eq!(spoken_texts(&mut ops), Vec::<String>::new());
    assert!(history_text(&mut chat, &mut events).contains("Spoken answer"));
}
