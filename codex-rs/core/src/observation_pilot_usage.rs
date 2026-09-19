//! Body-free, finite request accounting. Unknown/cancelled attempts retain their charge.
use super::PilotAuthorityError;
use super::PilotLedger;
use super::PilotRequestReservation;
use crate::ObservationOwner;
use crate::ObservationSlot;
use crate::ObservationSubmitted;
use crate::observation::ObservationAttempt;
use crate::observation::audit::DecisionAudit;
use codex_protocol::ThreadId;
use codex_protocol::protocol::TokenUsage;
use std::collections::BTreeMap;
use uuid::Uuid;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PilotAttemptRecord {
    pub decision_id: Uuid,
    pub attempt_id: Uuid,
    pub request_id: Uuid,
    pub turn_id: String,
    pub admission_request_id: Option<Uuid>,
    pub reservation: PilotRequestReservation,
    pub response_id: Option<String>,
    pub usage: Option<TokenUsage>,
    pub response_complete: bool,
    pub usage_conflict: bool,
}

/// A native accounting snapshot, never proof of process-tree or remote retirement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PilotReport {
    pub owner: ObservationOwner,
    pub thread_id: ThreadId,
    pub grant_id: Uuid,
    pub revoked: bool,
    pub active_decision: Option<Uuid>,
    pub admitted_turns: BTreeMap<String, Uuid>,
    pub reserved_tokens: u64,
    pub attempts: Vec<PilotAttemptRecord>,
}

impl PilotLedger {
    pub(in crate::observation) fn record_attempt(
        &mut self,
        record: &ObservationSubmitted,
        reservation: PilotRequestReservation,
    ) {
        self.records.push(PilotAttemptRecord {
            decision_id: record.decision_id,
            attempt_id: record.attempt_id,
            request_id: record.request_id,
            turn_id: record.turn_id.clone(),
            admission_request_id: self.admissions.get(&record.turn_id).copied(),
            reservation,
            response_id: None,
            usage: None,
            response_complete: false,
            usage_conflict: false,
        });
    }
}

impl ObservationSlot {
    /// The decoder binds this to the concrete attempt before forwarding events.
    /// Late completion can update only that retained attempt, never a newer retry.
    /// Duplicates cannot refund a reservation; conflicting/over-budget usage fences
    /// all future admissions while retaining the original report and charge.
    pub(crate) fn pilot_attempt_completed(
        &self,
        attempt: ObservationAttempt,
        response_id: &str,
        usage: Option<&TokenUsage>,
    ) -> Result<(), PilotAuthorityError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        let Some(ledger) = state.pilot.as_mut() else {
            return Ok(());
        };
        let Some(record) = ledger.records.iter_mut().find(|record| {
            record.decision_id == attempt.decision_id && record.attempt_id == attempt.attempt_id
        }) else {
            return Err(PilotAuthorityError::Denied);
        };
        let valid_id = !response_id.is_empty()
            && response_id.len() <= 256
            && response_id.bytes().all(|byte| byte.is_ascii_graphic());
        let valid_usage = usage.is_none_or(|usage| {
            [
                usage.input_tokens,
                usage.cached_input_tokens,
                usage.cache_write_input_tokens,
                usage.output_tokens,
                usage.reasoning_output_tokens,
                usage.total_tokens,
            ]
            .into_iter()
            .all(|count| count >= 0)
                && usage
                    .input_tokens
                    .checked_add(usage.output_tokens)
                    .is_some_and(|total| total <= usage.total_tokens)
                && u64::try_from(usage.total_tokens)
                    .is_ok_and(|total| total <= record.reservation.token_ceiling.get())
        });
        if !valid_id
            || !valid_usage
            || (record.response_complete
                && (record.response_id.as_deref() != Some(response_id)
                    || record.usage.as_ref() != usage))
        {
            record.usage_conflict = true;
            ledger.expired = true;
            return Err(PilotAuthorityError::Denied);
        }
        if !record.response_complete {
            record.response_id = Some(response_id.to_owned());
            record.usage = usage.cloned();
            record.response_complete = true;
        }
        Ok(())
    }

    /// Original-owner read remains available after revocation for cleanup/accounting.
    /// It does not reauthorize any work or infer that external resources retired.
    pub fn pilot_report(
        &self,
        owner: ObservationOwner,
    ) -> Result<PilotReport, PilotAuthorityError> {
        let state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        if state.owner != owner {
            return Err(PilotAuthorityError::Denied);
        }
        let ledger = state
            .pilot
            .as_ref()
            .ok_or(PilotAuthorityError::Unavailable)?;
        Ok(PilotReport {
            owner,
            thread_id: ledger.claims.thread_id,
            grant_id: ledger.claims.grant_id,
            revoked: state.revoked || ledger.expired,
            active_decision: state
                .active_capture
                .as_ref()
                .map(DecisionAudit::decision_id),
            admitted_turns: ledger.admissions.clone(),
            reserved_tokens: ledger.reserved_tokens,
            attempts: ledger.records.clone(),
        })
    }
}
