//! Original-thread native pilot entry points. These never manufacture launch authority.
use super::CodexThread;
use crate::NativePilotIssuer;
use crate::ObservationBinding;
use crate::ObservationOwner;
use crate::PilotAuthorityError;
use crate::PilotPermission;
use crate::PilotReport;
use codex_protocol::error::CodexErr;
use codex_protocol::error::Result as CodexResult;
use codex_protocol::turn_input::IdleTurnSource;
use codex_protocol::turn_input::StartIfIdleSubmission;
use codex_protocol::turn_input::TurnInput;
use codex_protocol::turn_input::TurnInputRequest;
use std::sync::Arc;
use uuid::Uuid;

/// The original native thread and its request/decoder callback leases have joined;
/// its active decision was finalized. Queued audit delivery is still external custody.
/// The original process owner must join transport/process-tree and observer/store
/// custody. Provider completion does not prove remote effect cancellation or cleanup.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativePilotRetirement {
    pub report: PilotReport,
    pub remaining_retirement: PilotRetirementRemainder,
}

/// Core cannot certify resources controlled by the original external owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PilotRetirementRemainder {
    OriginalProcessOuterCustodyAndRemoteEffectsUnverified,
}

impl CodexThread {
    /// Install only while excluding actual native turns and idle-start reservations.
    /// `issuer` must come from the trusted launch bootstrap, never an RPC grant field.
    pub async fn install_pilot_authority(
        &self,
        owner: ObservationOwner,
        envelope: &[u8],
        issuer: Arc<dyn NativePilotIssuer>,
    ) -> Result<(), PilotAuthorityError> {
        let active = self.session.active_turn.lock().await;
        if active.is_some() {
            return Err(PilotAuthorityError::Denied);
        }
        let binding = self
            .thread_extension_data()
            .get::<ObservationBinding>()
            .ok_or(PilotAuthorityError::Unavailable)?;
        binding
            .slot
            .install_pilot_authority(owner, self.session.thread_id, envelope, issuer)
    }

    pub fn check_pilot_grant(
        &self,
        owner: ObservationOwner,
        scope: &str,
        permission: PilotPermission,
        request_id: Uuid,
    ) -> Result<(), PilotAuthorityError> {
        let binding = self
            .thread_extension_data()
            .get::<ObservationBinding>()
            .ok_or(PilotAuthorityError::Unavailable)?;
        binding
            .slot
            .check_pilot_grant(owner, scope, permission, request_id)
    }

    /// The actual installed observation binding owns both commit authority and
    /// HTTP-attempt budgeting; a detached slot cannot authorize an unmetered turn.
    pub async fn start_pilot_turn(
        &self,
        owner: ObservationOwner,
        scope: String,
        request_id: Uuid,
        request: TurnInputRequest,
    ) -> CodexResult<StartIfIdleSubmission> {
        if request.idle_turn_admission.is_some()
            || request.idle_turn_source != IdleTurnSource::Unspecified
        {
            return Err(CodexErr::InvalidRequest(
                "pilot admission cannot replace another owner guard".to_owned(),
            ));
        }
        match &request.input {
            TurnInput::UserInput { content, .. } | TurnInput::DeveloperInput { content }
                if !content.is_empty() =>
            {
                return Err(CodexErr::InvalidRequest(
                    "pilot admission requires automatic input".to_owned(),
                ));
            }
            TurnInput::UserInput { .. }
            | TurnInput::DeveloperInput { .. }
            | TurnInput::ResponseItem(_)
            | TurnInput::InterAgentCommunication(_) => {}
        }
        let binding = self
            .thread_extension_data()
            .get::<ObservationBinding>()
            .ok_or_else(|| {
                CodexErr::InvalidRequest("native pilot binding unavailable".to_owned())
            })?;
        let guard = binding.slot.pilot_turn_guard(owner, scope, request_id);
        self.start_turn_if_idle(request.with_idle_turn_admission(guard))
            .await
    }

    /// Revoke first, then join the ORIGINAL native session shutdown. Cancellation
    /// of this future never restores authority or refunds attempts. The caller must
    /// retain this thread/slot custody and finish the same join; it must not substitute
    /// a fresh process, an EOF observation or a resolved wrapper for physical cleanup.
    pub async fn retire_pilot(
        &self,
        owner: ObservationOwner,
    ) -> CodexResult<NativePilotRetirement> {
        let binding = self
            .thread_extension_data()
            .get::<ObservationBinding>()
            .ok_or_else(|| {
                CodexErr::InvalidRequest("native pilot binding unavailable".to_owned())
            })?;
        binding
            .slot
            .pilot_report(owner)
            .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
        binding
            .slot
            .revoke()
            .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
        self.shutdown_and_wait().await?;
        binding.slot.wait_for_transport_drain().await;
        let report = binding
            .slot
            .pilot_report(owner)
            .map_err(|error| CodexErr::InvalidRequest(error.to_string()))?;
        if report.active_decision.is_some() {
            return Err(CodexErr::InvalidRequest(
                "native pilot audit has not retired".to_owned(),
            ));
        }
        Ok(NativePilotRetirement {
            report,
            remaining_retirement:
                PilotRetirementRemainder::OriginalProcessOuterCustodyAndRemoteEffectsUnverified,
        })
    }
}
