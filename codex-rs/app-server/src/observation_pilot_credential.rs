//! Original held credential custody. Never serialize or debug the prepared key.
use super::denied;
use crate::observation_pilot_decision::PilotLaunchDecision;
use codex_http_client::Request;
use serde::Deserialize;
use sha2::Digest;
use sha2::Sha256;
use std::fs::File;
use std::io;
use std::sync::Arc;

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct InputIdentity {
    device: u64,
    inode: u64,
    uid: u32,
    mode: u32,
    links: u64,
    bytes: u64,
    mtime_ns: u64,
    ctime_ns: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Prepared {
    version: u32,
    delivery_id: String,
    decision_digest: String,
    claim_sha256: String,
    input_identity: InputIdentity,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    version: u32,
    delivery_id: String,
    decision_digest: String,
    allocation_id: String,
    provider: String,
    purpose: String,
    account: String,
    reference_sha256: String,
    not_before_ms: u64,
    expires_ms: u64,
    credential: ApiKey,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ApiKey {
    #[serde(rename = "type")]
    kind: String,
    key: String,
}

/// Private identity is minted only after this original descriptor was received.
/// Its Arc identity, not a public UUID or header label, accompanies the request.
#[derive(Clone)]
struct CredentialProvenance(Arc<()>);

pub(super) struct CredentialInput {
    _input: File,
    _receipt: File,
    envelope: Envelope,
    identity: Arc<()>,
}

#[cfg(target_os = "linux")]
fn identity(file: &File) -> io::Result<InputIdentity> {
    use std::os::unix::fs::MetadataExt;
    let stat = file.metadata()?;
    let ns = |seconds: i64, nanos: i64| {
        u64::try_from(seconds)
            .ok()
            .and_then(|seconds| seconds.checked_mul(1_000_000_000))
            .and_then(|seconds| {
                u64::try_from(nanos)
                    .ok()
                    .and_then(|nanos| seconds.checked_add(nanos))
            })
            .ok_or_else(denied)
    };
    if !stat.is_file() {
        return Err(denied());
    }
    Ok(InputIdentity {
        device: stat.dev(),
        inode: stat.ino(),
        uid: stat.uid(),
        mode: stat.mode() & 0o7777,
        links: stat.nlink(),
        bytes: stat.len(),
        mtime_ns: ns(stat.mtime(), stat.mtime_nsec())?,
        ctime_ns: ns(stat.ctime(), stat.ctime_nsec())?,
    })
}

/// Receives only a fixed original-launch descriptor; caller invokes this before
/// any child creation. Takes sole ownership and sets FD_CLOEXEC in place.
#[cfg(target_os = "linux")]
pub(super) fn receive_fd(fd: i32) -> io::Result<File> {
    use std::os::fd::FromRawFd;
    if !matches!(fd, 3..=5) {
        return Err(denied());
    }
    // SAFETY: fcntl validates this numeric descriptor without dereferencing memory.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || flags & libc::O_ACCMODE != libc::O_RDONLY {
        return Err(denied());
    }
    // SAFETY: fixed private launcher ABI transfers sole ownership once, before
    // other startup code may acquire these descriptors. The caller enforces once.
    let file = unsafe { File::from_raw_fd(fd) };
    // SAFETY: the owned descriptor is live; these fcntl operations take integers.
    let descriptor_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if descriptor_flags < 0
        || unsafe { libc::fcntl(fd, libc::F_SETFD, descriptor_flags | libc::FD_CLOEXEC) } < 0
    {
        return Err(denied());
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn read_held(file: &File) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;
    let before = identity(file)?;
    if before.bytes == 0 || before.bytes > 65_536 || before.links != 1 || before.mode & 0o222 != 0 {
        return Err(denied());
    }
    let mut bytes = vec![0; before.bytes as usize];
    file.read_exact_at(&mut bytes, /*offset*/ 0)?;
    if identity(file)? != before {
        return Err(denied());
    }
    Ok(bytes)
}

#[cfg(target_os = "linux")]
fn require_tmpfs(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: this owned file remains live and the correctly sized output is
    // initialized by fstatfs on success. Unlike mountinfo, this works for a held
    // descriptor whose original mount is outside the child's mount namespace.
    if unsafe { libc::fstatfs(file.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(denied());
    }
    // SAFETY: successful fstatfs initialized the whole output above.
    if unsafe { stat.assume_init() }.f_type != libc::TMPFS_MAGIC {
        return Err(denied());
    }
    Ok(())
}

impl CredentialInput {
    #[cfg(target_os = "linux")]
    pub(super) fn receive(
        policy: &PilotLaunchDecision,
        digest: &str,
        input: File,
        receipt: File,
        receipt_sha256: &str,
    ) -> io::Result<Self> {
        let receipt_bytes = read_held(&receipt)?;
        if format!("{:x}", Sha256::digest(&receipt_bytes)) != receipt_sha256 {
            return Err(denied());
        }
        let prepared: Prepared = serde_json::from_slice(&receipt_bytes).map_err(|_| denied())?;
        let credential = policy.provider.credential.as_ref().ok_or_else(denied)?;
        if prepared.version != 1
            || prepared.decision_digest != digest
            || prepared.delivery_id != credential.generation
            || prepared.claim_sha256.len() != 64
            || !prepared
                .claim_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || identity(&input)? != prepared.input_identity
            || prepared.input_identity.uid != policy.target.uid
            || prepared.input_identity.mode != 0o400
        {
            return Err(denied());
        }
        require_tmpfs(&input)?;
        let mut bytes = read_held(&input)?;
        let parsed = serde_json::from_slice::<Envelope>(&bytes);
        bytes.fill(0);
        // Never propagate serde's error, which could contain input key material.
        let envelope = parsed.map_err(|_| denied())?;
        if identity(&input)? != prepared.input_identity {
            return Err(denied());
        }
        Self::validate_envelope(&envelope, policy, digest)?;
        Ok(Self {
            _input: input,
            _receipt: receipt,
            envelope,
            identity: Arc::new(()),
        })
    }

    fn validate_envelope(
        envelope: &Envelope,
        policy: &PilotLaunchDecision,
        digest: &str,
    ) -> io::Result<()> {
        let expected = policy.provider.credential.as_ref().ok_or_else(denied)?;
        if policy.version != 2
            || envelope.version != 1
            || envelope.delivery_id != expected.generation
            || envelope.decision_digest != digest
            || envelope.allocation_id != policy.allocation_id
            || envelope.provider != policy.provider.provider
            || envelope.purpose != policy.provider.purpose
            || envelope.account != policy.provider.account
            || envelope.reference_sha256 != expected.reference_sha256
            || envelope.not_before_ms != policy.limits.not_before_ms
            || envelope.expires_ms != policy.limits.expires_ms
            || envelope.credential.kind != "api_key"
            || envelope.credential.key.is_empty()
            || envelope.credential.key.len() > 8192
            || !envelope
                .credential
                .key
                .bytes()
                .all(|byte| (0x21..=0x7e).contains(&byte))
        {
            return Err(denied());
        }
        Ok(())
    }

    /// Caller holds issuer generation/lifetime lock, after native owner binding.
    /// Return no key or header to another caller; attach only to this final request.
    pub(super) fn attach(&self, request: &mut Request, endpoint: &str) -> io::Result<()> {
        if request.url != endpoint
            || request.method.as_str() != "POST"
            || request.body.is_none()
            || [
                "authorization",
                "proxy-authorization",
                "api-key",
                "x-api-key",
                "cookie",
            ]
            .iter()
            .any(|header| request.headers.contains_key(*header))
        {
            return Err(denied());
        }
        *request = request.clone().into_prepared().map_err(|_| denied())?;
        let mut header: axum::http::HeaderValue =
            format!("Bearer {}", self.envelope.credential.key)
                .parse()
                .map_err(|_| denied())?;
        // The http header implementation suppresses the value in Debug output.
        request.headers.insert("authorization", {
            header.set_sensitive(true);
            header
        });
        request
            .extensions
            .insert(CredentialProvenance(Arc::clone(&self.identity)));
        Ok(())
    }

    pub(super) fn matches(&self, request: &Request) -> bool {
        request
            .extensions
            .get::<CredentialProvenance>()
            .is_some_and(|value| Arc::ptr_eq(&value.0, &self.identity))
            && request.headers.get("authorization").is_some_and(|header| {
                header.as_bytes().strip_prefix(b"Bearer ")
                    == Some(self.envelope.credential.key.as_bytes())
            })
    }
}

#[cfg(test)]
#[path = "observation_pilot_credential_tests.rs"]
mod tests;
