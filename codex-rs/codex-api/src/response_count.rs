//! Pure whole-request wire preparation, not permission, a count result, or a
//! qualification receipt. Native custody must bind these exact bytes separately.

use crate::ResponsesApiRequest;
use codex_client::EncodedJsonBody;
use serde::Deserialize;
use serde::Serialize;
use serde::de::MapAccess;
use serde::de::SeqAccess;
use serde::de::Visitor;
use serde_json::Value;
use serde_json::value::RawValue;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::num::NonZeroU64;

const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

#[derive(Debug, thiserror::Error)]
#[error("unsupported native count wire input")]
pub struct CountWireError;

/// Immutable serializer output only. Its hashes, model and output limit must be
/// joined by the original ledger; possession of this value grants no operation.
pub struct CountWire {
    inference: EncodedJsonBody,
    count: EncodedJsonBody,
    model: String,
    output_tokens: NonZeroU64,
}

impl CountWire {
    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn output_tokens(&self) -> NonZeroU64 {
        self.output_tokens
    }

    pub fn inference_body(&self) -> &EncodedJsonBody {
        &self.inference
    }

    pub fn count_body(&self) -> &EncodedJsonBody {
        &self.count
    }
}

/// Encode the actual native request with an explicit output limit, then project
/// ALL supported context-bearing fields without re-encoding their scalar values.
/// Unsupported metadata must be resolved at the native caller, never silently
/// omitted here. The caller must send these inference bytes, not serialize again.
pub fn prepare_response_count(
    request: &ResponsesApiRequest,
    output_tokens: NonZeroU64,
) -> Result<CountWire, CountWireError> {
    if request.model.is_empty()
        || request.model.len() > 256
        || !request.stream
        || request.store
        || output_tokens.get() > 2_000_000
        || request
            .max_output_tokens
            .is_some_and(|limit| limit != output_tokens)
        || request.client_metadata.is_some()
        || request.stream_options.is_some()
    {
        return Err(CountWireError);
    }
    let mut request = request.clone();
    request.max_output_tokens = Some(output_tokens);
    #[derive(Serialize)]
    struct BoundedRequest<'a> {
        #[serde(flatten)]
        request: &'a ResponsesApiRequest,
        truncation: &'static str,
    }
    let inference = EncodedJsonBody::encode(&BoundedRequest {
        request: &request,
        truncation: "disabled",
    })
    .map_err(|_| CountWireError)?
    .without_body_trace();
    let bytes = inference.as_bytes();
    if bytes.len() > MAX_BODY_BYTES {
        return Err(CountWireError);
    }
    // Raw tools can contain duplicate nested keys even though the outer request
    // is typed. Validate uniqueness before any Value projection can erase them.
    serde_json::from_slice::<UniqueJson>(bytes).map_err(|_| CountWireError)?;
    let wire: BTreeMap<String, Box<RawValue>> =
        serde_json::from_slice(bytes).map_err(|_| CountWireError)?;
    let input: Value = serde_json::from_str(wire.get("input").ok_or(CountWireError)?.get())
        .map_err(|_| CountWireError)?;
    for item in input.as_array().ok_or(CountWireError)? {
        let fields = item.as_object().ok_or(CountWireError)?;
        let kind = fields
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("message");
        if !matches!(
            kind,
            "message"
                | "function_call"
                | "function_call_output"
                | "custom_tool_call"
                | "custom_tool_call_output"
                | "reasoning"
                | "compaction"
        ) {
            return Err(CountWireError);
        }
        let content = if matches!(kind, "function_call_output" | "custom_tool_call_output") {
            fields.get("output")
        } else {
            fields.get("content")
        };
        if let Some(parts) = content.and_then(Value::as_array) {
            for part in parts {
                match (kind, part.get("type").and_then(Value::as_str)) {
                    ("reasoning", Some("reasoning_text" | "text")) => {}
                    ("reasoning", _) => return Err(CountWireError),
                    (_, Some("input_text" | "output_text" | "refusal")) => {}
                    (_, Some("input_image")) => {
                        if part.get("file_id").is_some() {
                            return Err(CountWireError);
                        }
                        let url = part
                            .get("image_url")
                            .and_then(Value::as_str)
                            .ok_or(CountWireError)?;
                        let (mime, data) = url
                            .strip_prefix("data:image/")
                            .and_then(|url| url.split_once(";base64,"))
                            .ok_or(CountWireError)?;
                        let unpadded = data.trim_end_matches('=');
                        if !matches!(mime, "png" | "jpeg" | "webp" | "gif")
                            || unpadded.is_empty()
                            || !unpadded.bytes().all(|byte| {
                                byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/')
                            })
                            || data.len() - unpadded.len() > 2
                        {
                            return Err(CountWireError);
                        }
                    }
                    _ => return Err(CountWireError),
                }
            }
        }
    }
    if let Some(tools) = wire.get("tools") {
        let tools: Value = serde_json::from_str(tools.get()).map_err(|_| CountWireError)?;
        for tool in tools.as_array().ok_or(CountWireError)? {
            if !matches!(
                tool.get("type").and_then(Value::as_str),
                Some("function" | "custom")
            ) {
                return Err(CountWireError);
            }
        }
    }
    let mut projected = BTreeMap::new();
    for (name, value) in &wire {
        match name.as_str() {
            "model"
            | "input"
            | "instructions"
            | "parallel_tool_calls"
            | "reasoning"
            | "text"
            | "tool_choice"
            | "tools"
            | "truncation" => {
                projected.insert(name.as_str(), value.as_ref());
            }
            "stream" | "store" | "max_output_tokens" | "service_tier" | "include"
            | "prompt_cache_key" => {}
            _ => return Err(CountWireError),
        }
    }
    let count = EncodedJsonBody::encode(&projected)
        .map_err(|_| CountWireError)?
        .without_body_trace();
    if count.as_bytes().len() > MAX_BODY_BYTES {
        return Err(CountWireError);
    }
    Ok(CountWire {
        inference,
        count,
        model: request.model,
        output_tokens,
    })
}

/// Parse data only, not an authenticated CountResult. The native producer must
/// additionally join the exact response, plan and completed original operation.
pub fn parse_response_count(bytes: &[u8]) -> Result<u64, CountWireError> {
    #[derive(Deserialize)]
    enum Object {
        #[serde(rename = "response.input_tokens")]
        InputTokens,
    }
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Reply {
        #[serde(rename = "object")]
        _object: Object,
        input_tokens: Box<RawValue>,
    }
    // This two-field ASCII schema needs no escaped keys or values. Match the
    // reviewed Pi grammar rather than accepting alternate escaped spellings.
    if bytes.len() > 4096 || bytes.contains(&b'\\') {
        return Err(CountWireError);
    }
    let reply: Reply = serde_json::from_slice(bytes).map_err(|_| CountWireError)?;
    let digits = reply.input_tokens.get().as_bytes();
    if digits.is_empty()
        || !digits.iter().all(u8::is_ascii_digit)
        || (digits.len() > 1 && digits[0] == b'0')
    {
        return Err(CountWireError);
    }
    let tokens = reply
        .input_tokens
        .get()
        .parse::<u64>()
        .map_err(|_| CountWireError)?;
    if tokens > MAX_SAFE_INTEGER {
        return Err(CountWireError);
    }
    Ok(tokens)
}

struct UniqueJson;
impl<'de> Deserialize<'de> for UniqueJson {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;
        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueJson;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("JSON without duplicate object keys")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<UniqueJson, A::Error> {
                let mut names = HashSet::new();
                while let Some(name) = map.next_key::<String>()? {
                    if !names.insert(name) {
                        return Err(serde::de::Error::custom("duplicate count input key"));
                    }
                    map.next_value::<UniqueJson>()?;
                }
                Ok(UniqueJson)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<UniqueJson, A::Error> {
                while seq.next_element::<UniqueJson>()?.is_some() {}
                Ok(UniqueJson)
            }
            fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<UniqueJson, E> {
                Ok(UniqueJson)
            }
        }
        deserializer.deserialize_any(UniqueVisitor)
    }
}

#[cfg(test)]
#[path = "response_count_tests.rs"]
mod tests;
