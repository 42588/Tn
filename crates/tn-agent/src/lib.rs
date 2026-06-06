//! Agent Host platform (headless).
//!
//! The agent-agnostic core that lets Tn host **any** coding agent without
//! per-agent special-casing. It owns identity ([`AgentId`] / [`AgentDescriptor`]),
//! capabilities ([`AgentCapabilities`]), run location ([`AgentRuntimeKind`]),
//! the UI's event contract ([`AgentEvent`]), the observation trait
//! ([`AgentAdapter`]), the [`AgentRegistry`], and the unified usage + pricing
//! model ([`AiUsage`] / [`pricing_for`]).
//!
//! This crate has **zero knowledge of any concrete agent**. The built-in
//! Claude/Codex adapters live in `tn-ai` (and are removable — see the P6 end
//! state where the default registry is empty). External agents (P5) register an
//! adapter that speaks stdio/JSON-RPC. No GPUI, no UI, fully headless.
//!
//! Boundary (铁律): [`AgentRuntimeKind`] describes where a process/protocol runs;
//! it is **not** a file namespace. `AgentEvent::CwdChanged` feeds only the owning
//! pane's context, never the global Explorer.

mod adapter;
mod descriptor;
mod event;
mod id;
pub mod pricing;
mod registry;
mod usage;

pub use adapter::{AgentAdapter, SessionRef};
pub use descriptor::{AgentCapabilities, AgentDescriptor, AgentRuntimeKind};
pub use event::{AgentEvent, AgentStatus};
pub use id::AgentId;
pub use pricing::{pricing_for, Pricing};
pub use registry::AgentRegistry;
pub use usage::AiUsage;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A telemetry-free stub adapter — just identity, the config-level case.
    struct StubAdapter(AgentDescriptor);
    impl AgentAdapter for StubAdapter {
        fn descriptor(&self) -> &AgentDescriptor {
            &self.0
        }
    }

    fn desc(id: &str, alias: &str) -> AgentDescriptor {
        let mut d = AgentDescriptor::generic(AgentId::new(id), id);
        d.command_aliases = vec![alias.to_string()];
        d
    }

    #[test]
    fn registry_matches_command_by_alias() {
        let reg = AgentRegistry::new()
            .with(Arc::new(StubAdapter(desc("claude", "claude"))))
            .with(Arc::new(StubAdapter(desc("codex", "codex"))));
        assert_eq!(reg.match_command("claude"), Some(AgentId::new("claude")));
        assert_eq!(
            reg.match_command("C:/npm/codex.cmd"),
            Some(AgentId::new("codex"))
        );
        assert_eq!(
            reg.match_command("& 'claude' '--resume'"),
            Some(AgentId::new("claude"))
        );
        assert_eq!(reg.match_command("powershell.exe"), None);
    }

    #[test]
    fn registry_get_and_first_match_wins() {
        // Two agents whose aliases both substring-match "mycodex-tool": the first
        // registered wins, so registration order is the tie-break.
        let reg = AgentRegistry::new()
            .with(Arc::new(StubAdapter(desc("codex", "codex"))))
            .with(Arc::new(StubAdapter(desc("other", "codex"))));
        assert_eq!(reg.match_command("mycodex-tool"), Some(AgentId::new("codex")));
        assert!(reg.get(&AgentId::new("codex")).is_some());
        assert!(reg.get(&AgentId::new("nope")).is_none());
    }

    #[test]
    fn generic_descriptor_is_terminal_only_no_usage() {
        let d = AgentDescriptor::generic(AgentId::new("gemini"), "Gemini CLI");
        assert_eq!(d.label, "Gemini CLI");
        assert_eq!(d.short, "Gemini CLI");
        assert!(d.capabilities.terminal);
        assert!(d.capabilities.cwd_sync);
        assert!(d.capabilities.git_diff);
        assert!(!d.capabilities.usage); // no adapter → no usage ring
        assert!(!d.manages_own_cursor);
        // Matches its own id as a command substring.
        assert!(d.matches_command("gemini"));
        assert!(!d.matches_command("powershell"));
    }

    #[test]
    fn descriptor_or_generic_falls_back() {
        let reg = AgentRegistry::new();
        let d = reg.descriptor_or_generic(&AgentId::new("aider"), "Aider");
        assert_eq!(d.id, AgentId::new("aider"));
        assert!(d.capabilities.terminal);
        assert!(reg.is_empty());
    }

    #[test]
    fn empty_registry_hosts_no_agents() {
        let reg = AgentRegistry::new();
        assert_eq!(reg.match_command("claude"), None);
        assert_eq!(reg.descriptors().count(), 0);
    }
}
