use std::borrow::Cow;

use crate::session::TurnInput;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::SubAgentSource;
use codex_protocol::user_input::UserInput;

/// Review requests may select skills without becoming direct user authorization.
/// This input must not be used for plugin, app, extension, or user-prompt handling.
pub(super) fn explicit_skill_input<'a>(
    user_input: &'a [UserInput],
    input: &[TurnInput],
    source: &SessionSource,
) -> Cow<'a, [UserInput]> {
    if !matches!(source, SessionSource::SubAgent(SubAgentSource::Review)) {
        return Cow::Borrowed(user_input);
    }
    let mut selected = user_input.to_vec();
    selected.extend(
        input
            .iter()
            .filter_map(|item| match item {
                TurnInput::DeveloperInput { content } => Some(content.as_slice()),
                TurnInput::UserInput { .. }
                | TurnInput::ResponseItem(_)
                | TurnInput::InterAgentCommunication(_) => None,
            })
            .flatten()
            .cloned(),
    );
    Cow::Owned(selected)
}

#[cfg(test)]
#[path = "review_skill_input_tests.rs"]
mod tests;
