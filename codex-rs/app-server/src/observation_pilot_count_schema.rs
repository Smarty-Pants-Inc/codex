//! Versioned nonsecret count input schema. Deserialization alone is not authority.
use super::super::denied;
use crate::observation_pilot_decision::PilotLaunchDecision;
use serde::Deserialize;
use std::io;

pub(super) const SERIALIZER_SHA256: &str =
    "3763888d007d27ac721c18df6090c9993334eb0d8502575d38a699d4e8b7525f";
const CONTRACT: &str = "codex-responses-static-input-v1";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Scope {
    pub version: u32,
    pub kind: String,
    pub decision_sha256: String,
    pub allocation_id: String,
    pub instruction: String,
    pub not_before_ms: u64,
    pub expires_ms: u64,
    pub credential_generation: String,
    pub count: Count,
    pub ledger: Ledger,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Count {
    pub url: String,
    pub method: String,
    pub operations: u8,
    pub provider: String,
    pub wire_model: String,
    pub purpose: String,
    pub account: String,
    pub semantics_sha256: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Ledger {
    pub device: String,
    pub inode: String,
    pub uid: u32,
    pub gid: u32,
    pub mode: u32,
    pub links: u64,
    pub initial_bytes: u64,
    pub access: String,
    pub recovery: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Semantics {
    pub version: u32,
    pub kind: String,
    pub contract: String,
    pub application: Application,
    pub serializer: Serializer,
    pub provider: String,
    pub wire_model: String,
    pub inference_url: String,
    pub count_url: String,
    pub shapes: Shapes,
    pub limits: Limits,
    pub qualification: Qualification,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Application {
    commit: String,
    tree: String,
    binary_sha256: String,
    closure_sha256: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Serializer {
    revision: u32,
    source_sha256: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Shapes {
    items: Vec<String>,
    tools: Vec<String>,
    images: String,
    client_metadata: String,
    stream_options: String,
    access_programs: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Limits {
    pub context_tokens: u64,
    pub output_tokens: u64,
}
#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "camelCase", deny_unknown_fields)]
pub(super) enum Qualification {
    Qualified { evidence: Evidence },
    Unknown,
    Unsupported,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Evidence {
    issuer: String,
    identity: String,
    sha256: String,
}

pub(super) fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
pub(super) fn decimal(value: &str) -> io::Result<u64> {
    if value.is_empty()
        || (value.len() > 1 && value.starts_with('0'))
        || !value.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(denied());
    }
    value.parse().map_err(|_| denied())
}
pub(super) fn endpoint(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.as_str() == value
            && url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
    })
}

impl Scope {
    pub(super) fn validate(
        &self,
        policy: &PilotLaunchDecision,
        decision_digest: &str,
        semantics_pin: &str,
    ) -> io::Result<()> {
        let credential = policy.provider.credential.as_ref().ok_or_else(denied)?;
        if policy.version != 2
            || self.version != 1
            || self.kind != "codex-pilot-count-scope"
            || self.decision_sha256 != decision_digest
            || self.allocation_id != policy.allocation_id
            || self.instruction != policy.instruction
            || self.credential_generation != credential.generation
            || self.not_before_ms < policy.limits.not_before_ms
            || self.expires_ms > policy.limits.expires_ms
            || self.not_before_ms >= self.expires_ms
            || self.count.method != "POST"
            || !(1..=8).contains(&self.count.operations)
            || !endpoint(&self.count.url)
            || self.count.provider != policy.provider.provider
            || self.count.wire_model != policy.provider.model
            || self.count.purpose != policy.provider.purpose
            || self.count.account != policy.provider.account
            || self.count.semantics_sha256 != semantics_pin
            || !digest(semantics_pin)
            || self.ledger.uid != policy.target.uid
            || self.ledger.mode != 0o600
            || self.ledger.links != 1
            || self.ledger.initial_bytes != 0
            || self.ledger.access != "append"
            || self.ledger.recovery != "none"
        {
            return Err(denied());
        }
        decimal(&self.ledger.device)?;
        decimal(&self.ledger.inode)?;
        Ok(())
    }
}

impl Semantics {
    /// Only called after original protected descriptor/pin and launch validation.
    /// This joins original independent evidence identity, not a caller boolean.
    pub(super) fn validate(&self, policy: &PilotLaunchDecision, scope: &Scope) -> io::Result<()> {
        if self.version != 1
            || self.kind != "codex-pilot-count-semantics"
            || self.contract != CONTRACT
            || self.application.commit != policy.source.commit
            || self.application.tree != policy.source.tree
            || self.application.binary_sha256 != policy.source.binary_sha256
            || self.application.closure_sha256 != policy.source.closure_sha256
            || self.serializer.revision != 1
            || self.serializer.source_sha256 != SERIALIZER_SHA256
            || self.provider != policy.provider.provider
            || self.wire_model != policy.provider.model
            || self.inference_url != policy.provider.endpoint
            || !endpoint(&self.inference_url)
            || self.count_url != scope.count.url
            || !endpoint(&self.count_url)
            || self.shapes.items
                != [
                    "compaction",
                    "custom_tool_call",
                    "custom_tool_call_output",
                    "function_call",
                    "function_call_output",
                    "message",
                    "reasoning",
                ]
            || self.shapes.tools != ["custom", "function"]
            || self.shapes.images != "inlineDataOnly"
            || self.shapes.client_metadata != "unsupported"
            || self.shapes.stream_options != "unsupported"
            || self.shapes.access_programs != "unsupported"
            || self.limits.output_tokens == 0
            || self.limits.output_tokens > 2_000_000
            || self.limits.context_tokens < self.limits.output_tokens
            || self.limits.context_tokens > policy.limits.reserved_tokens
        {
            return Err(denied());
        }
        match &self.qualification {
            Qualification::Unknown => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native count semantics unknown",
            )),
            Qualification::Unsupported => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native count semantics unsupported",
            )),
            Qualification::Qualified { evidence } => {
                if evidence.issuer != "ci-delivery"
                    || !digest(&evidence.sha256)
                    || evidence.identity != policy.evidence.context
                    || evidence.identity != format!("sha256:{}", evidence.sha256)
                {
                    return Err(denied());
                }
                Ok(())
            }
        }
    }
}
