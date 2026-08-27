use super::AgentControl;
use crate::codex_thread::GuardianAuthorizationVersion;
use crate::codex_thread::GuardianRootMessage;
use crate::codex_thread::GuardianRootSnapshot;
use crate::compact::is_summary_message;
use crate::context::GuardianReviewEvidence;
use crate::context::is_contextual_user_fragment;
use crate::event_mapping::parse_turn_item;
use crate::guardian::guardian_truncate_text;
use codex_protocol::AgentPath;
use codex_protocol::ThreadId;
use codex_protocol::items::AgentMessageContent;
use codex_protocol::items::TurnItem;
use codex_protocol::models::MessagePhase;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::MultiAgentVersion;
use std::borrow::Cow;

const MAX_ROOT_MESSAGES: usize = 8;
const MAX_ROOT_MESSAGE_TOKENS: usize = 900;

fn root_authorization_item<'a>(item: &'a ResponseItem) -> Option<Cow<'a, ResponseItem>> {
    let ResponseItem::Message {
        id,
        role,
        content,
        phase,
        internal_chat_message_metadata_passthrough,
    } = item
    else {
        return Some(Cow::Borrowed(item));
    };
    if role != "user" {
        return Some(Cow::Borrowed(item));
    }

    let filtered_content = content
        .iter()
        .filter(|content| !is_contextual_user_fragment(content))
        .cloned()
        .collect::<Vec<_>>();
    if filtered_content.is_empty() {
        return None;
    }
    if filtered_content.len() == content.len() {
        return Some(Cow::Borrowed(item));
    }
    Some(Cow::Owned(ResponseItem::Message {
        id: id.clone(),
        role: role.clone(),
        content: filtered_content,
        phase: phase.clone(),
        internal_chat_message_metadata_passthrough: internal_chat_message_metadata_passthrough
            .clone(),
    }))
}

impl AgentControl {
    /// Returns bounded root conversation and authorization state for a MultiAgent V2 worker.
    pub(crate) async fn root_user_authorization(
        &self,
        thread_id: ThreadId,
    ) -> Option<GuardianRootSnapshot> {
        let root_thread_id = self.state.agent_id_for_path(&AgentPath::root())?;
        if root_thread_id == thread_id {
            return None;
        }
        let manager = self.upgrade().ok()?;
        let root_thread = manager.get_thread(root_thread_id).await.ok()?;
        if root_thread.multi_agent_version() != Some(MultiAgentVersion::V2) {
            return None;
        }

        let root_history = root_thread.session.clone_history().await;
        let root_evidence = root_thread
            .session
            .services
            .thread_extension_data
            .get::<GuardianReviewEvidence>();
        let mut messages = root_history
            .raw_items()
            .filter_map(root_authorization_item)
            .filter_map(
                |item| match (parse_turn_item(item.as_ref()), item.as_ref()) {
                    (Some(TurnItem::UserMessage(message)), _) => {
                        let message = message.message();
                        (!is_summary_message(&message)
                            && !message.trim_start().starts_with("<user_action>"))
                        .then(|| {
                            GuardianRootMessage::User(
                                guardian_truncate_text(&message, MAX_ROOT_MESSAGE_TOKENS).0,
                            )
                        })
                    }
                    (Some(TurnItem::AgentMessage(message)), _)
                        if matches!(message.phase, None | Some(MessagePhase::FinalAnswer)) =>
                    {
                        let text = message
                            .content
                            .iter()
                            .map(|content| match content {
                                AgentMessageContent::Text { text } => text.as_str(),
                            })
                            .collect::<String>();
                        Some(GuardianRootMessage::Assistant(
                            guardian_truncate_text(&text, MAX_ROOT_MESSAGE_TOKENS).0,
                        ))
                    }
                    (_, ResponseItem::FunctionCall { call_id, .. }) => root_evidence
                        .as_ref()
                        .and_then(|evidence| evidence.user_input_for_call(call_id))
                        .map(GuardianRootMessage::UserInput),
                    _ => None,
                },
            )
            .collect::<Vec<_>>();

        messages.drain(..messages.len().saturating_sub(MAX_ROOT_MESSAGES));
        let history = root_history.conversation_history_snapshot();
        let authorization_version = root_evidence.as_ref().map_or_else(
            || GuardianAuthorizationVersion::from_history(history.as_ref()),
            |evidence| evidence.authorization_version(history.as_ref()),
        );
        Some(GuardianRootSnapshot {
            authorization_version,
            messages,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context_manager::ContextManager;
    use codex_protocol::models::ContentItem;
    use codex_utils_output_truncation::TruncationPolicy;

    #[test]
    fn contextual_fragments_are_removed_without_dropping_direct_user_input() {
        let mixed = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![
                ContentItem::InputText {
                    text: "direct-looking text".to_string(),
                },
                ContentItem::InputText {
                    text: "<environment_context>\n<cwd>/tmp</cwd>\n</environment_context>"
                        .to_string(),
                },
            ],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        let direct = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "deploy the reviewed change".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        let all_contextual = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "<environment_context>context</environment_context>".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };

        let sanitized = root_authorization_item(&mixed).expect("direct content should remain");
        let Some(TurnItem::UserMessage(message)) = parse_turn_item(sanitized.as_ref()) else {
            panic!("sanitized direct content should parse as a user message");
        };
        assert_eq!(message.message(), "direct-looking text");
        assert!(root_authorization_item(&direct).is_some());
        assert!(root_authorization_item(&all_contextual).is_none());
    }
    #[test]
    fn contextual_root_user_items_advance_authorization_version() {
        let contextual = ResponseItem::Message {
            id: None,
            role: "user".to_string(),
            content: vec![ContentItem::InputText {
                text: "<environment_context>context</environment_context>".to_string(),
            }],
            phase: None,
            internal_chat_message_metadata_passthrough: None,
        };
        let mut history = ContextManager::new();
        history.record_items(std::iter::once(&contextual), TruncationPolicy::Tokens(128));

        let version = GuardianAuthorizationVersion::from_history(
            history.conversation_history_snapshot().as_ref(),
        );
        assert_eq!(version.history_version, 0);
        assert_eq!(version.user_message_count, 1);
    }
}
