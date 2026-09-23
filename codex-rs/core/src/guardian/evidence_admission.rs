use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;

/// Acknowledges live-history recording on the owning review session's event stream.
/// This does not establish durable persistence or successful image preparation.
pub(super) struct PendingNodeReplEvidenceAdmission {
    pub(super) turn_id: String,
    pub(super) response_sequence: u64,
    pub(super) content: Vec<ContentItem>,
}

impl PendingNodeReplEvidenceAdmission {
    pub(super) fn matches(&self, event: &Event) -> bool {
        let EventMsg::RawResponseItem(recorded) = &event.msg else {
            return false;
        };
        let ResponseItem::Message {
            role,
            content,
            internal_chat_message_metadata_passthrough,
            ..
        } = &recorded.item
        else {
            return false;
        };
        if event.id != self.turn_id
            || recorded.item.turn_id() != Some(self.turn_id.as_str())
            || role != "developer"
            || content.len() != self.content.len()
            || content.is_empty()
        {
            return false;
        }
        self.content
            .iter()
            .zip(content)
            .enumerate()
            .all(|(index, (submitted, recorded))| match submitted {
                ContentItem::InputImage { .. } => match recorded {
                    ContentItem::InputImage { .. } => true,
                    ContentItem::InputText { .. } => internal_chat_message_metadata_passthrough
                        .as_ref()
                        .and_then(|metadata| metadata.content_item_kinds.as_ref())
                        .and_then(|kinds| kinds.get(index))
                        .is_some_and(|kind| kind.0 == "images.preparation_error"),
                    ContentItem::InputAudio { .. } | ContentItem::OutputText { .. } => false,
                },
                ContentItem::InputText { .. }
                | ContentItem::InputAudio { .. }
                | ContentItem::OutputText { .. } => submitted == recorded,
            })
    }
}

#[cfg(test)]
#[path = "evidence_admission_tests.rs"]
mod tests;
