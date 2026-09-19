//! Original-owner wake RPCs. JSON cannot install a policy or grant authority.
use super::ThreadRequestProcessor;
use crate::error_code::internal_error;
use crate::observation_control::rejected;
use crate::observation_control::store_error;
use crate::outgoing_message::ConnectionRequestId;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadObservationRejectionCode as Code;
use codex_app_server_protocol::ThreadObservationWakeInvalidateParams;
use codex_app_server_protocol::ThreadObservationWakeInvalidateResponse;
use codex_app_server_protocol::ThreadObservationWakeReadParams;
use codex_app_server_protocol::ThreadObservationWakeReadResponse;
use codex_app_server_protocol::ThreadObservationWakeRetireParams;
use codex_app_server_protocol::ThreadObservationWakeRetireResponse;
use codex_app_server_protocol::ThreadObservationWakeStartParams;
use codex_app_server_protocol::ThreadObservationWakeStartResponse;
use codex_app_server_protocol::WakeOutcome;
use codex_app_server_protocol::WakeReadQuery;
use codex_app_server_protocol::WakeReceipt;
use codex_core::ObservationWakeHostPolicy;
use codex_core::ObservationWakeIntent;
use codex_core::ObservationWakeOutcome;
use std::sync::Arc;

pub(crate) enum WakeOperation {
    Start(ThreadObservationWakeStartParams),
    Read(ThreadObservationWakeReadParams),
    Invalidate(ThreadObservationWakeInvalidateParams),
    Retire(ThreadObservationWakeRetireParams),
}

fn sequence(value: u64) -> Result<(), JSONRPCErrorError> {
    if value == 0 || value > (1_u64 << 53) - 1 {
        return Err(rejected(Code::InvalidInput));
    }
    Ok(())
}

fn digest(value: &str) -> Result<(), JSONRPCErrorError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(rejected(Code::InvalidInput));
    }
    Ok(())
}

fn receipt(value: codex_core::ObservationWakeReceipt) -> WakeReceipt {
    WakeReceipt {
        sequence: value.sequence,
        operand_digest: value.operand_digest,
        outcome: match value.outcome {
            ObservationWakeOutcome::Pending { turn_id } => WakeOutcome::Pending { turn_id },
            ObservationWakeOutcome::Started { turn_id } => WakeOutcome::Started { turn_id },
            ObservationWakeOutcome::Suppressed => WakeOutcome::Suppressed,
            ObservationWakeOutcome::Unknown => WakeOutcome::Unknown,
        },
    }
}

impl ThreadRequestProcessor {
    pub(crate) async fn observation_wake_request(
        &self,
        request: ConnectionRequestId,
        operation: WakeOperation,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let (thread_id, epoch) = match &operation {
            WakeOperation::Start(params) => (&params.thread_id, &params.owner_epoch),
            WakeOperation::Read(params) => (&params.thread_id, &params.owner_epoch),
            WakeOperation::Invalidate(params) => (&params.thread_id, &params.owner_epoch),
            WakeOperation::Retire(params) => (&params.thread_id, &params.owner_epoch),
        };
        if epoch.len() != 36
            || [8, 13, 18, 23]
                .into_iter()
                .any(|index| epoch.as_bytes()[index] != b'-')
        {
            return Err(rejected(Code::Denied));
        }
        let bridge = self
            .observation_bridge_for_request(&request, thread_id, epoch)
            .await?;
        let result = match operation {
            WakeOperation::Start(params) => {
                let thread = self
                    .thread_manager
                    .get_thread(bridge.thread_id)
                    .await
                    .map_err(|_| rejected(Code::Denied))?;
                start(&bridge, &thread, params).await?.into()
            }
            operation => control(&bridge, operation)?,
        };
        Ok(Some(result))
    }
}

fn control(
    bridge: &crate::observation_bridge::ObservationBridge,
    operation: WakeOperation,
) -> Result<ClientResponsePayload, JSONRPCErrorError> {
    Ok(match operation {
        WakeOperation::Start(_) => return Err(rejected(Code::Unsupported)),
        WakeOperation::Read(params) => {
            let snapshot = bridge
                .slot
                .observation_wake_snapshot(bridge.owner)
                .map_err(store_error)?;
            match params.query {
                WakeReadQuery::Attempt {
                    sequence: value,
                    operand_digest,
                } => {
                    sequence(value)?;
                    digest(&operand_digest)?;
                    let original = snapshot.receipt.filter(|entry| entry.sequence == value);
                    if original
                        .as_ref()
                        .is_some_and(|entry| entry.operand_digest != operand_digest)
                    {
                        return Err(rejected(Code::RevisionMismatch));
                    }
                    ThreadObservationWakeReadResponse::Attempt {
                        protocol: 1,
                        intent_floor: snapshot.intent_floor,
                        receipt: original.map(receipt),
                    }
                    .into()
                }
                WakeReadQuery::Fence { sequence: value } => {
                    sequence(value)?;
                    ThreadObservationWakeReadResponse::Fence {
                        protocol: 1,
                        intent_floor: snapshot.intent_floor,
                    }
                    .into()
                }
            }
        }
        WakeOperation::Invalidate(params) => {
            sequence(params.sequence)?;
            let intent_floor = bridge
                .slot
                .invalidate_observation_wake(bridge.owner, params.sequence)
                .map_err(store_error)?;
            ThreadObservationWakeInvalidateResponse {
                protocol: 1,
                intent_floor,
            }
            .into()
        }
        WakeOperation::Retire(params) => {
            sequence(params.sequence)?;
            digest(&params.operand_digest)?;
            let intent_floor = bridge
                .slot
                .retire_observation_wake(bridge.owner, params.sequence, &params.operand_digest)
                .map_err(store_error)?;
            ThreadObservationWakeRetireResponse {
                protocol: 1,
                intent_floor,
            }
            .into()
        }
    })
}

async fn start(
    bridge: &crate::observation_bridge::ObservationBridge,
    thread: &codex_core::CodexThread,
    params: ThreadObservationWakeStartParams,
) -> Result<ThreadObservationWakeStartResponse, JSONRPCErrorError> {
    let policy = thread
        .thread_extension_data()
        .get::<ObservationWakeHostPolicy>()
        .ok_or_else(|| rejected(Code::Unsupported))?;
    if policy.thread_id != bridge.thread_id || policy.owner != bridge.owner {
        return Err(rejected(Code::Denied));
    }
    let intent = ObservationWakeIntent {
        sequence: params.intent.sequence,
        frame_revision: params.intent.frame_revision,
        frame_hash: params.intent.frame_hash,
        budget_generation: params.intent.budget_generation,
        expected_commit_order: params.intent.expected_commit_order,
    };
    for value in [
        intent.sequence,
        intent.frame_revision,
        intent.budget_generation,
        intent.expected_commit_order,
    ] {
        sequence(value)?;
    }
    digest(&intent.frame_hash)?;
    digest(&params.operand_digest)?;
    if intent.operand_digest(bridge.owner) != params.operand_digest {
        return Err(rejected(Code::InvalidInput));
    }
    let native = thread
        .start_observation_wake(bridge.owner, intent, Arc::clone(&policy.admission))
        .await
        // Preparation may already have consumed the original intent.
        // Never turn a later owner/transport failure into no-commit proof.
        .map_err(|_| internal_error("original observation wake outcome unavailable"))?;
    Ok(ThreadObservationWakeStartResponse {
        protocol: 1,
        receipt: receipt(native),
    })
}

#[cfg(test)]
#[path = "thread_observation_wake_tests.rs"]
mod tests;
