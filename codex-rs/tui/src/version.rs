/// The current Codex CLI version as embedded at compile time.
#[cfg(test)]
pub const CODEX_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Local builds resolve this from the latest upstream Rust release tag when available so the
/// visible CLI version tracks upstream instead of the workspace placeholder `0.0.0`.
#[cfg(not(test))]
pub const CODEX_CLI_VERSION: &str = match option_env!("CODEX_RS_RESOLVED_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};
