//! Experimental observation wire types. Registration and capability admission
//! belong to the owner-bound app-server path, not these data definitions.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;
use ts_rs::TS;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadObservationOptions {
    pub protocol: u32,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadObservationCapabilities {
    pub protocol: u32,
    pub owner_epoch: String,
    pub max_frame_bytes: u32,
    pub reserved_tokens: u32,
    pub max_lease_seconds: u32,
    pub max_in_flight_decisions: u32,
    pub replacement: ObservationReplacement,
    pub automatic_admission: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum ObservationReplacement {
    FullContext,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ObservationFrame {
    pub text: String,
    pub hash: String,
    /// Unix seconds. Native admission validates the bounded active lease.
    pub expires_at: i64,
}

/// Required nullable value: omitting `frame` must not silently clear a slot.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(untagged)]
#[ts(export_to = "v2/")]
pub enum ObservationFrameUpdate {
    Frame(ObservationFrame),
    Clear(()),
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadObservationSetParams {
    pub thread_id: String,
    pub owner_epoch: String,
    /// JSON-safe, monotonically increasing, except active identical-frame renewal.
    pub revision: u64,
    pub frame: ObservationFrameUpdate,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadObservationReadParams {
    pub thread_id: String,
    pub owner_epoch: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadObservationSetResponse {
    pub owner_epoch: String,
    pub revision: u64,
    pub hash: Option<String>,
    pub state: ObservationPublicationState,
    pub expires_at: Option<i64>,
    pub commit_order: u64,
}

pub type ThreadObservationReadResponse = ThreadObservationSetResponse;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum ObservationPublicationState {
    Current,
    Cleared,
    Expired,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum ObservationCaptureState {
    Current,
    Cleared,
    Expired,
    Unavailable,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadObservationCapturedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub owner_epoch: String,
    pub decision_id: String,
    pub commit_order: u64,
    pub frame_revision: u64,
    pub frame_hash: Option<String>,
    pub state: ObservationCaptureState,
    /// Native per-decision capture time, in Unix seconds; not host composition time.
    pub captured_at: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadObservationSubmittedNotification {
    pub thread_id: String,
    pub turn_id: String,
    pub owner_epoch: String,
    pub decision_id: String,
    pub attempt_id: String,
    /// Native wire-request identity, not a fabricated upstream ID.
    pub request_id: String,
    pub provider_request_id: Option<String>,
    pub commit_order: u64,
    pub frame_revision: u64,
    pub frame_hash: Option<String>,
    pub state: ObservationCaptureState,
    pub captured_at: i64,
    pub outcome: ObservationSubmissionOutcome,
    pub terminal_decision: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(rename_all = "camelCase", export_to = "v2/")]
pub enum ObservationSubmissionOutcome {
    Accepted,
    Rejected,
    Unknown,
}

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
