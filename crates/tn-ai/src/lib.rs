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

pub use claude::{
    claude_projects_dir, encode_project_dir, latest_session_file, parse_claude_session,
    usage_for_cwd,
};
pub use codex::{
    codex_sessions_dir, latest_codex_session_file, parse_codex_session, usage_for_cwd_codex,
};
pub use detect::{
    agent_kind_for_command, detect_subscription, parse_session, resolve_session,
    resolve_session_for_pane, session_mtimes, update_session, usage_for_cwd as usage_for_cwd_any,
    SessionRef,
};

// The usage + pricing model moved to the `tn-agent` platform crate (it's the
// agent-agnostic contract, not Claude/Codex-specific). Re-exported here so
// existing `tn_ai::{AiUsage, Pricing, pricing_for, pricing}` paths keep working
// — `claude.rs`/`codex.rs` reference `crate::pricing` / `crate::AiUsage`.
pub use tn_agent::{pricing, pricing_for, AiUsage, Pricing};

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
