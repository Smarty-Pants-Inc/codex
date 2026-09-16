//! Native projection of Foundation 7dc3dc4's private PilotLaunchDecision.
//! This format is launch data, never an RPC grant or a credential receipt.

use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct PilotLaunchDecision {
    pub version: u32,
    pub instruction: String,
    pub allocation_id: String,
    pub source: Source,
    pub target: Target,
    pub operation: String,
    pub scope: String,
    pub roots: Roots,
    pub permissions: Vec<Permission>,
    pub limits: Limits,
    pub provider: Provider,
    pub evidence: Evidence,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Source {
    pub commit: String,
    pub tree: String,
    pub binary_sha256: String,
    pub closure_sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Target {
    pub machine_id: String,
    pub uid: u32,
    pub gid: u32,
    pub cwd: String,
    pub codex_home: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Roots {
    #[serde(rename = "W")]
    pub write: Root,
    #[serde(rename = "R")]
    pub read: Root,
    #[serde(rename = "D")]
    pub data: Root,
    #[serde(rename = "T")]
    pub temporary: Root,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Root {
    pub path: String,
    pub device: u64,
    pub inode: u64,
    pub access: Access,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Access {
    ReadOnly,
    ReadWrite,
}

#[derive(Clone, Copy, Deserialize, Eq, Ord, PartialEq, PartialOrd)]
#[serde(rename_all = "camelCase")]
pub(super) enum Permission {
    PrepareSource,
    SampleSource,
    ActOnSource,
    ForegroundTurn,
    AutomaticTurn,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Limits {
    pub attempts: u32,
    pub turns: u32,
    pub reserved_tokens: u64,
    pub not_before_ms: u64,
    pub expires_ms: u64,
    pub cooldown_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Provider {
    pub provider: String,
    pub model: String,
    pub api: String,
    pub endpoint: String,
    pub purpose: String,
    pub account: String,
    /// Only version2 carries this independently admitted preparation binding.
    pub credential: Option<Credential>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(super) struct Credential {
    pub generation: String,
    pub reference_sha256: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Evidence {
    pub credential: String,
    pub context: String,
}
