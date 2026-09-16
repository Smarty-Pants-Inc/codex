//! Native-owner admission at the idle turn's settings-commit boundary.

use crate::ThreadId;
use std::fmt::Debug;

/// Checks an already acquired native owner's authority and commits its finite admission.
///
/// Core supplies the actual thread and newly allocated turn identity after its idle,
/// pending-work and settings checks. Implementations must synchronously validate current
/// authority and atomically reserve their admission budget before returning `true`.
/// Returning `false` must not commit an admission. This is not a grant issuer: source
/// content, model callbacks and a selected observation profile cannot provide authority.
///
/// Implementations must use already acquired state, perform no blocking external IO and
/// never re-enter their `TurnStartGuard`. The guard serializes this call with revocation
/// and prevents a successful owner admission from being committed twice through clones.
/// An accepted turn still requires separate request-budget and retirement accounting.
pub trait TurnStartAdmission: Debug + Send + Sync {
    fn try_commit(&self, thread_id: &ThreadId, turn_id: &str) -> bool;
}

#[cfg(test)]
#[path = "admission_tests.rs"]
mod tests;
