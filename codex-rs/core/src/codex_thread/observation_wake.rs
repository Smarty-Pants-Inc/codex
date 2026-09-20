//! Trusted embedding SPI only. No app-server automatic capability is enabled.
use super::CodexThread;
use crate::ObservationBinding;
use crate::ObservationError;
use crate::ObservationOwner;
use crate::ObservationWakeIntent;
use crate::ObservationWakeOutcome;
use crate::ObservationWakeReceipt;
use codex_protocol::turn_input::IdleTurnAdmission;
use codex_protocol::turn_input::StartIfIdleSubmission;
use codex_protocol::turn_input::TurnInput;
use codex_protocol::turn_input::TurnInputRequest;
use std::sync::Arc;

impl CodexThread {
    /// Start one original-owner automatic attempt through native idle admission.
    /// `policy` MUST be the original trusted host's thread/intent-bound admission
    /// guard, with its already reserved cooldown and per-session budget; never an
    /// RPC boolean or replacement pilot guard. It must not await or reenter native
    /// turn admission while its synchronous reserve callback runs. The caller also
    /// owns external context/profile qualification and retains/joins this future.
    /// Cancellation after preparation leaves a pending receipt, never retry permission.
    pub async fn start_observation_wake(
        &self,
        owner: ObservationOwner,
        intent: ObservationWakeIntent,
        policy: Arc<dyn IdleTurnAdmission>,
    ) -> Result<ObservationWakeReceipt, ObservationError> {
        let binding = self
            .thread_extension_data()
            .get::<ObservationBinding>()
            .ok_or(ObservationError::Unavailable)?;
        let guard = Arc::new(binding.slot.prepare_wake(owner, intent, policy)?);
        self.start_prepared_observation_wake(guard).await
    }

    /// Host RPC preparation is retained with the original receipt, not authority.
    /// The same trusted policy and native idle admission rules apply as for embedding.
    pub async fn start_host_observation_wake(
        &self,
        owner: ObservationOwner,
        intent: ObservationWakeIntent,
        policy: Arc<dyn IdleTurnAdmission>,
        preparation: crate::ObservationHostPreparation,
    ) -> Result<ObservationWakeReceipt, ObservationError> {
        let binding = self
            .thread_extension_data()
            .get::<ObservationBinding>()
            .ok_or(ObservationError::Unavailable)?;
        let guard = Arc::new(
            binding
                .slot
                .prepare_host_wake(owner, intent, policy, preparation)?,
        );
        self.start_prepared_observation_wake(guard).await
    }

    async fn start_prepared_observation_wake(
        &self,
        guard: Arc<crate::observation::WakeAdmission>,
    ) -> Result<ObservationWakeReceipt, ObservationError> {
        // Empty automatic input adds no pretend human instruction or durable wake
        // message. The existing request-only observation supplies current context.
        let request = TurnInputRequest::new(TurnInput::UserInput {
            content: Vec::new(),
            client_id: None,
        })
        .with_idle_turn_admission(guard.clone());
        let outcome = match self.start_turn_if_idle(request).await {
            Ok(StartIfIdleSubmission::Started { turn_id }) => {
                ObservationWakeOutcome::Started { turn_id }
            }
            Ok(StartIfIdleSubmission::NotSubmitted { .. }) => ObservationWakeOutcome::Suppressed,
            Err(_) => ObservationWakeOutcome::Unknown,
        };
        // The reservation callback cannot certify Started: mailbox/Plan/settings
        // checks after it still determine the actual native result.
        guard.finish(outcome)
    }
}
