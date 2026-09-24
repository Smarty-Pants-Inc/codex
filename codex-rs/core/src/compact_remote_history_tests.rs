use super::*;
use crate::compact_remote_v2::is_retained_for_remote_compaction_v2;
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
    assert!(is_retained_for_remote_compaction_v2(
        &ResponseItemEnvelope::new(hook.clone()),
        /*retain_client_developer_messages*/ false,
    ));

    let mut legacy_hook = hook;
    let ResponseItem::Message { role, .. } = &mut legacy_hook else {
        panic!("expected hook prompt message");
    };
    *role = "user".to_string();
    assert!(is_retained_for_remote_compaction_v2(
        &ResponseItemEnvelope::new(legacy_hook),
        /*retain_client_developer_messages*/ false,
    ));

    let ordinary_developer = ResponseItem::Message {
        id: None,
        role: "developer".to_string(),
        content: vec![ContentItem::InputText {
            text: "ordinary developer context".to_string(),
        }],
        phase: None,
        internal_chat_message_metadata_passthrough: None,
    };
    assert!(!is_retained_for_remote_compaction_v2(
        &ResponseItemEnvelope::new(ordinary_developer),
        /*retain_client_developer_messages*/ false,
    ));
}
