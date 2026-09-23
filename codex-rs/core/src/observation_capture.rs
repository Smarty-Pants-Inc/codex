use super::DecisionAudit;
use super::MAX_SEQUENCE;
use super::ObservationError;
use super::ObservationEvent;
use super::ObservationMetadata;
use super::ObservationSlot;
use super::ObservationStatus;
use sha2::Digest;
use sha2::Sha256;
use std::sync::Arc;
use uuid::Uuid;

const UNAVAILABLE_TEXT: &str = "Current observations unavailable.";

/// Immutable input for one logical foreground decision, not one transport send.
/// Retries with unchanged canonical input retain this capture. Changed canonical
/// input requires release and a new capture after the preceding terminal outcome.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationCapture {
    pub turn_id: String,
    pub decision_id: Uuid,
    pub metadata: ObservationMetadata,
    pub captured_at: i64,
    pub text: Option<Arc<str>>,
}

impl ObservationSlot {
    /// Capture after canonical history/tool-result assembly. Publication, time
    /// sampling and this event share one critical section. Failure means no send.
    pub fn capture(&self, turn_id: &str) -> Result<ObservationCapture, ObservationError> {
        self.capture_checked(turn_id, /*model*/ None, |_| Ok(None))
    }

    pub(crate) fn capture_checked(
        &self,
        turn_id: &str,
        model: Option<&codex_protocol::openai_models::ModelInfo>,
        validate: impl FnOnce(&ObservationCapture) -> Result<Option<[u8; 32]>, ObservationError>,
    ) -> Result<ObservationCapture, ObservationError> {
        if turn_id.is_empty()
            || turn_id.len() > 128
            || !turn_id.is_ascii()
            || turn_id.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(ObservationError::InvalidFrame);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        if let Some(model) = model {
            self.check_budget_model_locked(&mut state, model)?;
        }
        if state.budget.is_some() {
            state.authorize(state.owner)?;
            if !state.frame_budget_valid() {
                return Err(ObservationError::BudgetInvalid);
            }
        }
        let now = (self.clock)()?;
        state.expire(now);
        if state.active_capture.is_some() || state.commit_order >= MAX_SEQUENCE - 1 {
            return Err(ObservationError::ResourceLimit);
        }
        let permit = self
            .events
            .try_reserve()
            .map_err(|_| ObservationError::ResourceLimit)?;
        let terminal = self
            .events
            .clone()
            .try_reserve_owned()
            .map_err(|_| ObservationError::ResourceLimit)?;
        let mut metadata = state.metadata();
        metadata.commit_order += 1;
        let text = match metadata.status {
            ObservationStatus::Current => state.frame.as_ref().map(|frame| Arc::clone(&frame.text)),
            ObservationStatus::Cleared => None,
            ObservationStatus::Expired | ObservationStatus::Unavailable => {
                metadata.hash = Some(format!("{:x}", Sha256::digest(UNAVAILABLE_TEXT.as_bytes())));
                Some(Arc::from(UNAVAILABLE_TEXT))
            }
        };
        let capture = ObservationCapture {
            turn_id: turn_id.to_owned(),
            decision_id: Uuid::new_v4(),
            metadata,
            captured_at: now.wall_seconds,
            text,
        };
        let request_input = validate(&capture)?;
        state.commit_order = capture.metadata.commit_order;
        let deadline = (capture.metadata.status == ObservationStatus::Current)
            .then_some(state.deadline)
            .flatten();
        state.active_capture = Some(DecisionAudit::new(
            &capture,
            terminal,
            request_input,
            deadline,
        ));
        permit.send(ObservationEvent::Captured(capture.clone()));
        Ok(capture)
    }
}
