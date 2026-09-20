use anyhow::Result;
use app_test_support::MockResponsesConfig;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ClientInfo;
use codex_app_server_protocol::InitializeCapabilities;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_app_server_protocol::RequestId;
use codex_app_server_protocol::ThreadObservationOptions;
use codex_app_server_protocol::ThreadObservationReadResponse;
use codex_app_server_protocol::ThreadObservationSetResponse;
use codex_app_server_protocol::ThreadResumeParams;
use codex_app_server_protocol::ThreadResumeResponse;
use codex_app_server_protocol::ThreadStartParams;
use codex_app_server_protocol::ThreadStartResponse;
use codex_app_server_protocol::TurnStartParams;
use codex_app_server_protocol::UserInput;
use codex_models_manager::model_info::model_info_from_slug;
use core_test_support::responses;
use pretty_assertions::assert_eq;
use serde_json::json;
use sha2::Digest;
use sha2::Sha256;
use std::time::Duration;
use tokio::time::timeout;

fn rejection(code: &str) -> JSONRPCErrorError {
    JSONRPCErrorError {
        code: -32002,
        message: "observation control request rejected".into(),
        data: Some(json!({"type":"threadObservationRejected", "protocol":2, "code":code})),
    }
}

/// Real stdio RPC lifecycle with a mock provider. The launch selection is a
/// fixture declaration, not qualification of an installed host or real provider.
#[tokio::test]
async fn observation_start_and_admitted_resume_install_fresh_owned_relays() -> Result<()> {
    timeout(Duration::from_secs(/*secs*/ 60), async {
        let home = tempfile::tempdir()?;
        let server = responses::start_mock_server().await;
        let mock = responses::mount_sse_once(&server, responses::sse_completed("completed")).await;
        let mut model = model_info_from_slug("gpt-oss-20b");
        model.context_window = Some(131_072);
        model.effective_context_window_percent = 100;
        model.use_responses_lite = false;
        let catalog = home.path().join("models.json");
        std::fs::write(&catalog, serde_json::to_vec(&json!({"models":[model]}))?)?;
        MockResponsesConfig::new(&server.uri()).with_model("gpt-oss-20b")
            .with_root_config(&format!("observation_max_output_tokens = 128\nmodel_catalog_json = {}", serde_json::to_string(&catalog)?))
            .write(home.path())?;
        let mut app = TestAppServer::builder().with_codex_home(home.path())
            .without_managed_config().build_initialized().await?;
        let id = app.send_thread_start_request_with_auto_env(ThreadStartParams {
            observation: Some(ThreadObservationOptions { protocol: 2 }), ..Default::default()
        }).await?;
        assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("DENIED"));
        app.shutdown_gracefully().await?;

        let mut app = TestAppServer::builder().with_codex_home(home.path()).without_managed_config()
            .with_args(&["--observation-profile", "harmony-gpt-oss"]).build().await?;
        app.initialize_with_capabilities(ClientInfo {
            name: "sense-source-fixture".into(), title: None, version: "1".into(),
        }, Some(InitializeCapabilities {
            experimental_api: true,
            opt_out_notification_methods: Some(vec!["thread/observation/captured".into()]),
            ..Default::default()
        })).await?;
        let id = app.send_thread_start_request_with_auto_env(ThreadStartParams {
            observation: Some(ThreadObservationOptions { protocol: 2 }), ..Default::default()
        }).await?;
        assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("DENIED"));
        app.shutdown_gracefully().await?;

        let mut app = TestAppServer::builder().with_codex_home(home.path()).without_managed_config()
            .with_args(&["--observation-profile", "harmony-gpt-oss"]).build_initialized().await?;
        let id = app.send_thread_start_request_with_auto_env(ThreadStartParams {
            observation: Some(ThreadObservationOptions { protocol: 2 }), ..Default::default()
        }).await?;
        let started: ThreadStartResponse = app.read_response(id).await?;
        let initial = started.observation.expect("installed start capability");
        assert_eq!(
            (initial.max_frame_bytes, initial.reserved_tokens),
            (initial.native_reservation.max_frame_bytes, initial.native_reservation.reserved_tokens),
        );
        let thread_id = started.thread.id;
        // A real owned observation connection/profile still installs no pilot
        // authority. These public methods must not resume or mint one from IDs.
        for (method, extra) in [
            ("thread/pilot/read", json!({})),
            ("thread/pilot/retire", json!({})),
            ("thread/pilot/check", json!({"operation":"prepareSource"})),
            ("thread/pilot/start", json!({"input":"bounded automatic opportunity"})),
        ] {
            let mut params = extra;
            params["threadId"] = json!(thread_id);
            let id = app.send_request(method, Some(params)).await?;
            assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error,
                rejection("DENIED"));
        }
        let id = app.send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.clone(), ..Default::default()
        }).await?;
        assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("DENIED"));
        let id = app.send_request("thread/observation/read", Some(json!({
            "threadId":thread_id, "ownerEpoch":initial.owner_epoch,
        }))).await?;
        let first: ThreadObservationReadResponse = app.read_response(id).await?;
        assert_eq!(&first.native_reservation, &initial.native_reservation);
        assert_eq!((&first.owner_epoch, first.revision, &first.hash), (&initial.owner_epoch, 0, &None));
        // Cleanup/fence remains available without an automatic policy, while
        // ordinary launch ownership alone never enables automatic start.
        assert!(!initial.automatic_admission);
        let id = app.send_request("thread/observation/wake/start", Some(json!({
            "threadId":thread_id, "ownerEpoch":initial.owner_epoch,
            "intent":{"sequence":1,"frameRevision":1,"frameHash":"a".repeat(64),
                "budgetGeneration":1,"expectedCommitOrder":1}, "operandDigest":"b".repeat(64),
        }))).await?;
        assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("UNSUPPORTED"));
        for (epoch, code) in [(initial.owner_epoch.replace('-', ""), "DENIED"), ("00000000-0000-0000-0000-000000000000".into(), "STALE_OWNER")] {
            let id = app.send_request("thread/observation/wake/read", Some(json!({
                "threadId":thread_id, "ownerEpoch":epoch, "query":{"type":"fence","sequence":1},
            }))).await?;
            assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection(code));
        }
        for sequence in [0_u64, 1_u64 << 53] {
            let id = app.send_request("thread/observation/wake/invalidate", Some(json!({
                "threadId":thread_id, "ownerEpoch":initial.owner_epoch, "sequence":sequence,
            }))).await?;
            assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("INVALID_INPUT"));
        }
        let id = app.send_request("thread/observation/wake/invalidate", Some(json!({
            "threadId":thread_id, "ownerEpoch":initial.owner_epoch, "sequence":2,
        }))).await?;
        assert_eq!(app.read_response::<serde_json::Value>(id).await?, json!({"protocol":1,"intentFloor":2}));
        let id = app.send_request("thread/observation/wake/read", Some(json!({
            "threadId":thread_id, "ownerEpoch":initial.owner_epoch, "query":{"type":"fence","sequence":2},
        }))).await?;
        assert_eq!(app.read_response::<serde_json::Value>(id).await?, json!({"protocol":1,"type":"fence","intentFloor":2}));
        let id = app.send_request("thread/observation/wake/read", Some(json!({
            "threadId":thread_id, "ownerEpoch":initial.owner_epoch,
            "query":{"type":"attempt","sequence":1,"operandDigest":"a".repeat(64)},
        }))).await?;
        assert_eq!(app.read_response::<serde_json::Value>(id).await?, json!({"protocol":1,"type":"attempt","intentFloor":2,"receipt":null}));
        let id = app.send_request("thread/observation/wake/retire", Some(json!({
            "threadId":thread_id, "ownerEpoch":initial.owner_epoch,"sequence":1,"operandDigest":"a".repeat(64),
        }))).await?;
        assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("REVISION_MISMATCH"));
        let text = "SOURCE19_OWNER_ONLY";
        let expires_at = i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs())? + 60;
        let id = app.send_request("thread/observation/set", Some(json!({
            "threadId":thread_id, "ownerEpoch":initial.owner_epoch, "revision":1,
            "expectedBudgetGeneration":first.native_reservation.generation,
            "frame":{"text":text, "hash":format!("{:x}", Sha256::digest(text.as_bytes())), "expiresAt":expires_at},
        }))).await?;
        let published: ThreadObservationSetResponse = app.read_response(id).await?;
        app.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![UserInput::Text { text: "record one completed turn".into(), text_elements: vec![] }],
            ..Default::default()
        }).await?;
        let captured: codex_app_server_protocol::ThreadObservationCapturedNotification = serde_json::from_value(
            app.read_stream_until_notification_message("thread/observation/captured").await?.params.expect("capture params"))?;
        let submitted: codex_app_server_protocol::ThreadObservationSubmittedNotification = serde_json::from_value(
            app.read_stream_until_notification_message("thread/observation/submitted").await?.params.expect("submission params"))?;
        assert_eq!((submitted.protocol, &submitted.decision_id, submitted.commit_order, submitted.budget_generation),
            (captured.protocol, &captured.decision_id, captured.commit_order, captured.budget_generation));
        assert_eq!(captured.budget_generation, first.native_reservation.generation);
        let mut generation = first.native_reservation.generation;
        for selected_model in ["gpt-5", "gpt-oss-20b"] {
            let id = app.send_request("thread/settings/update", Some(json!({"threadId":thread_id, "model":selected_model}))).await?;
            let _: codex_app_server_protocol::ThreadSettingsUpdateResponse = app.read_response(id).await?;
            app.read_stream_until_notification_message("thread/settings/updated").await?;
            let id = app.send_request("thread/observation/read", Some(json!({"threadId":thread_id, "ownerEpoch":initial.owner_epoch}))).await?;
            let read: ThreadObservationReadResponse = app.read_response(id).await?;
            assert!(read.native_reservation.generation > generation);
            generation = read.native_reservation.generation;
            if selected_model == "gpt-5" {
                assert_eq!(read.native_reservation.state, codex_app_server_protocol::NativeReservationState::Unsupported);
                assert_eq!(read.state, codex_app_server_protocol::ObservationPublicationState::Unavailable);
                assert_eq!((&read.owner_epoch, read.revision, &read.hash, read.expires_at, read.frame_budget_generation),
                    (&published.owner_epoch, published.revision, &published.hash, published.expires_at, published.frame_budget_generation));
                assert!(read.commit_order > published.commit_order);
                let id = app.send_request("thread/observation/set", Some(json!({
                    "threadId":thread_id, "ownerEpoch":initial.owner_epoch, "revision":2, "frame":null,
                    "expectedBudgetGeneration":first.native_reservation.generation,
                }))).await?;
                assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("BUDGET_GENERATION_MISMATCH"));
                let id = app.send_request("thread/observation/set", Some(json!({
                    "threadId":thread_id, "ownerEpoch":initial.owner_epoch, "revision":2, "frame":null,
                    "expectedBudgetGeneration":generation,
                }))).await?;
                let cleared: ThreadObservationSetResponse = app.read_response(id).await?;
                assert_eq!((cleared.state, cleared.frame_budget_generation, cleared.native_reservation),
                    (codex_app_server_protocol::ObservationPublicationState::Cleared, None, read.native_reservation));
            } else {
                assert_eq!(read.native_reservation.state, codex_app_server_protocol::NativeReservationState::Valid);
                assert_eq!(read.state, codex_app_server_protocol::ObservationPublicationState::Cleared);
                let id = app.send_request("thread/observation/set", Some(json!({
                    "threadId":thread_id, "ownerEpoch":initial.owner_epoch, "revision":3,
                    "expectedBudgetGeneration":generation,
                    "frame":{"text":text,"hash":published.hash,"expiresAt":expires_at},
                }))).await?;
                let replaced: ThreadObservationSetResponse = app.read_response(id).await?;
                assert_eq!((replaced.state, replaced.frame_budget_generation),
                    (codex_app_server_protocol::ObservationPublicationState::Current, Some(generation)));
            }
        }
        app.shutdown_gracefully().await?;

        // A selected profile still cannot resume an arbitrary persisted thread.
        let mut app = TestAppServer::builder().with_codex_home(home.path()).without_managed_config()
            .with_args(&["--observation-profile", "harmony-gpt-oss"]).build_initialized().await?;
        let id = app.send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.clone(), observation: Some(ThreadObservationOptions { protocol: 2 }),
            ..Default::default()
        }).await?;
        assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("DENIED"));
        app.shutdown_gracefully().await?;

        let mut app = TestAppServer::builder().with_codex_home(home.path()).without_managed_config()
            .with_args(&["--observation-profile", "harmony-gpt-oss", "--observation-resume-thread", &thread_id])
            .build_initialized().await?;
        let id = app.send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.clone(), observation: Some(ThreadObservationOptions { protocol: 2 }),
            ..Default::default()
        }).await?;
        let resumed: ThreadResumeResponse = app.read_response(id).await?;
        let current = resumed.observation.expect("installed resume capability");
        assert_eq!(
            (current.max_frame_bytes, current.reserved_tokens),
            (current.native_reservation.max_frame_bytes, current.native_reservation.reserved_tokens),
        );
        assert_ne!(current.owner_epoch, initial.owner_epoch);
        let id = app.send_request("thread/observation/read", Some(json!({
            "threadId":thread_id, "ownerEpoch":initial.owner_epoch,
        }))).await?;
        assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("STALE_OWNER"));
        let id = app.send_request("thread/observation/read", Some(json!({
            "threadId":thread_id, "ownerEpoch":current.owner_epoch,
        }))).await?;
        let mut fresh: ThreadObservationReadResponse = app.read_response(id).await?;
        assert_eq!(&fresh.native_reservation, &current.native_reservation);
        // Fresh admission restores neither body, revision nor prior audit order.
        fresh.owner_epoch = first.owner_epoch.clone();
        assert_eq!(fresh, first);
        app.shutdown_gracefully().await?;
        assert!(mock.single_request().body_contains_text("SOURCE19_OWNER_ONLY"));
        Ok::<(), anyhow::Error>(())
    }).await?
}
