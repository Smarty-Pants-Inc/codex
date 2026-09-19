//! Owner-fenced, transient observation storage. This does not enable observation
//! requests: the app-server must first admit an exclusive owner and Core must
//! separately qualify the rendered token budget and provider transport.

use sha2::Digest;
use sha2::Sha256;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use tokio::sync::mpsc;
use uuid::Uuid;

// Sense's full reference allocation: 8 views, 2048 UTF-8 body bytes each,
// six-byte JSON escaping, 2048 metadata/error bytes per view, 1024 frame bytes.
// Native code treats this envelope as opaque; it does not parse watches.
pub(crate) const MAX_FRAME_BYTES: usize = 1024 + 8 * (6 * 2048 + 2048);
pub(crate) const OBSERVATION_HEADER_BYTES: usize = 128;
pub(crate) const OBSERVATION_PART_BYTES: usize = 7168;
pub(crate) const OBSERVATION_MARKER_BYTES: usize = 96;
pub(crate) const OBSERVATION_FRAMING_TOKENS: usize = 8;
// A UTF-8 split can leave at most three bytes unused in a nonfinal part.
pub(crate) const MAX_OBSERVATION_ITEMS: usize =
    (MAX_FRAME_BYTES + OBSERVATION_HEADER_BYTES).div_ceil(OBSERVATION_PART_BYTES - 3);
// Ordinary byte-level BPE uses at most one token per UTF-8 byte. Include every
// marker and conservatively charge the assistant prefill for every message.
// This allocation is not qualification of an external provider's rendering.
pub(crate) const RESERVED_TOKENS: i64 = (MAX_FRAME_BYTES
    + OBSERVATION_HEADER_BYTES
    + MAX_OBSERVATION_ITEMS * (OBSERVATION_MARKER_BYTES + OBSERVATION_FRAMING_TOKENS))
    as i64;
const MAX_LEASE_SECONDS: i64 = 60;
const MAX_SEQUENCE: u64 = (1_u64 << 53) - 1;
// Sense receives Unix seconds as safe JSON integers within the JavaScript Date range.
const MAX_TIMESTAMP: i64 = 8_640_000_000_000;
const EVENT_CAPACITY: usize = 32;

#[path = "observation_wake.rs"]
mod wake;
pub use wake::ObservationWakeHostPolicy;
pub use wake::ObservationWakeIntent;
pub use wake::ObservationWakeOutcome;
pub use wake::ObservationWakeReceipt;
pub use wake::ObservationWakeSnapshot;

#[path = "observation_budget.rs"]
mod budget;
pub use budget::ObservationReservation;
pub use budget::ObservationReservationState;

#[path = "observation_capture.rs"]
mod capture;
pub use capture::ObservationCapture;
#[path = "observation_audit.rs"]
mod audit;
use audit::DecisionAudit;
pub(crate) use audit::ObservationAttempt;
pub use audit::ObservationOutcome;
pub use audit::ObservationSubmitted;
#[path = "observation_transport_lease.rs"]
mod transport;
pub(crate) use transport::ObservationTransportLease;
#[path = "observation_pilot.rs"]
mod pilot;
pub use pilot::NativePilotIssuer;
pub use pilot::PilotAttemptRecord;
pub use pilot::PilotAuthorityError;
pub use pilot::PilotCountJournal;
pub use pilot::PilotCountScope;
pub use pilot::PilotGrantClaims;
pub use pilot::PilotLedgerIdentity;
pub use pilot::PilotPermission;
pub use pilot::PilotReport;
pub use pilot::PilotRequestReservation;

/// One atomic thread attachment shared by Core sampling and the owner relay.
/// Constructing this value does not admit a host, connection or provider profile.
/// The app-server installs it only after its trusted lifecycle admission.
pub struct ObservationBinding {
    pub slot: Arc<ObservationSlot>,
    pub profile: crate::ObservationProfile,
}

/// Connection identity is supplied by native transport, never by tool input.
/// The epoch fences this admission; it cannot authorize another connection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservationOwner {
    pub connection_id: u64,
    pub epoch: Uuid,
}

/// Sampled by the slot's supplier inside the publication/capture critical section.
#[derive(Clone, Copy)]
struct ObservationClock {
    wall_seconds: i64,
    monotonic: Instant,
}

type ClockSource = Box<dyn Fn() -> Result<ObservationClock, ObservationError> + Send + Sync>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationFrame {
    pub text: Arc<str>,
    pub hash: String,
    pub expires_at: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationStatus {
    Current,
    Cleared,
    Expired,
    Unavailable,
}

/// Body-free publication/readback metadata. Expiry retains the publication hash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationMetadata {
    pub owner: ObservationOwner,
    pub revision: u64,
    pub commit_order: u64,
    pub hash: Option<String>,
    pub expires_at: Option<i64>,
    pub status: ObservationStatus,
    pub native_reservation: Option<ObservationReservation>,
    pub frame_budget_generation: Option<u64>,
}

/// The app-server must forward this FIFO before sending corresponding set/read
/// ACKs. Sending ACKs directly from the method return would break capture ordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservationEvent {
    Budget {
        owner: ObservationOwner,
        commit_order: u64,
        reservation: ObservationReservation,
    },
    Published(ObservationMetadata),
    Read(ObservationMetadata),
    Captured(ObservationCapture),
    Submitted(ObservationSubmitted),
    /// Native RPC correlation token, never a client-supplied ownership claim.
    Control {
        request_id: Uuid,
        metadata: ObservationMetadata,
    },
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum ObservationError {
    #[error("stale observation owner")]
    StaleOwner,
    #[error("invalid observation frame or lease")]
    InvalidFrame,
    #[error("observation revision mismatch")]
    RevisionMismatch,
    #[error("observation resource limit")]
    ResourceLimit,
    #[error("observation state unavailable")]
    Unavailable,
    #[error("observation budget invalid")]
    BudgetInvalid,
    #[error("observation budget generation mismatch")]
    BudgetGenerationMismatch,
}

struct SlotState {
    owner: ObservationOwner,
    revision: u64,
    commit_order: u64,
    frame: Option<ObservationFrame>,
    deadline: Option<Instant>,
    expired: bool,
    revoked: bool,
    active_capture: Option<DecisionAudit>,
    pilot: Option<pilot::PilotLedger>,
    budget: Option<budget::BudgetState>,
    frame_budget_generation: Option<u64>,
    wake: wake::WakeState,
}

impl SlotState {
    fn expire(&mut self, now: ObservationClock) {
        self.expired |= self
            .deadline
            .is_some_and(|deadline| now.monotonic >= deadline)
            || self
                .frame
                .as_ref()
                .is_some_and(|frame| now.wall_seconds >= frame.expires_at);
    }

    fn authorize(&self, owner: ObservationOwner) -> Result<(), ObservationError> {
        if self.revoked || self.owner != owner {
            return Err(ObservationError::StaleOwner);
        }
        Ok(())
    }

    fn metadata(&self) -> ObservationMetadata {
        ObservationMetadata {
            owner: self.owner,
            revision: self.revision,
            commit_order: self.commit_order,
            hash: self.frame.as_ref().map(|frame| frame.hash.clone()),
            expires_at: self.frame.as_ref().map(|frame| frame.expires_at),
            native_reservation: self.budget.as_ref().map(|budget| budget.snapshot.clone()),
            frame_budget_generation: self.frame_budget_generation,
            status: if self.revoked {
                ObservationStatus::Unavailable
            } else if self.frame.is_none() {
                ObservationStatus::Cleared
            } else if self.expired {
                ObservationStatus::Expired
            } else if !self.frame_budget_valid() {
                ObservationStatus::Unavailable
            } else {
                ObservationStatus::Current
            },
        }
    }
}

/// One slot per admitted thread. No persistence, timer, owner takeover, observer,
/// model request, or token-capacity claim is implemented by this store.
pub struct ObservationSlot {
    state: Mutex<SlotState>,
    events: mpsc::Sender<ObservationEvent>,
    clock: ClockSource,
    transports: transport::ObservationTransportTracker,
}

impl ObservationSlot {
    pub fn new(connection_id: u64) -> (Self, mpsc::Receiver<ObservationEvent>, ObservationOwner) {
        Self::with_clock(
            connection_id,
            Box::new(|| {
                let wall_seconds = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
                    .ok_or(ObservationError::Unavailable)?;
                Ok(ObservationClock {
                    wall_seconds,
                    monotonic: Instant::now(),
                })
            }),
        )
    }

    fn with_clock(
        connection_id: u64,
        clock: ClockSource,
    ) -> (Self, mpsc::Receiver<ObservationEvent>, ObservationOwner) {
        let owner = ObservationOwner {
            connection_id,
            epoch: Uuid::new_v4(),
        };
        let (events, receiver) = mpsc::channel(EVENT_CAPACITY);
        (
            Self {
                state: Mutex::new(SlotState {
                    owner,
                    revision: 0,
                    commit_order: 0,
                    frame: None,
                    deadline: None,
                    expired: false,
                    revoked: false,
                    active_capture: None,
                    pilot: None,
                    budget: None,
                    frame_budget_generation: None,
                    wake: wake::WakeState::default(),
                }),
                events,
                clock: Box::new(move || {
                    let now = clock()?;
                    if !(0..=MAX_TIMESTAMP).contains(&now.wall_seconds) {
                        return Err(ObservationError::Unavailable);
                    }
                    Ok(now)
                }),
                transports: transport::ObservationTransportTracker::default(),
            },
            receiver,
            owner,
        )
    }

    /// Equal revisions only extend an active, byte-identical frame's lease.
    /// Publication and its FIFO record commit under the slot lock.
    pub fn set(
        &self,
        owner: ObservationOwner,
        revision: u64,
        frame: Option<ObservationFrame>,
    ) -> Result<ObservationMetadata, ObservationError> {
        self.set_inner(
            owner, revision, frame, /*request_id*/ None, /*budget_generation*/ None,
        )
    }

    /// The same publication boundary, with one correlated FIFO ACK instead of
    /// an uncorrelated Published event. No additional queue or postcommit step.
    pub fn set_for_request(
        &self,
        owner: ObservationOwner,
        revision: u64,
        frame: Option<ObservationFrame>,
        request_id: Uuid,
    ) -> Result<(), ObservationError> {
        self.set_inner(
            owner,
            revision,
            frame,
            Some(request_id),
            /*budget_generation*/ None,
        )
        .map(|_| ())
    }

    pub fn set_for_request_at_budget(
        &self,
        owner: ObservationOwner,
        revision: u64,
        frame: Option<ObservationFrame>,
        request_id: Uuid,
        budget_generation: u64,
    ) -> Result<(), ObservationError> {
        self.set_inner(
            owner,
            revision,
            frame,
            Some(request_id),
            Some(budget_generation),
        )
        .map(|_| ())
    }

    fn set_inner(
        &self,
        owner: ObservationOwner,
        revision: u64,
        frame: Option<ObservationFrame>,
        request_id: Option<Uuid>,
        budget_generation: Option<u64>,
    ) -> Result<ObservationMetadata, ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.authorize(owner)?;
        if budget_generation.is_some() && state.budget.is_none() {
            return Err(ObservationError::BudgetInvalid);
        }
        if state.budget.is_some() {
            let generation = budget_generation.ok_or(ObservationError::BudgetGenerationMismatch)?;
            state.require_budget(generation, frame.is_some())?;
            if revision == state.revision && state.frame_budget_generation != Some(generation) {
                return Err(ObservationError::BudgetGenerationMismatch);
            }
        }
        let now = (self.clock)()?;
        state.expire(now);
        if revision > MAX_SEQUENCE
            || state.commit_order + u64::from(state.active_capture.is_some()) >= MAX_SEQUENCE
        {
            return Err(ObservationError::ResourceLimit);
        }
        if revision < state.revision {
            return Err(ObservationError::RevisionMismatch);
        }
        let deadline = if let Some(frame) = &frame {
            if state.expired {
                return Err(ObservationError::StaleOwner);
            }
            let seconds = frame
                .expires_at
                .checked_sub(now.wall_seconds)
                .filter(|seconds| (1..=MAX_LEASE_SECONDS).contains(seconds))
                .ok_or(ObservationError::InvalidFrame)?;
            if !(0..=MAX_TIMESTAMP).contains(&frame.expires_at)
                || frame.text.len() > MAX_FRAME_BYTES
                || frame
                    .text
                    .chars()
                    .any(|c| c.is_control() && c != '\n' && c != '\t')
                || frame.hash != format!("{:x}", Sha256::digest(frame.text.as_bytes()))
            {
                return Err(ObservationError::InvalidFrame);
            }
            if revision == state.revision
                && !state.frame.as_ref().is_some_and(|current| {
                    current.text == frame.text
                        && current.hash == frame.hash
                        && frame.expires_at > current.expires_at
                })
            {
                return Err(ObservationError::RevisionMismatch);
            }
            Some(
                now.monotonic
                    .checked_add(Duration::from_secs(seconds as u64))
                    .ok_or(ObservationError::InvalidFrame)?,
            )
        } else {
            if revision == state.revision {
                return Err(ObservationError::RevisionMismatch);
            }
            None
        };
        let permit = self
            .events
            .try_reserve()
            .map_err(|_| ObservationError::ResourceLimit)?;
        state.revision = revision;
        state.commit_order += 1;
        state.frame_budget_generation = frame.as_ref().and(budget_generation);
        state.frame = frame;
        state.deadline = deadline;
        let metadata = state.metadata();
        permit.send(match request_id {
            Some(request_id) => ObservationEvent::Control {
                request_id,
                metadata: metadata.clone(),
            },
            None => ObservationEvent::Published(metadata.clone()),
        });
        Ok(metadata)
    }

    pub fn read(&self, owner: ObservationOwner) -> Result<ObservationMetadata, ObservationError> {
        self.read_inner(owner, /*request_id*/ None)
    }

    pub fn read_for_request(
        &self,
        owner: ObservationOwner,
        request_id: Uuid,
    ) -> Result<(), ObservationError> {
        self.read_inner(owner, Some(request_id)).map(|_| ())
    }

    fn read_inner(
        &self,
        owner: ObservationOwner,
        request_id: Option<Uuid>,
    ) -> Result<ObservationMetadata, ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.authorize(owner)?;
        state.expire((self.clock)()?);
        let permit = self
            .events
            .try_reserve()
            .map_err(|_| ObservationError::ResourceLimit)?;
        let metadata = state.metadata();
        permit.send(match request_id {
            Some(request_id) => ObservationEvent::Control {
                request_id,
                metadata: metadata.clone(),
            },
            None => ObservationEvent::Read(metadata.clone()),
        });
        Ok(metadata)
    }

    /// Disconnect is terminal for this owner, independent of frame expiry.
    pub fn revoke(&self) -> Result<(), ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.revoked = true;
        if let Some(pilot) = state.pilot.as_mut() {
            pilot.revoke();
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "observation_control_event_tests.rs"]
mod control_event_tests;

#[cfg(test)]
#[path = "observation_tests.rs"]
mod tests;
