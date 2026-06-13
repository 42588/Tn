//! Built-in model pricing + context windows (USD per million tokens).
//!
//! Maintained by hand (LiteLLM-style). Unknown models fall back to a zero-cost
//! entry so the UI still shows tokens / context but never a wrong dollar figure.
//! Subscription plans (Claude Max / Codex) should read these as "equivalent API
//! cost", not a literal bill — see docs/产品体验/智能体用量与诚实原则.md.

/// Per-million-token rates + context window for a model family.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
    pub context_window: u32,
}

impl Pricing {
    /// Estimated cost (USD) for the given token counts.
    /// Anthropic reports `input_tokens`, `cache_creation_input_tokens`, and
    /// `cache_read_input_tokens` as three **separate, additive** buckets — input
    /// does NOT include either cache count (a real turn reads 47K cached tokens
    /// with `input_tokens=2`). Charge each at its own rate; never subtract cache
    /// from input (that underflows u64 → astronomical cost / debug panic).
    pub fn cost(&self, input: u64, output: u64, cache_create: u64, cache_read: u64) -> f64 {
        const M: f64 = 1_000_000.0;
        input as f64 / M * self.input
            + output as f64 / M * self.output
            + cache_create as f64 / M * self.cache_write
            + cache_read as f64 / M * self.cache_read
    }
}

/// Context-window size (tokens) inferred from a model id.
///
/// Claude's session logs don't record the window (unlike Codex, which logs
/// `model_context_window`), so we infer it from the id. **Current-generation
/// Claude models ship a 1M window by default** — Opus 4.6 / 4.7 / 4.8, Sonnet 4.6,
/// and Fable 5 / Mythos 5 — while Haiku and older families stay at 200K. The
/// legacy `…-1m` marker still wins for an explicit long-context variant
/// (e.g. `claude-sonnet-4-6-1m`). Unknown / non-Claude ids fall back to 200K;
/// [`parse_claude_session`](crate::claude::parse_claude_session) then widens to 1M
/// if an observed turn already exceeds that, so the ring never reads >100%.
///
/// Substring + generation checks (not an exact-id table) so dated snapshots like
/// `claude-haiku-4-5-20251001` still match. Catalog source: claude-api model
/// reference (cached 2026-06) — extend the 1M list as new families land.
pub fn context_window_for(model: &str) -> u32 {
    let id = model.to_ascii_lowercase();
    let one_m = id.contains("1m")
        || id.contains("fable")
        || id.contains("mythos")
        || id.contains("opus-4-6")
        || id.contains("opus-4-7")
        || id.contains("opus-4-8")
        || id.contains("sonnet-4-6");
    if one_m {
        1_000_000
    } else {
        200_000
    }
}

/// Look up pricing for a model id by family substring. The context window comes
/// from [`context_window_for`] (current-gen Claude models are 1M, not 200K).
pub fn pricing_for(model: &str) -> Pricing {
    let id = model.to_ascii_lowercase();
    let context_window = context_window_for(&id);
    let (input, output, cache_write, cache_read) = if id.contains("opus") {
        (15.0, 75.0, 18.75, 1.50)
    } else if id.contains("haiku") {
        (1.0, 5.0, 1.25, 0.10)
    } else if id.contains("sonnet") {
        (3.0, 15.0, 3.75, 0.30)
    } else if id.contains("gpt") || id.contains("codex") || id.contains("o3") || id.contains("o4") {
        // Codex / OpenAI families — coarse placeholder, refined when Codex lands.
        (2.5, 10.0, 0.0, 0.25)
    } else {
        (0.0, 0.0, 0.0, 0.0) // unknown: report tokens/context, no cost guess
    };
    Pricing {
        input,
        output,
        cache_write,
        cache_read,
        context_window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn families_and_window() {
        assert_eq!(pricing_for("claude-opus-4-7").input, 15.0);
        assert_eq!(pricing_for("claude-sonnet-4-6").output, 15.0);
        assert_eq!(pricing_for("claude-haiku-4-5").input, 1.0);
        assert_eq!(
            pricing_for("claude-sonnet-4-6-1m").context_window,
            1_000_000
        );
        assert_eq!(pricing_for("totally-unknown").input, 0.0);
    }

    #[test]
    fn context_window_matches_current_catalog() {
        // Current-gen Claude → 1M (the bug was these reading 200K).
        for m in [
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-fable-5",
            "claude-mythos-5",
            "claude-sonnet-4-6-1m",
        ] {
            assert_eq!(context_window_for(m), 1_000_000, "{m} should be 1M");
        }
        // Haiku, older families, dated snapshots, and non-Claude → 200K.
        for m in [
            "claude-haiku-4-5-20251001",
            "claude-opus-4-5",
            "claude-opus-4-1",
            "claude-sonnet-4-5",
            "deepseek-v4-pro",
            "totally-unknown",
        ] {
            assert_eq!(context_window_for(m), 200_000, "{m} should be 200K");
        }
    }

    #[test]
    fn cost_math() {
        let p = pricing_for("claude-sonnet-4-6"); // 3 / 15 / 3.75 / 0.30 per MTok
                                                  // 1M input + 1M output = $3 + $15 = $18.
        assert!((p.cost(1_000_000, 1_000_000, 0, 0) - 18.0).abs() < 1e-9);
    }
}
