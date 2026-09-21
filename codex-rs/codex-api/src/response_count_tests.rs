use super::*;
use crate::ResponsesApiTools;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::sync::Arc;

fn request() -> ResponsesApiRequest {
    ResponsesApiRequest {
        model: "fixture-model".into(),
        instructions: "Complete instructions".into(),
        input: vec![
            serde_json::from_value(json!({
                "type": "message", "role": "user",
                "content": [{"type": "input_text", "text": "Complete input"}]
            }))
            .unwrap(),
        ],
        tools: None,
        tool_choice: "auto".into(),
        parallel_tool_calls: true,
        reasoning: None,
        store: false,
        stream: true,
        stream_options: None,
        include: vec![],
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
    }
}

#[test]
fn actual_native_encoder_preserves_complete_context_and_exact_raw_tool_numbers() {
    let mut request = request();
    // Codec fixture only, not a captured frame or a provider-qualified image.
    request.input.push(
        serde_json::from_value(json!({
            "type": "message", "role": "user",
            "content": [{"type": "input_image", "image_url": "data:image/png;base64,iVBORw0KGgo="}]
        }))
        .unwrap(),
    );
    let raw = RawValue::from_string(r#"[{"type":"function","name":"local","parameters":{"type":"number","const":1.234567890123456789}}]"#.into()).unwrap();
    request.tools = Some(ResponsesApiTools::from(Arc::<RawValue>::from(raw)));
    let wire = prepare_response_count(&request, NonZeroU64::new(/*n*/ 17).unwrap()).unwrap();
    let mut expected_inference = serde_json::to_value(&request).unwrap();
    expected_inference["max_output_tokens"] = json!(17);
    expected_inference["truncation"] = json!("disabled");
    assert_eq!(
        serde_json::from_slice::<Value>(wire.inference_body().as_bytes()).unwrap(),
        expected_inference
    );
    let mut expected_count = expected_inference;
    for key in ["max_output_tokens", "stream", "store", "include"] {
        expected_count.as_object_mut().unwrap().remove(key);
    }
    assert_eq!(
        serde_json::from_slice::<Value>(wire.count_body().as_bytes()).unwrap(),
        expected_count
    );
    let inference: BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(wire.inference_body().as_bytes()).unwrap();
    let count: BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(wire.count_body().as_bytes()).unwrap();
    assert_eq!(count["tools"].get(), inference["tools"].get());
    assert!(count["tools"].get().contains("1.234567890123456789"));
}

#[test]
fn reasoning_content_and_populated_controls_preserve_exact_projection() {
    use codex_protocol::models::ReasoningItemContent;
    use codex_protocol::models::ReasoningItemReasoningSummary;
    use codex_protocol::models::ResponseItem;

    let mut request = request();
    request.input.push(ResponseItem::Reasoning {
        id: None,
        summary: vec![ReasoningItemReasoningSummary::SummaryText {
            text: "Retained summary".into(),
        }],
        content: Some(vec![
            ReasoningItemContent::ReasoningText {
                text: "Retained reasoning é\"\\".into(),
            },
            ReasoningItemContent::Text {
                text: "Retained legacy text".into(),
            },
        ]),
        encrypted_content: Some("opaque-retained-content".into()),
        internal_chat_message_metadata_passthrough: None,
    });
    request.reasoning = Some(crate::Reasoning {
        effort: Some(codex_protocol::openai_models::ReasoningEffort::High),
        summary: Some(codex_protocol::config_types::ReasoningSummary::Detailed),
        context: Some(crate::ReasoningContext::AllTurns),
    });
    request.text = Some(crate::TextControls {
        verbosity: Some(crate::common::OpenAiVerbosity::High),
        format: Some(crate::common::TextFormat {
            r#type: crate::common::TextFormatType::JsonSchema,
            strict: true,
            schema: json!({"type": "object", "properties": {"answer": {"type": "string"}}}),
            name: "answer".into(),
        }),
    });
    let wire = prepare_response_count(&request, NonZeroU64::new(/*n*/ 17).unwrap()).unwrap();
    let inference: BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(wire.inference_body().as_bytes()).unwrap();
    let count: BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(wire.count_body().as_bytes()).unwrap();
    for field in ["input", "reasoning", "text"] {
        assert_eq!(count[field].get(), inference[field].get());
    }
    assert_eq!(
        serde_json::from_str::<Value>(count["input"].get()).unwrap(),
        serde_json::to_value(&request.input).unwrap()
    );
}

#[test]
fn model_and_output_boundaries_refuse_without_truncating() {
    for (model, accepted) in [
        (String::new(), false),
        ("m".repeat(256), true),
        ("m".repeat(257), false),
        ("é".repeat(128), true),
        ("é".repeat(129), false),
    ] {
        let mut request = request();
        request.model = model;
        let result = prepare_response_count(&request, NonZeroU64::new(/*n*/ 17).unwrap());
        assert_eq!(result.is_ok(), accepted);
        if let Ok(wire) = result {
            assert_eq!(wire.model(), request.model);
        }
    }
    for (limit, accepted) in [(1, true), (2_000_000, true), (2_000_001, false)] {
        let limit = NonZeroU64::new(limit).unwrap();
        let result = prepare_response_count(&request(), limit);
        assert_eq!(result.is_ok(), accepted);
        if let Ok(wire) = result {
            assert_eq!(wire.output_tokens(), limit);
        }
    }
}

#[test]
fn output_changes_final_bytes_even_when_count_input_is_unchanged() {
    let request = request();
    let first = prepare_response_count(&request, NonZeroU64::new(/*n*/ 17).unwrap()).unwrap();
    let second = prepare_response_count(&request, NonZeroU64::new(/*n*/ 18).unwrap()).unwrap();
    assert_ne!(
        first.inference_body().as_bytes(),
        second.inference_body().as_bytes()
    );
    assert_eq!(
        first.count_body().as_bytes(),
        second.count_body().as_bytes()
    );
}

#[test]
fn unsupported_metadata_hosted_tools_duplicate_keys_and_remote_media_refuse() {
    let output = NonZeroU64::new(/*n*/ 17).unwrap();
    let mut value = request();
    value.client_metadata = Some(Default::default());
    assert!(prepare_response_count(&value, output).is_err());
    value.client_metadata = None;
    for tools in [
        r#"[{"type":"web_search"}]"#,
        r#"[{"type":"function","parameters":{"x":1,"x":2}}]"#,
        r#"[{"type":"function","parameters":{"x":1,"\u0078":2}}]"#,
    ] {
        let raw = RawValue::from_string(tools.into()).unwrap();
        value.tools = Some(ResponsesApiTools::from(Arc::<RawValue>::from(raw)));
        assert!(prepare_response_count(&value, output).is_err());
    }
    value.tools = None;
    value.input = vec![
        serde_json::from_value(json!({
            "type":"message", "role":"user",
            "content":[{"type":"input_image", "image_url":"https://mutable.invalid/image.png"}]
        }))
        .unwrap(),
    ];
    assert!(prepare_response_count(&value, output).is_err());
}

#[test]
fn count_reply_is_strict_bounded_data_not_a_qualification_receipt() {
    assert_eq!(
        parse_response_count(br#" {"input_tokens":7,"object":"response.input_tokens"} "#).unwrap(),
        7
    );
    for reply in [
        r#"{"object":"response.input_tokens","input_tokens":7,"input_tokens":8}"#,
        r#"{"object":"response.input_tokens","input_tokens":-1}"#,
        r#"{"object":"response.input_tokens","input_tokens":-0}"#,
        r#"{"object":"response.input_tokens","input_tokens":1.0}"#,
        r#"{"object":"response.input_tokens","input_tokens":1e0}"#,
        r#"{"object":"response.input_tokens","input_tokens":9007199254740992}"#,
        r#"{"object":"response.input_tokens","input_tokens":7,"extra":true}"#,
        r#"{"object":"response","input_tokens":7}"#,
        r#"{"object":"response.input_tokens","input_\u0074okens":7}"#,
        r#"{"object":"response\u002einput_tokens","input_tokens":7}"#,
        r#"{"object":"response.input_tokens","input_tokens":7"#,
    ] {
        assert!(parse_response_count(reply.as_bytes()).is_err());
    }
    assert!(parse_response_count(&vec![b' '; 4097]).is_err());
}
