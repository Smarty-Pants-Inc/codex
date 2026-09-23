use super::MAX_SEQUENCE;
use super::ObservationCapture;
use super::ObservationError;
use super::ObservationEvent;
use super::ObservationMetadata;
use super::ObservationSlot;
use codex_client::RequestBody;
use codex_client::RequestCompression;
use codex_protocol::models::ResponseItem;
use sha2::Digest;
use sha2::Sha256;
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
    pub(super) decision_id: Uuid,
    pub(super) attempt_id: Uuid,
}

pub(super) struct DecisionAudit {
    record: ObservationSubmitted,
    attempt_started: bool,
    terminal: OwnedPermit<ObservationEvent>,
    request_input: Option<[u8; 32]>,
    deadline: Option<std::time::Instant>,
}

impl DecisionAudit {
    pub(super) fn new(
        capture: &ObservationCapture,
        terminal: OwnedPermit<ObservationEvent>,
        request_input: Option<[u8; 32]>,
        deadline: Option<std::time::Instant>,
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
            request_input,
            deadline,
        }
    }

    fn validate_request(
        &self,
        request: Option<&codex_client::Request>,
    ) -> Result<(), ObservationError> {
        let Some(expected) = self.request_input else {
            // Store-only captures do not claim a rendered request overlay.
            return Ok(());
        };
        let request = request.ok_or(ObservationError::InvalidFrame)?;
        if request.method != http::Method::POST
            || request.compression != RequestCompression::None
            || request.headers.contains_key(http::header::CONTENT_ENCODING)
        {
            return Err(ObservationError::InvalidFrame);
        }
        let body = match request.body.as_ref() {
            Some(RequestBody::Json(body)) => body.clone(),
            Some(RequestBody::EncodedJson(body)) => serde_json::from_slice(body.as_bytes())
                .map_err(|_| ObservationError::InvalidFrame)?,
            Some(RequestBody::Raw(_)) | None => return Err(ObservationError::InvalidFrame),
        };
        if body.get("previous_response_id").is_some()
            || body.get("stream") != Some(&serde_json::Value::Bool(true))
            || body.get("store") != Some(&serde_json::Value::Bool(false))
        {
            return Err(ObservationError::InvalidFrame);
        }
        let input = body
            .get("input")
            .filter(|input| input.is_array())
            .cloned()
            .ok_or(ObservationError::InvalidFrame)?;
        if input_digest(input)? != expected {
            return Err(ObservationError::InvalidFrame);
        }
        Ok(())
    }

    fn validate_lease(&self, now: super::ObservationClock) -> Result<(), ObservationError> {
        if self.request_input.is_some()
            && self.deadline.is_some()
            && (self
                .deadline
                .is_some_and(|deadline| now.monotonic >= deadline)
                || self
                    .record
                    .metadata
                    .expires_at
                    .is_some_and(|expiry| now.wall_seconds >= expiry))
        {
            return Err(ObservationError::InvalidFrame);
        }
        Ok(())
    }

    pub(super) fn decision_id(&self) -> Uuid {
        self.record.decision_id
    }

    pub(in crate::observation) fn matches_pending_count(
        &self,
        decision: Uuid,
        attempt: Uuid,
        request: Uuid,
    ) -> bool {
        !self.attempt_started
            && self.record.decision_id == decision
            && self.record.attempt_id == attempt
            && self.record.request_id == request
    }
}

impl super::SlotState {
    fn active_budget_valid(&self) -> bool {
        self.budget.as_ref().is_none_or(|budget| {
            budget.snapshot.state == super::ObservationReservationState::Valid
                && self.active_capture.as_ref().is_some_and(|audit| {
                    audit
                        .record
                        .metadata
                        .native_reservation
                        .as_ref()
                        .is_some_and(|captured| captured.generation == budget.snapshot.generation)
                })
        })
    }
}

fn input_digest(mut input: serde_json::Value) -> Result<[u8; 32], ObservationError> {
    // Compare JSON objects independent of key order without deserializing into
    // ResponseItem: that would discard warehouse-only/unknown wire fields.
    input.sort_all_objects();
    let bytes = serde_json::to_vec(&input).map_err(|_| ObservationError::InvalidFrame)?;
    Ok(Sha256::digest(bytes).into())
}

impl ObservationSlot {
    /// Bind the complete ordered sender-normalized input, including annotations,
    /// not a marker search or a subset of group parts.
    pub(crate) fn request_input_digest(
        input: Vec<ResponseItem>,
    ) -> Result<[u8; 32], ObservationError> {
        input_digest(serde_json::to_value(input).map_err(|_| ObservationError::InvalidFrame)?)
    }

    /// Reserve room for the preceding attempt's nonterminal outcome before a new
    /// send. The terminal outcome's capacity/order were reserved at capture time.
    pub(crate) fn begin_attempt(
        &self,
        decision_id: Uuid,
    ) -> Result<ObservationAttempt, ObservationError> {
        self.begin_attempt_inner(decision_id, /*request*/ None)
    }

    pub(crate) fn begin_attempt_for_request(
        &self,
        decision_id: Uuid,
        request: &codex_client::Request,
    ) -> Result<ObservationAttempt, ObservationError> {
        self.begin_attempt_inner(decision_id, Some(request))
    }

    /// Allocate the original audit IDs and charge the existing ledger before
    /// count IO. This is a pending request, NOT an inference transport start.
    pub(in crate::observation) fn prepare_count_attempt(
        &self,
        decision_id: Uuid,
        request: &codex_client::Request,
        wire: std::sync::Arc<codex_api::CountWire>,
    ) -> Result<super::pilot::count::CountPlan, ObservationError> {
        let mut state = self.state.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => ObservationError::ResourceLimit,
            std::sync::TryLockError::Poisoned(_) => ObservationError::Unavailable,
        })?;
        let previous = self.reserve_previous_attempt(&state, decision_id, Some(request))?;
        let turn_id = state
            .active_capture
            .as_ref()
            .ok_or(ObservationError::Unavailable)?
            .record
            .turn_id
            .clone();
        let plan = state
            .pilot
            .as_mut()
            .ok_or(ObservationError::Unavailable)?
            .prepare_count((self.clock)()?, &turn_id, decision_id, request, wire)
            .map_err(|_| ObservationError::Unavailable)?;
        if let Some((mut previous, permit)) = previous {
            state.commit_order += 1;
            previous.commit_order = state.commit_order;
            permit.send(ObservationEvent::Submitted(previous));
        }
        let audit = state
            .active_capture
            .as_mut()
            .ok_or(ObservationError::Unavailable)?;
        audit.record.attempt_id = plan.attempt_id;
        audit.record.request_id = plan.request_id;
        audit.record.provider_request_id = None;
        audit.record.outcome = ObservationOutcome::Rejected;
        audit.attempt_started = false;
        Ok(plan)
    }

    /// Rejoin the original still-pending audit after durable IO. The ledger
    /// additionally checks its private operation identity, scope and generation.
    pub(in crate::observation) fn acknowledge_count_debit(
        &self,
        plan: &super::pilot::count::DurableCountPlan,
    ) -> Result<(), super::pilot::PilotAuthorityError> {
        use super::pilot::PilotAuthorityError;
        let mut state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        let original = &plan.original;
        if state.revoked
            || self.events.is_closed()
            || !state.active_budget_valid()
            || !state.active_capture.as_ref().is_some_and(|audit| {
                !audit.attempt_started
                    && audit.record.decision_id == original.decision_id
                    && audit.record.attempt_id == original.attempt_id
                    && audit.record.request_id == original.request_id
            })
        {
            return Err(PilotAuthorityError::Denied);
        }
        state
            .pilot
            .as_mut()
            .ok_or(PilotAuthorityError::Unavailable)?
            .acknowledge_count_debit(
                (self.clock)().map_err(|_| PilotAuthorityError::Unavailable)?,
                plan,
            )
    }

    /// Atomically consume one final receipt and mark only its original pending
    /// audit as an inference attempt. The reservation was already charged.
    pub(in crate::observation) fn consume_count_receipt(
        &self,
        plan: &super::pilot::count::CountPlan,
        actual: &codex_client::Request,
    ) -> Result<(), super::pilot::PilotAuthorityError> {
        use super::pilot::PilotAuthorityError;
        let mut state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        if state.revoked
            || self.events.is_closed()
            || !state.active_budget_valid()
            || !state.active_capture.as_ref().is_some_and(|audit| {
                audit.matches_pending_count(plan.decision_id, plan.attempt_id, plan.request_id)
            })
        {
            return Err(PilotAuthorityError::Denied);
        }
        let audit = state
            .active_capture
            .as_ref()
            .ok_or(PilotAuthorityError::Unavailable)?;
        audit
            .validate_request(Some(actual))
            .map_err(|_| PilotAuthorityError::Denied)?;
        audit
            .validate_lease((self.clock)().map_err(|_| PilotAuthorityError::Unavailable)?)
            .map_err(|_| PilotAuthorityError::Denied)?;
        let reservation = state
            .pilot
            .as_mut()
            .ok_or(PilotAuthorityError::Unavailable)?
            .consume_count_plan(
                (self.clock)().map_err(|_| PilotAuthorityError::Unavailable)?,
                plan,
                actual,
            )?;
        let audit = state
            .active_capture
            .as_mut()
            .ok_or(PilotAuthorityError::Denied)?;
        audit.attempt_started = true;
        audit.record.outcome = ObservationOutcome::Unknown;
        let record = audit.record.clone();
        state
            .pilot
            .as_mut()
            .ok_or(PilotAuthorityError::Unavailable)?
            .record_attempt(&record, reservation);
        Ok(())
    }

    fn reserve_previous_attempt(
        &self,
        state: &super::SlotState,
        decision_id: Uuid,
        request: Option<&codex_client::Request>,
    ) -> Result<
        Option<(
            ObservationSubmitted,
            tokio::sync::mpsc::Permit<'_, ObservationEvent>,
        )>,
        ObservationError,
    > {
        if state.revoked || self.events.is_closed() {
            return Err(ObservationError::Unavailable);
        }
        if !state.active_budget_valid() {
            return Err(ObservationError::BudgetInvalid);
        }
        let audit = state
            .active_capture
            .as_ref()
            .ok_or(ObservationError::Unavailable)?;
        if audit.decision_id() != decision_id {
            return Err(ObservationError::RevisionMismatch);
        }
        audit.validate_request(request)?;
        audit.validate_lease((self.clock)()?)?;
        if !audit.attempt_started {
            return Ok(None);
        }
        if state.commit_order >= MAX_SEQUENCE - 1 {
            return Err(ObservationError::ResourceLimit);
        }
        let permit = self
            .events
            .try_reserve()
            .map_err(|_| ObservationError::ResourceLimit)?;
        Ok(Some((audit.record.clone(), permit)))
    }

    fn begin_attempt_inner(
        &self,
        decision_id: Uuid,
        request: Option<&codex_client::Request>,
    ) -> Result<ObservationAttempt, ObservationError> {
        let mut state = self.state.try_lock().map_err(|error| match error {
            std::sync::TryLockError::WouldBlock => ObservationError::ResourceLimit,
            std::sync::TryLockError::Poisoned(_) => ObservationError::Unavailable,
        })?;
        let previous = self.reserve_previous_attempt(&state, decision_id, request)?;
        let turn_id = state
            .active_capture
            .as_ref()
            .ok_or(ObservationError::Unavailable)?
            .record
            .turn_id
            .clone();
        // Check all fallible native audit capacity before reserving a pilot send.
        let reservation = if let Some(pilot) = state.pilot.as_mut() {
            let request = request.ok_or(ObservationError::Unavailable)?;
            Some(
                pilot
                    .reserve((self.clock)()?, &turn_id, request)
                    .map_err(|_| ObservationError::Unavailable)?,
            )
        } else {
            None
        };
        if let Some((mut previous, permit)) = previous {
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
        let attempt = ObservationAttempt {
            decision_id,
            attempt_id: audit.record.attempt_id,
        };
        let reserved_record = reservation.map(|reservation| (audit.record.clone(), reservation));
        if let Some((record, reservation)) = reserved_record
            && let Some(pilot) = state.pilot.as_mut()
        {
            pilot.record_attempt(&record, reservation);
        }
        Ok(attempt)
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
