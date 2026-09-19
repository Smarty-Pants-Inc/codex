//! One original-owner automatic admission receipt, not a queue or durable journal.
use super::*;
use codex_protocol::ThreadId;
use codex_protocol::turn_input::IdleTurnAdmission;

/// Installed only by the original trusted native host in thread extension data.
/// No RPC may create or replace this policy. The host supplies qualification and
/// the original synchronized admission guard; this binding invents no defaults.
#[derive(Clone, Debug)]
pub struct ObservationWakeHostPolicy {
    pub thread_id: ThreadId,
    pub owner: ObservationOwner,
    pub admission: Arc<dyn IdleTurnAdmission>,
}

/// Immutable operands from the original owner's ordered observation read.
/// Identity is (owner epoch, sequence); sequences are persisted before dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationWakeIntent {
    pub sequence: u64,
    pub frame_revision: u64,
    pub frame_hash: String,
    pub budget_generation: u64,
    pub expected_commit_order: u64,
}

impl ObservationWakeIntent {
    /// SHA256(domain NUL, epoch UUID bytes, four big-endian u64 operands in
    /// declaration order excluding hash, then the lowercase ASCII frame hash).
    /// This identifies operands, not authority or provider qualification.
    pub fn operand_digest(&self, owner: ObservationOwner) -> String {
        let mut hash = Sha256::new();
        hash.update(b"codex-observation-wake-v1\0");
        hash.update(owner.epoch.as_bytes());
        for value in [
            self.sequence,
            self.frame_revision,
            self.budget_generation,
            self.expected_commit_order,
        ] {
            hash.update(value.to_be_bytes());
        }
        hash.update(self.frame_hash.as_bytes());
        format!("{:x}", hash.finalize())
    }
}

/// Pending/unknown never authorize redispatch or receipt eviction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ObservationWakeOutcome {
    Pending { turn_id: Option<String> },
    Started { turn_id: String },
    Suppressed,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationWakeReceipt {
    pub sequence: u64,
    pub operand_digest: String,
    pub outcome: ObservationWakeOutcome,
}

/// One atomic original-owner read. A missing receipt is unknown history;
/// the floor fences only lower-sequence attempts that have not reserved yet.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationWakeSnapshot {
    pub intent_floor: u64,
    pub receipt: Option<ObservationWakeReceipt>,
}

#[derive(Default)]
pub(super) struct WakeState {
    floor: u64,
    receipt: Option<ObservationWakeReceipt>,
}

pub(crate) struct WakeAdmission {
    slot: Arc<ObservationSlot>,
    owner: ObservationOwner,
    intent: ObservationWakeIntent,
    policy: Arc<dyn IdleTurnAdmission>,
}

impl std::fmt::Debug for WakeAdmission {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WakeAdmission")
            .field("sequence", &self.intent.sequence)
            .finish_non_exhaustive()
    }
}

impl ObservationSlot {
    pub(crate) fn prepare_wake(
        self: &Arc<Self>,
        owner: ObservationOwner,
        intent: ObservationWakeIntent,
        policy: Arc<dyn IdleTurnAdmission>,
    ) -> Result<WakeAdmission, ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.authorize(owner)?;
        if state.pilot.is_some() || state.budget.is_none() {
            return Err(ObservationError::Unavailable);
        }
        if [
            intent.sequence,
            intent.frame_revision,
            intent.budget_generation,
            intent.expected_commit_order,
        ]
        .into_iter()
        .any(|value| value == 0 || value > MAX_SEQUENCE)
            || intent.frame_hash.len() != 64
            || !intent
                .frame_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ObservationError::InvalidFrame);
        }
        // Even an exact duplicate is readback-only, never another start operation.
        if state.wake.receipt.is_some() || intent.sequence <= state.wake.floor {
            return Err(ObservationError::RevisionMismatch);
        }
        state.wake.floor = intent.sequence;
        state.wake.receipt = Some(ObservationWakeReceipt {
            sequence: intent.sequence,
            operand_digest: intent.operand_digest(owner),
            outcome: ObservationWakeOutcome::Pending { turn_id: None },
        });
        Ok(WakeAdmission {
            slot: Arc::clone(self),
            owner,
            intent,
            policy,
        })
    }

    /// Native-accepted invalidation. Local invocation alone is not this boundary.
    /// An already reserved attempt remains truthful and must be joined separately.
    pub fn invalidate_observation_wake(
        &self,
        owner: ObservationOwner,
        sequence: u64,
    ) -> Result<u64, ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.authorize(owner)?;
        if sequence <= state.wake.floor || sequence > MAX_SEQUENCE {
            return Err(ObservationError::RevisionMismatch);
        }
        state.wake.floor = sequence;
        Ok(state.wake.floor)
    }

    /// Exact historical result only; never revalidates eligibility or restarts work.
    pub fn read_observation_wake(
        &self,
        owner: ObservationOwner,
        sequence: u64,
        operand_digest: &str,
    ) -> Result<ObservationWakeReceipt, ObservationError> {
        self.observation_wake_snapshot(owner)?
            .receipt
            .filter(|receipt| {
                receipt.sequence == sequence && receipt.operand_digest == operand_digest
            })
            .ok_or(ObservationError::RevisionMismatch)
    }

    /// Read the existing floor and optional receipt at one native lock boundary.
    /// This neither revalidates the observation budget nor dispatches a turn.
    pub fn observation_wake_snapshot(
        &self,
        owner: ObservationOwner,
    ) -> Result<ObservationWakeSnapshot, ObservationError> {
        let state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.authorize(owner)?;
        Ok(ObservationWakeSnapshot {
            intent_floor: state.wake.floor,
            receipt: state.wake.receipt.clone(),
        })
    }

    /// Original owner calls this only after durably recording the terminal result.
    /// The monotonic floor survives retirement, so old identities cannot restart.
    pub fn retire_observation_wake(
        &self,
        owner: ObservationOwner,
        sequence: u64,
        operand_digest: &str,
    ) -> Result<u64, ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.authorize(owner)?;
        let receipt = state
            .wake
            .receipt
            .as_ref()
            .ok_or(ObservationError::RevisionMismatch)?;
        if receipt.sequence != sequence || receipt.operand_digest != operand_digest {
            return Err(ObservationError::RevisionMismatch);
        }
        match receipt.outcome {
            ObservationWakeOutcome::Started { .. } | ObservationWakeOutcome::Suppressed => {}
            ObservationWakeOutcome::Pending { .. } | ObservationWakeOutcome::Unknown => {
                return Err(ObservationError::Unavailable);
            }
        }
        state.wake.receipt = None;
        Ok(state.wake.floor)
    }
}

impl WakeAdmission {
    pub(crate) fn finish(
        &self,
        outcome: ObservationWakeOutcome,
    ) -> Result<ObservationWakeReceipt, ObservationError> {
        let mut state = self
            .slot
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.authorize(self.owner)?;
        let receipt = state
            .wake
            .receipt
            .as_mut()
            .ok_or(ObservationError::Unavailable)?;
        if receipt.sequence != self.intent.sequence
            || receipt.operand_digest != self.intent.operand_digest(self.owner)
        {
            return Err(ObservationError::RevisionMismatch);
        }
        match (&receipt.outcome, &outcome) {
            (
                ObservationWakeOutcome::Pending {
                    turn_id: Some(reserved),
                },
                ObservationWakeOutcome::Started { turn_id },
            ) if reserved == turn_id => {}
            (
                ObservationWakeOutcome::Pending { .. },
                ObservationWakeOutcome::Suppressed | ObservationWakeOutcome::Unknown,
            ) => {}
            _ => return Err(ObservationError::Unavailable),
        }
        receipt.outcome = outcome;
        Ok(receipt.clone())
    }
}

impl IdleTurnAdmission for WakeAdmission {
    fn reserve_if_allowed(&self, _reserve: &mut dyn FnMut()) -> bool {
        false // No admission without the actual native thread/turn identity.
    }

    fn reserve_turn_if_allowed(
        &self,
        thread: &ThreadId,
        turn: &str,
        reserve: &mut dyn FnMut(),
    ) -> bool {
        let mut admitted = false;
        // Original policy owns its synchronous registration guard. Lock order is
        // native active turn -> original policy -> slot; never call policy under slot.
        let policy_allowed = self.policy.reserve_turn_if_allowed(thread, turn, &mut || {
            let Ok(mut state) = self.slot.state.lock() else {
                return;
            };
            let Ok(now) = (self.slot.clock)() else {
                return;
            };
            state.expire(now);
            if admitted
                || state.authorize(self.owner).is_err()
                || state.wake.floor != self.intent.sequence
                || state.active_capture.is_some()
                || state.pilot.is_some()
                || state.commit_order != self.intent.expected_commit_order
                || state.revision != self.intent.frame_revision
                || state.metadata().status != ObservationStatus::Current
                || state.frame.as_ref().map(|frame| frame.hash.as_str())
                    != Some(self.intent.frame_hash.as_str())
                || state.frame_budget_generation != Some(self.intent.budget_generation)
                || state
                    .budget
                    .as_ref()
                    .map(|budget| budget.snapshot.generation)
                    != Some(self.intent.budget_generation)
                || turn.is_empty()
                || turn.len() > 128
            {
                return;
            }
            let Some(receipt) = state.wake.receipt.as_mut() else {
                return;
            };
            if receipt.sequence != self.intent.sequence
                || receipt.outcome != (ObservationWakeOutcome::Pending { turn_id: None })
            {
                return;
            }
            receipt.outcome = ObservationWakeOutcome::Pending {
                turn_id: Some(turn.to_owned()),
            };
            reserve();
            admitted = true;
        });
        policy_allowed && admitted
    }
}
