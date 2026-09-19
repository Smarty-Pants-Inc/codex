//! Native pilot authority. Only a trusted launch issuer may supply a grant.
//! Profile selection, source text and RPC ownership are not permission issuers.

use super::ObservationClock;
use super::ObservationOwner;
use super::ObservationSlot;
use codex_client::Request;
use codex_protocol::ThreadId;
use codex_protocol::turn_input::IdleTurnAdmission;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::fmt;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::sync::Weak;
use std::time::Duration;
use std::time::Instant;
use uuid::Uuid;

#[path = "observation_pilot_usage.rs"]
mod usage;
pub use usage::PilotAttemptRecord;
pub use usage::PilotReport;

#[cfg(test)]
#[path = "observation_pilot_tests.rs"]
mod tests;

const MAX_RECORDS: u32 = 256;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PilotPermission {
    PrepareSource,
    SampleSource,
    ActOnSource,
    ForegroundTurn,
    AutomaticTurn,
}

/// Claims returned by the trusted issuer, never deserialized as self-authenticating rights.
#[derive(Clone, Debug)]
pub struct PilotGrantClaims {
    pub grant_id: Uuid,
    /// Issuer-owned monotonic revocation generation, not a semantic policy ID.
    pub issuer_generation: u64,
    pub owner: ObservationOwner,
    pub thread_id: ThreadId,
    pub scope: String,
    pub permissions: BTreeSet<PilotPermission>,
    pub expires_at: i64,
    pub cooldown: Duration,
    pub max_turns: u32,
    pub max_attempts: u32,
    pub max_reserved_tokens: NonZeroU64,
}

/// An issuer-qualified ceiling for the actual post-authentication request, not an estimate.
/// Neither a model name nor an unverified receipt identifier constructs this qualification.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PilotRequestReservation {
    pub token_ceiling: NonZeroU64,
    pub credential_receipt: Uuid,
    pub context_receipt: Uuid,
}

/// Implemented by the existing trusted native launch authority, not by an RPC/model caller.
/// Implementations authenticate the opaque grant and validate actual protected credentials,
/// model, serializer and context/output bounds for each prepared send. Missing qualification
/// must return an error. Methods perform bounded local validation, never network IO or logging
/// of the request, credentials, grant envelope or source bodies.
pub trait NativePilotIssuer: Send + Sync {
    fn verify_grant(&self, envelope: &[u8]) -> Result<PilotGrantClaims, PilotAuthorityError>;
    /// Check current issuer revocation state without IO or re-entering the slot.
    /// Compare the granted issuer generation even after A-to-B-to-A policy changes.
    /// Any failure permanently fences this grant; a new owner epoch must reauthorize.
    fn recheck_grant(&self, claims: &PilotGrantClaims) -> Result<(), PilotAuthorityError>;
    /// Fence local issuer generation before native retirement can await anything.
    /// Like recheck, this must not perform IO or re-enter the slot.
    fn revoke(&self);
    /// Attach the original prepared credential before provider auth resolution.
    /// No ambient fallback is allowed when an installed issuer returns an error.
    fn authenticate_request(
        &self,
        claims: &PilotGrantClaims,
        request: &mut Request,
    ) -> Result<codex_client::RequestAuthentication, PilotAuthorityError>;
    /// Qualification only; the native slot owns the actual debit. Do not maintain
    /// another credit balance here or retain the borrowed credential/body data.
    fn qualify_request(
        &self,
        claims: &PilotGrantClaims,
        request: &Request,
    ) -> Result<PilotRequestReservation, PilotAuthorityError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum PilotAuthorityError {
    #[error("native pilot issuer or qualification unavailable")]
    Unavailable,
    #[error("native pilot authority mismatch")]
    Denied,
    #[error("native pilot authority expired or revoked")]
    Expired,
    #[error("native pilot request already consumed")]
    Replay,
    #[error("native pilot finite budget exhausted")]
    Exhausted,
}

pub(super) struct PilotLedger {
    claims: PilotGrantClaims,
    issuer: Arc<dyn NativePilotIssuer>,
    deadline: Instant,
    expired: bool,
    next_turn: Instant,
    admissions: BTreeMap<String, Uuid>,
    attempts: u32,
    reserved_tokens: u64,
    consumed: BTreeSet<Uuid>,
    records: Vec<PilotAttemptRecord>,
}

impl PilotLedger {
    pub(super) fn revoke(&mut self) {
        self.expired = true;
        self.issuer.revoke();
    }

    fn check(
        &mut self,
        now: ObservationClock,
        scope: &str,
        permission: PilotPermission,
    ) -> Result<(), PilotAuthorityError> {
        self.expired |=
            now.monotonic >= self.deadline || now.wall_seconds >= self.claims.expires_at;
        self.expired |= self.issuer.recheck_grant(&self.claims).is_err();
        if self.expired {
            return Err(PilotAuthorityError::Expired);
        }
        if scope != self.claims.scope || !self.claims.permissions.contains(&permission) {
            return Err(PilotAuthorityError::Denied);
        }
        Ok(())
    }

    fn consume(&mut self, request_id: Uuid) -> Result<(), PilotAuthorityError> {
        if request_id.is_nil() || self.consumed.contains(&request_id) {
            return Err(PilotAuthorityError::Replay);
        }
        if self.consumed.len() >= MAX_RECORDS as usize {
            return Err(PilotAuthorityError::Exhausted);
        }
        self.consumed.insert(request_id);
        Ok(())
    }

    pub(super) fn reserve(
        &mut self,
        now: ObservationClock,
        turn_id: &str,
        request: &Request,
    ) -> Result<PilotRequestReservation, PilotAuthorityError> {
        self.expired |=
            now.monotonic >= self.deadline || now.wall_seconds >= self.claims.expires_at;
        self.expired |= self.issuer.recheck_grant(&self.claims).is_err();
        if self.expired {
            return Err(PilotAuthorityError::Expired);
        }
        if !self.admissions.contains_key(turn_id)
            && !self
                .claims
                .permissions
                .contains(&PilotPermission::ForegroundTurn)
        {
            return Err(PilotAuthorityError::Denied);
        }
        if self.attempts >= self.claims.max_attempts {
            return Err(PilotAuthorityError::Exhausted);
        }
        let reservation = self.issuer.qualify_request(&self.claims, request)?;
        if reservation.credential_receipt.is_nil() || reservation.context_receipt.is_nil() {
            return Err(PilotAuthorityError::Unavailable);
        }
        let reserved = self
            .reserved_tokens
            .checked_add(reservation.token_ceiling.get())
            .filter(|total| *total <= self.claims.max_reserved_tokens.get())
            .ok_or(PilotAuthorityError::Exhausted)?;
        // ponytail: Hold each qualified ceiling through retirement, even when usage
        // is known. Reclaim credit only with an approved provider settlement contract;
        // retries, cancellation and uncertain requests must not create new credit.
        self.attempts += 1;
        self.reserved_tokens = reserved;
        Ok(reservation)
    }
}

impl ObservationSlot {
    pub(crate) fn has_pilot_authority(&self) -> Result<bool, PilotAuthorityError> {
        let state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        // Retain this route after expiry/revocation; never recover through ambient auth.
        Ok(state.pilot.is_some())
    }

    pub(crate) fn authenticate_pilot_request(
        &self,
        request: &mut Request,
    ) -> Result<codex_client::RequestAuthentication, PilotAuthorityError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        if state.revoked {
            return Err(PilotAuthorityError::Expired);
        }
        match &mut state.pilot {
            Some(pilot) => {
                pilot.expired |= pilot.issuer.recheck_grant(&pilot.claims).is_err();
                if pilot.expired {
                    return Err(PilotAuthorityError::Expired);
                }
                pilot.issuer.authenticate_request(&pilot.claims, request)
            }
            None => Ok(codex_client::RequestAuthentication::Provider),
        }
    }

    /// The native launch owner supplies the issuer out of band. RPC grant bytes alone
    /// cannot call this with a trusted issuer. One grant per native owner epoch; no reset.
    pub(crate) fn install_pilot_authority(
        &self,
        owner: ObservationOwner,
        thread_id: ThreadId,
        envelope: &[u8],
        issuer: Arc<dyn NativePilotIssuer>,
    ) -> Result<(), PilotAuthorityError> {
        if envelope.is_empty() || envelope.len() > 16_384 {
            return Err(PilotAuthorityError::Denied);
        }
        let claims = issuer.verify_grant(envelope)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        state
            .authorize(owner)
            .map_err(|_| PilotAuthorityError::Denied)?;
        let now = (self.clock)().map_err(|_| PilotAuthorityError::Unavailable)?;
        if state.pilot.is_some()
            || state.active_capture.is_some()
            || claims.owner != owner
            || claims.thread_id != thread_id
            || claims.grant_id.is_nil()
            || claims.scope.is_empty()
            || claims.scope.len() > 512
            || claims.permissions.is_empty()
            || claims.max_attempts == 0
            || claims.max_attempts > MAX_RECORDS
            || claims.max_turns > claims.max_attempts
            || (claims.permissions.contains(&PilotPermission::AutomaticTurn)
                && (claims.max_turns == 0 || claims.cooldown.is_zero()))
        {
            return Err(PilotAuthorityError::Denied);
        }
        let remaining = claims
            .expires_at
            .checked_sub(now.wall_seconds)
            .filter(|remaining| *remaining > 0)
            .ok_or(PilotAuthorityError::Expired)?;
        let deadline = now
            .monotonic
            .checked_add(Duration::from_secs(remaining as u64))
            .ok_or(PilotAuthorityError::Denied)?;
        state.pilot = Some(PilotLedger {
            claims,
            issuer,
            deadline,
            expired: false,
            next_turn: now.monotonic,
            admissions: BTreeMap::new(),
            attempts: 0,
            reserved_tokens: 0,
            consumed: BTreeSet::new(),
            records: Vec::new(),
        });
        Ok(())
    }

    /// Source/action permission consumes a unique request, not a cached Boolean grant.
    /// Automatic turns can only consume authority through Core's native commit path.
    pub(crate) fn check_pilot_grant(
        &self,
        owner: ObservationOwner,
        scope: &str,
        permission: PilotPermission,
        request_id: Uuid,
    ) -> Result<(), PilotAuthorityError> {
        if permission == PilotPermission::AutomaticTurn {
            return Err(PilotAuthorityError::Denied);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        state
            .authorize(owner)
            .map_err(|_| PilotAuthorityError::Denied)?;
        let now = (self.clock)().map_err(|_| PilotAuthorityError::Unavailable)?;
        let ledger = state
            .pilot
            .as_mut()
            .ok_or(PilotAuthorityError::Unavailable)?;
        ledger.check(now, scope, permission)?;
        ledger.consume(request_id)
    }

    pub(crate) fn pilot_turn_guard(
        self: &Arc<Self>,
        owner: ObservationOwner,
        scope: String,
        request_id: Uuid,
    ) -> Arc<dyn IdleTurnAdmission> {
        Arc::new(PilotTurnAdmission {
            slot: Arc::downgrade(self),
            owner,
            scope,
            request_id,
        })
    }
}

struct PilotTurnAdmission {
    slot: Weak<ObservationSlot>,
    owner: ObservationOwner,
    scope: String,
    request_id: Uuid,
}

impl fmt::Debug for PilotTurnAdmission {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PilotTurnAdmission")
            .field("request_id", &self.request_id)
            .finish_non_exhaustive()
    }
}

impl IdleTurnAdmission for PilotTurnAdmission {
    fn reserve_if_allowed(&self, _reserve: &mut dyn FnMut()) -> bool {
        // Pilot authority cannot be consumed without a concrete native turn.
        false
    }

    fn reserve_turn_if_allowed(
        &self,
        thread_id: &ThreadId,
        turn_id: &str,
        reserve: &mut dyn FnMut(),
    ) -> bool {
        let Some(slot) = self.slot.upgrade() else {
            return false;
        };
        let Ok(mut state) = slot.state.lock() else {
            return false;
        };
        if state.authorize(self.owner).is_err() {
            return false;
        }
        let Ok(now) = (slot.clock)() else {
            return false;
        };
        let Some(ledger) = state.pilot.as_mut() else {
            return false;
        };
        if ledger.claims.thread_id != *thread_id
            || turn_id.is_empty()
            || turn_id.len() > 128
            || ledger.admissions.contains_key(turn_id)
            || ledger
                .check(now, &self.scope, PilotPermission::AutomaticTurn)
                .is_err()
            || now.monotonic < ledger.next_turn
            || ledger.admissions.len() >= ledger.claims.max_turns as usize
            || ledger.attempts >= ledger.claims.max_attempts
            || ledger.reserved_tokens >= ledger.claims.max_reserved_tokens.get()
        {
            return false;
        }
        let Some(next_turn) = now.monotonic.checked_add(ledger.claims.cooldown) else {
            return false;
        };
        if ledger.consume(self.request_id).is_err() {
            return false;
        }
        ledger
            .admissions
            .insert(turn_id.to_owned(), self.request_id);
        ledger.next_turn = next_turn;
        reserve();
        true
    }
}
