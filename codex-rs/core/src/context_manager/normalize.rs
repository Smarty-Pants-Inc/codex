use codex_context_fragments::set_annotated_content;
use codex_context_fragments::to_annotated_content;
use codex_history::ResponseItemEnvelope;
use codex_protocol::ResponseItemId;
use codex_protocol::models::ContentItem;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::InputModality;
use std::collections::HashSet;
use uuid::Uuid;

use crate::context::ContextualUserFragment;
use crate::context::UnsupportedMedia;

use crate::util::error_or_panic;
use tracing::info;

const IMAGE_CONTENT_OMITTED_PLACEHOLDER: &str =
    "image content omitted because you do not support image input";
const AUDIO_CONTENT_OMITTED_PLACEHOLDER: &str =
    "audio content omitted because you do not support audio input";
// Changing this value would change model-visible IDs and invalidate prompt caches.
const SYNTHETIC_OUTPUT_ID_NAMESPACE: Uuid = Uuid::from_u128(0x90d38d3e_6a5b_4d52_bfe2_2f1e634bfac4);

pub(crate) fn ensure_call_outputs_present(items: &mut Vec<ResponseItemEnvelope>) {
    let mut function_output_ids = HashSet::new();
    let mut tool_search_output_ids = HashSet::new();
    let mut custom_tool_output_ids = HashSet::new();
    for envelope in items.iter() {
        match &envelope.item {
            ResponseItem::FunctionCallOutput {
                call_id: Some(call_id),
                ..
            } => {
                function_output_ids.insert(call_id.as_str());
            }
            ResponseItem::ToolSearchOutput {
                call_id: Some(call_id),
                ..
            } => {
                tool_search_output_ids.insert(call_id.as_str());
            }
            ResponseItem::CustomToolCallOutput { call_id, .. } => {
                custom_tool_output_ids.insert(call_id.as_str());
            }
            _ => {}
        }
    }

    // Collect synthetic outputs to insert immediately after their calls.
    // Store the insertion position (index of call) alongside the item so
    // we can insert in reverse order and avoid index shifting.
    let mut missing_outputs_to_insert: Vec<(usize, ResponseItemEnvelope)> = Vec::new();

    for (idx, envelope) in items.iter().enumerate() {
        match &envelope.item {
            ResponseItem::FunctionCall { id, call_id, .. }
                if !function_output_ids.contains(call_id.as_str()) =>
            {
                info!("Function call output is missing for call id: {call_id}");
                missing_outputs_to_insert.push((
                    idx,
                    ResponseItemEnvelope::new(ResponseItem::FunctionCallOutput {
                        id: synthetic_output_id("fco", id.as_deref()),
                        call_id: Some(call_id.clone()),
                        name: None,
                        namespace: None,
                        output: FunctionCallOutputPayload::from_text("aborted".to_string()),
                        internal_chat_message_metadata_passthrough: None,
                    }),
                ));
            }
            ResponseItem::ToolSearchCall {
                id,
                call_id: Some(call_id),
                ..
            } if !tool_search_output_ids.contains(call_id.as_str()) => {
                info!("Tool search output is missing for call id: {call_id}");
                missing_outputs_to_insert.push((
                    idx,
                    ResponseItemEnvelope::new(ResponseItem::ToolSearchOutput {
                        id: synthetic_output_id("tso", id.as_deref()),
                        call_id: Some(call_id.clone()),
                        status: "completed".to_string(),
                        execution: "client".to_string(),
                        tools: Vec::new(),
                        internal_chat_message_metadata_passthrough: None,
                    }),
                ));
            }
            ResponseItem::CustomToolCall { id, call_id, .. }
                if !custom_tool_output_ids.contains(call_id.as_str()) =>
            {
                error_or_panic(format!(
                    "Custom tool call output is missing for call id: {call_id}"
                ));
                missing_outputs_to_insert.push((
                    idx,
                    ResponseItemEnvelope::new(ResponseItem::CustomToolCallOutput {
                        id: synthetic_output_id("ctco", id.as_deref()),
                        call_id: call_id.clone(),
                        name: None,
                        output: FunctionCallOutputPayload::from_text("aborted".to_string()),
                        internal_chat_message_metadata_passthrough: None,
                    }),
                ));
            }
            // LocalShellCall is represented in upstream streams by a FunctionCallOutput
            ResponseItem::LocalShellCall {
                id,
                call_id: Some(call_id),
                ..
            } if !function_output_ids.contains(call_id.as_str()) => {
                error_or_panic(format!(
                    "Local shell call output is missing for call id: {call_id}"
                ));
                missing_outputs_to_insert.push((
                    idx,
                    ResponseItemEnvelope::new(ResponseItem::FunctionCallOutput {
                        id: synthetic_output_id("fco", id.as_deref()),
                        call_id: Some(call_id.clone()),
                        name: None,
                        namespace: None,
                        output: FunctionCallOutputPayload::from_text("aborted".to_string()),
                        internal_chat_message_metadata_passthrough: None,
                    }),
                ));
            }
            _ => {}
        }
    }
    drop((
        function_output_ids,
        tool_search_output_ids,
        custom_tool_output_ids,
    ));

    // Insert synthetic outputs in reverse index order to avoid re-indexing.
    for (idx, output_item) in missing_outputs_to_insert.into_iter().rev() {
        items.insert(idx + 1, output_item);
    }
}

/// Derives a stable ID for a prompt-only output from its source call's item ID.
///
/// Prompt normalization can run repeatedly without persisting its synthetic
/// outputs, so the namespace and name format must remain stable across retries
/// and resumes to preserve prompt-cache reuse. Returning `None` when the source
/// call has no ID preserves the legacy behavior for older history items.
fn synthetic_output_id(prefix: &str, item_id: Option<&str>) -> Option<ResponseItemId> {
    let source_id = item_id.filter(|id| !id.is_empty())?;
    let name = format!("{prefix}:{source_id}");
    Some(ResponseItemId::with_suffix(
        prefix,
        Uuid::new_v5(&SYNTHETIC_OUTPUT_ID_NAMESPACE, name.as_bytes()),
    ))
}

pub(crate) fn remove_orphan_outputs(items: &mut Vec<ResponseItemEnvelope>) {
    let mut function_call_ids = HashSet::new();
    let mut tool_search_call_ids = HashSet::new();
    let mut custom_tool_call_ids = HashSet::new();
    for envelope in items.iter() {
        match &envelope.item {
            ResponseItem::FunctionCall { call_id, .. }
            | ResponseItem::LocalShellCall {
                call_id: Some(call_id),
                ..
            } => {
                function_call_ids.insert(call_id.as_str());
            }
            ResponseItem::ToolSearchCall {
                call_id: Some(call_id),
                ..
            } => {
                tool_search_call_ids.insert(call_id.as_str());
            }
            ResponseItem::CustomToolCall { call_id, .. } => {
                custom_tool_call_ids.insert(call_id.as_str());
            }
            _ => {}
        }
    }

    let mut orphan_positions = Vec::new();
    for (position, envelope) in items.iter().enumerate() {
        match &envelope.item {
            ResponseItem::FunctionCallOutput {
                call_id: Some(call_id),
                ..
            } if !function_call_ids.contains(call_id.as_str()) => {
                error_or_panic(format!(
                    "Orphan function call output for call id: {call_id}"
                ));
                orphan_positions.push(position);
            }
            ResponseItem::CustomToolCallOutput { call_id, .. }
                if !custom_tool_call_ids.contains(call_id.as_str()) =>
            {
                error_or_panic(format!(
                    "Orphan custom tool call output for call id: {call_id}"
                ));
                orphan_positions.push(position);
            }
            ResponseItem::ToolSearchOutput {
                call_id: Some(call_id),
                execution,
                ..
            } if execution != "server" && !tool_search_call_ids.contains(call_id.as_str()) => {
                error_or_panic(format!("Orphan tool search output for call id: {call_id}"));
                orphan_positions.push(position);
            }
            _ => {}
        }
    }

    if !orphan_positions.is_empty() {
        let mut orphan_positions = orphan_positions.into_iter().peekable();
        let mut position = 0;
        items.retain(|_| {
            let retain = orphan_positions.peek() != Some(&position);
            if !retain {
                orphan_positions.next();
            }
            position += 1;
            retain
        });
    }
}

pub(crate) fn remove_corresponding_for(items: &mut Vec<ResponseItemEnvelope>, item: &ResponseItem) {
    match item {
        ResponseItem::FunctionCall { call_id, .. } => {
            remove_first_matching(items, |i| {
                matches!(
                    i,
                    ResponseItem::FunctionCallOutput {
                        call_id: Some(existing),
                        ..
                    } if existing == call_id
                )
            });
        }
        ResponseItem::FunctionCallOutput {
            call_id: Some(call_id),
            ..
        } => {
            if let Some(pos) = items.iter().position(|envelope| {
                matches!(&envelope.item, ResponseItem::FunctionCall { call_id: existing, .. } if existing == call_id)
            }) {
                items.remove(pos);
            } else if let Some(pos) = items.iter().position(|envelope| {
                matches!(&envelope.item, ResponseItem::LocalShellCall { call_id: Some(existing), .. } if existing == call_id)
            }) {
                items.remove(pos);
            }
        }
        ResponseItem::ToolSearchCall {
            call_id: Some(call_id),
            ..
        } => {
            remove_first_matching(items, |i| {
                matches!(
                    i,
                    ResponseItem::ToolSearchOutput {
                        call_id: Some(existing),
                        ..
                    } if existing == call_id
                )
            });
        }
        ResponseItem::ToolSearchOutput {
            call_id: Some(call_id),
            ..
        } => {
            remove_first_matching(
                items,
                |i| {
                    matches!(
                        i,
                        ResponseItem::ToolSearchCall {
                            call_id: Some(existing),
                            ..
                        } if existing == call_id
                    )
                },
            );
        }
        ResponseItem::CustomToolCall { call_id, .. } => {
            remove_first_matching(items, |i| {
                matches!(
                    i,
                    ResponseItem::CustomToolCallOutput {
                        call_id: existing, ..
                    } if existing == call_id
                )
            });
        }
        ResponseItem::CustomToolCallOutput { call_id, .. } => {
            remove_first_matching(
                items,
                |i| matches!(i, ResponseItem::CustomToolCall { call_id: existing, .. } if existing == call_id),
            );
        }
        ResponseItem::LocalShellCall {
            call_id: Some(call_id),
            ..
        } => {
            remove_first_matching(items, |i| {
                matches!(
                    i,
                    ResponseItem::FunctionCallOutput {
                        call_id: Some(existing),
                        ..
                    } if existing == call_id
                )
            });
        }
        _ => {}
    }
}

fn remove_first_matching<F>(items: &mut Vec<ResponseItemEnvelope>, predicate: F)
where
    F: Fn(&ResponseItem) -> bool,
{
    if let Some(pos) = items.iter().position(|envelope| predicate(&envelope.item)) {
        items.remove(pos);
    }
}

/// Strip unsupported media from messages and tool outputs.
///
/// Both public entry points use the same idempotent pass because a user message can contain both
/// media types, and generated notices must retain their source order.
pub(crate) fn strip_images_when_unsupported(
    input_modalities: &[InputModality],
    items: &mut Vec<ResponseItemEnvelope>,
) {
    strip_unsupported_media(input_modalities, items);
}

/// Strip unsupported media from messages and tool outputs.
pub(crate) fn strip_audio_when_unsupported(
    input_modalities: &[InputModality],
    items: &mut Vec<ResponseItemEnvelope>,
) {
    strip_unsupported_media(input_modalities, items);
}

fn strip_unsupported_media(
    input_modalities: &[InputModality],
    items: &mut Vec<ResponseItemEnvelope>,
) {
    let supports_images = input_modalities.contains(&InputModality::Image);
    let supports_audio = input_modalities.contains(&InputModality::Audio);
    if supports_images && supports_audio {
        return;
    }

    let mut normalized_items = Vec::with_capacity(items.len());
    for mut envelope in std::mem::take(items) {
        let message_state = match &envelope.item {
            ResponseItem::Message { role, content, .. } => Some((
                role == "user",
                content.iter().any(|content_item| match content_item {
                    ContentItem::InputImage { .. } => !supports_images,
                    ContentItem::InputAudio { .. } => !supports_audio,
                    _ => false,
                }),
            )),
            _ => None,
        };
        let (omitted_content, omitted_content_has_kinds) = match message_state {
            Some((_, false)) => (Vec::new(), false),
            Some((direct_user_message, true)) => {
                let original_metadata = match &envelope.item {
                    ResponseItem::Message {
                        internal_chat_message_metadata_passthrough,
                        ..
                    } => internal_chat_message_metadata_passthrough.clone(),
                    _ => None,
                };
                let has_content_item_kinds = original_metadata
                    .as_ref()
                    .and_then(|metadata| metadata.content_item_kinds.as_ref())
                    .is_some();
                let Some(annotated_content) = to_annotated_content(&mut envelope.item) else {
                    error_or_panic("message changed shape during media normalization");
                    normalized_items.push(envelope);
                    continue;
                };
                let mut normalized_content = Vec::with_capacity(annotated_content.len());
                let mut omitted_content = Vec::new();
                for annotated in annotated_content {
                    let unsupported_media = match annotated.content() {
                        ContentItem::InputImage { .. } if !supports_images => {
                            Some(UnsupportedMedia::IMAGE)
                        }
                        ContentItem::InputAudio { .. } if !supports_audio => {
                            Some(UnsupportedMedia::AUDIO)
                        }
                        _ => None,
                    };
                    let Some(unsupported_media) = unsupported_media else {
                        normalized_content.push(annotated);
                        continue;
                    };
                    let (_, replacement) = unsupported_media.render_fragment().into_parts();
                    if direct_user_message {
                        omitted_content.push(replacement);
                    } else {
                        normalized_content.push(replacement);
                    }
                }
                let _ = set_annotated_content(&mut envelope.item, normalized_content);
                if !has_content_item_kinds
                    && let ResponseItem::Message {
                        internal_chat_message_metadata_passthrough,
                        ..
                    } = &mut envelope.item
                {
                    *internal_chat_message_metadata_passthrough = original_metadata;
                }
                (omitted_content, has_content_item_kinds)
            }
            None => match &mut envelope.item {
                ResponseItem::FunctionCallOutput { output, .. }
                | ResponseItem::CustomToolCallOutput { output, .. } => {
                    if let Some(content_items) = output.content_items_mut() {
                        for content_item in content_items {
                            match content_item {
                                FunctionCallOutputContentItem::InputImage { .. }
                                    if !supports_images =>
                                {
                                    *content_item = FunctionCallOutputContentItem::InputText {
                                        text: IMAGE_CONTENT_OMITTED_PLACEHOLDER.to_string(),
                                    };
                                }
                                FunctionCallOutputContentItem::InputAudio { .. }
                                    if !supports_audio =>
                                {
                                    *content_item = FunctionCallOutputContentItem::InputText {
                                        text: AUDIO_CONTENT_OMITTED_PLACEHOLDER.to_string(),
                                    };
                                }
                                _ => {}
                            }
                        }
                    }
                    (Vec::new(), false)
                }
                ResponseItem::ImageGenerationCall { result, .. } => {
                    if !supports_images {
                        result.clear();
                    }
                    (Vec::new(), false)
                }
                _ => (Vec::new(), false),
            },
        };
        normalized_items.push(envelope);
        if !omitted_content.is_empty() {
            let mut notice = ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: Vec::new(),
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            };
            if omitted_content_has_kinds {
                let _ = set_annotated_content(&mut notice, omitted_content);
            } else if let ResponseItem::Message { content, .. } = &mut notice {
                *content = omitted_content
                    .into_iter()
                    .map(|annotated| annotated.into_parts().0)
                    .collect();
            }
            normalized_items.push(ResponseItemEnvelope::new(notice));
        }
    }
    *items = normalized_items;
}
