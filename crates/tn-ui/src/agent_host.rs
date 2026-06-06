//! The app-wide [`AgentRegistry`] as a GPUI global. Built once at startup
//! (default = built-in Claude/Codex via `tn_ai::builtin_registry`) and read
//! wherever the UI resolves an agent's identity, presentation, capabilities, or
//! usage adapter — so no UI code names a concrete agent (the Agent Host model).
//!
//! The P6 end state installs an **empty** registry here (pure shell host) plus
//! whatever agents the user registers; nothing else in the UI changes.

use gpui::App;
use tn_agent::AgentRegistry;

/// GPUI global wrapper for the active agent registry.
pub(crate) struct AgentHost(pub(crate) AgentRegistry);

impl gpui::Global for AgentHost {}

/// The active registry — a cheap (Arc-backed `Vec`) clone. Returns an **empty**
/// registry when none is installed (e.g. a headless unit test that never called
/// `set_global`), so agent-dependent UI degrades to "no agent" instead of
/// panicking. Cloning sidesteps holding an `&App` borrow across view mutations.
pub(crate) fn agent_registry(cx: &App) -> AgentRegistry {
    cx.try_global::<AgentHost>()
        .map(|g| g.0.clone())
        .unwrap_or_default()
}
