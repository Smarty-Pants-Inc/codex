use super::*;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::ImageReference;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;
use codex_protocol::protocol::RawResponseItemEvent;
use pretty_assertions::assert_eq;

#[test]
fn admission_requires_exact_turn_role_text_and_media_slots() {
    let text = ContentItem::InputText {
        text: "bound review evidence".to_string(),
    };
    let image = ContentItem::InputImage {
        image: ImageReference::Inline {
            image_url: "data:image/png;base64,original".to_string(),
        },
        detail: None,
    };
    let prepared_image = ContentItem::InputImage {
        image: ImageReference::Inline {
            image_url: "data:image/png;base64,prepared".to_string(),
        },
        detail: None,
    };
    let error = ContentItem::InputText {
        text: "image preparation failed".to_string(),
    };
    let pending = PendingNodeReplEvidenceAdmission {
        turn_id: "review-turn".to_string(),
        response_sequence: 7,
        content: vec![text.clone(), image.clone()],
    };
    for (name, event_turn, item_turn, role, content, kinds, expected) in [
        (
            "recorded",
            "review-turn",
            "review-turn",
            "developer",
            vec![text.clone(), image.clone()],
            vec!["unknown", "unknown"],
            true,
        ),
        (
            "prepared image",
            "review-turn",
            "review-turn",
            "developer",
            vec![text.clone(), prepared_image],
            vec!["unknown", "unknown"],
            true,
        ),
        (
            "typed image error",
            "review-turn",
            "review-turn",
            "developer",
            vec![text.clone(), error.clone()],
            vec!["unknown", "images.preparation_error"],
            true,
        ),
        (
            "untyped image error",
            "review-turn",
            "review-turn",
            "developer",
            vec![text.clone(), error.clone()],
            vec!["unknown", "unknown"],
            false,
        ),
        (
            "wrong event turn",
            "other-turn",
            "review-turn",
            "developer",
            vec![text.clone(), image.clone()],
            vec!["unknown", "unknown"],
            false,
        ),
        (
            "wrong recorded turn",
            "review-turn",
            "other-turn",
            "developer",
            vec![text.clone(), image.clone()],
            vec!["unknown", "unknown"],
            false,
        ),
        (
            "wrong role",
            "review-turn",
            "review-turn",
            "user",
            vec![text.clone(), image.clone()],
            vec!["unknown", "unknown"],
            false,
        ),
        (
            "unrelated text",
            "review-turn",
            "review-turn",
            "developer",
            vec![error.clone(), image.clone()],
            vec!["unknown", "unknown"],
            false,
        ),
        (
            "missing image",
            "review-turn",
            "review-turn",
            "developer",
            vec![text.clone()],
            vec!["unknown"],
            false,
        ),
        (
            "inserted text",
            "review-turn",
            "review-turn",
            "developer",
            vec![text.clone(), error, image.clone()],
            vec!["unknown", "unknown", "unknown"],
            false,
        ),
        (
            "reordered",
            "review-turn",
            "review-turn",
            "developer",
            vec![image, text],
            vec!["unknown", "unknown"],
            false,
        ),
    ] {
        let event = Event {
            id: event_turn.to_string(),
            msg: EventMsg::RawResponseItem(RawResponseItemEvent {
                item: ResponseItem::Message {
                    id: None,
                    role: role.to_string(),
                    content,
                    phase: None,
                    internal_chat_message_metadata_passthrough: Some(
                        InternalChatMessageMetadataPassthrough {
                            turn_id: Some(item_turn.to_string()),
                            content_item_kinds: Some(
                                kinds
                                    .into_iter()
                                    .map(|kind| ContentItemKind(kind.to_string()))
                                    .collect(),
                            ),
                            ..Default::default()
                        },
                    ),
                },
            }),
        };
        assert_eq!(pending.matches(&event), expected, "{name}");
    }
}
