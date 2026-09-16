//! Original launch-bound pilot controls. No request installs or extends authority.
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;
use std::collections::BTreeMap;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct ThreadPilotReadParams {
    pub thread_id: String,
}
pub type ThreadPilotRetireParams = ThreadPilotReadParams;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct ThreadPilotCheckParams {
    pub thread_id: String,
    pub operation: PilotSourceOperation,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum PilotSourceOperation {
    PrepareSource,
    SampleSource,
    ActOnSource,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct ThreadPilotStartParams {
    pub thread_id: String,
    /// At most1024 UTF8 bytes. No settings, tools, grant or owner override.
    pub input: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadPilotReadResponse {
    pub identity: PilotNativeIdentity,
    pub instruction: String,
    pub allocation_id: String,
    pub decision_sha256: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotNativeIdentity {
    pub connection_id: String,
    pub owner_epoch: String,
    pub thread_id: String,
    pub grant_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadPilotCheckResponse {
    /// Native diagnostic correlation, not a reusable bearer grant.
    pub request_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadPilotStartResponse {
    pub turn_id: Option<String>,
    /// False means no turn was committed. It is not a sampled-idle promise.
    pub started: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadPilotRetireResponse {
    pub identity: PilotNativeIdentity,
    pub revoked: bool,
    pub active_decision: Option<String>,
    pub admitted_turns: BTreeMap<String, String>,
    pub reserved_tokens: u64,
    pub attempts: Vec<PilotNativeAttempt>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotNativeAttempt {
    pub decision_id: String,
    pub attempt_id: String,
    pub request_id: String,
    pub turn_id: String,
    pub admission_request_id: Option<String>,
    pub token_ceiling: u64,
    pub credential_receipt: String,
    pub context_receipt: String,
    pub response_id: Option<String>,
    pub usage: Option<PilotNativeUsage>,
    pub response_complete: bool,
    pub usage_conflict: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct PilotNativeUsage {
    pub input_tokens: i64,
    pub cached_input_tokens: i64,
    pub cache_write_input_tokens: i64,
    pub output_tokens: i64,
    pub reasoning_output_tokens: i64,
    pub total_tokens: i64,
}
