use codex_http_client::TransportError;
use http::HeaderMap;
use http::StatusCode;
use std::time::Duration;

/// API specific telemetry.
pub trait RequestTelemetry: Send + Sync {
    /// Called after successful auth and immediately before each transport invocation,
    /// including lower HTTP retries. Reserve bounded audit capacity here; returning
    /// a body-free error prevents this send and becomes a non-retryable build error.
    /// This synchronous hook must not wait for I/O. Success is not acceptance or
    /// proof that bytes reached the provider; retain terminal audit capacity until
    /// the owning decision finishes, including cancellation after this boundary.
    fn on_request_start(&self) -> Result<(), String> {
        Ok(())
    }

    /// Response headers for this concrete send, including failed HTTP responses.
    /// Headers are audit information, not evidence that inference was accepted.
    fn on_response_headers(&self, _headers: &HeaderMap) {}

    fn on_request(
        &self,
        attempt: u64,
        status: Option<StatusCode>,
        error: Option<&TransportError>,
        duration: Duration,
    );
}
