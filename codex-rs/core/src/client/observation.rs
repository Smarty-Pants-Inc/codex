use super::ModelClientSession;

impl ModelClientSession {
    /// The controlled observation profile uses full-context Responses HTTP only.
    /// This is turn-local policy, not a provider failure or a global WS fallback.
    /// Keep it enabled after a clear so an old overlay cannot be inherited.
    pub(crate) fn enable_observation_full_context(&mut self) {
        self.observation_full_context = true;
        self.websocket_session.last_request = None;
        self.websocket_session.last_response_rx = None;
        self.websocket_session.last_response_from_untraced_warmup = false;
    }

    pub(super) fn uses_websocket_transport(&self) -> bool {
        !self.observation_full_context && self.client.responses_websocket_enabled()
    }
}
