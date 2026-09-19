use super::super::LastResponse;
use super::*;
use pretty_assertions::assert_eq;
use tokio::sync::oneshot;

#[test]
fn observation_policy_retires_inherited_incremental_state() -> anyhow::Result<()> {
    let client = test_model_client(SessionSource::Cli);
    let request = client.build_responses_request(
        &Prompt::default(),
        &test_model_info(),
        /*effort*/ None,
        codex_protocol::config_types::ReasoningSummary::None,
        /*service_tier*/ None,
        &test_responses_metadata_for_client(
            &client,
            /*turn_id*/ None,
            "window".into(),
            /*parent_thread_id*/ None,
            TestCodexResponsesRequestKind::Turn,
        ),
    )?;
    let mut session = client.new_session();
    session.websocket_session.last_request = Some(request.clone());
    let (tx, rx) = oneshot::channel();
    tx.send(LastResponse {
        response_id: "prior-overlay".into(),
        items_added: vec![],
    })
    .unwrap();
    session.websocket_session.last_response_rx = Some(rx);
    session.websocket_session.last_response_from_untraced_warmup = true;
    session.enable_observation_full_context();
    assert_eq!(session.prepare_websocket_request(&request), (None, false));
    assert!(!session.uses_websocket_transport());
    assert!(session.websocket_session.last_request.is_none());
    // Even accidentally restored cache state cannot turn a clear into an
    // inherited old overlay or an incremental request.
    session.websocket_session.last_request = Some(request.clone());
    assert_eq!(
        session.get_incremental_items(
            &request, /*last_response*/ None, /*allow_empty_delta*/ true
        ),
        None
    );
    Ok(())
}

#[test]
fn observation_capture_normalization_matches_actual_foreign_provider_input() -> anyhow::Result<()> {
    let client = test_model_client(SessionSource::Cli);
    let mut session = client.new_session();
    session.enable_observation_full_context();
    let input = serde_json::from_value(serde_json::json!([
        {"type": "message", "id": "unprefixed", "role": "user", "content": [{"type": "input_text", "text": "canonical"}],
         "internal_chat_message_metadata_passthrough": {"turn_id": "warehouse", "content_item_kinds": ["observation.current"]}},
        {"type": "function_call", "call_id": "call", "name": "tool", "arguments": "{}", "encrypted_function_args": ["private"]}
    ]))?;
    let prompt = Prompt {
        input,
        ..Default::default()
    };
    let mut expected = prompt.input.clone();
    session.prepare_response_items_for_request(&mut expected);
    let mut request = client.build_responses_request(
        &prompt,
        &test_model_info(),
        /*effort*/ None,
        codex_protocol::config_types::ReasoningSummary::None,
        /*service_tier*/ None,
        &test_responses_metadata_for_client(
            &client,
            /*turn_id*/ None,
            "window".into(),
            /*parent_thread_id*/ None,
            TestCodexResponsesRequestKind::Turn,
        ),
    )?;
    session.prepare_response_items_for_request(&mut request.input);
    assert_eq!(request.input, expected);
    let capture_digest = crate::ObservationSlot::request_input_digest(expected)?;
    assert_eq!(
        crate::ObservationSlot::request_input_digest(request.input)?,
        capture_digest
    );
    Ok(())
}
