//! Join reservation readback to committed session settings, not notification timing.
use super::Session;
use crate::ObservationBinding;
use crate::ObservationError;
use crate::ObservationOwner;

impl Session {
    /// Called while holding Session.state before committing model/settings changes.
    pub(super) fn invalidate_observation_budget(&self) {
        if let Some(binding) = self
            .services
            .thread_extension_data
            .get::<ObservationBinding>()
            && let Err(error) = binding.slot.invalidate_budget()
        {
            // The slot latches revocation on queue/sequence failure. Settings may
            // still commit, but this owner can no longer publish or send.
            tracing::warn!(%error, "observation reservation invalidation failed closed");
        }
    }

    pub(crate) async fn install_budgeted_observation_binding(
        &self,
        binding: ObservationBinding,
        owner: ObservationOwner,
    ) -> Result<(), ObservationError> {
        let active = self.active_turn.lock().await;
        if active.is_some()
            || self
                .services
                .thread_extension_data
                .get::<ObservationBinding>()
                .is_some()
        {
            return Err(ObservationError::Unavailable);
        }
        let state = self.state.lock().await;
        let config = self.build_effective_session_config(&state.session_configuration);
        let model = self
            .services
            .models_manager
            .get_model_info(
                state.session_configuration.collaboration_mode.model(),
                &config.to_models_manager_config(),
            )
            .await;
        // Initialization is once-only before the app-server exposes admission.
        // Session.state stays held; no slot lock is held across model resolution.
        binding
            .slot
            .initialize_budget(owner, &model, binding.profile)?;
        if !self
            .services
            .thread_extension_data
            .insert_if(binding, |current| current.is_none())
        {
            return Err(ObservationError::Unavailable);
        }
        Ok(())
    }

    pub(crate) async fn revalidate_observation_budget(
        &self,
        owner: ObservationOwner,
    ) -> Result<(), ObservationError> {
        let binding = self
            .services
            .thread_extension_data
            .get::<ObservationBinding>()
            .ok_or(ObservationError::Unavailable)?;
        let (config, model, generation) = {
            let state = self.state.lock().await;
            let reservation = binding.slot.native_reservation(owner)?;
            (
                self.build_effective_session_config(&state.session_configuration),
                state
                    .session_configuration
                    .collaboration_mode
                    .model()
                    .to_owned(),
                reservation.generation,
            )
        };
        let model = self
            .services
            .models_manager
            .get_model_info(&model, &config.to_models_manager_config())
            .await;
        // Lock order is Session.state -> slot everywhere. A settings writer cannot
        // invalidate then leave OLD config visible to this revalidation commit.
        let _state = self.state.lock().await;
        binding.slot.revalidate_budget(owner, generation, &model)
    }
}

#[cfg(test)]
#[path = "observation_budget_tests.rs"]
mod tests;
