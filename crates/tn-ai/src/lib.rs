//! Built-in agent adapters (Claude Code / Codex) for the Agent Host — headless.
//!
//! These are the platform's two **seed** [`tn_agent::AgentAdapter`]s: they parse
//! local agent session logs into token / context-window / cost usage **without
//! the agent's cooperation** — Claude Code writes
//! `~/.claude/projects/<proj>/<session>.jsonl` (one JSON object per line;
//! assistant lines carry `message.usage`), Codex writes
//! `$CODEX_HOME/sessions/**/rollout-*.jsonl`. No GPUI, no UI — fully unit-testable
//! from string fixtures; the platform ([`tn_agent`]) and UI know nothing of these
//! agents, resolving everything through the registry. The closed `AgentKind` enum
//! is gone — identity is the open [`tn_agent::AgentId`].
//!
//! [`builtin_registry`] is **not** wired into the default app registry (the Agent
//! Host ships agent-less); it's the reusable seed for tests and for a user who
//! re-registers Claude/Codex with telemetry.

mod adapter;
mod claude;
mod codex;
mod detect;

pub use adapter::{
    builtin_adapter_for_manifest, builtin_registry, ClaudeAdapter, CodexAdapter,
};
pub use detect::{adapter_session_mtimes, resolve_pane_session};

// The usage + pricing model lives in the `tn-agent` platform crate (the
// agent-agnostic contract). Re-exported so `tn_ai::{AiUsage, Pricing,
// pricing_for, pricing}` keep working — `claude.rs`/`codex.rs` reference
// `crate::pricing` / `crate::AiUsage`.
pub use tn_agent::{pricing, pricing_for, AiUsage, Pricing};
