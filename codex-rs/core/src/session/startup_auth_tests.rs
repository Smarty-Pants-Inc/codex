//! Session-local reader seams: no global overrides or credential environment access.
use crate::ProviderStartupPolicy;
use codex_login::AuthManager;
use codex_login::CodexAuth;
use codex_login::auth_env_telemetry::AuthEnvTelemetry;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::Ordering;

pub(crate) struct StartupAuthProbe {
    policy: ProviderStartupPolicy,
    metadata_reads: AtomicUsize,
}

impl StartupAuthProbe {
    pub(crate) fn read_metadata(&self) -> AuthEnvTelemetry {
        self.metadata_reads.fetch_add(1, Ordering::SeqCst);
        assert_eq!(self.policy, ProviderStartupPolicy::Ordinary);
        AuthEnvTelemetry::default()
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn explicit_model_start_and_resume_fence_session_auth_readers() -> anyhow::Result<()> {
    use crate::CodexAppsToolsCache;
    use crate::StartThreadOptions;
    use crate::ThreadManager;
    use crate::config::test_config;
    use crate::thread_manager::build_models_manager;
    use crate::thread_manager::thread_store_from_config;
    use codex_extension_api::ExtensionDataInit;
    use codex_extension_api::empty_extension_registry;
    use codex_history::InitialHistory;
    use codex_history::ResumedHistory;
    use codex_models_manager::model_info::model_info_from_slug;
    use codex_protocol::openai_models::ModelsResponse;
    use codex_protocol::protocol::SessionSource;
    use core_test_support::PathBufExt;

    let server = wiremock::MockServer::start().await;
    for (policy, provider_name) in [
        (ProviderStartupPolicy::NativePilot, "Amazon Bedrock"),
        (ProviderStartupPolicy::NativePilot, "Amazon Bedrock Runtime"),
        (ProviderStartupPolicy::NativePilot, "OpenAI"),
        (ProviderStartupPolicy::Ordinary, "OpenAI"),
    ] {
        let home = tempfile::tempdir()?;
        let mut config = test_config().await;
        config.codex_home = home.path().join("codex-home").abs();
        std::fs::create_dir_all(&config.codex_home)?;
        config.cwd = config.codex_home.clone();
        config.model = Some("gpt-oss-20b".into());
        config.model_catalog = Some(ModelsResponse {
            models: vec![model_info_from_slug("gpt-oss-20b")],
        });
        config.model_provider.name = "OpenAI".into();
        config.model_provider.base_url = Some(format!("{}/backend-api/codex", server.uri()));
        config.model_provider.requires_openai_auth = true;
        config.model_provider.env_key = None;
        config.model_provider.experimental_bearer_token = None;
        config.model_provider.auth = None;
        config.model_provider.aws = None;
        config.model_provider.supports_websockets = false;
        // Unsigned, local fixture claims only. Never loaded from a real auth file,
        // environment variable or account, and never used for an inference turn.
        let auth = CodexAuth::from_external_chatgpt_tokens(
            "e30.eyJleHAiOjQxMDI0NDQ4MDB9.fixture",
            "startup-fixture-account",
            Some("plus"),
        )?;
        let auth_manager = AuthManager::from_auth_for_testing(auth);
        let probe = Arc::new(StartupAuthProbe {
            policy,
            metadata_reads: AtomicUsize::new(0),
        });
        let manager = ThreadManager::new(
            &config,
            Arc::clone(&auth_manager),
            build_models_manager(&config, Arc::clone(&auth_manager)),
            CodexAppsToolsCache::default(),
            SessionSource::Exec,
            Arc::new(codex_exec_server::EnvironmentManager::default_for_tests()),
            empty_extension_registry(),
            Arc::new(crate::test_support::EmptyUserInstructionsProvider),
            /*analytics_events_client*/ None,
            thread_store_from_config(&config, /*state_db*/ None),
            /*agent_graph_store*/ None,
            "11111111-1111-4111-8111-111111111111".into(),
            /*attestation_provider*/ None,
            /*external_time_provider*/ None,
        );
        // Supply an ordinary, fixture-only models manager above. Select the
        // adversarial backend only for the actual start/resume session factory,
        // so test setup itself cannot construct an ambient Bedrock provider.
        config.model_provider.name = provider_name.into();
        let mut init = ExtensionDataInit::new();
        init.insert(policy);
        init.insert(Arc::clone(&probe));
        let started = manager
            .start_thread(StartThreadOptions {
                environments: Some(Vec::new()),
                thread_extension_init: init.clone(),
                ..StartThreadOptions::new(config.clone())
            })
            .await?;
        let expected = match policy {
            ProviderStartupPolicy::NativePilot => 0,
            ProviderStartupPolicy::Ordinary => 1,
        };
        assert_eq!(probe.metadata_reads.load(Ordering::SeqCst), expected);
        let expected_provider = match policy {
            ProviderStartupPolicy::NativePilot => ("Native pilot", false),
            ProviderStartupPolicy::Ordinary => (provider_name, true),
        };
        {
            let state = started.thread.session.state.lock().await;
            let provider = &state.session_configuration.provider;
            // Inspect the actual session provider, not the later ModelClient.
            // A Bedrock factory here would already have sampled ambient auth.
            assert_eq!(
                (
                    provider.info().name.as_str(),
                    provider.auth_manager().is_some()
                ),
                expected_provider,
            );
        }
        started.thread.shutdown_and_wait().await?;
        let resumed = manager
            .resume_thread_with_history_and_init(
                config,
                InitialHistory::Resumed(ResumedHistory {
                    conversation_id: started.thread_id,
                    history: Arc::new(Vec::new()),
                    rollout_path: None,
                }),
                auth_manager,
                /*parent_trace*/ None,
                Default::default(),
                init,
            )
            .await?;
        assert_eq!(probe.metadata_reads.load(Ordering::SeqCst), expected * 2);
        {
            let state = resumed.thread.session.state.lock().await;
            let provider = &state.session_configuration.provider;
            assert_eq!(
                (
                    provider.info().name.as_str(),
                    provider.auth_manager().is_some()
                ),
                expected_provider,
            );
        }
        resumed.thread.shutdown_and_wait().await?;
    }
    Ok(())
}
