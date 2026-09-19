use crate::ObservationCapture;
use crate::ObservationError;
use crate::ObservationProfile;
use crate::ObservationSlot;
use crate::context::CurrentObservations;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use std::sync::Arc;

struct Active {
    canonical_input: Vec<ResponseItem>,
    capture: ObservationCapture,
    items: Vec<ResponseItem>,
}

/// One guard per run_sampling_request: step/model/tools/base instructions are
/// fixed there. Compare input only after history normalization and pending tool
/// attachment, using the same final ID/content-kind preparation as the sender.
pub(super) struct ObservationSampling {
    pub(super) slot: Arc<ObservationSlot>,
    profile: ObservationProfile,
    turn_id: String,
    active: Option<Active>,
}

impl ObservationSampling {
    pub(super) fn new(
        slot: Arc<ObservationSlot>,
        profile: ObservationProfile,
        turn_id: String,
    ) -> Self {
        // Load the embedded, pinned vocabulary before entering the slot lock.
        let _ = tiktoken_rs::o200k_harmony_singleton();
        Self {
            slot,
            profile,
            turn_id,
            active: None,
        }
    }

    pub(super) fn prepare(
        &mut self,
        canonical_input: Vec<ResponseItem>,
        model: &ModelInfo,
    ) -> Result<(Vec<ResponseItem>, uuid::Uuid), ObservationError> {
        self.slot.check_budget_model(model)?;
        self.profile.validate_model(model)?;
        if let Some(active) = &self.active
            && active.canonical_input == canonical_input
        {
            return Ok((active.items.clone(), active.capture.decision_id));
        }
        if let Some(previous) = self.active.take() {
            self.slot.release(previous.capture.decision_id)?;
        }
        let mut items = Vec::new();
        let capture = self
            .slot
            .capture_checked(&self.turn_id, Some(model), |capture| {
                items = CurrentObservations::new(self.profile, model, capture)?
                    .map(CurrentObservations::into_request_items)
                    .unwrap_or_default();
                Ok(())
            })?;
        let decision_id = capture.decision_id;
        self.active = Some(Active {
            canonical_input,
            capture,
            items: items.clone(),
        });
        Ok((items, decision_id))
    }
}

impl Drop for ObservationSampling {
    fn drop(&mut self) {
        if let Some(active) = self.active.take()
            && let Err(error) = self.slot.release(active.capture.decision_id)
        {
            tracing::warn!(%error, "failed to release observation decision");
        }
    }
}

#[cfg(test)]
#[path = "observation_sampling_tests.rs"]
mod tests;
