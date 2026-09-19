use super::*;
use crate::observation_bridge::ControlOperation;
use crate::outgoing_message::ConnectionId;
use crate::outgoing_message::ConnectionRequestId;
use crate::request_processors::thread_settings_from_config_snapshot;
use crate::transport::ConnectionOrigin;
use codex_app_server_protocol::RequestId;
use codex_core::ObservationError;
use codex_core::TurnInputRequest;
use codex_file_watcher::WatchRegistration;
use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::user_input::UserInput;
use core_test_support::responses;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use tokio::sync::oneshot;
use tokio::time::Duration;
use tokio::time::timeout;

fn select_fixture_model(config: &mut codex_core::config::Config) {
    config.model = Some("gpt-oss-20b".into());
    let mut model = codex_models_manager::model_info::model_info_from_slug("gpt-oss-20b");
    model.context_window = Some(131_072);
    model.effective_context_window_percent = 100;
    model.use_responses_lite = false;
    config.model_catalog = Some(codex_protocol::openai_models::ModelsResponse {
        models: vec![model],
    });
}

#[tokio::test]
async fn listener_clear_replace_and_drop_keep_core_binding_revoked() -> anyhow::Result<()> {
    enum Boundary {
        Clear,
        Replace,
        Drop,
    }
    for boundary in [Boundary::Clear, Boundary::Replace, Boundary::Drop] {
        let server = responses::start_mock_server().await;
        let test = test_codex()
            .with_config(select_fixture_model)
            .build_with_auto_env(&server)
            .await?;
        let thread_id = ThreadId::from_string(test.codex.thread_extension_data().level_id())?;
        let (bridge, events) =
            ObservationBridge::new(ConnectionOrigin::Stdio, ConnectionId(42), thread_id)
                .expect("owner bridge");
        let mut state = ThreadState::default();
        let (cancel, _cancelled) = oneshot::channel();
        let (_commands, _generation) = state.set_listener(
            cancel,
            &test.codex,
            WatchRegistration::default(),
            thread_settings_from_config_snapshot(&test.codex.config_snapshot().await),
        );
        let (tx, mut rx) = mpsc::channel(/*buffer*/ 1);
        state
            .install_observation(
                &test.codex,
                Arc::clone(&bridge),
                events,
                ObservationProfile::HarmonyGptOss,
                Arc::new(OutgoingMessageSender::new(
                    tx,
                    codex_analytics::AnalyticsEventsClient::disabled(),
                )),
            )
            .await
            .expect("admitted binding");
        let binding = test
            .codex
            .thread_extension_data()
            .get::<ObservationBinding>()
            .expect("Core binding");
        assert!(Arc::ptr_eq(&binding.slot, &bridge.slot));
        bridge
            .submit(
                ConnectionRequestId {
                    connection_id: ConnectionId(42),
                    request_id: RequestId::Integer(1),
                },
                &bridge.owner.epoch.to_string(),
                ControlOperation::Read,
            )
            .expect("ordered read");
        assert!(
            timeout(Duration::from_secs(/*secs*/ 1), rx.recv())
                .await?
                .is_some()
        );
        match boundary {
            Boundary::Clear => state.clear_listener(),
            Boundary::Drop => drop(state),
            Boundary::Replace => {
                let next = test_codex()
                    .with_config(select_fixture_model)
                    .build_with_auto_env(&server)
                    .await?;
                let (cancel, _cancelled) = oneshot::channel();
                let (_commands, _generation) = state.set_listener(
                    cancel,
                    &next.codex,
                    WatchRegistration::default(),
                    thread_settings_from_config_snapshot(&next.codex.config_snapshot().await),
                );
            }
        }
        assert!(
            timeout(Duration::from_secs(/*secs*/ 1), rx.recv())
                .await?
                .is_none()
        );
        assert_eq!(
            binding.slot.capture("old-turn"),
            Err(ObservationError::ResourceLimit)
        );
        test.codex
            .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
                text: "must not send after observation owner teardown".into(),
                text_elements: Vec::new(),
            }]))
            .await?;
        let EventMsg::TurnComplete(completed) = wait_for_event(&test.codex, |event| {
            matches!(event, EventMsg::TurnComplete(_))
        })
        .await
        else {
            unreachable!("terminal predicate")
        };
        assert!(completed.error.is_some());
        assert_eq!(
            server
                .received_requests()
                .await
                .unwrap()
                .iter()
                .filter(|request| request.url.path().ends_with("/responses"))
                .count(),
            0,
        );
        assert!(Arc::ptr_eq(
            &binding,
            &test
                .codex
                .thread_extension_data()
                .get::<ObservationBinding>()
                .expect("revoked binding retained"),
        ));
    }
    Ok(())
}

#[tokio::test]
async fn conflicting_install_cannot_replace_core_binding() -> anyhow::Result<()> {
    let server = responses::start_mock_server().await;
    let test = test_codex()
        .with_config(select_fixture_model)
        .build_with_auto_env(&server)
        .await?;
    let thread_id = ThreadId::from_string(test.codex.thread_extension_data().level_id())?;
    let mut state = ThreadState::default();
    let (cancel, _cancelled) = oneshot::channel();
    let (_commands, _generation) = state.set_listener(
        cancel,
        &test.codex,
        WatchRegistration::default(),
        thread_settings_from_config_snapshot(&test.codex.config_snapshot().await),
    );
    let (tx, _rx) = mpsc::channel(/*buffer*/ 1);
    let outgoing = Arc::new(OutgoingMessageSender::new(
        tx,
        codex_analytics::AnalyticsEventsClient::disabled(),
    ));
    let (owner, events) =
        ObservationBridge::new(ConnectionOrigin::Stdio, ConnectionId(42), thread_id)
            .expect("owner");
    state
        .install_observation(
            &test.codex,
            Arc::clone(&owner),
            events,
            ObservationProfile::HarmonyGptOss,
            Arc::clone(&outgoing),
        )
        .await
        .expect("first install");
    let (candidate, events) =
        ObservationBridge::new(ConnectionOrigin::Stdio, ConnectionId(43), thread_id)
            .expect("candidate");
    assert_eq!(
        state
            .install_observation(
                &test.codex,
                Arc::clone(&candidate),
                events,
                ObservationProfile::HarmonyGptOss,
                outgoing
            )
            .await,
        Err(rejected(ThreadObservationRejectionCode::IncompatibleState))
    );
    assert!(Arc::ptr_eq(
        &owner.slot,
        &test
            .codex
            .thread_extension_data()
            .get::<ObservationBinding>()
            .expect("original binding")
            .slot
    ));
    assert_eq!(
        candidate.slot.capture("candidate"),
        Err(ObservationError::ResourceLimit)
    );
    let capture = owner.slot.capture("current")?;
    owner.slot.release(capture.decision_id)?;
    Ok(())
}
