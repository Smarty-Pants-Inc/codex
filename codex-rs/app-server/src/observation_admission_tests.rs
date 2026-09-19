use super::*;

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
