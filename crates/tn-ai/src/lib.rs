//! AI agent usage + detection (M4) — headless.
//!
//! Parses local agent session logs into token / context-window / cost usage
//! **without the agent's cooperation**: Claude Code writes
//! `~/.claude/projects/<proj>/<session>.jsonl` (one JSON object per line;
//! assistant lines carry `message.usage`), Codex writes
//! `$CODEX_HOME/sessions/**/rollout-*.jsonl`. No GPUI, no UI — the parsing is
//! fully unit-testable from string fixtures; the tn-ui layer renders the
//! [`AiUsage`] as a context ring / status bar (see docs/产品设计.md §5).

mod claude;
mod codex;
mod detect;
mod pricing;

pub use claude::{
    claude_projects_dir, encode_project_dir, latest_session_file, parse_claude_session,
    usage_for_cwd,
};
pub use codex::{
    codex_sessions_dir, latest_codex_session_file, parse_codex_session, usage_for_cwd_codex,
};
pub use detect::{
    agent_kind_for_command, detect_subscription, parse_session, resolve_session,
    resolve_session_for_pane, session_mtimes, update_session, usage_for_cwd as usage_for_cwd_any, SessionRef,
};
pub use pricing::{pricing_for, Pricing};

use serde::Serialize;

/// Which agent CLI a session belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentKind {
    ClaudeCode,
    Codex,
}

impl AgentKind {
    /// Short display label for the status bar / pane chrome.
    pub fn label(self) -> &'static str {
        match self {
            AgentKind::ClaudeCode => "Claude",
            AgentKind::Codex => "Codex",
        }
    }
}

/// A point-in-time usage snapshot for one agent session.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
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
