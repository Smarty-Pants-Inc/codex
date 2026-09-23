use super::ThreadRequestProcessor;
use crate::observation_bridge::ControlOperation;
use crate::observation_control::rejected;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::ConnectionRequestId;
use codex_app_server_protocol::ClientResponsePayload;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadObservationOptions;
use codex_app_server_protocol::ThreadObservationRejectionCode;

pub(super) enum ObservationThread {
    Start,
    Resume(ThreadId),
}
use codex_protocol::ThreadId;

impl ThreadRequestProcessor {
    pub(super) async fn validate_observation_admission(
        &self,
        connection_id: ConnectionId,
        options: &Option<ThreadObservationOptions>,
        thread: ObservationThread,
    ) -> Result<(), JSONRPCErrorError> {
        let Some(options) = options else {
            return Ok(());
        };
        if options.protocol != 2 {
            return Err(rejected(ThreadObservationRejectionCode::Unsupported));
        }
        let admission = self
            .thread_state_manager
            .observation_admission(connection_id)
            .await?;
        if let ObservationThread::Resume(thread_id) = thread
            && (!admission.permits_resume(thread_id)
                || self.thread_manager.get_thread(thread_id).await.is_ok())
        {
            return Err(rejected(ThreadObservationRejectionCode::Denied));
        }
        Ok(())
    }

    pub(crate) async fn observation_request(
        &self,
        request: ConnectionRequestId,
        thread_id: &str,
        epoch: &str,
        operation: ControlOperation,
    ) -> Result<Option<ClientResponsePayload>, JSONRPCErrorError> {
        let denied = || rejected(ThreadObservationRejectionCode::Denied);
        let thread_id = ThreadId::from_string(thread_id).map_err(|_| denied())?;
        // Lookup only. Never load/create/subscribe a thread for a control probe.
        let bridge = self
            .thread_state_manager
            .observation_bridge(thread_id, request.connection_id)
            .await
            .ok_or_else(denied)?;
        bridge.validate_owner(request.connection_id, epoch)?;
        if matches!(operation, ControlOperation::Read) {
            let thread = self
                .thread_manager
                .get_thread(thread_id)
                .await
                .map_err(|_| denied())?;
            thread
                .revalidate_observation_budget(bridge.owner)
                .await
                .map_err(crate::observation_control::store_error)?;
        }
        bridge.submit(request, epoch, operation)?;
        Ok(None) // The ordered relay, not this method return, sends success.
    }

    pub(crate) async fn revoke_observation_connection(&self, connection_id: ConnectionId) {
        self.thread_state_manager
            .revoke_observations(connection_id)
            .await;
    }
}
