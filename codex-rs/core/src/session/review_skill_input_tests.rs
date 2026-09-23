use super::*;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::InternalSessionSource;
use pretty_assertions::assert_eq;

#[test]
fn developer_skill_selection_requires_review_source_or_structured_skill() {
    let direct = UserInput::Text {
        text: "$direct-skill".to_string(),
        text_elements: Vec::new(),
    };
    let review = UserInput::Text {
        text: "$review-skill".to_string(),
        text_elements: Vec::new(),
    };
    let resolved_skill = UserInput::Skill {
        name: "resolved-agent".to_string(),
        path: std::path::PathBuf::from("system/resolved-agent/SKILL.md"),
    };
    let mention = UserInput::Mention {
        name: "unselected-app".to_string(),
        path: "app://unselected-app".to_string(),
    };
    let input = vec![
        TurnInput::UserInput {
            content: vec![direct.clone()],
            client_id: None,
        },
        TurnInput::DeveloperInput {
            content: vec![review.clone(), resolved_skill.clone(), mention.clone()],
        },
        TurnInput::ResponseItem(
            ResponseItem::from(ResponseInputItem::from(vec![UserInput::Text {
                text: "$raw-evidence-skill".to_string(),
                text_elements: Vec::new(),
            }]))
            .into(),
        ),
    ];
    let direct_input = vec![direct.clone()];
    for source in [
        SessionSource::Cli,
        SessionSource::Internal(InternalSessionSource::Guardian),
        SessionSource::SubAgent(SubAgentSource::Other("guardian".to_string())),
        SessionSource::SubAgent(SubAgentSource::Compact),
    ] {
        assert_eq!(
            explicit_skill_input(&direct_input, &input, &source).as_ref(),
            vec![direct.clone(), resolved_skill.clone()],
        );
    }
    assert_eq!(
        explicit_skill_input(
            &direct_input,
            &input,
            &SessionSource::SubAgent(SubAgentSource::Review)
        )
        .as_ref(),
        vec![direct, review, resolved_skill, mention],
    );
}
