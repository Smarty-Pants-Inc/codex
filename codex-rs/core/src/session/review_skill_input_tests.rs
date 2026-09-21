use super::*;
use codex_protocol::models::ResponseInputItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::InternalSessionSource;
use pretty_assertions::assert_eq;

#[test]
fn only_review_requests_add_developer_input_to_explicit_skill_selection() {
    let direct = UserInput::Text {
        text: "$direct-skill".to_string(),
        text_elements: Vec::new(),
    };
    let review = UserInput::Text {
        text: "$review-skill".to_string(),
        text_elements: Vec::new(),
    };
    let input = vec![
        TurnInput::UserInput {
            content: vec![direct.clone()],
            client_id: None,
        },
        TurnInput::DeveloperInput {
            content: vec![review.clone()],
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
            direct_input,
        );
    }
    assert_eq!(
        explicit_skill_input(
            &direct_input,
            &input,
            &SessionSource::SubAgent(SubAgentSource::Review)
        )
        .as_ref(),
        vec![direct, review],
    );
}
