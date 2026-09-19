use super::*;
use codex_client::RequestTelemetry;
use pretty_assertions::assert_eq;
use std::num::NonZeroU64;

struct NativeRoute(Mutex<Vec<Vec<u8>>>);
impl RequestTelemetry for NativeRoute {
    fn native_output_limit(&self) -> std::result::Result<Option<NonZeroU64>, String> {
        Ok(NonZeroU64::new(/*n*/ 17))
    }
    fn authenticate_request(
        &self,
        request: &mut Request,
    ) -> std::result::Result<codex_client::RequestAuthentication, String> {
        request.headers.insert(
            http::header::AUTHORIZATION,
            HeaderValue::from_static("Bearer fixture"),
        );
        Ok(codex_client::RequestAuthentication::Prepared)
    }
    fn stream_native_request<'a>(
        &'a self,
        request: &'a Request,
    ) -> codex_client::NativeStreamFuture<'a> {
        Box::pin(async move {
            assert_eq!(
                request.headers[http::header::AUTHORIZATION],
                "Bearer fixture"
            );
            assert_eq!(request.compression, codex_client::RequestCompression::None);
            let wire = request
                .extensions
                .get::<Arc<codex_api::CountWire>>()
                .expect("original typed wire");
            let body = request.prepare_body_for_send().unwrap().body.unwrap();
            assert_eq!(body.as_ref(), wire.inference_body().as_bytes());
            self.0.lock().unwrap().push(body.to_vec());
            // Terminal native refusal must not cause ordinary auth/transport retry.
            Err("controlled native refusal".into())
        })
    }
    fn on_request(&self, _: u64, _: Option<StatusCode>, _: Option<&TransportError>, _: Duration) {}
}

#[tokio::test]
async fn typed_native_wire_reaches_original_async_route_without_ordinary_send_or_retry()
-> Result<()> {
    let route = Arc::new(NativeRoute(Mutex::new(Vec::new())));
    let auth = Arc::new(FailsOnceAuth::transient());
    let transport = FlakyTransport::new();
    let mut provider = provider("openai");
    provider.retry.max_attempts = 4;
    let client = ResponsesClient::new(transport.clone(), provider, auth.clone())
        .with_telemetry(Some(route.clone()), /*sse*/ None);
    let request = ResponsesApiRequest {
        model: "fixture-model".into(),
        instructions: "all fixture instructions".into(),
        input: vec![],
        tools: None,
        tool_choice: "auto".into(),
        parallel_tool_calls: false,
        reasoning: None,
        store: false,
        stream: true,
        stream_options: None,
        include: vec![],
        service_tier: None,
        prompt_cache_key: None,
        text: None,
        client_metadata: None,
        access_programs: None,
    };
    let expected = codex_api::prepare_response_count(&request, NonZeroU64::new(/*n*/ 17).unwrap())?;
    let result = client
        .stream_request(
            request,
            ResponsesOptions {
                compression: Compression::Zstd,
                ..Default::default()
            },
        )
        .await;
    assert!(
        matches!(result, Err(ApiError::Transport(TransportError::Build(ref text))) if text == "controlled native refusal")
    );
    assert_eq!(
        (
            auth.attempts(),
            transport.attempts(),
            route.0.lock().unwrap().clone()
        ),
        (0, 0, vec![expected.inference_body().as_bytes().to_vec()])
    );
    Ok(())
}
