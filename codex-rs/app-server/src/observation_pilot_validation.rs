use super::denied;
use crate::observation_pilot_decision::Permission;
use crate::observation_pilot_decision::PilotLaunchDecision;
use std::collections::BTreeSet;
use std::io;
use std::path::Path;

const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

pub(super) fn validate(policy: &PilotLaunchDecision) -> io::Result<()> {
    let text = |value: &str| {
        !value.is_empty()
            && value.encode_utf16().count() <= 512
            && !value.bytes().any(|byte| byte <= 0x20 || byte == 0x7f)
    };
    let digest = |value: &str, size: usize| {
        value.len() == size
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    };
    let source = &policy.source;
    let provider = &policy.provider;
    let limits = &policy.limits;
    let credential_version_matches = match (policy.version, &provider.credential) {
        (1, None) => true,
        (2, Some(credential)) => {
            !credential.generation.is_empty()
                && credential.generation.len() <= 120
                && credential.generation.as_bytes()[0].is_ascii_alphanumeric()
                && credential
                    .generation
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
                && digest(&credential.reference_sha256, /*size*/ 64)
        }
        _ => false,
    };
    if !credential_version_matches
        || policy.operation != "codex-pilot"
        || ![
            &policy.instruction,
            &policy.allocation_id,
            &policy.scope,
            &policy.target.machine_id,
            &policy.target.cwd,
            &policy.target.codex_home,
            &provider.provider,
            &provider.model,
            &provider.api,
            &provider.endpoint,
            &provider.purpose,
            &provider.account,
            &policy.evidence.credential,
            &policy.evidence.context,
        ]
        .into_iter()
        .all(|value| text(value))
        || !digest(&source.commit, /*size*/ 40)
        || !digest(&source.tree, /*size*/ 40)
        || !digest(&source.binary_sha256, /*size*/ 64)
        || !digest(&source.closure_sha256, /*size*/ 64)
        || policy.permissions.is_empty()
        || policy.permissions.len() > 5
        || policy.permissions.iter().collect::<BTreeSet<_>>().len() != policy.permissions.len()
        || limits.attempts == 0
        || limits.attempts > 256
        || limits.turns > limits.attempts
        || limits.reserved_tokens == 0
        || limits.expires_ms <= limits.not_before_ms
        || [
            limits.reserved_tokens,
            limits.not_before_ms,
            limits.expires_ms,
            limits.cooldown_ms,
        ]
        .into_iter()
        .any(|value| value > MAX_SAFE_INTEGER)
        || (policy.permissions.contains(&Permission::AutomaticTurn)
            && (limits.turns == 0 || limits.cooldown_ms == 0))
    {
        return Err(denied());
    }
    let endpoint = url::Url::parse(&provider.endpoint).map_err(|_| denied())?;
    if endpoint.scheme() != "https"
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(denied());
    }
    for root in [
        &policy.roots.write,
        &policy.roots.read,
        &policy.roots.data,
        &policy.roots.temporary,
    ] {
        if !text(&root.path)
            || !Path::new(&root.path).is_absolute()
            || root.device > MAX_SAFE_INTEGER
            || root.inode > MAX_SAFE_INTEGER
        {
            return Err(denied());
        }
    }
    Ok(())
}

#[cfg(target_os = "linux")]
pub(super) fn validate_native_target(
    policy: &PilotLaunchDecision,
    exclude_artifact: impl FnOnce(&std::fs::File) -> io::Result<()>,
) -> io::Result<()> {
    use sha2::Digest;
    use sha2::Sha256;
    use std::fs::File;
    use std::io::Read;
    use std::os::unix::fs::MetadataExt;

    let process = std::fs::metadata("/proc/self")?;
    if process.uid() != policy.target.uid
        || process.gid() != policy.target.gid
        || std::env::current_dir()? != Path::new(&policy.target.cwd)
    {
        return Err(denied());
    }
    // Open the executing image, not its replaceable pathname. The launcher joins
    // this independently pinned binary to the admitted source/closure and target.
    let mut binary = File::open("/proc/self/exe")?;
    // New protected count inputs must not alias the actual artifact reader.
    // The original no-count launch keeps its previous validation path.
    exclude_artifact(&binary)?;
    let mut digest = Sha256::new();
    let mut buffer = [0; 32_768];
    loop {
        let count = binary.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if format!("{:x}", digest.finalize()) != policy.source.binary_sha256 {
        return Err(denied());
    }
    for root in [
        &policy.roots.write,
        &policy.roots.read,
        &policy.roots.data,
        &policy.roots.temporary,
    ] {
        if std::fs::canonicalize(&root.path)? != Path::new(&root.path) {
            return Err(denied());
        }
        let metadata = std::fs::metadata(&root.path)?;
        if !metadata.is_dir() || metadata.dev() != root.device || metadata.ino() != root.inode {
            return Err(denied());
        }
        // The original launcher's mount policy owns access enforcement. Native
        // receiving does not turn a read-only root into a read-write grant.
        match root.access {
            crate::observation_pilot_decision::Access::ReadOnly
            | crate::observation_pilot_decision::Access::ReadWrite => {}
        }
    }
    Ok(())
}
