//! A fixed-route, no-redirect/no-resend transport for individually charged operations.
use crate::ClientRouteClass;
use crate::HttpClient;
use crate::HttpClientBuilder;
use crate::HttpClientFactory;
use crate::Request;
use crate::RequestCompression;
use crate::StreamResponse;
use crate::TransportError;
use futures::StreamExt;

#[cfg(test)]
#[path = "single_attempt_tests.rs"]
mod tests;

/// Construction is restricted to a no-resend client; an arbitrary existing
/// reqwest client or transport cannot assert this property with a boolean.
pub struct SingleAttemptTransport {
    client: HttpClient,
    url: String,
}

impl SingleAttemptTransport {
    pub fn new(factory: &HttpClientFactory, url: &str) -> Result<Self, TransportError> {
        let parsed = reqwest::Url::parse(url)
            .map_err(|_| TransportError::Build("invalid fixed route".into()))?;
        if parsed.as_str() != url
            || parsed.scheme() != "https"
            || parsed.host_str().is_none()
            || !parsed.username().is_empty()
            || parsed.password().is_some()
            || parsed.fragment().is_some()
        {
            return Err(TransportError::Build("invalid fixed route".into()));
        }
        let client = HttpClientBuilder::new()
            .single_attempt()
            .build_respecting_outbound_proxy_policy(factory, url, ClientRouteClass::Api)
            .map_err(|_| TransportError::Build("fixed route unavailable".into()))?;
        Ok(Self {
            client,
            url: url.to_owned(),
        })
    }

    /// The synchronous gate sees the exact prepared bytes after body construction,
    /// immediately before the HTTP client sends. This is not an after-TLS witness.
    /// Consumes the transport; neither redirects nor an internal retry can reuse it.
    pub async fn stream(
        self,
        request: Request,
        gate: impl FnOnce(&Request) -> Result<(), String> + Send,
    ) -> Result<StreamResponse, TransportError> {
        if request.url != self.url
            || request.method != http::Method::POST
            || request.compression != RequestCompression::None
            || request.headers.contains_key(http::header::CONTENT_ENCODING)
            || request.headers.contains_key(http::header::HOST)
            || request.headers.contains_key(http::header::CONTENT_LENGTH)
            || request
                .headers
                .contains_key(http::header::TRANSFER_ENCODING)
        {
            return Err(TransportError::Build("fixed request mismatch".into()));
        }
        let request = request
            .into_prepared()
            .map_err(|_| TransportError::Build("fixed body unavailable".into()))?;
        if request.headers.get(http::header::CONTENT_TYPE)
            != Some(&http::HeaderValue::from_static("application/json"))
        {
            return Err(TransportError::Build("fixed content type mismatch".into()));
        }
        let prepared = request
            .prepare_body_for_send()
            .map_err(|_| TransportError::Build("fixed body unavailable".into()))?;
        let mut builder = self
            .client
            .request(http::Method::POST, &self.url)
            .headers(prepared.headers);
        if let Some(body) = prepared.body {
            builder = builder.body(body);
        }
        if let Some(timeout) = request.timeout {
            builder = builder.timeout(timeout);
        }
        gate(&request).map_err(TransportError::Build)?;
        let response = builder
            .send()
            .await
            .map_err(|_| TransportError::Build("fixed send failed".into()))?;
        // Never collect an HTTP error body. The owner bounds ALL response bytes,
        // including rejection/redirect bodies, before decoding or retaining them.
        Ok(StreamResponse {
            status: response.status(),
            headers: response.headers().clone(),
            bytes: Box::pin(
                response.bytes_stream().map(|chunk| {
                    chunk.map_err(|_| TransportError::Build("fixed body failed".into()))
                }),
            ),
        })
    }
}
