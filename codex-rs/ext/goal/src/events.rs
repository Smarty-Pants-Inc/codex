use std::sync::Arc;

use codex_extension_api::ExtensionEventSink;
use codex_extension_api::ExtensionWarning;

use codex_protocol::protocol::Event;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadGoal;
use codex_protocol::protocol::ThreadGoalUpdatedEvent;

#[derive(Clone)]
pub(crate) struct GoalEventEmitter {
    sink: Arc<dyn ExtensionEventSink>,
}

impl GoalEventEmitter {
    pub(crate) fn new(sink: Arc<dyn ExtensionEventSink>) -> Self {
        Self { sink }
    }

    pub(crate) fn thread_goal_updated(
        &self,
        event_id: impl Into<String>,
        turn_id: Option<String>,
        goal: ThreadGoal,
    ) {
        self.sink.emit(Event {
            id: event_id.into(),
            msg: EventMsg::ThreadGoalUpdated(ThreadGoalUpdatedEvent {
                thread_id: goal.thread_id,
                turn_id,
                goal,
            }),
        });
    }
    pub(crate) fn warning(&self, thread_id: impl Into<String>, message: impl Into<String>) {
        self.sink.emit_warning(ExtensionWarning {
            thread_id: thread_id.into(),
            turn_id: None,
            message: message.into(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use std::sync::Mutex;

    #[derive(Default)]
    struct RecordingSink {
        warnings: Mutex<Vec<ExtensionWarning>>,
    }

    impl ExtensionEventSink for RecordingSink {
        fn emit(&self, _event: Event) {}

        fn emit_warning(&self, warning: ExtensionWarning) {
            self.warnings.lock().expect("warning lock").push(warning);
        }
    }

    #[test]
    fn warning_targets_the_goal_thread() {
        let sink = Arc::new(RecordingSink::default());
        let emitter = GoalEventEmitter::new(sink.clone());
        emitter.warning(
            "thread-1",
            "Could not continue the active goal: storage failed",
        );

        assert_eq!(
            *sink.warnings.lock().expect("warning lock"),
            vec![ExtensionWarning {
                thread_id: "thread-1".to_string(),
                turn_id: None,
                message: "Could not continue the active goal: storage failed".to_string(),
            }]
        );
    }
}
