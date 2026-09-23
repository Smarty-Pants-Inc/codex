//! Append custody within the original PilotLedger, never a recoverable balance.
use super::PilotAuthorityError;
use serde::Serialize;
use std::fs::File;
use std::io::Write;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

/// Immutable identity received with the independently protected count scope.
/// Construction alone grants nothing; the original launch issuer must bind it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PilotLedgerIdentity {
    pub device: u64,
    pub inode: u64,
    pub uid: u32,
    pub gid: u32,
}

/// Owns only the original received child handle. No path, reopen or recovery API.
/// The trusted parent must exclude other writers and retain its original custody.
pub struct PilotCountJournal {
    identity: PilotLedgerIdentity,
    state: Mutex<JournalState>,
    failed: AtomicBool,
}

struct JournalState {
    file: File,
    bytes: u64,
    poisoned: bool,
}

impl PilotCountJournal {
    /// Validate FD16 against all five other held native inputs before any write.
    /// The receiver must also reject numeric FD collisions before taking ownership,
    /// and the parent must exclude its other original evidence/store handles.
    pub fn receive(
        file: File,
        expected: PilotLedgerIdentity,
        other_inputs: [&File; 5],
    ) -> Result<Self, PilotAuthorityError> {
        let mut identities = Vec::with_capacity(/*capacity*/ 6);
        for input in other_inputs.into_iter().chain(std::iter::once(&file)) {
            let identity = held_identity(input)?;
            if identities.contains(&(identity.device, identity.inode)) {
                return Err(PilotAuthorityError::Denied);
            }
            identities.push((identity.device, identity.inode));
        }
        validate(&file, expected, /*bytes*/ 0)?;
        Ok(Self {
            identity: expected,
            state: Mutex::new(JournalState {
                file,
                bytes: 0,
                poisoned: false,
            }),
            failed: AtomicBool::new(/*v*/ false),
        })
    }

    /// A lock-free failure latch: checking admission cannot wait for filesystem IO.
    pub(super) fn failed(&self) -> bool {
        self.failed.load(Ordering::Acquire) || self.state.is_poisoned()
    }

    /// Only the ledger's typed record can reach this writer. A failed or uncertain
    /// append permanently fences this handle, even if some bytes reached storage.
    pub(super) fn append(&self, record: &CountJournalRecord) -> Result<(), PilotAuthorityError> {
        let mut bytes = serde_json::to_vec(record).map_err(|_| PilotAuthorityError::Unavailable)?;
        bytes.push(b'\n');
        if bytes.len() > 4096 {
            return Err(PilotAuthorityError::Denied);
        }
        let mut state = self
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        if state.poisoned {
            return Err(PilotAuthorityError::Unavailable);
        }
        // Poison BEFORE any fallible write. No cancellation or partial tail can
        // turn a failed acknowledgement into reusable credit.
        state.poisoned = true;
        let result = (|| {
            validate(&state.file, self.identity, state.bytes)?;
            let expected = state
                .bytes
                .checked_add(bytes.len() as u64)
                .filter(|size| *size <= 16 * 4096)
                .ok_or(PilotAuthorityError::Exhausted)?;
            state
                .file
                .write_all(&bytes)
                .map_err(|_| PilotAuthorityError::Unavailable)?;
            state
                .file
                .sync_all()
                .map_err(|_| PilotAuthorityError::Unavailable)?;
            validate(&state.file, self.identity, expected)?;
            state.bytes = expected;
            state.poisoned = false;
            Ok(())
        })();
        if result.is_err() {
            self.failed.store(/*val*/ true, Ordering::Release);
        }
        result
    }
}

#[cfg(target_os = "linux")]
fn held_identity(file: &File) -> Result<PilotLedgerIdentity, PilotAuthorityError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file
        .metadata()
        .map_err(|_| PilotAuthorityError::Unavailable)?;
    if !metadata.is_file() {
        return Err(PilotAuthorityError::Denied);
    }
    Ok(PilotLedgerIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        uid: metadata.uid(),
        gid: metadata.gid(),
    })
}

#[cfg(target_os = "linux")]
fn validate(
    file: &File,
    expected: PilotLedgerIdentity,
    bytes: u64,
) -> Result<(), PilotAuthorityError> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    let metadata = file
        .metadata()
        .map_err(|_| PilotAuthorityError::Unavailable)?;
    // Ownership is already held by File. These queries neither duplicate nor close it.
    let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
    let descriptor_flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
    if held_identity(file)? != expected
        || metadata.nlink() != 1
        || metadata.mode() & 0o7777 != 0o600
        || metadata.len() != bytes
        || flags < 0
        || descriptor_flags < 0
        || flags & libc::O_APPEND == 0
        || flags & libc::O_PATH != 0
        || !matches!(flags & libc::O_ACCMODE, libc::O_WRONLY | libc::O_RDWR)
        || descriptor_flags & libc::FD_CLOEXEC == 0
    {
        return Err(PilotAuthorityError::Denied);
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn held_identity(_file: &File) -> Result<PilotLedgerIdentity, PilotAuthorityError> {
    Err(PilotAuthorityError::Unavailable)
}

#[cfg(not(target_os = "linux"))]
fn validate(
    _file: &File,
    _expected: PilotLedgerIdentity,
    _bytes: u64,
) -> Result<(), PilotAuthorityError> {
    Err(PilotAuthorityError::Unavailable)
}

// No Deserialize: the journal cannot restore an allocation or create a receipt.
#[derive(Serialize)]
#[serde(untagged)]
pub(super) enum CountJournalRecord {
    Debit(CountDebit),
    Complete(CountComplete),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CountDebit {
    pub version: u8,
    pub kind: &'static str,
    pub allocation_id: String,
    pub instruction: String,
    pub decision_sha256: String,
    pub owner_connection_id: String,
    pub owner_epoch: String,
    pub thread_id: String,
    pub grant_id: String,
    pub issuer_generation: String,
    pub credential_generation: String,
    pub operation: u8,
    pub decision_id: String,
    pub attempt_id: String,
    pub request_id: String,
    pub inference_sha256: String,
    pub count_sha256: String,
    pub semantics_sha256: String,
    pub wire_model: String,
    pub requested_output: String,
    pub token_ceiling: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct CountComplete {
    pub version: u8,
    pub kind: &'static str,
    pub operation: u8,
    pub decision_id: String,
    pub attempt_id: String,
    pub request_id: String,
    pub input_tokens: String,
    pub response_sha256: String,
}

#[cfg(all(test, target_os = "linux"))]
#[path = "observation_pilot_journal_tests.rs"]
mod tests;
