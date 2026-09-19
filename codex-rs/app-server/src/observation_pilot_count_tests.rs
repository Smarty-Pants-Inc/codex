//! Synthetic held files only. These tests do not qualify a native provider.
use super::super::LaunchState;
use super::super::PilotStartup;
use super::super::ReceivedLaunch;
use super::super::canonical;
use super::super::credential;
use super::super::wall_ms;
use super::*;
use codex_core::ObservationOwner;
use codex_core::PilotAuthorityError;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_http_client::Request;
use codex_protocol::ThreadId;
use pretty_assertions::assert_eq;
use serde_json::Value;
use serde_json::json;
use std::fs::OpenOptions;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use std::time::Instant;
use uuid::Uuid;

fn readonly(path: &Path, bytes: &[u8]) -> anyhow::Result<File> {
    std::fs::write(path, bytes)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(/*mode*/ 0o400))?;
    Ok(File::open(path)?)
}

struct Fixture {
    _directory: tempfile::TempDir,
    policy: PilotLaunchDecision,
    digest: String,
    originals: [File; 3],
    held: HeldCount,
    scope: Value,
    semantics: Value,
}
impl Fixture {
    fn new() -> anyhow::Result<Self> {
        let directory = tempfile::tempdir_in("/dev/shm")?;
        let ledger_path = directory.path().join("ledger");
        std::fs::write(&ledger_path, [])?;
        std::fs::set_permissions(
            &ledger_path,
            std::fs::Permissions::from_mode(/*mode*/ 0o600),
        )?;
        let ledger = OpenOptions::new().append(true).open(&ledger_path)?;
        let stat = ledger.metadata()?;
        let mut policy = super::super::tests::fixture_decision();
        policy["version"] = json!(2);
        policy["target"]["uid"] = json!(stat.uid());
        policy["target"]["gid"] = json!(stat.gid());
        policy["limits"]["notBeforeMs"] = json!(wall_ms()? - 10_000);
        policy["limits"]["expiresMs"] = json!(wall_ms()? + 60_000);
        policy["provider"]["credential"] =
            json!({"generation":"fixture-generation","referenceSha256":"a".repeat(/*n*/ 64)});
        policy["evidence"]["context"] = json!(format!("sha256:{}", "c".repeat(/*n*/ 64)));
        let policy_bytes = canonical::encode(&policy)?;
        let digest = format!("{:x}", Sha256::digest(&policy_bytes));
        let decision = readonly(&directory.path().join("decision"), &policy_bytes)?;
        let envelope = json!({"version":1,"deliveryId":"fixture-generation","decisionDigest":digest,
            "allocationId":policy["allocationId"],"provider":"fixture","purpose":"fixture","account":"fixture",
            "referenceSha256":"a".repeat(/*n*/ 64),"notBeforeMs":policy["limits"]["notBeforeMs"],"expiresMs":policy["limits"]["expiresMs"],
            "credential":{"type":"api_key","key":"synthetic-count-test-only"}});
        let input = readonly(
            &directory.path().join("credential"),
            &serde_json::to_vec(&envelope)?,
        )?;
        let id = input.metadata()?;
        let prepared = json!({"version":1,"deliveryId":"fixture-generation","decisionDigest":digest,"claimSha256":"b".repeat(/*n*/ 64),
            "inputIdentity":{"device":id.dev(),"inode":id.ino(),"uid":id.uid(),"mode":0o400,"links":1,"bytes":id.len(),
            "mtimeNs":id.mtime() as u64*1_000_000_000+id.mtime_nsec() as u64,"ctimeNs":id.ctime() as u64*1_000_000_000+id.ctime_nsec() as u64}});
        let receipt = readonly(
            &directory.path().join("prepared"),
            &serde_json::to_vec(&prepared)?,
        )?;
        let semantics = json!({"version":1,"kind":"codex-pilot-count-semantics","contract":"codex-responses-static-input-v1",
            "application":policy["source"],"serializer":{"revision":1,"sourceSha256":schema::SERIALIZER_SHA256},
            "provider":"fixture","wireModel":"fixture","inferenceUrl":policy["provider"]["endpoint"],"countUrl":"https://fixture.invalid/v1/responses/input_tokens",
            "shapes":{"items":["compaction","custom_tool_call","custom_tool_call_output","function_call","function_call_output","message","reasoning"],
              "tools":["custom","function"],"images":"inlineDataOnly","clientMetadata":"unsupported","streamOptions":"unsupported","accessPrograms":"unsupported"},
            "limits":{"contextTokens":1000,"outputTokens":100},
            "qualification":{"status":"qualified","evidence":{"issuer":"ci-delivery","identity":policy["evidence"]["context"],"sha256":"c".repeat(/*n*/ 64)}}});
        let semantics_bytes = canonical::encode(&semantics)?;
        let semantics_pin = format!("{:x}", Sha256::digest(&semantics_bytes));
        let scope = json!({"version":1,"kind":"codex-pilot-count-scope","decisionSha256":digest,
            "allocationId":policy["allocationId"],"instruction":policy["instruction"],"notBeforeMs":policy["limits"]["notBeforeMs"],"expiresMs":policy["limits"]["expiresMs"],"credentialGeneration":"fixture-generation",
            "count":{"url":semantics["countUrl"],"method":"POST","operations":2,"provider":"fixture","wireModel":"fixture","purpose":"fixture","account":"fixture","semanticsSha256":semantics_pin},
            "ledger":{"device":stat.dev().to_string(),"inode":stat.ino().to_string(),"uid":stat.uid(),"gid":stat.gid(),"mode":384,"links":1,"initialBytes":0,"access":"append","recovery":"none"}});
        let scope_bytes = canonical::encode(&scope)?;
        let held = HeldCount {
            scope: readonly(&directory.path().join("scope"), &scope_bytes)?,
            semantics: readonly(&directory.path().join("semantics"), &semantics_bytes)?,
            ledger,
            scope_pin: format!("{:x}", Sha256::digest(&scope_bytes)),
            semantics_pin,
        };
        Ok(Self {
            _directory: directory,
            policy: serde_json::from_value(policy)?,
            digest,
            originals: [decision, input, receipt],
            held,
            scope,
            semantics,
        })
    }
}

#[test]
fn semantics_scope_and_original_evidence_bindings_refuse_unknown_or_mutated_shapes()
-> anyhow::Result<()> {
    let f = Fixture::new()?;
    let scope: schema::Scope = serde_json::from_value(f.scope.clone())?;
    scope.validate(&f.policy, &f.digest, &f.held.semantics_pin)?;
    let semantics: schema::Semantics = serde_json::from_value(f.semantics.clone())?;
    semantics.validate(&f.policy, &scope)?;
    for (pointer, value) in [
        ("/application/tree", json!("f".repeat(/*n*/ 40))),
        ("/serializer/sourceSha256", json!("f".repeat(/*n*/ 64))),
        ("/wireModel", json!("alias-is-not-authority")),
        ("/countUrl", json!("https://foreign.invalid/count")),
        ("/shapes/clientMetadata", json!("ignored")),
        ("/limits/outputTokens", json!(1001)),
        ("/qualification/evidence/identity", json!("foreign")),
        (
            "/qualification/evidence/sha256",
            json!("f".repeat(/*n*/ 64)),
        ),
    ] {
        let mut changed = f.semantics.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            serde_json::from_value::<schema::Semantics>(changed)?
                .validate(&f.policy, &scope)
                .is_err(),
            "{pointer}"
        );
    }
    for status in ["unknown", "unsupported"] {
        let mut changed = f.semantics.clone();
        changed["qualification"] = json!({"status":status});
        let error = serde_json::from_value::<schema::Semantics>(changed)?
            .validate(&f.policy, &scope)
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            format!("native count semantics {status}")
        );
    }
    for (pointer, value) in [
        ("/credentialGeneration", json!("foreign")),
        ("/decisionSha256", json!("f".repeat(/*n*/ 64))),
        (
            "/count/url",
            json!("https://fixture.invalid/count?secret=not-admitted"),
        ),
        ("/ledger/device", json!("01")),
        ("/ledger/mode", json!(0o644)),
        ("/ledger/recovery", json!("resume")),
    ] {
        let mut changed = f.scope.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            serde_json::from_value::<schema::Scope>(changed)?
                .validate(&f.policy, &f.digest, &f.held.semantics_pin)
                .is_err(),
            "{pointer}"
        );
    }
    let mut unbound = f.semantics.clone();
    unbound["qualification"] = json!({"status":"qualified"});
    assert!(serde_json::from_value::<schema::Semantics>(unbound).is_err());
    Ok(())
}

#[test]
fn protected_files_refuse_alias_before_new_read_and_mismatched_pin_without_ledger_io()
-> anyhow::Result<()> {
    let mut artifact = Fixture::new()?;
    artifact.policy.target.cwd = std::env::current_dir()?.to_string_lossy().into_owned();
    artifact.held.scope = File::open("/proc/self/exe")?;
    let reached = std::cell::Cell::new(/*value*/ false);
    let result = super::super::validation::validate_native_target(&artifact.policy, |binary| {
        reached.set(/*val*/ true);
        artifact.held.reject_aliases([
            &artifact.originals[0],
            &artifact.originals[1],
            &artifact.originals[2],
            binary,
        ])
    });
    assert!(
        reached.get(),
        "the held artifact exclusion must precede its mismatched hash"
    );
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    let mut f = Fixture::new()?;
    // O_WRONLY scope would fail read_at with EBADF. Its alias must instead fail
    // before that read, even though it is regular and shares the right mode.
    let path = f._directory.path().join("write-only");
    std::fs::write(&path, b"not JSON")?;
    let write_only = OpenOptions::new().write(true).open(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(/*mode*/ 0o400))?;
    f.held.scope = write_only.try_clone()?;
    f.originals[0] = write_only;
    let error = f
        .held
        .qualify(
            &f.policy,
            &f.digest,
            [&f.originals[0], &f.originals[1], &f.originals[2]],
        )
        .err()
        .unwrap();
    assert_eq!(
        (error.kind(), error.to_string()),
        (
            io::ErrorKind::PermissionDenied,
            "pilot launch custody rejected".to_owned()
        )
    );
    let mut f = Fixture::new()?;
    let ledger = f.held.ledger.try_clone()?;
    f.held.scope_pin = "0".repeat(/*n*/ 64);
    assert!(
        f.held
            .qualify(
                &f.policy,
                &f.digest,
                [&f.originals[0], &f.originals[1], &f.originals[2]]
            )
            .is_err()
    );
    assert_eq!(ledger.metadata()?.len(), 0);
    Ok(())
}

#[test]
fn original_launch_transfers_one_journal_and_authenticates_only_original_count_route()
-> anyhow::Result<()> {
    let f = Fixture::new()?;
    let received = f.held.qualify(
        &f.policy,
        &f.digest,
        [&f.originals[0], &f.originals[1], &f.originals[2]],
    )?;
    let [decision, input, receipt] = f.originals;
    let pin = format!(
        "{:x}",
        Sha256::digest(std::fs::read(f._directory.path().join("prepared"))?)
    );
    let credential =
        credential::CredentialInput::receive(&f.policy, &f.digest, input, receipt, &pin)?;
    let launch = PilotStartup(Arc::new(ReceivedLaunch {
        decision: f.policy,
        digest: f.digest,
        _descriptor: decision,
        credential: Mutex::new(Some(credential)),
        count: Some(received),
        state: Mutex::new(LaunchState {
            generation: 1,
            bound: false,
            revoked: false,
            last_wall_ms: wall_ms()?,
            deadline: Instant::now() + Duration::from_secs(/*secs*/ 60),
        }),
    }));
    let owner = ObservationOwner {
        connection_id: 17,
        epoch: Uuid::now_v7(),
    };
    let factory = HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy);
    let (selector, issuer) = launch.bind(owner, ThreadId::new(), factory.clone())?;
    let claims = issuer.verify_grant(&selector)?;
    let journal = issuer.take_count_journal(&claims)?.unwrap();
    assert!(matches!(
        issuer.take_count_journal(&claims),
        Err(PilotAuthorityError::Replay)
    ));
    let scope = issuer.count_scope(&claims)?.unwrap();
    assert_eq!(
        issuer
            .count_transport_factory(&claims)?
            .outbound_proxy_policy(),
        factory.outbound_proxy_policy()
    );
    let mut count = Request::new(axum::http::Method::POST, scope.count_url.clone())
        .with_json(&json!({"model":"fixture","input":[]}));
    issuer.authenticate_count_request(&claims, &mut count)?;
    issuer.authenticate_count_request(&claims, &mut count)?; // exact original send recheck, no second credential delivery
    let mut copied = Request::new(axum::http::Method::POST, scope.count_url.clone())
        .with_json(&json!({"input":[]}));
    copied.headers = count.headers.clone();
    assert!(
        issuer
            .authenticate_count_request(&claims, &mut copied)
            .is_err()
    );
    let mut redirected = count.clone();
    redirected.url = "https://foreign.invalid/count".to_owned();
    assert!(
        issuer
            .authenticate_count_request(&claims, &mut redirected)
            .is_err()
    );
    let mut inference = Request::new(axum::http::Method::POST, scope.inference_url.clone())
        .with_json(&json!({"input":[]}));
    issuer.authenticate_request(&claims, &mut inference)?;
    issuer.validate_count_inference(&claims, &inference)?;
    assert!(issuer.validate_count_inference(&claims, &count).is_err());
    launch.revoke();
    assert!(
        issuer
            .authenticate_count_request(&claims, &mut count)
            .is_err()
    );
    assert!(issuer.count_scope(&claims).is_err());
    drop(journal); // no write or network; qualified fixture is not provider evidence
    assert_eq!(
        std::fs::metadata(f._directory.path().join("ledger"))?.len(),
        0
    );
    Ok(())
}
