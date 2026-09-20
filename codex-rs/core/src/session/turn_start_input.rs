//! Original initial-input custody across preparation, including a dropped commit.
use super::*;

#[derive(Debug, PartialEq, Eq)]
enum InputProgress {
    Unprocessed,
    Recording,
    Recorded { blocked: bool },
}

/// Owned by run_turn, borrowed by preparation. Dropping a preparation future does
/// not drop the original input or turn a partially recorded batch into fresh work.
/// This is in-memory custody, not a durable wake debit or restartable permit.
pub(super) struct TurnStartInput {
    original: Vec<TurnInput>,
    progress: InputProgress,
}

impl TurnStartInput {
    pub(super) fn new(original: Vec<TurnInput>) -> Self {
        Self {
            original,
            progress: InputProgress::Unprocessed,
        }
    }

    pub(super) fn original(&self) -> &[TurnInput] {
        &self.original
    }

    /// Retains the original per-item inspect->record order. If this future was
    /// dropped during commit, refuse replay: effects may already have occurred.
    pub(super) async fn record(
        &mut self,
        sess: &Arc<Session>,
        turn: &Arc<TurnContext>,
        persist_context: PersistContext,
    ) -> CodexResult<bool> {
        match self.progress {
            InputProgress::Recorded { blocked } => return Ok(blocked),
            InputProgress::Recording => {
                return Err(CodexErr::InvalidRequest(
                    "original turn input recording was interrupted; replay is not permitted"
                        .to_owned(),
                ));
            }
            InputProgress::Unprocessed => {}
        }
        self.progress = InputProgress::Recording;
        let blocked =
            run_hooks_and_record_inputs(sess, turn, &self.original, persist_context).await;
        self.progress = InputProgress::Recorded { blocked };
        Ok(blocked)
    }
}

#[cfg(test)]
#[path = "turn_start_input_tests.rs"]
mod tests;
