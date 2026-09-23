use super::CurrentClientSetup;
use super::ModelClient;
use super::ModelClientSession;
use codex_api::AuthError;
use codex_api::AuthProvider;
use codex_api::AuthProviderFuture;
use codex_login::AuthManager;
use codex_model_provider::SharedModelProvider;
use codex_model_provider::create_model_provider;
use codex_model_provider_info::ModelProviderInfo;
use codex_model_provider_info::WireApi;
use codex_model_provider_info::create_oss_provider_with_base_url;
use codex_protocol::error::CodexErr;
use std::sync::Arc;

/// Host-owned startup selection, not a grant. NativePilot only removes ambient
/// authentication paths; the original slot must separately install real authority.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ProviderStartupPolicy {
    #[default]
    Ordinary,
    NativePilot,
}

impl ProviderStartupPolicy {
    pub(crate) fn create_model_provider(
        self,
        info: ModelProviderInfo,
        auth_manager: Option<Arc<AuthManager>>,
    ) -> SharedModelProvider {
        match self {
            Self::Ordinary => create_model_provider(info, auth_manager),
            Self::NativePilot => {
                let mut transport = pilot_transport_info(&info);
                // The factory selects ambient backends by name. Sanitize before
                // either session or client construction can invoke that factory.
                transport.name = "Native pilot".into();
                create_model_provider(transport, /*auth_manager*/ None)
            }
        }
    }

    /// Keep the reader lazy: even presence telemetry otherwise reads credential
    /// environment values before the first session-owned request exists.
    pub(crate) fn read_auth_env_metadata(
        self,
        read: impl FnOnce() -> codex_login::auth_env_telemetry::AuthEnvTelemetry,
    ) -> codex_login::auth_env_telemetry::AuthEnvTelemetry {
        match self {
            Self::Ordinary => read(),
            Self::NativePilot => Default::default(),
        }
    }

    pub(crate) fn permits_ambient_auth(self) -> bool {
        match self {
            Self::Ordinary => true,
            Self::NativePilot => false,
        }
    }
}

impl ModelClient {
    #[cfg(test)]
    pub(crate) fn with_provider_startup_policy(mut self, policy: ProviderStartupPolicy) -> Self {
        self.provider_startup_policy = policy;
        self
    }

    pub(crate) fn permits_ambient_provider_setup(&self) -> bool {
        self.provider_startup_policy.permits_ambient_auth()
    }
}

/// Read only transport metadata. Installed pilot authority must never consult a
/// provider resolver, configured credential headers or environment header sources.
pub(super) fn pilot_client_setup(info: &ModelProviderInfo) -> Result<CurrentClientSetup, CodexErr> {
    if info.base_url.as_deref().is_none_or(str::is_empty) {
        return Err(CodexErr::InvalidRequest(
            "pilot transport requires an explicit base URL".into(),
        ));
    }
    Ok(CurrentClientSetup {
        auth: None,
        api_provider: pilot_transport_info(info).to_api_provider(/*auth_mode*/ None)?,
        api_auth: Arc::new(RequirePilotAuthentication),
        agent_identity_telemetry: None,
    })
}

/// Shared by construction and send setup; no ambient provider/auth object is
/// created for the pilot. A missing URL stays missing for the send-time refusal.
pub(super) fn pilot_transport_info(info: &ModelProviderInfo) -> ModelProviderInfo {
    let mut transport = create_oss_provider_with_base_url(
        info.base_url.as_deref().unwrap_or_default(),
        WireApi::Responses,
    );
    transport.base_url = info.base_url.clone();
    transport.name = info.name.clone();
    transport.request_max_retries = info.request_max_retries;
    transport.stream_idle_timeout_ms = info.stream_idle_timeout_ms;
    transport
}

struct RequirePilotAuthentication;

impl AuthProvider for RequirePilotAuthentication {
    fn add_auth_headers(&self, _: &mut http::HeaderMap) {}

    fn apply_auth(&self, _: codex_client::Request) -> AuthProviderFuture<'_> {
        // A missing/incorrect native Prepared selection is a refusal, not an
        // unauthenticated send or an opportunity to resolve ambient credentials.
        Box::pin(async {
            Err(AuthError::Build(
                "original pilot authentication unavailable".into(),
            ))
        })
    }
}

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
