//! Helpers for presenting token usage information in the TUI footer.

/// Baseline number of tokens reserved for system prompts, tool instructions,
/// and other always-on context. Subtracting this from both the numerator and
/// denominator keeps the "percent left" indicator focused on the
/// user-controlled portion of the window.
pub const BASELINE_TOKENS: u64 = 12_000;

/// Compute the number of tokens that still reside in the model's context
/// window. We subtract reasoning tokens because many providers do not retain
/// them in context after the completion stream is finished.
#[inline]
pub fn tokens_in_context_window(total_tokens: u64, reasoning_tokens: u64) -> u64 {
    total_tokens.saturating_sub(reasoning_tokens)
}

/// Return the percentage of the context window that remains available after
/// accounting for the baseline and the tokens currently in context.
#[inline]
pub fn percent_remaining(context_window: u64, tokens_in_context: u64) -> u8 {
    if context_window <= BASELINE_TOKENS {
        return 0;
    }

    let effective_window = context_window - BASELINE_TOKENS;
    let used = tokens_in_context.saturating_sub(BASELINE_TOKENS);
    let remaining = effective_window.saturating_sub(used);

    ((remaining as f64 / effective_window as f64) * 100.0)
        .clamp(0.0, 100.0) as u8
}

/// Format a token count with thousands separators for display in the footer.
#[inline]
pub fn format_with_thousands(value: u64) -> String {
    use thousands::Separable;
    value.separate_with_commas()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_window_respects_baseline() {
        let total = BASELINE_TOKENS + 2_000;
        assert_eq!(percent_remaining(total, total), 0);
        assert_eq!(percent_remaining(total, BASELINE_TOKENS), 100);
    }

    #[test]
    fn tokens_in_context_subtracts_reasoning() {
        assert_eq!(tokens_in_context_window(10_000, 2_000), 8_000);
        assert_eq!(tokens_in_context_window(10_000, 0), 10_000);
    }

    #[test]
    fn formatter_adds_commas() {
        assert_eq!(format_with_thousands(12_345), "12,345");
    }
}
