//! Custody of one original input after its hooks ran and before history commit.
//! No Clone: the original hook result is consumed by the original recorder once.
use super::Session;
use super::TurnContext;
use super::TurnInput;
use crate::hook_runtime::HookRuntimeOutcome;
use crate::hook_runtime::inspect_pending_input;
use crate::hook_runtime::record_additional_contexts;
use crate::hook_runtime::record_pending_input;
use codex_thread_store::PersistContext;
use std::sync::Arc;

pub(super) struct PreparedTurnInput {
    input: TurnInput,
    hooks: HookRuntimeOutcome,
}

pub(super) enum RecordedInput {
    Blocked,
    AcceptedUser,
    Other,
}

impl PreparedTurnInput {
    /// Runs the ORIGINAL hook operation, not a preview. The caller retains this
    /// value until commit, including on cancellation or unavailable qualification.
    pub(super) async fn prepare(
        sess: &Arc<Session>,
        turn: &Arc<TurnContext>,
        input: TurnInput,
    ) -> Self {
        let hooks = inspect_pending_input(sess, turn, &input).await;
        Self { input, hooks }
    }

    /// Consume both input and hook output; no inspection or hook can run again.
    pub(super) async fn record(
        self,
        sess: &Arc<Session>,
        turn: &Arc<TurnContext>,
        persist_context: PersistContext,
    ) -> RecordedInput {
        if self.hooks.should_stop {
            record_additional_contexts(sess, turn, self.hooks.additional_contexts).await;
            RecordedInput::Blocked
        } else {
            let disposition = match &self.input {
                TurnInput::UserInput { content, .. } if !content.is_empty() => {
                    RecordedInput::AcceptedUser
                }
                TurnInput::UserInput { .. }
                | TurnInput::DeveloperInput { .. }
                | TurnInput::ResponseItem(_)
                | TurnInput::InterAgentCommunication(_) => RecordedInput::Other,
            };
            record_pending_input(
                sess,
                turn,
                self.input,
                self.hooks.additional_contexts,
                persist_context,
            )
            .await;
            disposition
        }
    }
}
