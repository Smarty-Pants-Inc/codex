//! The shared transcript formatter hides output the server persisted as voice-private.

use super::RawReasoningVisibility;
use super::thread_items_to_transcript_cells;
use crate::history_cell::HistoryCell;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use codex_app_server_protocol::ItemOrigin;
use codex_app_server_protocol::ThreadItem;
use codex_protocol::models::MessagePhase;
use pretty_assertions::assert_eq;

fn agent(id: &str, text: &str, phase: MessagePhase, origin: Option<ItemOrigin>) -> ThreadItem {
    ThreadItem::AgentMessage {
        id: id.to_string(),
        text: text.to_string(),
        phase: Some(phase),
        memory_citation: None,
        delivery: None,
        questions: None,
        origin,
    }
}

#[test]
fn persisted_voice_private_items_are_not_projected() {
    let items = vec![
        ThreadItem::Reasoning {
            id: "voice-reasoning".to_string(),
            summary: vec!["Private voice reasoning".to_string()],
            content: Vec::new(),
            origin: Some(ItemOrigin::Voice),
        },
        agent(
            "voice-commentary",
            "Private voice commentary",
            MessagePhase::Commentary,
            Some(ItemOrigin::Voice),
        ),
        agent(
            "voice-answer",
            "Spoken final answer",
            MessagePhase::FinalAnswer,
            Some(ItemOrigin::Voice),
        ),
        agent(
            "typed-commentary",
            "Typed commentary",
            MessagePhase::Commentary,
            Some(ItemOrigin::Typed),
        ),
        agent(
            "unowned-commentary",
            "Commentary without an origin",
            MessagePhase::Commentary,
            /*origin*/ None,
        ),
    ];

    let rendered = thread_items_to_transcript_cells(
        /*thread_id*/ None,
        &test_path_buf("/workspace").abs(),
        items,
        RawReasoningVisibility::Visible,
        /*config*/ None,
    )
    .iter()
    .flat_map(|cell| cell.transcript_lines(/*width*/ 80))
    .map(|line| line.to_string())
    .collect::<Vec<_>>()
    .join("\n");
    let shown = [
        "Private voice reasoning",
        "Private voice commentary",
        "Spoken final answer",
        "Typed commentary",
        "Commentary without an origin",
    ]
    .into_iter()
    .filter(|text| rendered.contains(text))
    .collect::<Vec<_>>();
    assert_eq!(
        shown,
        vec![
            "Spoken final answer",
            "Typed commentary",
            "Commentary without an origin"
        ],
        "{rendered}"
    );
}
