//! Original count descriptor custody. No path opening, ambient FD search or balance.
#[cfg(target_os = "linux")]
use super::denied;
#[cfg(target_os = "linux")]
use crate::observation_pilot_decision::PilotLaunchDecision;
use codex_core::PilotCountJournal;
use codex_core::PilotCountScope;
#[cfg(target_os = "linux")]
use codex_core::PilotLedgerIdentity;
#[cfg(target_os = "linux")]
use sha2::Digest;
#[cfg(target_os = "linux")]
use sha2::Sha256;
use std::fs::File;
#[cfg(target_os = "linux")]
use std::io;
#[cfg(target_os = "linux")]
use std::num::NonZeroU64;
use std::sync::Mutex;

#[cfg(target_os = "linux")]
#[path = "observation_pilot_count_schema.rs"]
mod schema;

#[cfg(target_os = "linux")]
pub(super) struct HeldCount {
    scope: File,
    semantics: File,
    ledger: File,
    scope_pin: String,
    semantics_pin: String,
}

pub(super) struct ReceivedCount {
    _scope: File,
    _semantics: File,
    pub scope: PilotCountScope,
    pub journal: Mutex<Option<PilotCountJournal>>,
    pub not_before_ms: u64,
    pub expires_ms: u64,
    pub deadline: std::time::Instant,
}

#[cfg(target_os = "linux")]
fn take(fd: i32) -> io::Result<File> {
    use std::os::fd::FromRawFd;
    if !matches!(fd, 14..=16) {
        return Err(denied());
    }
    // SAFETY: fcntl only inspects the fixed original descriptor. This runs once,
    // before other startup code; the original launcher transfers child ownership.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0
        || flags & libc::O_PATH != 0
        || (fd != 16 && flags & libc::O_ACCMODE != libc::O_RDONLY)
        || (fd == 16
            && (flags & libc::O_APPEND == 0
                || !matches!(flags & libc::O_ACCMODE, libc::O_WRONLY | libc::O_RDWR)))
    {
        return Err(denied());
    }
    // SAFETY: the private launch ABI transfers each fixed number exactly once.
    let file = unsafe { File::from_raw_fd(fd) };
    // Set CLOEXEC only at final native custody, AFTER dash/bwrap/payload execs.
    let descriptor_flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if descriptor_flags < 0
        || unsafe { libc::fcntl(fd, libc::F_SETFD, descriptor_flags | libc::FD_CLOEXEC) } < 0
    {
        return Err(denied());
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn read_pinned(file: &File, pin: &str, uid: u32) -> io::Result<Vec<u8>> {
    use std::os::unix::fs::FileExt;
    use std::os::unix::fs::MetadataExt;
    let before = file.metadata()?;
    if !before.is_file()
        || before.nlink() != 1
        || before.uid() != uid
        || before.mode() & 0o7777 != 0o400
        || before.len() == 0
        || before.len() > 65_536
    {
        return Err(denied());
    }
    let mut bytes = vec![0; before.len() as usize];
    file.read_exact_at(&mut bytes, /*offset*/ 0)?;
    let after = file.metadata()?;
    if (
        before.dev(),
        before.ino(),
        before.len(),
        before.mode(),
        before.uid(),
        before.gid(),
        before.nlink(),
        before.mtime(),
        before.mtime_nsec(),
        before.ctime(),
        before.ctime_nsec(),
    ) != (
        after.dev(),
        after.ino(),
        after.len(),
        after.mode(),
        after.uid(),
        after.gid(),
        after.nlink(),
        after.mtime(),
        after.mtime_nsec(),
        after.ctime(),
        after.ctime_nsec(),
    ) || format!("{:x}", Sha256::digest(&bytes)) != pin
    {
        return Err(denied());
    }
    super::canonical::validate(&bytes)?;
    Ok(bytes)
}

#[cfg(target_os = "linux")]
impl HeldCount {
    /// All six original input identities are excluded before ANY new count-file
    /// read/hash (or credential body read). Parent separately excludes owned
    /// receiving/once/evidence/store/executable handles; native does not scan FDs.
    #[cfg(target_os = "linux")]
    pub(super) fn receive(pins: (&str, &str), originals: [&File; 3]) -> io::Result<Self> {
        if !schema::digest(pins.0) || !schema::digest(pins.1) {
            return Err(denied());
        }
        let scope = take(/*fd*/ 14)?;
        let semantics = take(/*fd*/ 15)?;
        let ledger = take(/*fd*/ 16)?;
        let held = Self {
            scope,
            semantics,
            ledger,
            scope_pin: pins.0.to_owned(),
            semantics_pin: pins.1.to_owned(),
        };
        held.reject_aliases(originals)?;
        Ok(held)
    }

    #[cfg(target_os = "linux")]
    pub(super) fn reject_aliases<const N: usize>(&self, originals: [&File; N]) -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        let mut identities = Vec::with_capacity(N + 3);
        for file in originals
            .into_iter()
            .chain([&self.scope, &self.semantics, &self.ledger])
        {
            let stat = file.metadata()?;
            if !stat.is_file() || identities.contains(&(stat.dev(), stat.ino())) {
                return Err(denied());
            }
            identities.push((stat.dev(), stat.ino()));
        }
        Ok(())
    }

    /// Consume held inputs only after the existing original v2 launch validation.
    /// The caller still owns FD3/4/5 here, before CredentialInput takes4/5.
    #[cfg(target_os = "linux")]
    pub(super) fn qualify(
        self,
        policy: &PilotLaunchDecision,
        decision_digest: &str,
        originals: [&File; 3],
    ) -> io::Result<ReceivedCount> {
        self.reject_aliases(originals)?;
        let scope: schema::Scope = serde_json::from_slice(&read_pinned(
            &self.scope,
            &self.scope_pin,
            policy.target.uid,
        )?)
        .map_err(|_| denied())?;
        scope.validate(policy, decision_digest, &self.semantics_pin)?;
        let semantics: schema::Semantics = serde_json::from_slice(&read_pinned(
            &self.semantics,
            &self.semantics_pin,
            policy.target.uid,
        )?)
        .map_err(|_| denied())?;
        semantics.validate(policy, &scope)?;
        let received_at = std::time::Instant::now();
        let now = super::wall_ms()?;
        if now < scope.not_before_ms || now >= scope.expires_ms {
            return Err(denied());
        }
        let deadline = received_at
            .checked_add(std::time::Duration::from_millis(scope.expires_ms - now))
            .ok_or_else(denied)?;
        let journal = PilotCountJournal::receive(
            self.ledger,
            PilotLedgerIdentity {
                device: schema::decimal(&scope.ledger.device)?,
                inode: schema::decimal(&scope.ledger.inode)?,
                uid: scope.ledger.uid,
                gid: scope.ledger.gid,
            },
            [
                originals[0],
                originals[1],
                originals[2],
                &self.scope,
                &self.semantics,
            ],
        )
        .map_err(|_| denied())?;
        let native_scope = PilotCountScope {
            allocation_id: scope.allocation_id,
            instruction: scope.instruction,
            decision_sha256: scope.decision_sha256,
            credential_generation: scope.credential_generation,
            // A diagnostic identity attached only after original protected input
            // validation, never a substitute for CredentialInput's private marker.
            credential_receipt: uuid::Uuid::now_v7(),
            semantics_sha256: self.semantics_pin,
            wire_model: scope.count.wire_model,
            provider: scope.count.provider,
            purpose: scope.count.purpose,
            account: scope.count.account,
            inference_url: semantics.inference_url,
            count_url: scope.count.url,
            not_before: i64::try_from(scope.not_before_ms.div_ceil(/*rhs*/ 1000))
                .map_err(|_| denied())?,
            expires_at: i64::try_from(scope.expires_ms / 1000).map_err(|_| denied())?,
            operations: scope.count.operations,
            output_tokens: NonZeroU64::new(semantics.limits.output_tokens).ok_or_else(denied)?,
            context_tokens: NonZeroU64::new(semantics.limits.context_tokens).ok_or_else(denied)?,
        };
        if native_scope.not_before >= native_scope.expires_at {
            return Err(denied());
        }
        Ok(ReceivedCount {
            _scope: self.scope,
            _semantics: self.semantics,
            scope: native_scope,
            journal: Mutex::new(Some(journal)),
            not_before_ms: scope.not_before_ms,
            expires_ms: scope.expires_ms,
            deadline,
        })
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "observation_pilot_count_tests.rs"]
mod tests;
