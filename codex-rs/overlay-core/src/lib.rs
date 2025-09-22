//! Shared overlay utilities for the Codex fork.

pub use codex_core as upstream_core;

pub fn conversation_prefix() -> &'static str {
    "smarty"
}
