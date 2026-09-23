//! Lifetime custody for native request telemetry and decoder callbacks.
use super::ObservationSlot;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;
use tokio::sync::Notify;

#[derive(Default)]
pub(super) struct ObservationTransportTracker {
    active: AtomicUsize,
    idle: Notify,
}

pub(crate) struct ObservationTransportLease {
    slot: Arc<ObservationSlot>,
}

impl ObservationTransportLease {
    pub(crate) fn slot(&self) -> &Arc<ObservationSlot> {
        &self.slot
    }
}

impl Drop for ObservationTransportLease {
    fn drop(&mut self) {
        if self
            .slot
            .transports
            .active
            .fetch_sub(/*val*/ 1, Ordering::AcqRel)
            == 1
        {
            self.slot.transports.idle.notify_waiters();
        }
    }
}

impl ObservationSlot {
    pub(crate) fn transport_lease(self: Arc<Self>) -> Arc<ObservationTransportLease> {
        self.transports
            .active
            .fetch_add(/*val*/ 1, Ordering::AcqRel);
        Arc::new(ObservationTransportLease { slot: self })
    }

    /// Call only after revocation and the original session-loop join exclude new
    /// send producers. Every captured callback retains the same lease until drop.
    /// Cancellation of this wait does not reset custody or report a successful drain.
    pub(crate) async fn wait_for_transport_drain(&self) {
        loop {
            let idle = self.transports.idle.notified();
            if self.transports.active.load(Ordering::Acquire) == 0 {
                return;
            }
            idle.await;
        }
    }
}

#[cfg(test)]
#[path = "observation_transport_lease_tests.rs"]
mod tests;
