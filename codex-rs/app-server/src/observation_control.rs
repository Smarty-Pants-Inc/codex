//! Observation control errors certify a pre-commit boundary, not a generic
//! operation failure. Do not reuse them for a failed ACK, stream or relay task.

use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::THREAD_OBSERVATION_REJECTED_ERROR_CODE;
use codex_app_server_protocol::ThreadObservationErrorData;
use codex_app_server_protocol::ThreadObservationRejectionCode;
use codex_core::ObservationError;

/// Only admission/validation failures known to precede mutation may use this.
/// No slot metadata, request text or diagnostic interpolation enters the error.
pub(crate) fn rejected(code: ThreadObservationRejectionCode) -> JSONRPCErrorError {
    JSONRPCErrorError {
        code: THREAD_OBSERVATION_REJECTED_ERROR_CODE,
        message: "observation control request rejected".into(),
        // Serialization failure must degrade to uncertainty, never manufacture
        // another no-commit certificate or turn a committed operation into Err.
        data: serde_json::to_value(ThreadObservationErrorData::ThreadObservationRejected {
            protocol: 2,
            code,
        })
        .ok(),
    }
}

pub(crate) fn store_error(error: ObservationError) -> JSONRPCErrorError {
    rejected(match error {
        ObservationError::InvalidFrame => ThreadObservationRejectionCode::InvalidInput,
        ObservationError::StaleOwner => ThreadObservationRejectionCode::StaleOwner,
        ObservationError::RevisionMismatch => ThreadObservationRejectionCode::RevisionMismatch,
        ObservationError::ResourceLimit => ThreadObservationRejectionCode::ResourceLimit,
        ObservationError::Unavailable => ThreadObservationRejectionCode::IncompatibleState,
        ObservationError::BudgetInvalid => ThreadObservationRejectionCode::BudgetInvalid,
        ObservationError::BudgetGenerationMismatch => {
            ThreadObservationRejectionCode::BudgetGenerationMismatch
        }
    })
}

#[cfg(test)]
#[path = "observation_control_tests.rs"]
mod tests;
