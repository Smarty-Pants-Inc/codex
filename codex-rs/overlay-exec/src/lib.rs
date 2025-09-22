//! Exec overlay scaffolding for the Codex upstream fork.

use std::sync::{Mutex, MutexGuard, OnceLock};

use codex_core::protocol::{Event, EventMsg};

/// Overlay-specific state that augments the upstream executor without
/// modifying its source.
#[derive(Debug, Default, Clone)]
pub struct ExecOverlayState {
    pub last_agent_message: Option<String>,
}

impl ExecOverlayState {
    /// Update the overlay state in response to a Codex event.
    pub fn observe_event(&mut self, event: &Event) {
        match &event.msg {
            EventMsg::TaskComplete(task_complete) => {
                self.last_agent_message = task_complete.last_agent_message.clone();
            }
            EventMsg::SessionConfigured(_)
            | EventMsg::TaskStarted(_)
            | EventMsg::TurnAborted(_)
            | EventMsg::ShutdownComplete => self.clear(),
            _ => {}
        }
    }

    /// Reset the overlay state back to its defaults.
    pub fn clear(&mut self) {
        self.last_agent_message = None;
    }
}

fn state_cell() -> &'static Mutex<ExecOverlayState> {
    static STATE: OnceLock<Mutex<ExecOverlayState>> = OnceLock::new();
    STATE.get_or_init(|| Mutex::new(ExecOverlayState::default()))
}

fn lock_state() -> MutexGuard<'static, ExecOverlayState> {
    match state_cell().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

pub mod observer {
    use super::*;

    /// Feed an event into the global overlay observer.
    pub fn observe(event: &Event) {
        let mut state = lock_state();
        state.observe_event(event);
    }

    /// Clear all state maintained by the overlay observer.
    pub fn clear() {
        lock_state().clear();
    }

    /// Snapshot the current overlay state.
    pub fn snapshot() -> ExecOverlayState {
        lock_state().clone()
    }

    /// Execute a closure with an immutable view of the current overlay state.
    pub fn with_state<R>(f: impl FnOnce(&ExecOverlayState) -> R) -> R {
        let state = lock_state();
        f(&state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_core::protocol::{
        Event, EventMsg, TaskCompleteEvent, TaskStartedEvent, TurnAbortReason, TurnAbortedEvent,
    };

    fn event(id: &str, msg: EventMsg) -> Event {
        Event {
            id: id.to_string(),
            msg,
        }
    }

    #[test]
    fn task_complete_populates_last_message() {
        observer::clear();

        let complete = EventMsg::TaskComplete(TaskCompleteEvent {
            last_agent_message: Some("done".to_string()),
        });
        observer::observe(&event("1", complete));

        let snapshot = observer::snapshot();
        assert_eq!(snapshot.last_agent_message, Some("done".to_string()));
    }

    #[test]
    fn task_started_clears_last_message() {
        observer::clear();

        observer::observe(&event(
            "1",
            EventMsg::TaskComplete(TaskCompleteEvent {
                last_agent_message: Some("message".to_string()),
            }),
        ));
        observer::observe(&event(
            "2",
            EventMsg::TaskStarted(TaskStartedEvent {
                model_context_window: None,
            }),
        ));

        let snapshot = observer::snapshot();
        assert!(snapshot.last_agent_message.is_none());
    }

    #[test]
    fn aborted_turn_clears_last_message() {
        observer::clear();

        observer::observe(&event(
            "1",
            EventMsg::TaskComplete(TaskCompleteEvent {
                last_agent_message: Some("message".into()),
            }),
        ));
        observer::observe(&event(
            "2",
            EventMsg::TurnAborted(TurnAbortedEvent {
                reason: TurnAbortReason::Interrupted,
            }),
        ));

        let snapshot = observer::snapshot();
        assert!(snapshot.last_agent_message.is_none());
    }
}
