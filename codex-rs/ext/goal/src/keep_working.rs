use super::settlement::Settlement;
use codex_protocol::ThreadId;
use codex_protocol::turn_input::TurnStartGuard;
use codex_state::GoalStore;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use tokio::sync::Semaphore;

pub(crate) enum ContinuityBoundary<'a> {
    HumanInput(Option<&'a str>),
    Resume,
    Retire,
}

pub(crate) enum StopScope<'a> {
    Turn(&'a str),
    Idle(Option<TurnStartGuard>),
}

pub(crate) struct KeepWorking {
    pub(crate) settlement: Mutex<Settlement>,
    pub(crate) halted: AtomicBool,
    pub(crate) eligible: AtomicBool,
    pub(crate) retired: AtomicBool,
    writer: Semaphore,
}

impl Default for KeepWorking {
    fn default() -> Self {
        Self {
            settlement: Mutex::default(),
            halted: AtomicBool::default(),
            eligible: AtomicBool::default(),
            retired: AtomicBool::default(),
            writer: Semaphore::new(/*permits*/ 1),
        }
    }
}

impl KeepWorking {
    pub(crate) fn idle_stop_scope(&self) -> StopScope<'static> {
        let settlement = self
            .settlement
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        StopScope::Idle(match &*settlement {
            Settlement::None => None,
            Settlement::Running { guard, .. }
            | Settlement::Completed(guard)
            | Settlement::Claimed(guard)
            | Settlement::Stopped(guard) => Some(guard.clone()),
        })
    }

    pub(crate) fn invalidate(&self, boundary: ContinuityBoundary<'_>) {
        let mut settlement = self
            .settlement
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.eligible.store(/*val*/ false, Ordering::SeqCst);
        match boundary {
            ContinuityBoundary::HumanInput(Some(turn_id))
                if !self.retired.load(Ordering::SeqCst) =>
            {
                settlement.start(turn_id);
            }
            ContinuityBoundary::HumanInput(_) | ContinuityBoundary::Resume => settlement.stop(),
            ContinuityBoundary::Retire => {
                self.retired.store(/*val*/ true, Ordering::SeqCst);
                settlement.stop();
            }
        }
    }

    pub(crate) fn intent_for_turn(&self, turn_id: &str) -> Result<TurnStartGuard, String> {
        self.settlement
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .guard_for_turn(turn_id)
            .ok_or_else(|| "keep_working requires the current running turn".to_string())
    }

    pub(crate) async fn set(
        &self,
        store: &GoalStore,
        thread_id: ThreadId,
        intent: TurnStartGuard,
        enabled: bool,
    ) -> Result<(), String> {
        let _writer = self.writer.acquire().await.map_err(|err| err.to_string())?;
        if intent.is_revoked() {
            return Err("keep_working intent was stopped or superseded".to_string());
        }
        store
            .set_keep_working_if_current(thread_id, enabled, &intent)
            .await
            .map_err(|err| err.to_string())?;
        let _settlement = self
            .settlement
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if intent.is_revoked() {
            return Err("keep_working intent was stopped or superseded".to_string());
        }
        self.halted.store(!enabled, Ordering::SeqCst);
        self.eligible.store(enabled, Ordering::SeqCst);
        Ok(())
    }

    pub(crate) async fn stop(
        &self,
        store: &GoalStore,
        thread_id: ThreadId,
        scope: StopScope<'_>,
    ) -> Result<(), String> {
        let intent = {
            let mut settlement = self
                .settlement
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if self.retired.load(Ordering::SeqCst)
                || match scope {
                    StopScope::Turn(turn_id) => settlement.guard_for_turn(turn_id).is_none(),
                    StopScope::Idle(Some(guard)) => guard.is_revoked(),
                    StopScope::Idle(None) => !matches!(*settlement, Settlement::None),
                }
            {
                return Ok(());
            }
            let intent = settlement.stop_intent();
            self.halted.store(/*val*/ true, Ordering::SeqCst);
            self.eligible.store(/*val*/ false, Ordering::SeqCst);
            intent
        };
        // ponytail: this permit spans only transactional flag writes, never native
        // admission. A stop callback must not wait for the goal-state semaphore.
        let _writer = self.writer.acquire().await.map_err(|err| err.to_string())?;
        store
            .set_keep_working_if_current(thread_id, /*enabled*/ false, &intent)
            .await
            .map_err(|err| err.to_string())
    }
}

#[cfg(test)]
#[path = "keep_working_tests.rs"]
mod tests;
