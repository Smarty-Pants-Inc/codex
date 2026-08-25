use crate::config::ManagedFeatures;
use crate::context::ContextualUserFragment;
use crate::context::ImageResizeNotice;
use crate::context::ImageResizeNoticeSource;
use crate::context::ResizedImage;
use crate::original_image_detail::can_request_original_image_detail;
use codex_analytics::ImageDetailSetting;
use codex_analytics::ImagePreparationMetadata;
use codex_context_fragments::AnnotatedContent;
use codex_context_fragments::set_annotated_content;
use codex_context_fragments::to_annotated_content;
use codex_features::Feature;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ContentItemKind;
use codex_protocol::models::FunctionCallOutputContentItem;
use codex_protocol::models::FunctionCallOutputPayload;
use codex_protocol::models::ImageDetail;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_utils_audio::prepare_audio_item;
use codex_utils_image::ImageProcessingError;
use codex_utils_image::PromptImageMode;
use codex_utils_image::PromptImageResizeLimits;
use codex_utils_image::load_data_url_for_prompt;
use std::collections::HashSet;
use tracing::warn;

pub(crate) const IMAGE_PROCESSING_ERROR_PLACEHOLDER: &str =
    "image content omitted because it could not be processed";
const IMAGE_TOO_LARGE_PLACEHOLDER: &str =
    "image content omitted because it exceeded the supported size limit; use a smaller image";
const UNSUPPORTED_LOW_DETAIL_PLACEHOLDER: &str = "image content omitted because detail 'low' is not supported; use 'high', 'original', or 'auto'";
const REMOTE_IMAGE_URL_PLACEHOLDER: &str =
    "image content omitted because remote image URLs are not supported";

const HIGH_DETAIL_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 2048,
    max_patches: 2_500,
};
const UNIFIED_IMAGE_LIMITS: PromptImageResizeLimits = PromptImageResizeLimits {
    max_dimension: 6000,
    max_patches: 10_000,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ImagePreparationMode {
    DetailBased,
    UnifiedBudget,
}

pub(crate) fn unified_image_budget_enabled(
    features: &ManagedFeatures,
    model_info: &ModelInfo,
) -> bool {
    features.enabled(Feature::UnifiedImageBudget)
        && (model_info.use_responses_lite || can_request_original_image_detail(model_info))
}

#[derive(Clone, Copy, Debug)]
struct ImageOrigin<'a> {
    message_role: Option<&'a str>,
    item_id: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ImageResizeNoticeMode {
    Disabled,
    Enabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PreparedImageResize {
    source_width: u32,
    source_height: u32,
    prepared_width: u32,
    prepared_height: u32,
}

#[derive(Debug, thiserror::Error)]
enum ImagePreparationError {
    #[error("remote image URLs are not supported")]
    RemoteUrlUnsupported,
    #[error("image detail `low` is not supported")]
    UnsupportedLowDetail,
    #[error(transparent)]
    Processing(#[from] ImageProcessingError),
}

impl ImagePreparationError {
    fn placeholder(&self) -> &'static str {
        match self {
            ImagePreparationError::RemoteUrlUnsupported => REMOTE_IMAGE_URL_PLACEHOLDER,
            ImagePreparationError::UnsupportedLowDetail => UNSUPPORTED_LOW_DETAIL_PLACEHOLDER,
            ImagePreparationError::Processing(ImageProcessingError::ImageTooLarge { .. }) => {
                IMAGE_TOO_LARGE_PLACEHOLDER
            }
            ImagePreparationError::Processing(_) => IMAGE_PROCESSING_ERROR_PLACEHOLDER,
        }
    }
}

pub(crate) fn prepare_response_items(
    items: &mut Vec<ResponseItem>,
    mode: ImagePreparationMode,
    resize_notice_mode: ImageResizeNoticeMode,
) -> Vec<ImagePreparationMetadata> {
    let mut metadata = Vec::new();
    let mut prepared_items = Vec::with_capacity(items.len());
    let prepare_tool_output =
        |output: &mut FunctionCallOutputPayload,
         item_id: Option<&str>,
         metadata: &mut Vec<ImagePreparationMetadata>| {
            output.content_items_mut().and_then(|content| {
                let resized_images = prepare_tool_output_content(
                    content,
                    ImageOrigin {
                        message_role: None,
                        item_id,
                    },
                    resize_notice_mode,
                    metadata,
                    mode,
                );
                (!resized_images.is_empty()).then(|| {
                    ImageResizeNotice::new(ImageResizeNoticeSource::ToolOutput, resized_images)
                })
            })
        };
    for mut item in std::mem::take(items) {
        let had_metadata = matches!(
            &item,
            ResponseItem::Message {
                internal_chat_message_metadata_passthrough: Some(_),
                ..
            }
        );
        let mut annotated_content = to_annotated_content(&mut item);
        let had_image = annotated_content.as_ref().is_some_and(|content| {
            content
                .iter()
                .any(|item| matches!(item.content(), ContentItem::InputImage { .. }))
        });
        let existing_texts = annotated_content
            .as_ref()
            .map_or_else(HashSet::new, |content| {
                content
                    .iter()
                    .filter_map(|item| match item.content() {
                        ContentItem::InputText { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect()
            });
        let mut typed_notice = None;
        let user_notice = match &mut item {
            ResponseItem::Message { role, .. } => {
                let Some(content) = annotated_content.take() else {
                    continue;
                };
                if role == "user" {
                    let (content, notices) = prepare_user_message_content(
                        content,
                        resize_notice_mode,
                        &mut metadata,
                        mode,
                        &existing_texts,
                    );
                    let content_is_empty = content.is_empty();
                    let _ = set_annotated_content(&mut item, content);
                    if content_is_empty
                        && !had_metadata
                        && let ResponseItem::Message {
                            internal_chat_message_metadata_passthrough,
                            ..
                        } = &mut item
                    {
                        *internal_chat_message_metadata_passthrough = None;
                    }
                    Some(notices)
                } else {
                    let mut content = content;
                    prepare_message_content(
                        &mut content,
                        ImageOrigin {
                            message_role: Some(role.as_str()),
                            item_id: None,
                        },
                        &mut metadata,
                        mode,
                    );
                    let _ = set_annotated_content(&mut item, content);
                    if !had_metadata
                        && !had_image
                        && let ResponseItem::Message {
                            internal_chat_message_metadata_passthrough,
                            ..
                        } = &mut item
                    {
                        *internal_chat_message_metadata_passthrough = None;
                    }
                    None
                }
            }
            ResponseItem::FunctionCallOutput {
                call_id, output, ..
            } => {
                typed_notice = prepare_tool_output(output, call_id.as_deref(), &mut metadata);
                None
            }
            ResponseItem::CustomToolCallOutput {
                call_id, output, ..
            } => {
                typed_notice = prepare_tool_output(output, Some(call_id.as_str()), &mut metadata);
                None
            }
            ResponseItem::AdditionalTools { .. }
            | ResponseItem::Reasoning { .. }
            | ResponseItem::AgentMessage { .. }
            | ResponseItem::LocalShellCall { .. }
            | ResponseItem::FunctionCall { .. }
            | ResponseItem::ToolSearchCall { .. }
            | ResponseItem::CustomToolCall { .. }
            | ResponseItem::ToolSearchOutput { .. }
            | ResponseItem::WebSearchCall { .. }
            | ResponseItem::ImageGenerationCall { .. }
            | ResponseItem::Compaction { .. }
            | ResponseItem::CompactionTrigger { .. }
            | ResponseItem::ContextCompaction { .. }
            | ResponseItem::Other => None,
        };
        prepared_items.push(item);
        if let Some(notices) = user_notice.filter(|notices| !notices.is_empty()) {
            prepared_items.push(ResponseItem::Message {
                id: None,
                role: "developer".to_string(),
                content: notices,
                phase: None,
                internal_chat_message_metadata_passthrough: None,
            });
        }
        if let Some(typed_notice) = typed_notice {
            prepared_items.push(ContextualUserFragment::into(typed_notice));
        }
    }
    *items = prepared_items;
    metadata
}

fn prepare_user_message_content(
    items: Vec<AnnotatedContent>,
    resize_notice_mode: ImageResizeNoticeMode,
    metadata: &mut Vec<ImagePreparationMetadata>,
    mode: ImagePreparationMode,
    existing_texts: &HashSet<String>,
) -> (Vec<AnnotatedContent>, Vec<ContentItem>) {
    let image_count = items
        .iter()
        .filter(|item| matches!(item.content(), ContentItem::InputImage { .. }))
        .count();
    let mut image_number = 0;
    let mut prepared_content = Vec::with_capacity(items.len());
    let mut developer_notices = Vec::new();
    for item in items {
        let (content, kind) = item.into_parts();
        match content {
            ContentItem::InputImage {
                mut image_url,
                mut detail,
            } => {
                image_number += 1;
                match prepare_image(
                    &mut image_url,
                    &mut detail,
                    ImageOrigin {
                        message_role: Some("user"),
                        item_id: None,
                    },
                    metadata,
                    mode,
                ) {
                    Ok(resize) => {
                        prepared_content.push(AnnotatedContent::new(
                            ContentItem::InputImage { image_url, detail },
                            kind,
                        ));
                        if let Some(resize) = resize
                            && resize_notice_mode == ImageResizeNoticeMode::Enabled
                        {
                            developer_notices.push(image_resize_notice(ResizedImage {
                                image_number,
                                image_count,
                                source_width: resize.source_width,
                                source_height: resize.source_height,
                                prepared_width: resize.prepared_width,
                                prepared_height: resize.prepared_height,
                            }));
                        }
                    }
                    Err(error) => {
                        warn!(%error, "failed to prepare message image");
                        let notice = error.placeholder().to_string();
                        if !existing_texts.contains(&notice) {
                            developer_notices.push(ContentItem::InputText { text: notice });
                        }
                    }
                }
            }
            ContentItem::InputAudio { mut audio_url } => {
                if let Some(placeholder) = prepare_audio_item(&mut audio_url) {
                    if !existing_texts.contains(&placeholder) {
                        developer_notices.push(ContentItem::InputText { text: placeholder });
                    }
                } else {
                    prepared_content.push(AnnotatedContent::new(
                        ContentItem::InputAudio { audio_url },
                        kind,
                    ));
                }
            }
            content => prepared_content.push(AnnotatedContent::new(content, kind)),
        }
    }
    (prepared_content, developer_notices)
}

fn prepare_message_content(
    items: &mut [AnnotatedContent],
    origin: ImageOrigin<'_>,
    metadata: &mut Vec<ImagePreparationMetadata>,
    mode: ImagePreparationMode,
) {
    for item in items {
        match item.content_mut() {
            ContentItem::InputImage { image_url, detail } => {
                if let Err(error) = prepare_image(image_url, detail, origin, metadata, mode) {
                    warn!(%error, "failed to prepare message image");
                    *item = AnnotatedContent::input_text(
                        error.placeholder(),
                        ContentItemKind("images.preparation_error".to_string()),
                    );
                }
            }
            ContentItem::InputAudio { audio_url } => {
                if let Some(placeholder) = prepare_audio_item(audio_url) {
                    *item = AnnotatedContent::input_text(
                        placeholder,
                        ContentItemKind("audio.preparation_error".to_string()),
                    );
                }
            }
            _ => {}
        }
    }
}

fn image_resize_notice(image: ResizedImage) -> ContentItem {
    ContentItem::InputText {
        text: ImageResizeNotice::new(ImageResizeNoticeSource::UserMessage, vec![image]).render(),
    }
}

fn prepare_tool_output_content(
    items: &mut [FunctionCallOutputContentItem],
    origin: ImageOrigin<'_>,
    resize_notice_mode: ImageResizeNoticeMode,
    metadata: &mut Vec<ImagePreparationMetadata>,
    mode: ImagePreparationMode,
) -> Vec<ResizedImage> {
    let image_count = items
        .iter()
        .filter(|item| matches!(item, FunctionCallOutputContentItem::InputImage { .. }))
        .count();
    let mut image_number = 0;
    let mut resized_images = Vec::new();
    for item in items {
        match item {
            FunctionCallOutputContentItem::InputImage { image_url, detail } => {
                image_number += 1;
                match prepare_image(image_url, detail, origin, metadata, mode) {
                    Ok(Some(resize)) if resize_notice_mode == ImageResizeNoticeMode::Enabled => {
                        resized_images.push(ResizedImage {
                            image_number,
                            image_count,
                            source_width: resize.source_width,
                            source_height: resize.source_height,
                            prepared_width: resize.prepared_width,
                            prepared_height: resize.prepared_height,
                        });
                    }
                    Ok(_) => {}
                    Err(error) => {
                        warn!(%error, "failed to prepare tool output image");
                        *item = FunctionCallOutputContentItem::InputText {
                            text: error.placeholder().to_string(),
                        };
                    }
                }
            }
            FunctionCallOutputContentItem::InputAudio { audio_url } => {
                if let Some(placeholder) = prepare_audio_item(audio_url) {
                    *item = FunctionCallOutputContentItem::InputText { text: placeholder };
                }
            }
            _ => {}
        }
    }
    resized_images
}

fn is_remote_image_url(image_url: &str) -> bool {
    image_url.split_once(':').is_some_and(|(scheme, _)| {
        scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https")
    })
}

fn is_data_url(image_url: &str) -> bool {
    image_url
        .get(.."data:".len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("data:"))
}

fn prepare_image(
    image_url: &mut String,
    detail: &mut Option<ImageDetail>,
    origin: ImageOrigin<'_>,
    metadata: &mut Vec<ImagePreparationMetadata>,
    mode: ImagePreparationMode,
) -> Result<Option<PreparedImageResize>, ImagePreparationError> {
    if is_remote_image_url(image_url) {
        return Err(ImagePreparationError::RemoteUrlUnsupported);
    }
    if !is_data_url(image_url) {
        return Ok(None);
    }

    let (effective_detail, limits) = match mode {
        ImagePreparationMode::UnifiedBudget => (ImageDetailSetting::Original, UNIFIED_IMAGE_LIMITS),
        ImagePreparationMode::DetailBased => match detail {
            None | Some(ImageDetail::Auto | ImageDetail::High) => {
                (ImageDetailSetting::High, HIGH_DETAIL_LIMITS)
            }
            Some(ImageDetail::Original) => (ImageDetailSetting::Original, UNIFIED_IMAGE_LIMITS),
            Some(ImageDetail::Low) => return Err(ImagePreparationError::UnsupportedLowDetail),
        },
    };
    let image = load_data_url_for_prompt(image_url, PromptImageMode::ResizeWithLimits(limits))?;
    metadata.push(ImagePreparationMetadata {
        message_role: origin.message_role.map(str::to_string),
        item_id: origin.item_id.map(str::to_string),
        effective_detail,
        source_width: image.source_width,
        source_height: image.source_height,
        prepared_width: image.width,
        prepared_height: image.height,
    });
    let resize = ((image.source_width, image.source_height) != (image.width, image.height))
        .then_some(PreparedImageResize {
            source_width: image.source_width,
            source_height: image.source_height,
            prepared_width: image.width,
            prepared_height: image.height,
        });
    *image_url = image.into_data_url();
    if mode == ImagePreparationMode::UnifiedBudget {
        // Preserve accurate context-window accounting while older transports still require an
        // image detail field. Responses Lite removes this compatibility hint before sending.
        *detail = Some(ImageDetail::Original);
    }
    Ok(resize)
}

#[cfg(test)]
#[path = "image_preparation_tests.rs"]
mod tests;
