use super::super::LastResponse;
use super::*;
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
