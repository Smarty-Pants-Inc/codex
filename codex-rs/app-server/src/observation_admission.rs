//! Trusted launch selection. RPC opt-in never creates this authority.

use crate::observation_pilot_launch::PilotStartup;
use crate::transport::AppServerTransport;
use crate::transport::ConnectionOrigin;
use codex_core::ObservationProfile;
use codex_protocol::ThreadId;
use std::fs::File;
use std::fs::OpenOptions;
use std::io;
use std::path::Path;
use std::sync::Arc;

/// The host selects this only for its owned private-home stdio launch and an
/// independently qualified controlled Responses adapter. This is not provider
/// discovery: neither a model alias nor an RPC field qualifies a profile.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObservationStartup {
    pub profile: ObservationProfile,
    /// Only this host-admitted persisted thread may resume with observations.
    /// The host must reconnect its approved dynamic-tool handler before resume.
    pub resume_thread: Option<ThreadId>,
    /// Original descriptor custody; never reconstructed from thread/RPC data.
    pub pilot: Option<PilotStartup>,
}

/// Explicit launch-only controls; these do not modify provider routing or config.
#[derive(clap::Args, Clone, Debug, Default)]
pub struct AppServerObservationArgs {
    #[arg(long, hide = true, value_parser = ["harmony-gpt-oss"], conflicts_with = "remote_control")]
    observation_profile: Option<String>,
    #[arg(long, hide = true, requires = "observation_profile")]
    observation_resume_thread: Option<String>,
    #[arg(long, hide = true, requires = "observation_profile", value_parser = ["3"])]
    sense_pilot_launch_fd: Option<String>,
    #[arg(long, hide = true, value_parser = ["4"], requires_all = ["sense_pilot_launch_fd", "sense_pilot_prepared_fd", "sense_pilot_prepared_sha256"])]
    sense_pilot_credential_fd: Option<String>,
    #[arg(long, hide = true, value_parser = ["5"], requires = "sense_pilot_credential_fd")]
    sense_pilot_prepared_fd: Option<String>,
    #[arg(long, hide = true, requires = "sense_pilot_credential_fd")]
    sense_pilot_prepared_sha256: Option<String>,
    #[arg(long, hide = true, value_parser = ["14"], requires_all = ["sense_pilot_credential_fd", "sense_pilot_count_scope_sha256", "sense_pilot_count_semantics_fd", "sense_pilot_count_semantics_sha256", "sense_pilot_count_ledger_fd"])]
    sense_pilot_count_scope_fd: Option<String>,
    #[arg(long, hide = true, requires = "sense_pilot_count_scope_fd")]
    sense_pilot_count_scope_sha256: Option<String>,
    #[arg(long, hide = true, value_parser = ["15"], requires = "sense_pilot_count_scope_fd")]
    sense_pilot_count_semantics_fd: Option<String>,
    #[arg(long, hide = true, requires = "sense_pilot_count_scope_fd")]
    sense_pilot_count_semantics_sha256: Option<String>,
    #[arg(long, hide = true, value_parser = ["16"], requires = "sense_pilot_count_scope_fd")]
    sense_pilot_count_ledger_fd: Option<String>,
}

impl AppServerObservationArgs {
    pub fn into_startup(self) -> anyhow::Result<Option<ObservationStartup>> {
        let Some(_profile) = self.observation_profile else {
            return Ok(None);
        };
        let count_pins = match (
            self.sense_pilot_count_scope_fd.as_deref(),
            self.sense_pilot_count_scope_sha256.as_deref(),
            self.sense_pilot_count_semantics_fd.as_deref(),
            self.sense_pilot_count_semantics_sha256.as_deref(),
            self.sense_pilot_count_ledger_fd.as_deref(),
        ) {
            (None, None, None, None, None) => None,
            (Some("14"), Some(scope), Some("15"), Some(semantics), Some("16"))
                if self.sense_pilot_launch_fd.as_deref() == Some("3")
                    && self.sense_pilot_credential_fd.as_deref() == Some("4")
                    && self.sense_pilot_prepared_fd.as_deref() == Some("5")
                    && self.sense_pilot_prepared_sha256.is_some() =>
            {
                Some((scope, semantics))
            }
            _ => anyhow::bail!("incomplete native count descriptor extension"),
        };
        let pilot = self
            .sense_pilot_launch_fd
            .map(|_| PilotStartup::receive(self.sense_pilot_prepared_sha256.as_deref(), count_pins))
            .transpose()?;
        Ok(Some(ObservationStartup {
            profile: ObservationProfile::HarmonyGptOss,
            pilot,
            resume_thread: self
                .observation_resume_thread
                .as_deref()
                .map(ThreadId::from_string)
                .transpose()?,
        }))
    }
}

#[derive(Debug)]
pub(crate) struct ObservationAdmission {
    pub(crate) selection: ObservationStartup,
    // Kept through connection/thread teardown; no separate ownership registry.
    _home_lock: File,
}

impl ObservationStartup {
    pub(crate) fn acquire(
        self,
        home: &Path,
        transport: &AppServerTransport,
    ) -> io::Result<Arc<ObservationAdmission>> {
        if !matches!(transport, AppServerTransport::Stdio) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "observation admission requires an exclusively owned stdio launch",
            ));
        }
        if let Some(pilot) = &self.pilot {
            pilot.check_home(home)?;
        }
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(home.join("observation-owner.lock"))?;
        lock.try_lock().map_err(|error| {
            io::Error::other(format!(
                "observation home is not exclusively admitted: {error}"
            ))
        })?;
        Ok(Arc::new(ObservationAdmission {
            selection: self,
            _home_lock: lock,
        }))
    }
}

#[cfg(test)]
#[path = "observation_admission_tests.rs"]
mod tests;

impl ObservationAdmission {
    pub(crate) fn for_connection(self: &Arc<Self>, origin: ConnectionOrigin) -> Option<Arc<Self>> {
        (origin == ConnectionOrigin::Stdio).then(|| Arc::clone(self))
    }

    pub(crate) fn permits_resume(&self, thread_id: ThreadId) -> bool {
        self.selection.resume_thread == Some(thread_id)
    }
}
