use super::request_body_for_trace;
use crate::request::EncodedJsonBody;
use crate::request::Request;
use crate::request::RequestBody;
use crate::request::RequestCompression;
use http::Method;
use pretty_assertions::assert_eq;
use serde_json::json;

#[test]
fn redaction_survives_compression_clone_and_repreparation_without_changing_wire_bytes() {
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(std::io::sink)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let json = json!({"input": [{"text": "OBSERVATION_FRAME_CANARY"}]});
        let encoded = EncodedJsonBody::encode(&json).unwrap();
        for compression in [RequestCompression::None, RequestCompression::Zstd] {
            let mut ordinary = Request::new(Method::POST, "http://unused.invalid/responses".into())
                .with_compression(compression);
            ordinary.body = Some(RequestBody::EncodedJson(encoded.clone()));
            let mut sensitive = ordinary.clone();
            sensitive.body = Some(RequestBody::EncodedJson(
                encoded.clone().without_body_trace(),
            ));
            assert_eq!(request_body_for_trace(&sensitive), "<redacted>");
            assert_eq!(
                sensitive.prepare_body_for_send().unwrap(),
                ordinary.prepare_body_for_send().unwrap()
            );
            let prepared = sensitive.into_prepared().unwrap();
            let ordinary = ordinary.into_prepared().unwrap();
            assert_eq!(request_body_for_trace(&ordinary), json.to_string());
            for retry in [prepared.clone(), prepared.clone().into_prepared().unwrap()] {
                assert_eq!(
                    retry.prepare_body_for_send().unwrap(),
                    ordinary.prepare_body_for_send().unwrap()
                );
                assert_eq!(request_body_for_trace(&retry), "<redacted>");
                assert!(!format!("{retry:?}").contains("OBSERVATION_FRAME_CANARY"));
            }
        }
    });
}

#[test]
fn redacting_an_already_prepared_body_drops_retained_trace_bytes() {
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_writer(std::io::sink)
        .finish();
    tracing::subscriber::with_default(subscriber, || {
        let mut request = Request::new(Method::POST, "http://unused.invalid/responses".into())
            .with_compression(RequestCompression::Zstd);
        request.body = Some(RequestBody::EncodedJson(
            EncodedJsonBody::encode(&json!({"text": "RETAINED_FRAME_CANARY"})).unwrap(),
        ));
        let mut request = request.into_prepared().unwrap();
        let original = request.prepare_body_for_send().unwrap();
        let Some(RequestBody::EncodedJson(body)) = request.body.take() else {
            panic!("expected encoded JSON")
        };
        request.body = Some(RequestBody::EncodedJson(body.without_body_trace()));
        assert_eq!(request.prepare_body_for_send().unwrap(), original);
        assert_eq!(request_body_for_trace(&request), "<redacted>");
        assert!(!format!("{request:?}").contains("RETAINED_FRAME_CANARY"));
    });
}
