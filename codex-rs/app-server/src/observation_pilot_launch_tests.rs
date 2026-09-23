use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

pub(super) fn fixture_decision() -> serde_json::Value {
    json!({
        "version": 1, "instruction": "fixture-once", "allocationId": "fixture-allocation",
        "source": {"commit": "1".repeat(40), "tree": "2".repeat(40),
            "binarySha256": "3".repeat(64), "closureSha256": "4".repeat(64)},
        "target": {"machineId": "fixture-host", "uid": 1000, "gid": 1000,
            "cwd": "/fixture/work", "codexHome": "/fixture/home"},
        "operation": "codex-pilot", "scope": "fixture-source",
        "roots": {
            "W": {"path": "/fixture/work", "device": 1, "inode": 1, "access": "read-write"},
            "R": {"path": "/fixture/read", "device": 1, "inode": 2, "access": "read-only"},
            "D": {"path": "/fixture/data", "device": 1, "inode": 3, "access": "read-write"},
            "T": {"path": "/fixture/temp", "device": 1, "inode": 4, "access": "read-write"}
        },
        "permissions": ["prepareSource", "automaticTurn"],
        "limits": {"attempts": 2, "turns": 1, "reservedTokens": 1000,
            "notBeforeMs": 0, "expiresMs": 9_007_199_254_740_000_u64, "cooldownMs": 1},
        "provider": {"provider": "fixture", "model": "fixture", "api": "responses",
            "endpoint": "https://fixture.invalid/v1/responses", "purpose": "fixture", "account": "fixture"},
        "evidence": {"credential": "fixture-reference-only", "context": "fixture-reference-only"}
    })
}

fn fixture_launch() -> anyhow::Result<PilotStartup> {
    let decision = serde_json::from_value(fixture_decision())?;
    validation::validate(&decision)?;
    Ok(PilotStartup(Arc::new(ReceivedLaunch {
        decision,
        digest: "fixture-selector-not-authority".to_owned(),
        credential: Mutex::new(None),
        count: None,
        _descriptor: tempfile::tempfile()?,
        state: Mutex::new(LaunchState {
            generation: 1,
            bound: false,
            revoked: false,
            last_wall_ms: wall_ms()?,
            deadline: Instant::now() + Duration::from_secs(60),
        }),
    })))
}

#[test]
fn original_binding_is_one_use_and_revocation_fences_issuer_generation() -> anyhow::Result<()> {
    let launch = fixture_launch()?;
    let owner = ObservationOwner {
        connection_id: 17,
        epoch: Uuid::now_v7(),
    };
    let thread = ThreadId::new();
    let factory = codex_http_client::HttpClientFactory::new(
        codex_http_client::OutboundProxyPolicy::ReqwestDefault,
    );
    let (selector, issuer) = launch.bind(owner, thread, factory.clone())?;
    assert!(matches!(
        launch.bind(owner, thread, factory.clone()),
        Err(PilotAuthorityError::Replay)
    ));
    let claims = issuer.verify_grant(&selector)?;
    assert!(issuer.count_scope(&claims)?.is_none());
    assert!(issuer.take_count_journal(&claims)?.is_none());
    assert_eq!(
        (claims.owner, claims.thread_id, claims.issuer_generation),
        (owner, thread, 1)
    );
    assert!(matches!(
        issuer.verify_grant(&selector),
        Err(PilotAuthorityError::Replay)
    ));
    issuer.revoke();
    assert_eq!(
        issuer.recheck_grant(&claims),
        Err(PilotAuthorityError::Expired)
    );
    assert_eq!(launch.0.state.lock().unwrap().generation, 2);
    assert!(matches!(
        launch.bind(owner, ThreadId::new(), factory),
        Err(PilotAuthorityError::Expired)
    ));
    Ok(())
}

#[test]
fn expiry_and_generation_mismatch_latch_even_if_state_changes_back() -> anyhow::Result<()> {
    let launch = fixture_launch()?;
    launch.0.state.lock().unwrap().generation = 2;
    assert_eq!(
        launch.recheck(/*generation*/ 1),
        Err(PilotAuthorityError::Expired)
    );
    launch.0.state.lock().unwrap().generation = 1;
    assert_eq!(
        launch.recheck(/*generation*/ 1),
        Err(PilotAuthorityError::Expired)
    );
    let expired = fixture_launch()?;
    expired.0.state.lock().unwrap().deadline = Instant::now();
    assert_eq!(
        expired.recheck(/*generation*/ 1),
        Err(PilotAuthorityError::Expired)
    );
    expired.0.state.lock().unwrap().deadline = Instant::now() + Duration::from_secs(60);
    assert_eq!(
        expired.recheck(/*generation*/ 1),
        Err(PilotAuthorityError::Expired)
    );
    Ok(())
}

#[test]
fn receiver_rejects_duplicate_fields_and_inapplicable_limits() -> anyhow::Result<()> {
    let bytes = serde_json::to_string(&fixture_decision())?;
    let duplicate = bytes.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    assert!(serde_json::from_str::<PilotLaunchDecision>(&duplicate).is_err());
    for (pointer, replacement) in [
        ("/limits/reservedTokens", json!(0)),
        ("/permissions", json!(["automaticTurn", "automaticTurn"])),
        ("/limits/reservedTokens", json!(9_007_199_254_740_992_u64)),
        (
            "/provider/endpoint",
            json!("https://fixture:fixture@fixture.invalid/"),
        ),
    ] {
        let mut value = fixture_decision();
        *value.pointer_mut(pointer).unwrap() = replacement;
        let policy = serde_json::from_value(value)?;
        assert!(validation::validate(&policy).is_err(), "{pointer}");
    }
    Ok(())
}
