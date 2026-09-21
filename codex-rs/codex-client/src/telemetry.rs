use codex_http_client::Request;
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

    /// Inspect the post-authentication request at the existing send boundary.
    /// The borrowed request can contain secrets: never retain or log its contents.
    /// This hook has the same no-I/O and non-retryable refusal contract as
    /// `on_request_start`; the default preserves existing admission hooks.
    fn on_request_prepared(&self, _request: &Request) -> Result<(), String> {
        self.on_request_start()
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
