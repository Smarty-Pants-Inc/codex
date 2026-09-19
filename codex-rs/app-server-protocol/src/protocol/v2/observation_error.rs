use crate::JsonSchema;
use crate::TS;
use serde::Deserialize;
use serde::Serialize;

/// Application error, distinct from the generic overloaded/internal errors.
pub const THREAD_OBSERVATION_REJECTED_ERROR_CODE: i64 = -32002;

/// Bounded data in the existing JSONRPCErrorError envelope. For protocol1 this
/// discriminator certifies that the correlated control invocation did not
/// commit a requested publication, replacement, clear or lease renewal. It does
/// not establish the disposition of a previous invocation with a lost response.
/// Read errors never reconcile a previous uncertain mutation.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, JsonSchema, TS)]
#[serde(tag = "type", rename_all = "camelCase", deny_unknown_fields)]
#[ts(tag = "type", rename_all = "camelCase", export_to = "v2/")]
pub enum ThreadObservationErrorData {
    ThreadObservationRejected {
        /// Exactly1 for the supported contract. Other versions are unknown.
        protocol: u32,
        code: ThreadObservationRejectionCode,
    },
}

/// Uppercase wire codes retain the accepted Sense control-error contract.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, JsonSchema, TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[ts(rename_all = "SCREAMING_SNAKE_CASE", export_to = "v2/")]
pub enum ThreadObservationRejectionCode {
    InvalidInput,
    Denied,
    StaleOwner,
    RevisionMismatch,
    ResourceLimit,
    Unsupported,
    IncompatibleState,
}
