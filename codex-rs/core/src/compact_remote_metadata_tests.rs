use super::*;
use codex_history::CodexHarnessMetadata;
use codex_protocol::items::HookPromptFragment;
use codex_protocol::items::build_hook_prompt_message;
use codex_protocol::models::ContentItem;

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
        std::slice::from_ref(&direct_user),
    );

    assert_eq!(filtered, vec![direct_user, assistant]);
}
