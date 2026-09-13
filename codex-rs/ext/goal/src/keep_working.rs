use super::settlement::Settlement;
use codex_protocol::ThreadId;
use codex_protocol::turn_input::TurnStartGuard;
use codex_state::GoalStore;
use std::sync::Mutex;
use std::sync::PoisonError;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

#[derive(Default)]
pub(crate) struct KeepWorking {
    pub(crate) settlement: Mutex<Settlement>,
    pub(crate) halted: AtomicBool,
    writer: tokio::sync::Mutex<()>,
}

impl KeepWorking {
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
        let _writer = self.writer.lock().await;
        if intent.is_revoked() {
            return Err("keep_working intent was stopped or superseded".to_string());
        }
        store
            .set_keep_working(thread_id, enabled)
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
        Ok(())
    }

    pub(crate) async fn stop(&self, store: &GoalStore, thread_id: ThreadId) -> Result<(), String> {
        {
            let mut settlement = self
                .settlement
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            settlement.stop();
            self.halted.store(/*val*/ true, Ordering::SeqCst);
        }
        // ponytail: this lock spans only transactional flag writes, never native
        // admission. A stop callback must not wait for the goal-state semaphore.
        let _writer = self.writer.lock().await;
        store
            .set_keep_working(thread_id, /*enabled*/ false)
            .await
            .map_err(|err| err.to_string())
    }
}

#[cfg(test)]
#[path = "keep_working_tests.rs"]
mod tests;
