use codex_api::SearchInput;
use codex_core::is_contextual_user_fragment;
use codex_core::parse_turn_item;
use codex_protocol::items::TurnItem;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::models::plaintext_agent_message_content;
use codex_tools::retain_tail_from_last_n_user_messages;
use codex_tools::truncate_assistant_output_text_to_token_budget;

const ASSISTANT_CONTEXT_TOKEN_LIMIT: usize = 1_000;
const ASSISTANT_ROLE: &str = "assistant";
const USER_ROLE: &str = "user";
/// Content kind that the direct user-input boundary gives to typed text.
const DIRECT_USER_TEXT_KIND: &str = "user.text";

/// Builds the conversation tail for standalone web search.
///
/// The tail keeps the previous user text message, up to 1k tokens of assistant
/// text that followed it, and the current user text message.
pub(crate) fn recent_input(items: &[ResponseItem]) -> Option<SearchInput> {
    let mut messages = Vec::new();
    for item in items {
        push_visible_message(&mut messages, item);
    }

    retain_tail_from_last_n_user_messages(&mut messages, /*user_message_count*/ 2);
    truncate_assistant_output_text_to_token_budget(&mut messages, ASSISTANT_CONTEXT_TOKEN_LIMIT);
    (!messages.is_empty()).then_some(SearchInput::Items(messages))
}

fn push_visible_message(messages: &mut Vec<ResponseItem>, item: &ResponseItem) {
    match item {
        ResponseItem::Message { role, .. } if role == ASSISTANT_ROLE => {
            let mut message = item.clone();
            message.set_id(/*new_id*/ None);
            messages.push(message);
        }
        ResponseItem::AgentMessage {
            author,
            content,
            internal_chat_message_metadata_passthrough: metadata,
            ..
        } => {
            if let Some(text) = plaintext_agent_message_content(content) {
                messages.push(ResponseItem::Message {
                    id: None,
                    role: ASSISTANT_ROLE.to_string(),
                    content: vec![ContentItem::OutputText {
                        text: format!("Agent message from {author}:\n{text}"),
                    }],
                    phase: None,
                    internal_chat_message_metadata_passthrough: metadata.clone(),
                });
            }
        }
        ResponseItem::Message {
            id: _,
            role,
            content,
            phase,
            internal_chat_message_metadata_passthrough: metadata,
        } if role == USER_ROLE
            && matches!(parse_turn_item(item), Some(TurnItem::UserMessage(_))) =>
        {
            // Direct input marks each typed text item, and that text stays even when it
            // looks like a context wrapper. Only unmarked items, such as legacy rollouts
            // that stored context as user messages, are dropped for their wrapper shape.
            let kinds = metadata
                .as_ref()
                .and_then(|metadata| metadata.content_item_kinds.as_deref())
                .filter(|kinds| kinds.len() == content.len());
            let content = content
                .iter()
                .enumerate()
                .filter(|(index, item)| {
                    matches!(item, ContentItem::InputText { .. })
                        && (kinds.is_some_and(|kinds| kinds[*index].0 == DIRECT_USER_TEXT_KIND)
                            || !is_contextual_user_fragment(item))
                })
                .map(|(_, item)| item.clone())
                .collect::<Vec<_>>();
            if !content.is_empty() {
                messages.push(ResponseItem::Message {
                    id: None,
                    role: role.clone(),
                    content,
                    phase: phase.clone(),
                    internal_chat_message_metadata_passthrough: metadata.clone(),
                });
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use codex_api::SearchInput;
    use codex_protocol::ResponseItemId;
    use codex_protocol::models::ContentItem;
    use codex_protocol::models::ContentItemKind;
    use codex_protocol::models::InternalChatMessageMetadataPassthrough;
    use codex_protocol::models::ResponseItem;
    use pretty_assertions::assert_eq;

    use super::ASSISTANT_ROLE;
    use super::DIRECT_USER_TEXT_KIND;
    use super::USER_ROLE;
    use super::recent_input;

    fn message(role: &str, text: &str) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: role.to_string(),
            content: vec![if role == ASSISTANT_ROLE {
                ContentItem::OutputText {
                    text: text.to_string(),
                }
            } else {
                ContentItem::InputText {
                    text: text.to_string(),
                }
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        }
    }

    #[test]
    fn keeps_current_user_and_previous_visible_turn() {
        let mut previous_user = message(USER_ROLE, "previous user");
        previous_user.set_id(Some(ResponseItemId::with_suffix("msg", "previous_user")));
        let mut previous_assistant = message(ASSISTANT_ROLE, "previous assistant");
        previous_assistant.set_id(Some(ResponseItemId::with_suffix(
            "msg",
            "previous_assistant",
        )));
        let items = vec![
            message("system", "system"),
            message(USER_ROLE, "old user"),
            message(ASSISTANT_ROLE, "old assistant"),
            previous_user,
            ResponseItem::FunctionCall {
                id: None,
                name: "tool".to_string(),
                namespace: None,
                arguments: "{}".to_string(),
                call_id: "call-1".to_string(),
                encrypted_function_args: None,
                internal_chat_message_metadata_passthrough: None,
            },
            previous_assistant,
            message("developer", "developer"),
            message(USER_ROLE, "current user"),
            message(ASSISTANT_ROLE, "current commentary"),
        ];

        assert_eq!(
            recent_input(&items),
            Some(SearchInput::Items(vec![
                message(USER_ROLE, "previous user"),
                message(ASSISTANT_ROLE, "previous assistant"),
                message(USER_ROLE, "current user"),
            ]))
        );
    }

    #[test]
    fn keeps_only_text_from_recent_user_messages() {
        let previous_user = ResponseItem::Message {
            id: None,
            role: USER_ROLE.to_string(),
            content: vec![
                ContentItem::InputText {
                    text: "previous user".to_string(),
                },
                ContentItem::InputImage {
                    image_url: "data:image/png;base64,image".to_string(),
                    detail: None,
                },
            ],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        let items = vec![
            previous_user,
            message(ASSISTANT_ROLE, "previous assistant"),
            message(USER_ROLE, "current user"),
        ];

        assert_eq!(
            recent_input(&items),
            Some(SearchInput::Items(vec![
                message(USER_ROLE, "previous user"),
                message(ASSISTANT_ROLE, "previous assistant"),
                message(USER_ROLE, "current user"),
            ]))
        );
    }

    #[test]
    fn ignores_contextual_user_messages_when_selecting_recent_turns() {
        let items = vec![
            message(USER_ROLE, "previous user"),
            message(ASSISTANT_ROLE, "previous assistant"),
            message(
                USER_ROLE,
                "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>",
            ),
            message(USER_ROLE, "current user"),
        ];

        assert_eq!(
            recent_input(&items),
            Some(SearchInput::Items(vec![
                message(USER_ROLE, "previous user"),
                message(ASSISTANT_ROLE, "previous assistant"),
                message(USER_ROLE, "current user"),
            ]))
        );
    }

    fn user_texts(texts: &[&str], kinds: Option<Vec<ContentItemKind>>) -> ResponseItem {
        ResponseItem::Message {
            id: None,
            role: USER_ROLE.to_string(),
            content: texts
                .iter()
                .map(|text| ContentItem::InputText {
                    text: (*text).to_string(),
                })
                .collect(),
            phase: None,
            internal_chat_message_metadata_passthrough: kinds.map(|kinds| {
                InternalChatMessageMetadataPassthrough {
                    content_item_kinds: Some(kinds),
                    ..Default::default()
                }
            }),
        }
    }

    #[test]
    fn keeps_direct_user_text_that_looks_like_context() {
        let wrapper = "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>";
        let direct_kinds = vec![ContentItemKind(DIRECT_USER_TEXT_KIND.to_string()); 2];
        let current_user = user_texts(&["why does this cwd fail?", wrapper], Some(direct_kinds));
        let items = vec![
            message(USER_ROLE, "previous user"),
            message(ASSISTANT_ROLE, "previous assistant"),
            current_user.clone(),
        ];

        assert_eq!(
            recent_input(&items),
            Some(SearchInput::Items(vec![
                message(USER_ROLE, "previous user"),
                message(ASSISTANT_ROLE, "previous assistant"),
                current_user,
            ]))
        );
    }

    #[test]
    fn drops_only_context_shaped_text_without_direct_user_provenance() {
        let wrapper = "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>";
        let items = vec![
            message(USER_ROLE, "previous user"),
            message(ASSISTANT_ROLE, "previous assistant"),
            user_texts(&["current user", wrapper], /*kinds*/ None),
        ];

        assert_eq!(
            recent_input(&items),
            Some(SearchInput::Items(vec![
                message(USER_ROLE, "previous user"),
                message(ASSISTANT_ROLE, "previous assistant"),
                message(USER_ROLE, "current user"),
            ]))
        );
    }
}
