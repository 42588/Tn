//! The unified usage model every agent reports through. Adapters translate
//! their private logs into this shape; the UI renders it as a context ring /
//! status bar (see docs/产品设计.md §5). Moved here from `tn-ai` so it's the
//! platform's contract, not a Claude/Codex-specific type.

use serde::{Deserialize, Serialize};

/// A point-in-time usage snapshot for one agent session.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AiUsage {
    pub model: String,
    /// Cumulative tokens over the whole session.
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
    /// Current context size = the latest turn's total input
    /// (`input + cache_read + cache_create`) — what `/context` shows.
    pub context_used: u32,
    /// Model context window (best-effort, from the pricing table).
    pub context_max: u32,
    /// Estimated cost (USD) from the built-in pricing table.
    pub cost_usd: f64,
    /// Number of assistant turns seen.
    pub turns: u32,
}

impl AiUsage {
    /// Context-window fill fraction, clamped to `[0, 1]` (drives the ring color:
    /// green → yellow → red as it climbs).
    pub fn context_frac(&self) -> f32 {
        if self.context_max == 0 {
            0.0
        } else {
            (self.context_used as f32 / self.context_max as f32).clamp(0.0, 1.0)
        }
    }

    /// Total tokens billed across the session.
    pub fn total_tokens(&self) -> u64 {
        self.input + self.output + self.cache_create + self.cache_read
    }
}
