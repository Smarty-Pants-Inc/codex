use super::*;

#[test]
fn native_count_descriptor_cli_requires_the_complete_original_v2_group() {
    use clap::Parser;
    #[derive(Parser)]
    struct Args {
        #[arg(long)]
        remote_control: bool,
        #[command(flatten)]
        observation: AppServerObservationArgs,
    }
    let pin = "a".repeat(/*n*/ 64);
    let base = vec![
        "test",
        "--observation-profile",
        "harmony-gpt-oss",
        "--sense-pilot-launch-fd",
        "3",
        "--sense-pilot-credential-fd",
        "4",
        "--sense-pilot-prepared-fd",
        "5",
        "--sense-pilot-prepared-sha256",
        &pin,
    ];
    let extension = vec![
        "--sense-pilot-count-scope-fd",
        "14",
        "--sense-pilot-count-scope-sha256",
        &pin,
        "--sense-pilot-count-semantics-fd",
        "15",
        "--sense-pilot-count-semantics-sha256",
        &pin,
        "--sense-pilot-count-ledger-fd",
        "16",
    ];
    let mut complete = base.clone();
    complete.extend(extension);
    let parsed = Args::try_parse_from(&complete).unwrap();
    assert!(!parsed.remote_control);
    assert!(parsed.observation.sense_pilot_count_scope_fd.is_some());
    assert!(
        Args::try_parse_from(&base)
            .unwrap()
            .observation
            .sense_pilot_count_scope_fd
            .is_none()
    );
    for index in 0..5 {
        let mut missing = complete.clone();
        missing.drain(base.len() + 2 * index..base.len() + 2 * index + 2);
        assert!(Args::try_parse_from(missing).is_err());
    }
    let mut wrong = complete.clone();
    wrong[base.len() + 9] = "6";
    assert!(Args::try_parse_from(wrong).is_err());
    let mut no_credential = complete;
    no_credential.drain(5..7);
    assert!(Args::try_parse_from(no_credential).is_err());
    // Parsing only: never call into_startup or touch the author's inherited FDs.
}

#[tokio::test]
async fn connection_revocation_prevents_late_install_admission() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let admission = ObservationStartup {
        profile: ObservationProfile::HarmonyGptOss,
        resume_thread: None,
        pilot: None,
    }
    .acquire(home.path(), &AppServerTransport::Stdio)?;
    let manager = crate::thread_state::ThreadStateManager::new();
    let connection = crate::outgoing_message::ConnectionId(42);
    manager
        .connection_initialized(
            connection,
            crate::thread_state::ConnectionCapabilities {
                observation: Some(Arc::clone(&admission)),
                ..Default::default()
            },
        )
        .await;
    assert!(manager.observation_admission(connection).await.is_ok());
    manager.revoke_observations(connection).await;
    assert!(manager.observation_admission(connection).await.is_err());
    Ok(())
}

#[test]
fn startup_lock_and_actual_origin_fence_admission() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    let thread_id = ThreadId::new();
    let selection = ObservationStartup {
        profile: ObservationProfile::HarmonyGptOss,
        resume_thread: Some(thread_id),
        pilot: None,
    };
    assert!(
        selection
            .clone()
            .acquire(home.path(), &AppServerTransport::Off)
            .is_err()
    );
    let admission = selection
        .clone()
        .acquire(home.path(), &AppServerTransport::Stdio)?;
    for origin in [
        ConnectionOrigin::InProcess,
        ConnectionOrigin::WebSocket,
        ConnectionOrigin::RemoteControl,
    ] {
        assert!(admission.for_connection(origin).is_none());
    }
    assert!(admission.permits_resume(thread_id));
    assert!(!admission.permits_resume(ThreadId::new()));
    let connected = admission
        .for_connection(ConnectionOrigin::Stdio)
        .expect("owned pipe");
    drop(admission);
    assert!(
        selection
            .clone()
            .acquire(home.path(), &AppServerTransport::Stdio)
            .is_err()
    );
    drop(connected);
    selection.acquire(home.path(), &AppServerTransport::Stdio)?;
    Ok(())
}
