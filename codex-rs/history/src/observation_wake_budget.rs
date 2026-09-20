//! Model-invisible accounting DATA, never a recovered admission permit.
//!
//! The original exclusive writer must validate the complete canonical stream.
//! This reducer cannot certify missing, skipped, truncated or unsynced records.
use std::collections::BTreeMap;
use std::num::NonZeroU64;

use codex_protocol::ThreadId;
use schemars::JsonSchema;
use serde::Deserialize;
use serde::Serialize;

/// Exact trusted selection to compare with persisted DATA, not a policy default.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationWakeBudgetSelection {
    pub thread_id: ThreadId,
    /// Original canonical rollout identity; a replacement/fork is not this writer.
    pub rollout_id: ThreadId,
    pub selection_digest: [u8; 32],
    pub reservation_limit: NonZeroU64,
}

/// One original operation consumes one unit regardless of its eventual outcome.
/// Digests and epoch are fixed bytes; no frame, prompt, arbitrary input or permit
/// is persisted. Host preparation is separate from the original operand digest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ObservationWakeDebit {
    pub ordinal: NonZeroU64,
    pub owner_epoch: [u8; 16],
    pub sequence: NonZeroU64,
    pub operand_digest: [u8; 32],
    pub cooldown_revision: NonZeroU64,
    pub wake_not_before_bits: u64,
}

/// Unknown versions or fields must fail strict recovery, not disappear from it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(
    tag = "type",
    content = "payload",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum ObservationWakeBudgetRecord {
    InitializedV1(ObservationWakeBudgetSelection),
    DebitedV1(ObservationWakeDebit),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ObservationWakeBudgetReplayError {
    InvalidLineage,
    InvalidDebit,
    ConflictingOperation,
    LimitExceeded,
    PreviouslyInvalid,
}

/// Tracks debt only. Successful observation is neither synced ACK nor permission
/// to execute. The caller supplies the trusted original selection; reading a
/// selection from the same file and trusting it does not establish authority.
pub struct ObservationWakeBudgetReplay {
    expected: ObservationWakeBudgetSelection,
    initialized: bool,
    invalid: bool,
    charged: u64,
    debits: BTreeMap<([u8; 16], NonZeroU64), ObservationWakeDebit>,
}

impl ObservationWakeBudgetReplay {
    pub fn new(expected: ObservationWakeBudgetSelection) -> Self {
        Self {
            expected,
            initialized: false,
            invalid: false,
            charged: 0,
            debits: BTreeMap::new(),
        }
    }

    /// Retains debt on any error. Once invalid, this instance cannot resume by
    /// ignoring a bad record and feeding it a later well-formed record.
    pub fn observe(
        &mut self,
        record: &ObservationWakeBudgetRecord,
    ) -> Result<(), ObservationWakeBudgetReplayError> {
        use ObservationWakeBudgetReplayError as Error;
        if self.invalid {
            return Err(Error::PreviouslyInvalid);
        }
        self.invalid = true;
        match record {
            ObservationWakeBudgetRecord::InitializedV1(selection) => {
                if self.initialized || selection != &self.expected {
                    return Err(Error::InvalidLineage);
                }
                self.initialized = true;
            }
            ObservationWakeBudgetRecord::DebitedV1(debit) => {
                if !self.initialized {
                    return Err(Error::InvalidLineage);
                }
                let not_before = f64::from_bits(debit.wake_not_before_bits);
                if debit.cooldown_revision.get() > 9_007_199_254_740_991
                    || !not_before.is_finite()
                    || not_before <= 0.0
                {
                    return Err(Error::InvalidDebit);
                }
                let identity = (debit.owner_epoch, debit.sequence);
                if let Some(original) = self.debits.get(&identity) {
                    if original != debit {
                        return Err(Error::ConflictingOperation);
                    }
                    // Repeated DATA only reconciles the same original charge.
                    // There is intentionally no reusable execution result here.
                } else {
                    let next = self.charged.checked_add(1).ok_or(Error::LimitExceeded)?;
                    if next > self.expected.reservation_limit.get() {
                        return Err(Error::LimitExceeded);
                    }
                    if debit.ordinal.get() != next {
                        return Err(Error::InvalidDebit);
                    }
                    self.debits.insert(identity, debit.clone());
                    self.charged = next;
                }
            }
        }
        self.invalid = false;
        Ok(())
    }

    /// Historical charged units, including Pending, Unknown and suppressed work.
    /// This count does not imply that any remaining units are available to spend.
    pub fn charged(&self) -> u64 {
        self.charged
    }
}

#[cfg(test)]
#[path = "observation_wake_budget_tests.rs"]
mod tests;
