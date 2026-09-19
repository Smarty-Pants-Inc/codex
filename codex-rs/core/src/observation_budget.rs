//! Native reservation state, not qualification of an external provider or adapter.
use super::MAX_FRAME_BYTES;
use super::MAX_SEQUENCE;
use super::ObservationError;
use super::ObservationEvent;
use super::ObservationOwner;
use super::ObservationSlot;
use super::SlotState;
use crate::ObservationProfile;
use codex_protocol::openai_models::ModelInfo;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservationReservationState {
    Valid,
    Invalid,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ObservationReservation {
    pub generation: u64,
    pub state: ObservationReservationState,
    pub model: Option<String>,
    pub profile: ObservationProfile,
    pub usable_context_tokens: Option<u64>,
    pub reserved_tokens: u32,
    pub max_frame_bytes: u32,
}

pub(super) struct BudgetState {
    pub snapshot: ObservationReservation,
    model: ModelInfo,
    observed_model: Option<ModelInfo>,
}

impl SlotState {
    pub(super) fn require_budget(
        &self,
        generation: u64,
        publishing: bool,
    ) -> Result<(), ObservationError> {
        let budget = self
            .budget
            .as_ref()
            .ok_or(ObservationError::BudgetInvalid)?;
        if budget.snapshot.generation != generation {
            return Err(ObservationError::BudgetGenerationMismatch);
        }
        if publishing && budget.snapshot.state != ObservationReservationState::Valid {
            return Err(ObservationError::BudgetInvalid);
        }
        Ok(())
    }

    pub(super) fn frame_budget_valid(&self) -> bool {
        self.budget.as_ref().is_none_or(|budget| {
            budget.snapshot.state == ObservationReservationState::Valid
                && (self.frame.is_none()
                    || self.frame_budget_generation == Some(budget.snapshot.generation))
        })
    }
}

impl ObservationSlot {
    pub(crate) fn initialize_budget(
        &self,
        owner: ObservationOwner,
        model: &ModelInfo,
        profile: ObservationProfile,
    ) -> Result<ObservationReservation, ObservationError> {
        profile.validate_model(model)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.authorize(owner)?;
        if state.budget.is_some() || state.active_capture.is_some() || state.frame.is_some() {
            return Err(ObservationError::Unavailable);
        }
        let snapshot = reservation(/*generation*/ 1, model, profile);
        state.budget = Some(BudgetState {
            snapshot: snapshot.clone(),
            model: model.clone(),
            observed_model: None,
        });
        Ok(snapshot)
    }

    pub fn native_reservation(
        &self,
        owner: ObservationOwner,
    ) -> Result<ObservationReservation, ObservationError> {
        let state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.authorize(owner)?;
        state
            .budget
            .as_ref()
            .map(|budget| budget.snapshot.clone())
            .ok_or(ObservationError::BudgetInvalid)
    }

    /// Caller holds Session.state across this transition and its settings commit.
    /// Failure permanently fences the slot rather than losing an invalidation.
    pub(crate) fn invalidate_budget(&self) -> Result<(), ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        if let Some(budget) = state.budget.as_mut() {
            budget.observed_model = None;
        }
        self.invalidate_budget_locked(&mut state)
    }

    fn invalidate_budget_locked(&self, state: &mut SlotState) -> Result<(), ObservationError> {
        let Some(budget) = state.budget.as_mut() else {
            return Ok(());
        };
        budget.snapshot.state = ObservationReservationState::Invalid;
        let Some(generation) = budget
            .snapshot
            .generation
            .checked_add(1)
            .filter(|g| *g <= MAX_SEQUENCE)
        else {
            state.revoked = true;
            return Err(ObservationError::ResourceLimit);
        };
        budget.snapshot.generation = generation;
        self.publish_budget(state)
    }

    fn publish_budget(&self, state: &mut SlotState) -> Result<(), ObservationError> {
        if state.commit_order + u64::from(state.active_capture.is_some()) >= MAX_SEQUENCE {
            state.revoked = true;
            return Err(ObservationError::ResourceLimit);
        }
        let permit = match self.events.try_reserve() {
            Ok(permit) => permit,
            Err(_) => {
                state.revoked = true;
                return Err(ObservationError::ResourceLimit);
            }
        };
        state.commit_order += 1;
        permit.send(ObservationEvent::Budget {
            owner: state.owner,
            commit_order: state.commit_order,
            reservation: state
                .budget
                .as_ref()
                .ok_or(ObservationError::BudgetInvalid)?
                .snapshot
                .clone(),
        });
        Ok(())
    }

    /// Rejoin the generation captured under Session.state before asynchronous model
    /// resolution. Caller holds that same state lock again through this commit.
    pub(crate) fn revalidate_budget(
        &self,
        owner: ObservationOwner,
        generation: u64,
        model: &ModelInfo,
    ) -> Result<(), ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        state.authorize(owner)?;
        state.require_budget(generation, /*publishing*/ false)?;
        let budget = state
            .budget
            .as_ref()
            .ok_or(ObservationError::BudgetInvalid)?;
        // A configured model is not proof that an effective per-decision reroute
        // ended. Return the invalid snapshot for cleanup, never certify the old
        // configuration over an observed fallback. A settings commit or a later
        // actual decision can establish a new effective-model generation.
        if budget
            .observed_model
            .as_ref()
            .is_some_and(|effective| effective != model)
        {
            return Ok(());
        }
        let next = reservation(generation, model, budget.snapshot.profile);
        if budget.model == *model && budget.snapshot == next {
            return Ok(());
        }
        let Some(generation) = generation.checked_add(1).filter(|g| *g <= MAX_SEQUENCE) else {
            state.revoked = true;
            return Err(ObservationError::ResourceLimit);
        };
        state.budget = Some(BudgetState {
            snapshot: ObservationReservation { generation, ..next },
            model: model.clone(),
            observed_model: Some(model.clone()),
        });
        self.publish_budget(&mut state)
    }

    /// Effective per-decision overrides are checked before reusing a capture too.
    /// A mismatch invalidates; only the owner read path can revalidate it.
    pub(crate) fn check_budget_model(&self, model: &ModelInfo) -> Result<(), ObservationError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ObservationError::Unavailable)?;
        self.check_budget_model_locked(&mut state, model)
    }

    pub(super) fn check_budget_model_locked(
        &self,
        state: &mut SlotState,
        model: &ModelInfo,
    ) -> Result<(), ObservationError> {
        if let Some(budget) = &state.budget {
            state.authorize(state.owner)?;
            if budget.observed_model.as_ref().unwrap_or(&budget.model) != model {
                self.invalidate_budget_locked(state)?;
                state
                    .budget
                    .as_mut()
                    .ok_or(ObservationError::BudgetInvalid)?
                    .observed_model = Some(model.clone());
                return Err(ObservationError::BudgetInvalid);
            }
            if !state.frame_budget_valid() {
                return Err(ObservationError::BudgetInvalid);
            }
        }
        Ok(())
    }
}

fn reservation(
    generation: u64,
    model: &ModelInfo,
    profile: ObservationProfile,
) -> ObservationReservation {
    let valid = profile.validate_model(model).is_ok();
    ObservationReservation {
        generation,
        state: if valid {
            ObservationReservationState::Valid
        } else {
            ObservationReservationState::Unsupported
        },
        model: Some(model.slug.clone()),
        profile,
        usable_context_tokens: model
            .resolved_context_window()
            .and_then(|window| window.checked_mul(model.effective_context_window_percent))
            .map(|window| window / 100)
            .and_then(|window| u64::try_from(window).ok())
            .filter(|window| *window <= MAX_SEQUENCE),
        reserved_tokens: profile.group_reservation() as u32,
        max_frame_bytes: MAX_FRAME_BYTES as u32,
    }
}
