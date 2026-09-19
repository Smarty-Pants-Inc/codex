//! Lookup-only access to the original exclusive-connection pilot controls.
use super::ThreadRequestProcessor;
use crate::observation_control::rejected;
use crate::observation_pilot_control::PilotControl;
use crate::outgoing_message::ConnectionId;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::ThreadObservationRejectionCode;
use codex_protocol::ThreadId;
use std::sync::Arc;

impl ThreadRequestProcessor {
    pub(crate) async fn pilot_control(
        &self,
        connection: ConnectionId,
        thread: &str,
    ) -> Result<Arc<PilotControl>, JSONRPCErrorError> {
        let denied = || rejected(ThreadObservationRejectionCode::Denied);
        let thread_id = ThreadId::from_string(thread).map_err(|_| denied())?;
        // No load/resume/new slot or owner supplied by an RPC. The bridge retains
        // the exact original thread even while its native shutdown is joining.
        let bridge = self
            .thread_state_manager
            .observation_bridge(thread_id, connection)
            .await
            .ok_or_else(denied)?;
        bridge.pilot.get().cloned().ok_or_else(denied)
    }
}
