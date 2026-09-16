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
        data: Some(json!({"type":"threadObservationRejected", "protocol":1, "code":code})),
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
            .with_root_config(&format!("model_catalog_json = {}", serde_json::to_string(&catalog)?))
            .write(home.path())?;
        let mut app = TestAppServer::builder().with_codex_home(home.path())
            .without_managed_config().build_initialized().await?;
        let id = app.send_thread_start_request_with_auto_env(ThreadStartParams {
            observation: Some(ThreadObservationOptions { protocol: 1 }), ..Default::default()
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
            observation: Some(ThreadObservationOptions { protocol: 1 }), ..Default::default()
        }).await?;
        assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("DENIED"));
        app.shutdown_gracefully().await?;

        let mut app = TestAppServer::builder().with_codex_home(home.path()).without_managed_config()
            .with_args(&["--observation-profile", "harmony-gpt-oss"]).build_initialized().await?;
        let id = app.send_thread_start_request_with_auto_env(ThreadStartParams {
            observation: Some(ThreadObservationOptions { protocol: 1 }), ..Default::default()
        }).await?;
        let started: ThreadStartResponse = app.read_response(id).await?;
        let initial = started.observation.expect("installed start capability");
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
        assert_eq!((&first.owner_epoch, first.revision, &first.hash), (&initial.owner_epoch, 0, &None));
        let text = "SOURCE19_OWNER_ONLY";
        let expires_at = i64::try_from(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs())? + 60;
        let id = app.send_request("thread/observation/set", Some(json!({
            "threadId":thread_id, "ownerEpoch":initial.owner_epoch, "revision":1,
            "frame":{"text":text, "hash":format!("{:x}", Sha256::digest(text.as_bytes())), "expiresAt":expires_at},
        }))).await?;
        let _: ThreadObservationSetResponse = app.read_response(id).await?;
        app.start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![UserInput::Text { text: "record one completed turn".into(), text_elements: vec![] }],
            ..Default::default()
        }).await?;
        app.shutdown_gracefully().await?;

        // A selected profile still cannot resume an arbitrary persisted thread.
        let mut app = TestAppServer::builder().with_codex_home(home.path()).without_managed_config()
            .with_args(&["--observation-profile", "harmony-gpt-oss"]).build_initialized().await?;
        let id = app.send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.clone(), observation: Some(ThreadObservationOptions { protocol: 1 }),
            ..Default::default()
        }).await?;
        assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("DENIED"));
        app.shutdown_gracefully().await?;

        let mut app = TestAppServer::builder().with_codex_home(home.path()).without_managed_config()
            .with_args(&["--observation-profile", "harmony-gpt-oss", "--observation-resume-thread", &thread_id])
            .build_initialized().await?;
        let id = app.send_thread_resume_request(ThreadResumeParams {
            thread_id: thread_id.clone(), observation: Some(ThreadObservationOptions { protocol: 1 }),
            ..Default::default()
        }).await?;
        let resumed: ThreadResumeResponse = app.read_response(id).await?;
        let current = resumed.observation.expect("installed resume capability");
        assert_ne!(current.owner_epoch, initial.owner_epoch);
        let id = app.send_request("thread/observation/read", Some(json!({
            "threadId":thread_id, "ownerEpoch":initial.owner_epoch,
        }))).await?;
        assert_eq!(app.read_stream_until_error_message(RequestId::Integer(id)).await?.error, rejection("STALE_OWNER"));
        let id = app.send_request("thread/observation/read", Some(json!({
            "threadId":thread_id, "ownerEpoch":current.owner_epoch,
        }))).await?;
        let mut fresh: ThreadObservationReadResponse = app.read_response(id).await?;
        // Fresh admission restores neither body, revision nor prior audit order.
        fresh.owner_epoch = first.owner_epoch.clone();
        assert_eq!(fresh, first);
        app.shutdown_gracefully().await?;
        assert!(mock.single_request().body_contains_text("SOURCE19_OWNER_ONLY"));
        Ok::<(), anyhow::Error>(())
    }).await?
}
