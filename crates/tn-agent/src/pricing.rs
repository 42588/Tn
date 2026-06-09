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

/// Look up pricing for a model id by family substring. The context window is
/// widened to 1M when the id signals the long-context variant (e.g. `…-1m`).
pub fn pricing_for(model: &str) -> Pricing {
    let id = model.to_ascii_lowercase();
    let context_window = if id.contains("1m") {
        1_000_000
    } else {
        200_000
    };
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
        assert_eq!(pricing_for("claude-opus-4-7").context_window, 200_000);
        assert_eq!(
            pricing_for("claude-sonnet-4-6-1m").context_window,
            1_000_000
        );
        assert_eq!(pricing_for("totally-unknown").input, 0.0);
    }

    #[test]
    fn cost_math() {
        let p = pricing_for("claude-sonnet-4-6"); // 3 / 15 / 3.75 / 0.30 per MTok
                                                  // 1M input + 1M output = $3 + $15 = $18.
        assert!((p.cost(1_000_000, 1_000_000, 0, 0) - 18.0).abs() < 1e-9);
    }
}
