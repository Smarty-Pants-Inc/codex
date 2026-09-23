//! Pending count operations belong to the original PilotLedger and never refund.
use super::PilotAuthorityError;
use super::PilotLedger;
use super::PilotRequestReservation;
use super::journal::CountDebit;
use super::journal::CountJournalRecord;
use super::journal::PilotCountJournal;
use crate::observation::ObservationClock;
use codex_api::CountWire;
use codex_client::Request;
use codex_client::RequestBody;
use codex_client::RequestCompression;
use sha2::Digest;
use sha2::Sha256;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;

#[path = "observation_pilot_count_admission.rs"]
mod admission;

#[path = "observation_pilot_count_operation.rs"]
mod operation;

/// Trusted original issuer input after protected scope/semantics qualification.
/// No serde or RPC constructor: metadata and matching hashes alone are not rights.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PilotCountScope {
    pub allocation_id: String,
    pub instruction: String,
    pub decision_sha256: String,
    pub credential_generation: String,
    pub credential_receipt: Uuid,
    pub semantics_sha256: String,
    pub wire_model: String,
    pub provider: String,
    pub purpose: String,
    pub account: String,
    pub inference_url: String,
    pub count_url: String,
    pub not_before: i64,
    pub expires_at: i64,
    pub operations: u8,
    pub output_tokens: NonZeroU64,
    pub context_tokens: NonZeroU64,
}

// These entries retain spend after a plan/future disappears. They are not a
// second credit balance: attempts and tokens debit PilotLedger's existing fields.
pub(super) struct PendingCount {
    brand: Arc<()>,
    durable: bool,
    completed: bool,
    consumed: bool,
    pub(super) attempt_id: Uuid,
    pub(super) input_tokens: Option<u64>,
    pub(super) output_tokens: u64,
    debit: Arc<CountJournalRecord>,
    reservation: PilotRequestReservation,
}

/// Private, non-cloneable plan. IDs diagnose joins; Arc identity authorizes them.
pub(in crate::observation) struct CountPlan {
    brand: Arc<()>,
    scope: PilotCountScope,
    journal: Arc<PilotCountJournal>,
    debit: Arc<CountJournalRecord>,
    pub(in crate::observation) decision_id: Uuid,
    pub(in crate::observation) attempt_id: Uuid,
    pub(in crate::observation) request_id: Uuid,
    operation: usize,
    deadline: Instant,
    wire: Arc<CountWire>,
    reservation: PilotRequestReservation,
    count_request: Request,
    factory: codex_http_client::HttpClientFactory,
}

/// A successful sync of this exact original debit, not authenticated completion.
pub(in crate::observation) struct DurableCountPlan {
    pub(in crate::observation) original: CountPlan,
    _synced: (),
}

impl PilotLedger {
    pub(in crate::observation) fn prepare_count(
        &mut self,
        now: ObservationClock,
        turn_id: &str,
        decision_id: Uuid,
        request: &Request,
        wire: Arc<CountWire>,
    ) -> Result<CountPlan, PilotAuthorityError> {
        self.expired |= now.monotonic >= self.deadline
            || now.wall_seconds >= self.claims.expires_at
            || self.issuer.recheck_grant(&self.claims).is_err();
        self.expired |= self
            .count_journal
            .as_ref()
            .is_some_and(|journal| journal.failed());
        if self.expired {
            return Err(PilotAuthorityError::Expired);
        }
        if !self.admissions.contains_key(turn_id)
            && !self
                .claims
                .permissions
                .contains(&super::PilotPermission::ForegroundTurn)
        {
            return Err(PilotAuthorityError::Denied);
        }
        let scope = self
            .issuer
            .count_scope(&self.claims)?
            .ok_or(PilotAuthorityError::Unavailable)?;
        let journal = self
            .count_journal
            .clone()
            .ok_or(PilotAuthorityError::Unavailable)?;
        if self.pending_counts.iter().any(|pending| !pending.consumed)
            || self
                .records
                .iter()
                .any(|record| !record.response_complete || record.usage.is_none())
        {
            return Err(PilotAuthorityError::Replay);
        }
        if !(1..=8).contains(&scope.operations)
            || self.pending_counts.len() >= usize::from(scope.operations)
            || self.attempts >= self.claims.max_attempts
        {
            return Err(PilotAuthorityError::Exhausted);
        }
        if scope.not_before > now.wall_seconds
            || scope.expires_at <= now.wall_seconds
            || scope.expires_at > self.claims.expires_at
            || wire.model() != scope.wire_model
            || wire.output_tokens() != scope.output_tokens
            || scope.output_tokens > scope.context_tokens
            || scope.credential_receipt.is_nil()
            || request.url != scope.inference_url
            || request.method != http::Method::POST
            || request.compression != RequestCompression::None
            || !matches!(&request.body, Some(RequestBody::EncodedJson(body))
                if body.as_bytes() == wire.inference_body().as_bytes())
        {
            return Err(PilotAuthorityError::Denied);
        }
        for value in [
            &scope.allocation_id,
            &scope.instruction,
            &scope.credential_generation,
            &scope.provider,
            &scope.purpose,
            &scope.account,
        ] {
            if value.is_empty() || value.len() > 512 || !value.bytes().all(|b| b.is_ascii_graphic())
            {
                return Err(PilotAuthorityError::Denied);
            }
        }
        for value in [&scope.decision_sha256, &scope.semantics_sha256] {
            if value.len() != 64
                || !value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            {
                return Err(PilotAuthorityError::Denied);
            }
        }
        for address in [&scope.count_url, &scope.inference_url] {
            let url = url::Url::parse(address).map_err(|_| PilotAuthorityError::Denied)?;
            if url.scheme() != "https"
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.fragment().is_some()
            {
                return Err(PilotAuthorityError::Denied);
            }
        }
        self.issuer
            .validate_count_inference(&self.claims, request)?;
        let mut count_request = Request::new(http::Method::POST, scope.count_url.clone());
        count_request.body = Some(RequestBody::EncodedJson(wire.count_body().clone()));
        let mut count_request = count_request
            .into_prepared()
            .map_err(|_| PilotAuthorityError::Denied)?;
        self.issuer
            .authenticate_count_request(&self.claims, &mut count_request)?;
        if count_request.url != scope.count_url
            || count_request.method != http::Method::POST
            || count_request.compression != RequestCompression::None
            || !matches!(&count_request.body, Some(RequestBody::EncodedJson(body))
                if body.as_bytes() == wire.count_body().as_bytes())
        {
            return Err(PilotAuthorityError::Denied);
        }
        #[cfg(all(test, target_os = "linux"))]
        if let Some(controlled) = request.extensions.get::<Arc<operation::ControlledIo>>() {
            count_request.extensions.insert(Arc::clone(controlled));
        }
        let factory = self.issuer.count_transport_factory(&self.claims)?;
        let reserved = self
            .reserved_tokens
            .checked_add(scope.context_tokens.get())
            .filter(|total| *total <= self.claims.max_reserved_tokens.get())
            .ok_or(PilotAuthorityError::Exhausted)?;
        let remaining = scope
            .expires_at
            .checked_sub(now.wall_seconds)
            .and_then(|remaining| u64::try_from(remaining).ok())
            .ok_or(PilotAuthorityError::Expired)?;
        let deadline = now
            .monotonic
            .checked_add(std::time::Duration::from_secs(remaining))
            .ok_or(PilotAuthorityError::Denied)?
            .min(self.deadline);
        let attempt_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let operation = self.pending_counts.len();
        let brand = Arc::new(());
        let reservation = PilotRequestReservation {
            token_ceiling: scope.context_tokens,
            credential_receipt: scope.credential_receipt,
            context_receipt: Uuid::new_v4(),
        };
        let debit = Arc::new(CountJournalRecord::Debit(CountDebit {
            version: 1,
            kind: "debit",
            allocation_id: scope.allocation_id.clone(),
            instruction: scope.instruction.clone(),
            decision_sha256: scope.decision_sha256.clone(),
            owner_connection_id: self.claims.owner.connection_id.to_string(),
            owner_epoch: self.claims.owner.epoch.to_string(),
            thread_id: self.claims.thread_id.to_string(),
            grant_id: self.claims.grant_id.to_string(),
            issuer_generation: self.claims.issuer_generation.to_string(),
            credential_generation: scope.credential_generation.clone(),
            operation: (operation + 1) as u8,
            decision_id: decision_id.to_string(),
            attempt_id: attempt_id.to_string(),
            request_id: request_id.to_string(),
            inference_sha256: format!("{:x}", Sha256::digest(wire.inference_body().as_bytes())),
            count_sha256: format!("{:x}", Sha256::digest(wire.count_body().as_bytes())),
            semantics_sha256: scope.semantics_sha256.clone(),
            wire_model: scope.wire_model.clone(),
            requested_output: scope.output_tokens.to_string(),
            token_ceiling: scope.context_tokens.to_string(),
        }));
        // The existing balance is charged BEFORE durable IO. Dropping this plan
        // cannot remove its pending entry or recover attempts/tokens.
        self.attempts += 1;
        self.reserved_tokens = reserved;
        self.pending_counts.push(PendingCount {
            brand: Arc::clone(&brand),
            durable: false,
            completed: false,
            consumed: false,
            attempt_id,
            input_tokens: None,
            output_tokens: scope.output_tokens.get(),
            debit: Arc::clone(&debit),
            reservation: reservation.clone(),
        });
        Ok(CountPlan {
            brand,
            scope,
            journal,
            debit,
            decision_id,
            attempt_id,
            request_id,
            operation,
            deadline,
            wire,
            reservation,
            count_request,
            factory,
        })
    }

    /// Recheck original identity after durable IO and before any count send.
    /// The caller must also hold the original slot/decision and transport lease.
    pub(in crate::observation) fn acknowledge_count_debit(
        &mut self,
        now: ObservationClock,
        plan: &DurableCountPlan,
    ) -> Result<(), PilotAuthorityError> {
        let plan = &plan.original;
        self.expired |= now.monotonic >= plan.deadline
            || now.wall_seconds >= plan.scope.expires_at
            || self.issuer.recheck_grant(&self.claims).is_err();
        self.expired |= self
            .count_journal
            .as_ref()
            .is_some_and(|journal| journal.failed());
        if self.expired {
            return Err(PilotAuthorityError::Expired);
        }
        if self.issuer.count_scope(&self.claims)?.as_ref() != Some(&plan.scope) {
            self.expired = true;
            return Err(PilotAuthorityError::Denied);
        }
        let pending = self
            .pending_counts
            .get_mut(plan.operation)
            .ok_or(PilotAuthorityError::Denied)?;
        if !Arc::ptr_eq(&pending.brand, &plan.brand) || pending.durable {
            return Err(PilotAuthorityError::Replay);
        }
        pending.durable = true;
        Ok(())
    }
}

impl CountPlan {
    /// Must run outside the slot lock on a blocking worker retaining the original
    /// transport lease. Cancellation of the waiter does not cancel the append.
    pub(in crate::observation) fn persist(self) -> Result<DurableCountPlan, PilotAuthorityError> {
        self.journal.append(&self.debit)?;
        Ok(DurableCountPlan {
            original: self,
            _synced: (),
        })
    }
}
