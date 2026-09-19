//! Observation resources follow the existing native thread listener owner.
//! Admission belongs to startup/start/resume, never to an observation probe.

use super::ThreadState;
use super::ThreadStateManager;
use crate::observation_admission::ObservationAdmission;
use crate::observation_bridge::ObservationBridge;
use crate::observation_control::rejected;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::OutgoingMessageSender;
use crate::transport::ConnectionOrigin;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ObservationReplacement;
use codex_app_server_protocol::ThreadObservationCapabilities;
use codex_app_server_protocol::ThreadObservationRejectionCode;
use codex_core::CodexThread;
use codex_core::ObservationBinding;
use codex_core::ObservationEvent;
use codex_core::ObservationProfile;
use codex_protocol::ThreadId;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub(super) struct ObservationRelay {
    bridge: Arc<ObservationBridge>,
    task: JoinHandle<()>,
}

impl Drop for ObservationRelay {
    fn drop(&mut self) {
        // Revocation is synchronous; abort also drops a relay blocked on outgoing
        // capacity. A committed but unacknowledged control remains uncertain.
        self.bridge.revoke();
        self.task.abort();
    }
}

impl ThreadState {
    /// Called only after trusted host/profile admission and native idle admission,
    /// with this thread's listener state locked. Does not itself grant authority.
    pub(crate) async fn install_observation(
        &mut self,
        conversation: &Arc<CodexThread>,
        bridge: Arc<ObservationBridge>,
        events: mpsc::Receiver<ObservationEvent>,
        profile: ObservationProfile,
        outgoing: Arc<OutgoingMessageSender>,
    ) -> Result<(), JSONRPCErrorError> {
        if !self.listener_matches(conversation)
            || self.observation.is_some()
            || bridge.thread_id.to_string() != conversation.thread_extension_data().level_id()
        {
            bridge.revoke();
            return Err(rejected(ThreadObservationRejectionCode::IncompatibleState));
        }
        let binding = ObservationBinding {
            slot: Arc::clone(&bridge.slot),
            profile,
        };
        if conversation
            .install_observation_binding(binding)
            .await
            .is_err()
        {
            bridge.revoke();
            return Err(rejected(ThreadObservationRejectionCode::IncompatibleState));
        }
        let task = tokio::spawn(bridge.relay(events, outgoing));
        self.observation_relay = Some(ObservationRelay {
            bridge: Arc::clone(&bridge),
            task,
        });
        self.observation = Some(bridge);
        Ok(())
    }

    pub(super) fn clear_observation(&mut self) {
        if let Some(bridge) = self.observation.take() {
            bridge.revoke();
        }
        self.observation_relay = None;
        // Keep the revoked Core attachment: a surviving old runtime must fail
        // closed, not silently fall back to unobserved requests or inherit a slot.
    }
}

impl ThreadStateManager {
    pub(crate) async fn observation_admission(
        &self,
        connection_id: ConnectionId,
    ) -> Result<Arc<ObservationAdmission>, JSONRPCErrorError> {
        let state = self.state.lock().await;
        if state.live_connections.len() != 1 {
            return Err(rejected(ThreadObservationRejectionCode::Denied));
        }
        state
            .live_connections
            .get(&connection_id)
            .and_then(|capabilities| capabilities.observation.clone())
            .ok_or_else(|| rejected(ThreadObservationRejectionCode::Denied))
    }

    pub(crate) async fn install_thread_observation(
        &self,
        thread_id: ThreadId,
        connection_id: ConnectionId,
        conversation: &Arc<CodexThread>,
        outgoing: Arc<OutgoingMessageSender>,
    ) -> Result<ThreadObservationCapabilities, JSONRPCErrorError> {
        // Keep the existing connection registry locked through installation so
        // close cannot pass revocation and leave a newly installed live slot.
        let registry = self.state.lock().await;
        if registry.live_connections.len() != 1 {
            return Err(rejected(ThreadObservationRejectionCode::Denied));
        }
        let admission = registry
            .live_connections
            .get(&connection_id)
            .and_then(|capabilities| capabilities.observation.as_ref())
            .ok_or_else(|| rejected(ThreadObservationRejectionCode::Denied))?;
        let entry = registry
            .threads
            .get(&thread_id)
            .filter(|entry| entry.connection_ids.contains(&connection_id))
            .ok_or_else(|| rejected(ThreadObservationRejectionCode::Denied))?;
        let mut state = entry.state.lock().await;
        let (bridge, events) =
            ObservationBridge::new(ConnectionOrigin::Stdio, connection_id, thread_id)?;
        let owner_epoch = bridge.owner.epoch.to_string();
        state
            .install_observation(
                conversation,
                bridge,
                events,
                admission.selection.profile,
                outgoing,
            )
            .await?;
        Ok(ThreadObservationCapabilities {
            protocol: 1,
            owner_epoch,
            max_frame_bytes: 4096,
            reserved_tokens: 4608,
            max_lease_seconds: 60,
            max_in_flight_decisions: 1,
            replacement: ObservationReplacement::FullContext,
            automatic_admission: false,
        })
    }
}

impl Drop for ThreadState {
    fn drop(&mut self) {
        self.clear_observation();
    }
}

#[cfg(test)]
#[path = "observation_lifecycle_tests.rs"]
mod tests;
