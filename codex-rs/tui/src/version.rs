/// The current Codex CLI version as embedded at compile time.
pub const CODEX_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

// Pin display input before layout: replacing a version after rendering cannot
// normalize padding or wrapping. Keep update checks and client metadata real.
#[cfg(test)]
pub(crate) const CODEX_DISPLAY_VERSION: &str = "0.0.0";
#[cfg(not(test))]
pub(crate) const CODEX_DISPLAY_VERSION: &str = CODEX_CLI_VERSION;
