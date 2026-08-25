use super::super::PreviousSectionState;
use super::super::test_support::render_section_cases;
use super::*;

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

#[test]
fn raw_user_lookalike_does_not_match_legacy_agents_instructions() {
    let loaded = LoadedAgentsMd::from_text_for_testing("use the project formatter");
    let state = AgentsMdState::new(Some(&loaded));
    let expected = state
        .instructions
        .as_ref()
        .expect("loaded instructions")
        .render();
    let user_lookalike = codex_protocol::models::ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![codex_protocol::models::ContentItem::InputText {
            text: expected.clone(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let mut world_state = super::super::WorldState::default();
    world_state.add_section(state);

    assert_eq!(
        world_state
            .render_history_diff(/*previous*/ None, &[user_lookalike])
            .into_iter()
            .map(|fragment| fragment.render())
            .collect::<Vec<_>>(),
        vec![expected]
    );
}
