use super::*;

#[tokio::test]
async fn body_redaction_survives_real_api_retry_and_telemetry_builder() -> Result<()> {
    let transport = FlakyTransport::new();
    let mut provider = provider("openai");
    provider.retry.max_attempts = 2;
    let client = ResponsesClient::new(transport.clone(), provider, Arc::new(NoAuth))
        .without_body_trace()
        .with_telemetry(/*request*/ None, /*sse*/ None);
    let body = serde_json::json!({"input": [{"role": "user", "content": [{"type": "input_text", "text": "OBSERVATION_FRAME_CANARY"}]}]});
    let _stream = client
        .stream(
            body.clone(),
            HeaderMap::new(),
            Compression::Zstd,
            /*turn_state*/ None,
        )
        .await?;
    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0], requests[1]);
    for (body_bytes, _, _) in &requests {
        let debug = format!("{body_bytes:?}");
        assert!(debug.contains("<redacted>"));
        assert!(!debug.contains("OBSERVATION_FRAME_CANARY"));
        let RequestBody::EncodedJson(encoded) = body_bytes else {
            panic!("expected encoded JSON")
        };
        let decoded = zstd::stream::decode_all(encoded.as_bytes())?;
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&decoded)?, body);
    }
    Ok(())
}
