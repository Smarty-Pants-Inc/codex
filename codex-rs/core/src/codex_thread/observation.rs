use super::CodexThread;
use crate::ObservationBinding;
use crate::ObservationError;

impl CodexThread {
    /// Attach the admitted slot/profile atomically while excluding native turn
    /// admission, including idle-start reservations. This grants no host authority.
    pub async fn install_observation_binding(
        &self,
        binding: ObservationBinding,
    ) -> Result<(), ObservationError> {
        let config = self.config().await;
        let snapshot = self.config_snapshot().await;
        let model = self
            .session
            .services
            .models_manager
            .get_model_info(&snapshot.model, &config.to_models_manager_config())
            .await;
        binding.profile.validate_model(&model)?;
        let active = self.session.active_turn.lock().await;
        if active.is_some()
            || !self
                .thread_extension_data()
                .insert_if(binding, |current| current.is_none())
        {
            return Err(ObservationError::Unavailable);
        }
        Ok(())
    }
}
