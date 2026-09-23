use super::*;
use pretty_assertions::assert_eq;
use serde_json::json;

fn fixture() -> (PilotLaunchDecision, Vec<u8>, String) {
    let mut policy = super::super::tests::fixture_decision();
    policy["version"] = json!(2);
    policy["provider"]["credential"] =
        json!({"generation":"fixture-generation", "referenceSha256":"a".repeat(64)});
    let raw = super::super::canonical::encode(&policy).unwrap();
    let digest = format!("{:x}", Sha256::digest(&raw));
    let policy: PilotLaunchDecision = serde_json::from_slice(&raw).unwrap();
    // The common helper envelope is NOT version2 sorted decision encoding.
    let raw = format!(
        "{{\"version\":1,\"deliveryId\":\"fixture-generation\",\"decisionDigest\":\"{digest}\",\"allocationId\":\"fixture-allocation\",\"provider\":\"fixture\",\"purpose\":\"fixture\",\"account\":\"fixture\",\"referenceSha256\":\"{}\",\"notBeforeMs\":0,\"expiresMs\":9007199254740000,\"credential\":{{\"type\":\"api_key\",\"key\":\"synthetic-test-only\"}}}}\n",
        "a".repeat(64)
    );
    (policy, raw.into_bytes(), digest)
}

#[test]
fn prepared_envelope_preserves_v1_and_rejects_foreign_bindings() -> anyhow::Result<()> {
    let (policy, raw, digest) = fixture();
    let envelope: Envelope = serde_json::from_slice(&raw)?;
    CredentialInput::validate_envelope(&envelope, &policy, &digest)?;
    let original: serde_json::Value = serde_json::from_slice(&raw)?;
    for pointer in [
        "/deliveryId",
        "/decisionDigest",
        "/allocationId",
        "/provider",
        "/purpose",
        "/account",
        "/referenceSha256",
    ] {
        let mut changed = original.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("foreign");
        let envelope = serde_json::from_value(changed)?;
        assert!(
            CredentialInput::validate_envelope(&envelope, &policy, &digest).is_err(),
            "{pointer}"
        );
    }
    let mut changed = original;
    changed["expiresMs"] = json!(9007199254739999_u64);
    assert!(
        CredentialInput::validate_envelope(&serde_json::from_value(changed)?, &policy, &digest)
            .is_err()
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn original_tmpfs_input_receipt_and_private_request_provenance() -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let directory = tempfile::tempdir_in("/dev/shm")?;
    let (mut policy, raw, digest) = fixture();
    let path = directory.path().join("credential.json");
    std::fs::write(&path, raw)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o400))?;
    let input = File::open(&path)?;
    let id = identity(&input)?;
    policy.target.uid = id.uid;
    let prepared = json!({"version":1,"deliveryId":"fixture-generation","decisionDigest":digest,
    "claimSha256":"b".repeat(64),"inputIdentity": {
        "device":id.device,"inode":id.inode,"uid":id.uid,"mode":id.mode,"links":id.links,
        "bytes":id.bytes,"mtimeNs":id.mtime_ns,"ctimeNs":id.ctime_ns
    }});
    let receipt_path = directory.path().join("prepared.json");
    let receipt_bytes = serde_json::to_vec(&prepared)?;
    std::fs::write(&receipt_path, &receipt_bytes)?;
    std::fs::set_permissions(&receipt_path, std::fs::Permissions::from_mode(0o400))?;
    let pin = format!("{:x}", Sha256::digest(&receipt_bytes));
    let credential =
        CredentialInput::receive(&policy, &digest, input, File::open(&receipt_path)?, &pin)?;
    let mut request = Request::new(axum::http::Method::POST, policy.provider.endpoint.clone())
        .with_json(&json!({"input":[]}));
    credential.attach(&mut request, &policy.provider.endpoint)?;
    assert!(credential.matches(&request));
    assert!(request.headers["authorization"].is_sensitive());
    let mut copied_headers =
        Request::new(axum::http::Method::POST, policy.provider.endpoint.clone());
    copied_headers.headers = request.headers.clone();
    assert!(!credential.matches(&copied_headers));
    request.headers.remove("authorization");
    assert!(!credential.matches(&request));
    let mut changed = prepared;
    changed["inputIdentity"]["mtimeNs"] = json!(id.mtime_ns + 1);
    let changed_bytes = serde_json::to_vec(&changed)?;
    let changed_path = directory.path().join("wrong.json");
    std::fs::write(&changed_path, &changed_bytes)?;
    std::fs::set_permissions(&changed_path, std::fs::Permissions::from_mode(0o400))?;
    let changed_pin = format!("{:x}", Sha256::digest(&changed_bytes));
    assert!(
        CredentialInput::receive(
            &policy,
            &digest,
            File::open(&path)?,
            File::open(&changed_path)?,
            &changed_pin
        )
        .is_err()
    );
    assert!(
        CredentialInput::receive(
            &policy,
            &digest,
            File::open(&path)?,
            File::open(&receipt_path)?,
            &"0".repeat(64)
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&receipt_path)?, receipt_bytes);
    Ok(())
}
