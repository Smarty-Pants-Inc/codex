//! Experimental original-owner wake controls. No request installs host policy.
use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct WakeIntent {
    #[ts(type = "number")]
    pub sequence: u64,
    #[ts(type = "number")]
    pub frame_revision: u64,
    pub frame_hash: String,
    #[ts(type = "number")]
    pub budget_generation: u64,
    #[ts(type = "number")]
    pub expected_commit_order: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct ThreadObservationWakeStartParams {
    pub thread_id: String,
    pub owner_epoch: String,
    pub intent: WakeIntent,
    pub operand_digest: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum WakeOutcome {
    Pending {
        #[serde(rename = "turnId")]
        #[ts(rename = "turnId")]
        turn_id: Option<String>,
    },
    Started {
        #[serde(rename = "turnId")]
        #[ts(rename = "turnId")]
        turn_id: String,
    },
    Suppressed,
    Unknown,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct WakeReceipt {
    #[ts(type = "number")]
    pub sequence: u64,
    pub operand_digest: String,
    pub outcome: WakeOutcome,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadObservationWakeStartResponse {
    pub protocol: u32,
    pub receipt: WakeReceipt,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum WakeReadQuery {
    Attempt {
        #[ts(type = "number")]
        sequence: u64,
        #[serde(rename = "operandDigest")]
        #[ts(rename = "operandDigest")]
        operand_digest: String,
    },
    Fence {
        #[ts(type = "number")]
        sequence: u64,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct ThreadObservationWakeReadParams {
    pub thread_id: String,
    pub owner_epoch: String,
    pub query: WakeReadQuery,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase")]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum ThreadObservationWakeReadResponse {
    Attempt {
        protocol: u32,
        #[serde(rename = "intentFloor")]
        #[ts(rename = "intentFloor", type = "number")]
        intent_floor: u64,
        receipt: Option<WakeReceipt>,
    },
    Fence {
        protocol: u32,
        #[serde(rename = "intentFloor")]
        #[ts(rename = "intentFloor", type = "number")]
        intent_floor: u64,
    },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct ThreadObservationWakeInvalidateParams {
    pub thread_id: String,
    pub owner_epoch: String,
    #[ts(type = "number")]
    pub sequence: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadObservationWakeInvalidateResponse {
    pub protocol: u32,
    #[ts(type = "number")]
    pub intent_floor: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export_to = "v2/")]
pub struct ThreadObservationWakeRetireParams {
    pub thread_id: String,
    pub owner_epoch: String,
    #[ts(type = "number")]
    pub sequence: u64,
    pub operand_digest: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "v2/")]
pub struct ThreadObservationWakeRetireResponse {
    pub protocol: u32,
    #[ts(type = "number")]
    pub intent_floor: u64,
}
