use super::MAX_SEQUENCE;
use super::ObservationCapture;
use super::ObservationError;
use super::ObservationEvent;
use super::ObservationMetadata;
use super::ObservationSlot;
use tokio::sync::mpsc::OwnedPermit;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationOutcome {
    Accepted,
    Rejected,
    Unknown,
}

/// Body-free outcome for one concrete attempt, or the terminal unsent decision.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationSubmitted {
    pub turn_id: String,
    pub decision_id: Uuid,
    pub attempt_id: Uuid,
    pub request_id: Uuid,
    pub provider_request_id: Option<String>,
    pub metadata: ObservationMetadata,
    pub captured_at: i64,
    pub commit_order: u64,
    pub outcome: ObservationOutcome,
    pub terminal_decision: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ObservationAttempt {
    decision_id: Uuid,
    attempt_id: Uuid,
}

pub(super) struct DecisionAudit {
    record: ObservationSubmitted,
    attempt_started: bool,
    terminal: OwnedPermit<ObservationEvent>,
}

impl DecisionAudit {
    pub(super) fn new(
        capture: &ObservationCapture,
        terminal: OwnedPermit<ObservationEvent>,
    ) -> Self {
        Self {
            record: ObservationSubmitted {
                turn_id: capture.turn_id.clone(),
                decision_id: capture.decision_id,
                attempt_id: Uuid::new_v4(),
                request_id: Uuid::new_v4(),
                provider_request_id: None,
                metadata: capture.metadata.clone(),
                captured_at: capture.captured_at,
                commit_order: 0,
                outcome: ObservationOutcome::Rejected,
                terminal_decision: false,
            },
            attempt_started: false,
            terminal,
        }
    }

    pub(super) fn decision_id(&self) -> Uuid {
        self.record.decision_id
    }
}

impl ObservationSlot {
    /// Reserve room for the preceding attempt's nonterminal outcome before a new
    /// send. The terminal outcome's capacity/order were reserved at capture time.
    pub(crate) fn begin_attempt(
        &self,
        decision_id: Uuid,
    ) -> Result<ObservationAttempt, ObservationError> {
        let mut state = self.state.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => ObservationError::ResourceLimit,
            std::sync::TryLockError::Poisoned(_) => ObservationError::Unavailable,
        })?;
        if state.revoked || self.events.is_closed() {
            return Err(ObservationError::Unavailable);
        }
        let audit = state
            .active_capture
            .as_ref()
            .ok_or(ObservationError::Unavailable)?;
        if audit.decision_id() != decision_id {
            return Err(ObservationError::RevisionMismatch);
        }
        if audit.attempt_started {
            if state.commit_order >= MAX_SEQUENCE - 1 {
                return Err(ObservationError::ResourceLimit);
            }
            let permit = self
                .events
                .try_reserve()
                .map_err(|_| ObservationError::ResourceLimit)?;
            let mut previous = audit.record.clone();
            state.commit_order += 1;
            previous.commit_order = state.commit_order;
            permit.send(ObservationEvent::Submitted(previous));
        }
        let audit = state
            .active_capture
            .as_mut()
            .ok_or(ObservationError::Unavailable)?;
        audit.record.attempt_id = Uuid::new_v4();
        audit.record.request_id = Uuid::new_v4();
        audit.record.provider_request_id = None;
        audit.record.outcome = ObservationOutcome::Unknown;
        audit.attempt_started = true;
        Ok(ObservationAttempt {
            decision_id,
            attempt_id: audit.record.attempt_id,
        })
    }

    /// A concrete attempt token cannot update a later retry/decision. Header ID
    /// is actual upstream x-request-id only; malformed/oversize values stay null.
    pub(crate) fn attempt_headers(
        &self,
        attempt: ObservationAttempt,
        id: Option<&str>,
    ) -> Result<(), ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        if let Some(audit) = state.active_capture.as_mut()
            && audit.record.decision_id == attempt.decision_id
            && audit.record.attempt_id == attempt.attempt_id
        {
            audit.record.provider_request_id = id
                .filter(|id| {
                    !id.is_empty()
                        && id.len() <= 256
                        && id.bytes().all(|byte| byte.is_ascii_graphic())
                })
                .map(str::to_owned);
        }
        Ok(())
    }

    /// Acceptance and finalization serialize under the slot lock. After the
    /// attempt is finalized its callback is fenced and cannot alter another
    /// record. Unknown remains honest when observation ended before acceptance.
    pub(crate) fn attempt_accepted(
        &self,
        attempt: ObservationAttempt,
    ) -> Result<(), ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        if let Some(audit) = state.active_capture.as_mut()
            && audit.record.decision_id == attempt.decision_id
            && audit.record.attempt_id == attempt.attempt_id
        {
            audit.record.outcome = ObservationOutcome::Accepted;
        }
        Ok(())
    }

    /// Commit the one terminal record before releasing capture. Even a full FIFO
    /// cannot consume its reserved capacity. No prior send means rejected/unsent;
    /// a send without observed created remains unknown, including cancellation.
    pub fn release(&self, decision_id: Uuid) -> Result<(), ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        if state
            .active_capture
            .as_ref()
            .map(DecisionAudit::decision_id)
            != Some(decision_id)
        {
            return Err(ObservationError::RevisionMismatch);
        }
        let Some(mut audit) = state.active_capture.take() else {
            return Err(ObservationError::Unavailable);
        };
        state.commit_order += 1;
        audit.record.commit_order = state.commit_order;
        audit.record.terminal_decision = true;
        audit
            .terminal
            .send(ObservationEvent::Submitted(audit.record));
        Ok(())
    }
}

#[cfg(test)]
#[path = "observation_audit_tests.rs"]
mod tests;
