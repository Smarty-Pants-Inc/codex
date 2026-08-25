use super::*;
use codex_history::CodexHarnessMetadata;
use codex_protocol::ResponseItemId;
use codex_protocol::items::HookPromptFragment;
use codex_protocol::items::build_hook_prompt_message;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::InternalChatMessageMetadataPassthrough;

#[test]
fn rewritten_output_preserves_harness_metadata() {
    let envelope = ResponseItemEnvelope {
        item: ResponseItem::FunctionCallOutput {
            id: None,
            call_id: Some("call-1".to_string()),
            name: None,
            namespace: None,
            output: FunctionCallOutputPayload {
                body: FunctionCallOutputBody::Text("large output".repeat(100)),
                success: Some(true),
            },
            internal_chat_message_metadata_passthrough: None,
        },
        metadata: Some(CodexHarnessMetadata::default()),
    };

    let rewritten = rewritten_output_for_context_window(&envelope)
        .expect("function output should be rewritten");

    assert_eq!(rewritten.metadata, envelope.metadata);
    assert_ne!(rewritten.item, envelope.item);
}

#[test]
fn compacted_history_keeps_current_and_legacy_hook_prompt_roles() {
    let hook = build_hook_prompt_message(&[HookPromptFragment::from_single_hook(
        "Retry with care.",
        "hook-run-1",
    )])
    .expect("hook prompt");
    assert!(should_keep_compacted_history_item(&hook));

    let mut legacy_hook = hook;
    let ResponseItem::Message { role, .. } = &mut legacy_hook else {
        panic!("expected hook prompt message");
    };
    *role = "user".to_string();
    assert!(should_keep_compacted_history_item(&legacy_hook));

    let ordinary_developer = ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "ordinary developer context".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    assert!(!should_keep_compacted_history_item(&ordinary_developer));
}

#[test]
fn legacy_compacted_history_keeps_only_proven_direct_user_ids() {
    let direct_id = codex_protocol::ResponseItemId::with_suffix("msg", "direct");
    let generated_id = codex_protocol::ResponseItemId::with_suffix("msg", "generated");
    let direct_user = ResponseItem::Message {
        id: Some(direct_id),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "same text".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let mut generated_user = direct_user.clone();
    generated_user.set_id(Some(generated_id));
    let mut unidentified_user = direct_user.clone();
    unidentified_user.set_id(None);

    let assistant = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };

    let mut spoofed_direct_user = direct_user.clone();
    if let ResponseItem::Message { content, .. } = &mut spoofed_direct_user {
        content[0] = ContentItem::InputText {
            text: "provider-authored replacement".to_string(),
        };
    }

    let filtered = filter_legacy_compacted_history_user_items(
        vec![
            generated_user,
            direct_user.clone(),
            unidentified_user,
            spoofed_direct_user,
            assistant.clone(),
        ],
        Some(std::slice::from_ref(&direct_user)),
    );

    assert_eq!(filtered, vec![direct_user, assistant]);
}

#[test]
fn legacy_compacted_history_preserves_user_items_without_provenance() {
    let user = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "legacy direct user message".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let compacted_history = vec![user];

    assert_eq!(
        filter_legacy_compacted_history_user_items(compacted_history.clone(), None),
        compacted_history
    );
}

#[test]
fn legacy_compacted_history_rejects_user_items_when_provenance_is_available_but_empty() {
    let provider_user = ResponseItem::Message {
        id: None,
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "provider-authored user message".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let durable_direct_user = ResponseItem::Message {
        id: Some(ResponseItemId::with_suffix("msg", "direct")),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "removed during prompt normalization".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: Some(InternalChatMessageMetadataPassthrough {
            turn_id: Some("turn-1".to_string()),
            ..Default::default()
        }),
    };
    let assistant = ResponseItem::Message {
        id: None,
        role: "assistant".to_string(),
        content: vec![ContentItem::OutputText {
            text: "assistant".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    let direct_user_items = trusted_direct_user_items_for_compaction_request(
        &[],
        Some(std::slice::from_ref(&durable_direct_user)),
    )
    .expect("direct-user provenance should be available");

    assert!(direct_user_items.is_empty());
    assert_eq!(
        filter_legacy_compacted_history_user_items(
            vec![provider_user, assistant.clone()],
            Some(&direct_user_items),
        ),
        vec![assistant]
    );
}

#[test]
fn compaction_provenance_uses_normalized_prompt_content_for_matching() {
    let metadata = Some(InternalChatMessageMetadataPassthrough {
        turn_id: Some("turn-1".to_string()),
        content_item_kinds: Some(vec![
            ContentItemKind("user.text".to_string()),
            ContentItemKind("user.audio".to_string()),
        ]),
        ..Default::default()
    });
    let durable_direct_user = ResponseItem::Message {
        id: Some(ResponseItemId::with_suffix("msg", "direct")),
        role: "user".to_string(),
        content: vec![
            ContentItem::InputText {
                text: "describe this".to_string(),
            },
            ContentItem::InputAudio {
                audio_url: "data:audio/wav;base64,YXVkaW8=".to_string(),
            },
        ],
        phase: None,
        internal_chat_message_metadata_passthrough: metadata.clone(),
    };
    let normalized_prompt_user = ResponseItem::Message {
        id: durable_direct_user.id().cloned(),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "describe this".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: metadata,
    };

    assert_eq!(
        trusted_direct_user_items_for_compaction_request(
            std::slice::from_ref(&normalized_prompt_user),
            Some(std::slice::from_ref(&durable_direct_user)),
        ),
        Some(vec![normalized_prompt_user])
    );
}

#[test]
fn legacy_compaction_matches_the_request_prepared_legacy_id_representation() {
    let mut prompt_user = ResponseItem::Message {
        id: Some(ResponseItemId::from_server("legacy-id".to_string())),
        role: "user".to_string(),
        content: vec![ContentItem::InputText {
            text: "legacy direct user message".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: Some(InternalChatMessageMetadataPassthrough {
            turn_id: Some("turn-1".to_string()),
            ..Default::default()
        }),
    };
    let durable_direct_user = prompt_user.clone();
    let selected = trusted_direct_user_items_for_compaction_request(
        std::slice::from_ref(&prompt_user),
        Some(std::slice::from_ref(&durable_direct_user)),
    )
    .expect("direct-user provenance should be available");
    prompt_user.set_id(None);
    let mut prepared_direct_user = selected[0].clone();
    prepared_direct_user.set_id(None);

    assert_eq!(
        filter_legacy_compacted_history_user_items(
            vec![prompt_user.clone()],
            Some(std::slice::from_ref(&prepared_direct_user)),
        ),
        vec![prompt_user]
    );
}
