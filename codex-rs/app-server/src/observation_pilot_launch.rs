//! Original-process receiver for Foundation's independently received once instruction.
//! The inherited descriptor is a trusted-launch boundary, not a bearer capability.

use crate::observation_pilot_decision::PilotLaunchDecision;
use codex_core::NativePilotIssuer;
use codex_core::ObservationOwner;
use codex_core::PilotAuthorityError;
use codex_core::PilotGrantClaims;
use codex_core::PilotPermission;
use codex_core::PilotRequestReservation;
use codex_http_client::Request;
use codex_protocol::ThreadId;
use sha2::Digest;
use sha2::Sha256;
use std::fmt;
use std::fs::File;
use std::io;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::time::Duration;
use std::time::Instant;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;
use uuid::Uuid;

#[path = "observation_pilot_canonical.rs"]
mod canonical;
#[path = "observation_pilot_credential.rs"]
mod credential;
#[path = "observation_pilot_validation.rs"]
mod validation;

/// Opaque original launch custody. Copies share the same one-use binding and fence.
#[derive(Clone)]
pub struct PilotStartup(Arc<ReceivedLaunch>);

struct ReceivedLaunch {
    decision: PilotLaunchDecision,
    digest: String,
    _descriptor: File,
    state: Mutex<LaunchState>,
    credential: Mutex<Option<credential::CredentialInput>>,
}

struct LaunchState {
    generation: u64,
    bound: bool,
    revoked: bool,
    last_wall_ms: u64,
    deadline: Instant,
}

impl fmt::Debug for PilotStartup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PilotStartup(<original launch custody>)")
    }
}

impl PartialEq for PilotStartup {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for PilotStartup {}

fn denied() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "pilot launch custody rejected",
    )
}

fn wall_ms() -> io::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| u64::try_from(duration.as_millis()).ok())
        .ok_or_else(denied)
}

impl PilotStartup {
    /// Only the fixed original launcher may transfer FD3. The received decision
    /// and durable once claim are joined by Foundation before it spawns this image.
    /// No RPC or persisted thread state can call this receiver.
    #[cfg(target_os = "linux")]
    pub(crate) fn receive(prepared_sha256: Option<&str>) -> io::Result<Self> {
        use std::os::unix::fs::FileExt;
        use std::os::unix::fs::MetadataExt;

        static RECEIVED: AtomicBool = AtomicBool::new(false);
        if RECEIVED.swap(true, Ordering::AcqRel) {
            return Err(denied());
        }
        let descriptor = credential::receive_fd(/*fd*/ 3)?;
        // Receive all original handles before parsing or child creation. No FD4/5
        // lookup occurs on the version1 path without the explicit private flags.
        let credential_files = prepared_sha256
            .map(|_| {
                Ok::<_, io::Error>((
                    credential::receive_fd(/*fd*/ 4)?,
                    credential::receive_fd(/*fd*/ 5)?,
                ))
            })
            .transpose()?;
        let before = descriptor.metadata()?;
        if !before.is_file()
            || before.nlink() != 1
            || before.mode() & 0o222 != 0
            || before.len() == 0
            || before.len() > 65_536
        {
            return Err(denied());
        }
        let mut bytes = vec![0; before.len() as usize];
        descriptor.read_exact_at(&mut bytes, /*offset*/ 0)?;
        let after = descriptor.metadata()?;
        if before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
            || before.ctime() != after.ctime()
            || before.ctime_nsec() != after.ctime_nsec()
        {
            return Err(denied());
        }
        let decision: PilotLaunchDecision = serde_json::from_slice(&bytes).map_err(|_| denied())?;
        let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| denied())?;
        if decision.version == 1 && value["provider"].get("credential").is_some() {
            return Err(denied());
        }
        if decision.version == 2 {
            canonical::validate(&bytes)?;
        }
        validation::validate(&decision)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        validation::validate_native_target(&decision)?;
        let received_at = Instant::now();
        let now = wall_ms()?;
        if now < decision.limits.not_before_ms || now >= decision.limits.expires_ms {
            return Err(denied());
        }
        let credential = match (decision.version, credential_files, prepared_sha256) {
            (1, None, None) => None,
            (2, Some((input, receipt)), Some(pin)) => Some(credential::CredentialInput::receive(
                &decision, &digest, input, receipt, pin,
            )?),
            _ => return Err(denied()),
        };
        let deadline = received_at
            .checked_add(Duration::from_millis(decision.limits.expires_ms - now))
            .ok_or_else(denied)?;
        Ok(Self(Arc::new(ReceivedLaunch {
            decision,
            digest,
            _descriptor: descriptor,
            credential: Mutex::new(credential),
            state: Mutex::new(LaunchState {
                generation: 1,
                bound: false,
                revoked: false,
                last_wall_ms: now,
                deadline,
            }),
        })))
    }

    #[cfg(not(target_os = "linux"))]
    pub(crate) fn receive(_prepared_sha256: Option<&str>) -> io::Result<Self> {
        Err(denied())
    }

    /// Construct controls only after the original Core slot accepted this issuer.
    pub(crate) fn install_control(
        &self,
        thread: Arc<codex_core::CodexThread>,
        bridge: &crate::observation_bridge::ObservationBridge,
    ) -> Result<(), codex_app_server_protocol::JSONRPCErrorError> {
        let control = crate::observation_pilot_control::PilotControl::new(
            thread,
            Arc::clone(&bridge.slot),
            bridge.owner,
            self.0.decision.scope.clone(),
            self.0.decision.instruction.clone(),
            self.0.decision.allocation_id.clone(),
            self.0.digest.clone(),
        )?;
        bridge.pilot.set(control).map_err(|_| {
            crate::observation_control::rejected(
                codex_app_server_protocol::ThreadObservationRejectionCode::Denied,
            )
        })
    }

    pub(crate) fn check_home(&self, home: &Path) -> io::Result<()> {
        if home != Path::new(&self.0.decision.target.codex_home) {
            return Err(denied());
        }
        self.recheck(/*generation*/ 1).map_err(|_| denied())
    }

    fn recheck(&self, generation: u64) -> Result<(), PilotAuthorityError> {
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        let now = match wall_ms() {
            Ok(now) => now,
            Err(_) => {
                state.revoked = true;
                state.generation = state.generation.saturating_add(1);
                return Err(PilotAuthorityError::Unavailable);
            }
        };
        if !state.revoked
            && (now < state.last_wall_ms
                || now >= self.0.decision.limits.expires_ms
                || Instant::now() >= state.deadline
                || generation != state.generation)
        {
            state.revoked = true;
            state.generation = state.generation.saturating_add(1);
        }
        state.last_wall_ms = now;
        if state.revoked {
            Err(PilotAuthorityError::Expired)
        } else {
            Ok(())
        }
    }

    fn revoke(&self) {
        if let Ok(mut state) = self.0.state.lock() {
            state.revoked = true;
            state.generation = state.generation.saturating_add(1);
        }
        if let Ok(mut credential) = self.0.credential.lock() {
            credential.take();
        }
        // A poisoned lock is permanently unavailable to every recheck.
    }

    pub(crate) fn bind(
        &self,
        owner: ObservationOwner,
        thread_id: ThreadId,
    ) -> Result<(Vec<u8>, Arc<dyn NativePilotIssuer>), PilotAuthorityError> {
        self.recheck(/*generation*/ 1)?;
        let mut state = self
            .0
            .state
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        if state.bound || state.revoked {
            return Err(PilotAuthorityError::Replay);
        }
        // The outer durable once claim already exists. Failed native installation
        // keeps this consumed binding; neither startup retry nor rollback refunds it.
        state.bound = true;
        let policy = &self.0.decision;
        let claims = PilotGrantClaims {
            grant_id: Uuid::now_v7(),
            issuer_generation: state.generation,
            owner,
            thread_id,
            scope: policy.scope.clone(),
            permissions: policy
                .permissions
                .iter()
                .map(|permission| {
                    use crate::observation_pilot_decision::Permission;
                    match permission {
                        Permission::PrepareSource => PilotPermission::PrepareSource,
                        Permission::SampleSource => PilotPermission::SampleSource,
                        Permission::ActOnSource => PilotPermission::ActOnSource,
                        Permission::ForegroundTurn => PilotPermission::ForegroundTurn,
                        Permission::AutomaticTurn => PilotPermission::AutomaticTurn,
                    }
                })
                .collect(),
            // Floor to seconds: the Core ledger may refuse early, never late.
            expires_at: (policy.limits.expires_ms / 1000) as i64,
            cooldown: Duration::from_millis(policy.limits.cooldown_ms),
            max_turns: policy.limits.turns,
            max_attempts: policy.limits.attempts,
            max_reserved_tokens: NonZeroU64::new(policy.limits.reserved_tokens)
                .ok_or(PilotAuthorityError::Denied)?,
        };
        Ok((
            self.0.digest.as_bytes().to_vec(),
            Arc::new(LaunchIssuer {
                launch: self.clone(),
                claims,
                verified: AtomicBool::new(false),
            }),
        ))
    }
}

#[cfg(test)]
#[path = "observation_pilot_launch_tests.rs"]
mod tests;

struct LaunchIssuer {
    launch: PilotStartup,
    claims: PilotGrantClaims,
    verified: AtomicBool,
}

impl NativePilotIssuer for LaunchIssuer {
    fn verify_grant(&self, envelope: &[u8]) -> Result<PilotGrantClaims, PilotAuthorityError> {
        self.recheck_grant(&self.claims)?;
        if envelope != self.launch.0.digest.as_bytes() {
            return Err(PilotAuthorityError::Denied);
        }
        if self.verified.swap(true, Ordering::AcqRel) {
            return Err(PilotAuthorityError::Replay);
        }
        Ok(self.claims.clone())
    }

    fn recheck_grant(&self, claims: &PilotGrantClaims) -> Result<(), PilotAuthorityError> {
        if claims.grant_id != self.claims.grant_id
            || claims.owner != self.claims.owner
            || claims.thread_id != self.claims.thread_id
        {
            return Err(PilotAuthorityError::Denied);
        }
        self.launch.recheck(claims.issuer_generation)
    }

    fn revoke(&self) {
        self.launch.revoke();
    }

    fn authenticate_request(
        &self,
        claims: &PilotGrantClaims,
        request: &mut Request,
    ) -> Result<codex_core::PilotRequestAuthentication, PilotAuthorityError> {
        self.recheck_grant(claims)?;
        let credential = self
            .launch
            .0
            .credential
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        let credential = credential
            .as_ref()
            .ok_or(PilotAuthorityError::Unavailable)?;
        credential
            .attach(request, &self.launch.0.decision.provider.endpoint)
            .map_err(|_| PilotAuthorityError::Denied)?;
        self.recheck_grant(claims)?;
        Ok(codex_core::PilotRequestAuthentication::Prepared)
    }

    fn qualify_request(
        &self,
        claims: &PilotGrantClaims,
        request: &Request,
    ) -> Result<PilotRequestReservation, PilotAuthorityError> {
        self.recheck_grant(claims)?;
        let credential = self
            .launch
            .0
            .credential
            .lock()
            .map_err(|_| PilotAuthorityError::Unavailable)?;
        if !credential
            .as_ref()
            .is_some_and(|credential| credential.matches(request))
        {
            return Err(PilotAuthorityError::Denied);
        }
        // Prepared credential provenance is not whole-request context evidence.
        // No production context receipt transport/schema is selected in FD3/4/5;
        // do not turn the decision's context reference into a token reservation.
        Err(PilotAuthorityError::Unavailable)
    }
}
