use super::super::PreviousSectionState;
use super::super::WorldState;
use super::super::test_support::render_section_cases;
use super::*;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;

#[test]
fn snapshots() {
    use PreviousSectionState::Absent;
    use PreviousSectionState::Known;
    use PreviousSectionState::Unknown;

    let empty = AgentsMdState::default();
    let project_formatter = LoadedAgentsMd::from_text_for_testing("use the project formatter");
    let project_formatter = AgentsMdState::new(Some(&project_formatter));
    let old = LoadedAgentsMd::from_text_for_testing("old instructions");
    let old = AgentsMdState::new(Some(&old));
    let new = LoadedAgentsMd::from_text_for_testing("new instructions");
    let new = AgentsMdState::new(Some(&new));

    insta::assert_snapshot!(render_section_cases(&[
        (Absent, Absent),
        (Absent, Known(&empty)),
        (Absent, Known(&project_formatter)),
        (Known(&project_formatter), Known(&project_formatter)),
        (Known(&old), Known(&new)),
        (Known(&new), Known(&empty)),
        (Unknown, Known(&new)),
        (Unknown, Known(&empty)),
    ]));
}

fn legacy_agents_message(role: &str, text: &str) -> ResponseItem {
    ResponseItem::Message {
        id: None,
        role: role.to_string(),
        content: vec![ContentItem::InputText {
            text: format!("# AGENTS.md instructions\n\n<INSTRUCTIONS>\n{text}\n</INSTRUCTIONS>"),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    }
}

#[test]
fn history_diff_reconciles_legacy_agents_roles_without_snapshot() {
    for role in ["user", "developer"] {
        let legacy = legacy_agents_message(role, "old instructions");
        let loaded = LoadedAgentsMd::from_text_for_testing("new instructions");
        let mut world_state = WorldState::default();
        world_state.add_section(AgentsMdState::new(Some(&loaded)));

        let fragments = world_state.render_history_diff(None, [&legacy]);

        assert_eq!(fragments.len(), 1);
        assert_eq!(fragments[0].role(), "developer");
        assert!(fragments[0].render().contains(REPLACEMENT_NOTICE));
    }

    let legacy = legacy_agents_message("user", "removed instructions");
    let mut world_state = WorldState::default();
    world_state.add_section(AgentsMdState::default());

    let fragments = world_state.render_history_diff(None, [&legacy]);

    assert_eq!(fragments.len(), 1);
    assert_eq!(fragments[0].role(), "developer");
    assert!(fragments[0].render().contains(REMOVAL_NOTICE));
}
