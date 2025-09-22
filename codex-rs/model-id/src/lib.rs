//! Utilities for normalizing model identifiers and deriving model families.
//!
//! This crate centralizes the string handling that previously lived across a
//! handful of `codex-core` modules. Keeping the heuristics here allows both the
//! TUI and the protocol/backend layers to share the same interpretation logic
//! while keeping the surface area small.

use std::borrow::Cow;

/// Canonical separator used for provider-qualified model identifiers.
const SEP: char = '/';

/// Normalize a provider-qualified model slug into a canonical lowercase form.
///
/// Examples:
/// - `openai:gpt-5` → `openai/gpt-5`
/// - `OpenRouter/OpenAI/GPT-5o` → `openrouter/openai/gpt-5o`
/// - `gpt-4.1-mini` stays unchanged aside from casing
pub fn normalize<S: AsRef<str>>(input: S) -> String {
    let mut slug = input.as_ref().trim().to_ascii_lowercase();
    if slug.is_empty() {
        return slug;
    }

    // Collapse any runs of path separators/colons into a single separator so
    // downstream lookups can treat everything uniformly.
    slug = slug
        .split(|c| matches!(c, ':' | '/' | '\\'))
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("/");

    slug
}

/// Derive a coarse-grained model family identifier from a (possibly normalized)
/// slug. The returned value is suitable for indexing the TOML model map.
///
/// The heuristics intentionally err on the side of grouping together snapshots
/// that share the same base model (e.g., `gpt-4o-2024-08-06` → `gpt-4o`).
pub fn family<S: AsRef<str>>(input: S) -> Option<&'static str> {
    let slug = normalize(input);
    if slug.is_empty() {
        return None;
    }

    let candidate = slug.rsplit(SEP).next().unwrap_or(&slug);

    for (prefix, family) in FAMILY_TABLE.iter() {
        if candidate.starts_with(prefix) {
            return Some(*family);
        }
    }

    None
}

/// Provide a small lookup table covering the model families that we actively
/// support. This keeps the logic data-driven while avoiding runtime
/// allocations.
const FAMILY_TABLE: &[(&str, &str)] = &[
    ("gpt-5", "gpt-5"),
    ("gpt-4.1", "gpt-4.1"),
    ("gpt-4o", "gpt-4o"),
    ("gpt-4", "gpt-4"),
    ("gpt-3.5", "gpt-3.5"),
    ("o4-mini", "o4-mini"),
    ("o3-mini", "o3-mini"),
    ("gpt-oss", "gpt-oss"),
    ("claude-3.5", "claude-3.5"),
    ("claude-3", "claude-3"),
    ("sonnet", "claude-3.5"),
    ("haiku", "claude-3"),
];

/// Convenience helper that returns either the derived family or the normalized
/// slug when no family match is found.
pub fn family_or_slug<S: AsRef<str>>(input: S) -> Cow<'static, str> {
    if let Some(family) = family(&input) {
        Cow::Borrowed(family)
    } else {
        Cow::Owned(normalize(input))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_handles_provider_variants() {
        assert_eq!(normalize("openai:gpt-5"), "openai/gpt-5");
        assert_eq!(normalize("OpenRouter/OpenAI/GPT-5o"), "openrouter/openai/gpt-5o");
        assert_eq!(normalize("gpt-4.1-mini"), "gpt-4.1-mini");
        assert_eq!(normalize("  GPT-4O-2024-08-06  "), "gpt-4o-2024-08-06");
    }

    #[test]
    fn family_matches_known_prefixes() {
        assert_eq!(family("openai:gpt-5"), Some("gpt-5"));
        assert_eq!(family("openrouter/openai/gpt-5o"), Some("gpt-5"));
        assert_eq!(family("gpt-4.1-mini"), Some("gpt-4.1"));
        assert_eq!(family("openrouter/anthropic/claude-3.5-sonnet"), Some("claude-3.5"));
    }

    #[test]
    fn family_or_slug_falls_back() {
        assert_eq!(family_or_slug("mystery"), Cow::Owned(String::from("mystery")));
    }
}
