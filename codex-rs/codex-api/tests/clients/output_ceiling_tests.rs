use super::*;
use codex_client::ReqwestTransport;
use codex_http_client::HttpClientBuilder;
use pretty_assertions::assert_eq;
use std::num::NonZeroU64;
use wiremock::Mock;
use wiremock::MockServer;
use wiremock::ResponseTemplate;

#[tokio::test]
async fn ordinary_http_request_omits_or_transmits_output_ceiling() -> Result<()> {
    for max_output_tokens in [None, NonZeroU64::new(/*n*/ 128)] {
        let server = MockServer::start().await;
        Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/responses"))
            .respond_with(ResponseTemplate::new(http::StatusCode::OK))
            .mount(&server)
            .await;
        let transport =
            ReqwestTransport::from_http_client(HttpClientBuilder::new().build_direct()?);
        let mut provider = provider("openai");
        provider.base_url = server.uri();
        let client = ResponsesClient::new(transport, provider, Arc::new(NoAuth));
        let request = ResponsesApiRequest {
            model: "gpt-test".into(),
            max_output_tokens,
            instructions: "Say hi".into(),
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
        let mut expected = serde_json::json!({
            "model": "gpt-test", "instructions": "Say hi", "input": [],
            "tool_choice": "auto", "parallel_tool_calls": false, "reasoning": null,
            "store": false, "stream": true, "include": []
        });
        if let Some(limit) = max_output_tokens {
            expected["max_output_tokens"] = serde_json::json!(limit.get());
        }
        let _stream = client
            .stream_request(request, ResponsesOptions::default())
            .await?;
        let received = server.received_requests().await.unwrap();
        assert_eq!(received.len(), 1);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&received[0].body)?,
            expected
        );
    }
    Ok(())
}
