use std::borrow::Cow;

use crate::session::TurnInput;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;

/// Review requests and resolved agent skills do not become direct user authorization.
/// Outside inline review, only structured skill selections are accepted from
/// developer input; ordinary developer text cannot select skills.
/// This input must not be used for plugin, app, extension, or user-prompt handling.
pub(super) fn explicit_skill_input<'a>(
    user_input: &'a [UserInput],
    input: &[TurnInput],
    source: &SessionSource,
) -> Cow<'a, [UserInput]> {
    let is_review = matches!(source, SessionSource::SubAgent(SubAgentSource::Review));
    let mut selected = input
        .iter()
        .filter_map(|item| match item {
            TurnInput::DeveloperInput { content } => Some(content.as_slice()),
            TurnInput::UserInput { .. }
            | TurnInput::ResponseItem(_)
            | TurnInput::InterAgentCommunication(_) => None,
        })
        .flatten()
        .filter(|item| is_review || matches!(item, UserInput::Skill { .. }))
        .peekable();
    if selected.peek().is_none() {
        return Cow::Borrowed(user_input);
    }
    Cow::Owned(user_input.iter().chain(selected).cloned().collect())
}

#[cfg(test)]
#[path = "review_skill_input_tests.rs"]
mod tests;
