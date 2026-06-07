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
mod external;
mod id;
pub mod pricing;
mod registry;
mod usage;

pub use adapter::{AgentAdapter, GenericAdapter, SessionRef};
pub use descriptor::{
    pty_runtimes, AgentCapabilities, AgentDescriptor, AgentNetworkPolicy, AgentRuntimeKind,
    SidecarLaunch,
};
pub use event::{AgentEvent, AgentStatus};
pub use external::{ExternalEventAdapter, ExternalProcessAdapter};
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
        assert_eq!(
            reg.match_command("mycodex-tool"),
            Some(AgentId::new("codex"))
        );
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

    #[test]
    fn descriptor_from_manifest_maps_fields_and_caps() {
        let m = tn_config::AgentManifest {
            id: "gemini".into(),
            label: Some("Gemini CLI".into()),
            short: None,
            aliases: vec!["gemini".into(), "gmn".into()],
            accent: Some(tn_config::Color::new(0x44, 0x88, 0xFF)),
            glyph: None,
            manages_own_cursor: true,
            capabilities: vec!["usage".into(), "transcript".into(), "bogus".into()],
            runtime_support: Vec::new(),
            allow_network: false,
            sidecar: None,
        };
        let d = AgentDescriptor::from_manifest(&m);
        assert_eq!(d.id, AgentId::new("gemini"));
        assert_eq!(d.label, "Gemini CLI");
        assert_eq!(d.short, "Gemini CLI"); // short defaults to label
        assert_eq!(
            d.command_aliases,
            vec!["gemini".to_string(), "gmn".to_string()]
        );
        assert!(d.manages_own_cursor);
        // baseline always-on + listed extras; unknown ("bogus") ignored.
        assert!(d.capabilities.terminal && d.capabilities.cwd_sync && d.capabilities.git_diff);
        assert!(d.capabilities.usage && d.capabilities.transcript);
        assert!(!d.capabilities.tool_calls);
        assert!(d.matches_command("gmn-run"));
        assert_eq!(d.runtime_support, pty_runtimes());
        assert_eq!(d.network_policy, AgentNetworkPolicy::Deny);
        assert!(d.realtime_command.is_none());
        assert_eq!(d.sidecar_launch(), SidecarLaunch::None); // no sidecar declared
    }

    #[test]
    fn descriptor_from_manifest_maps_runtime_and_network_policy() {
        let m = tn_config::AgentManifest {
            id: "bridge".into(),
            label: Some("Bridge Agent".into()),
            short: Some("Bridge".into()),
            aliases: vec!["bridge".into()],
            accent: None,
            glyph: None,
            manages_own_cursor: false,
            capabilities: vec!["usage".into(), "permission_prompts".into()],
            runtime_support: vec![
                "structured".into(),
                "http".into(),
                "websocket".into(),
                "http".into(), // duplicate ignored
                "unknown".into(),
            ],
            allow_network: true,
            sidecar: Some("bridge --json".into()),
        };
        let d = AgentDescriptor::from_manifest(&m);
        assert_eq!(
            d.runtime_support,
            vec![
                AgentRuntimeKind::Structured,
                AgentRuntimeKind::Http,
                AgentRuntimeKind::WebSocket,
            ]
        );
        assert_eq!(d.network_policy, AgentNetworkPolicy::Ask);
        assert!(d.supports_runtime(AgentRuntimeKind::Structured));
        assert!(d.requires_network_confirmation(AgentRuntimeKind::Http));
        assert!(d.runtime_allowed_after_confirmation(AgentRuntimeKind::Http));
        assert!(d.runtime_allowed_after_confirmation(AgentRuntimeKind::Structured));
        assert!(!d.runtime_allowed_after_confirmation(AgentRuntimeKind::LocalPty));
        // Sidecar argv parsed; allow_network → Confirm (never a silent connect).
        assert_eq!(
            d.realtime_command,
            Some(vec!["bridge".to_string(), "--json".to_string()])
        );
        assert_eq!(d.sidecar_launch(), SidecarLaunch::Confirm);
    }

    #[test]
    fn sidecar_launch_keys_on_network_policy_not_runtime_support() {
        let base = |allow: bool, sidecar: Option<&str>| {
            AgentDescriptor::from_manifest(&tn_config::AgentManifest {
                id: "x".into(),
                label: None,
                short: None,
                aliases: vec![],
                accent: None,
                glyph: None,
                manages_own_cursor: false,
                capabilities: vec![],
                // runtime_support stays empty (PTY default) — the agent runs in a
                // PTY regardless; only the sidecar's allow_network gates the spawn.
                runtime_support: Vec::new(),
                allow_network: allow,
                sidecar: sidecar.map(String::from),
            })
        };
        // Local sidecar (no allow_network) → spawn now; the agent still PTY-runs.
        let local = base(false, Some("tele --json"));
        assert!(local.supports_runtime(AgentRuntimeKind::LocalPty)); // not refused by the launcher
        assert_eq!(local.sidecar_launch(), SidecarLaunch::SpawnNow);
        // Network-reaching sidecar (allow_network) → confirm first; agent still PTY-runs.
        let net = base(true, Some("tele --json"));
        assert!(net.supports_runtime(AgentRuntimeKind::LocalPty));
        assert_eq!(net.sidecar_launch(), SidecarLaunch::Confirm);
        // No sidecar → nothing, even with allow_network.
        assert_eq!(base(true, None).sidecar_launch(), SidecarLaunch::None);
    }

    #[test]
    fn register_manifest_adds_generic_but_never_overrides_builtin() {
        let claude_builtin = desc("claude", "claude");
        let mut reg = AgentRegistry::new().with(Arc::new(StubAdapter(claude_builtin)));
        // A manifest for a NEW id registers a generic (no-usage) agent.
        let gemini = tn_config::AgentManifest {
            id: "gemini".into(),
            label: None,
            short: None,
            aliases: vec![],
            accent: None,
            glyph: None,
            manages_own_cursor: false,
            capabilities: vec![],
            runtime_support: Vec::new(),
            allow_network: false,
            sidecar: None,
        };
        reg.register_manifest(&gemini);
        assert_eq!(reg.match_command("gemini"), Some(AgentId::new("gemini")));
        assert!(!reg.get(&AgentId::new("gemini")).unwrap().capabilities.usage);
        // A manifest re-declaring an existing id is a no-op (built-in wins).
        let shadow = tn_config::AgentManifest {
            id: "claude".into(),
            label: Some("Shadowed".into()),
            short: None,
            aliases: vec![],
            accent: None,
            glyph: None,
            manages_own_cursor: false,
            capabilities: vec![],
            runtime_support: Vec::new(),
            allow_network: false,
            sidecar: None,
        };
        reg.register_manifest(&shadow);
        assert_eq!(reg.get(&AgentId::new("claude")).unwrap().label, "claude");
    }
}
