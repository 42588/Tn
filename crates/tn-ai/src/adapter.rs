//! Built-in [`AgentAdapter`]s for Claude Code and Codex — the platform's two
//! seed providers. Each pairs a static [`AgentDescriptor`] (identity, accent,
//! capabilities, cursor quirk) with the existing `claude`/`codex` log parsers.
//!
//! These are **removable**: the default app starts from an empty registry (pure
//! shell host) and only registers config-declared agents. [`builtin_registry`] is
//! kept as the reusable seed for tests or for a build/user path that explicitly
//! re-adds Claude/Codex telemetry. The platform (`tn-agent`) has no knowledge of
//! either agent — all the Claude/Codex specifics live right here.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use tn_agent::{
    AgentAdapter, AgentCapabilities, AgentDescriptor, AgentId, AgentNetworkPolicy, AgentRegistry,
    AgentRuntimeKind, AiUsage,
};
use tn_config::Color;

use crate::{claude, codex, detect};

/// All PTY-family runtimes — agents can be hosted locally, in WSL, or over SSH.
fn pty_runtimes() -> Vec<AgentRuntimeKind> {
    vec![
        AgentRuntimeKind::LocalPty,
        AgentRuntimeKind::WslPty,
        AgentRuntimeKind::SshPty,
    ]
}

/// Terminal + usage + transcript + cwd sync + git-diff rail — the full built-in
/// agent surface. `transcript` unlocks Tn's own scrollable history (parsed from
/// the session log) since these TUI agents never fill terminal scrollback.
fn full_capabilities() -> AgentCapabilities {
    AgentCapabilities {
        terminal: true,
        usage: true,
        transcript: true,
        cwd_sync: true,
        git_diff: true,
        ..AgentCapabilities::default()
    }
}

fn claude_descriptor() -> AgentDescriptor {
    AgentDescriptor {
        id: AgentId::new("claude"),
        label: "Claude Code".into(),
        short: "Claude".into(),
        command_aliases: vec!["claude".into()],
        // Default coral accent (mockup `.tile.claude`); a theme `[agents.claude]`
        // entry overrides it (P2). Kept here so removing the theme hardcode still
        // has a fallback.
        accent: Some(Color::new(0xF0, 0x91, 0x6D)),
        glyph: None,
        // No inline-mode injection: Claude runs as its native full-screen Ink TUI.
        // History does NOT come from terminal scrollback (the agent only repaints
        // the visible viewport, so terminal scrollback never holds the full
        // conversation) — Tn owns the scrollable transcript from the session log.
        default_args: Vec::new(),
        default_env: Vec::new(),
        capabilities: full_capabilities(),
        runtime_support: pty_runtimes(),
        network_policy: AgentNetworkPolicy::Deny,
        // Claude Code is an Ink TUI that paints its own cursor → hide ours.
        manages_own_cursor: true,
        realtime_command: None, // telemetry via log parsing, not a sidecar
    }
}

fn codex_descriptor() -> AgentDescriptor {
    AgentDescriptor {
        id: AgentId::new("codex"),
        label: "Codex".into(),
        short: "Codex".into(),
        command_aliases: vec!["codex".into()],
        accent: Some(Color::new(0x73, 0xDA, 0xCA)), // teal (mockup `.tile.codex`)
        glyph: None,
        // No `--no-alt-screen`: Codex runs as its native full-screen TUI. Inline
        // mode only spilled a handful of lines into terminal scrollback (never the
        // full transcript) and broke inline cursor/IME/input — Tn owns the
        // scrollable history from the Codex rollout log instead.
        default_args: Vec::new(),
        default_env: Vec::new(),
        capabilities: full_capabilities(),
        runtime_support: pty_runtimes(),
        network_policy: AgentNetworkPolicy::Deny,
        manages_own_cursor: false,
        realtime_command: None, // telemetry via log parsing, not a sidecar
    }
}

/// Claude Code adapter: reads `~/.claude/projects/<proj>/<session>.jsonl`.
pub struct ClaudeAdapter {
    descriptor: AgentDescriptor,
}

impl ClaudeAdapter {
    pub fn new() -> Self {
        Self {
            descriptor: claude_descriptor(),
        }
    }

    /// Wrap the built-in Claude log parser with a **custom** descriptor — so a
    /// user's `[[agents]]` manifest keeps its own accent / label / cursor while
    /// still getting Tn's real Claude usage parsing (see [`builtin_adapter_for_manifest`]).
    pub fn with_descriptor(descriptor: AgentDescriptor) -> Self {
        Self { descriptor }
    }
}

impl Default for ClaudeAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for ClaudeAdapter {
    fn descriptor(&self) -> &AgentDescriptor {
        &self.descriptor
    }

    fn sessions_with_mtime(&self) -> Vec<(PathBuf, SystemTime)> {
        claude::claude_sessions_with_mtime()
    }

    fn latest_session_file(&self, cwd: &str) -> Option<PathBuf> {
        // cwd-specific session, else this agent's newest overall (a fresh pane's
        // session cwd often differs from the app cwd).
        claude::latest_session_file(cwd).or_else(claude::latest_claude_session_any)
    }

    fn parse_usage(&self, text: &str) -> Option<AiUsage> {
        claude::parse_claude_session(text)
    }

    fn update_usage(&self, text: &str, prev: AiUsage) -> AiUsage {
        claude::update_claude_session(text, prev)
    }

    fn parse_transcript(&self, text: &str) -> Vec<tn_agent::TranscriptEntry> {
        claude::parse_claude_transcript(text)
    }

    fn is_subscription(&self) -> bool {
        detect::claude_is_subscription()
    }
}

/// Codex adapter: reads `$CODEX_HOME/sessions/**/rollout-*.jsonl`.
pub struct CodexAdapter {
    descriptor: AgentDescriptor,
}

impl CodexAdapter {
    pub fn new() -> Self {
        Self {
            descriptor: codex_descriptor(),
        }
    }

    /// Like [`ClaudeAdapter::with_descriptor`] — built-in Codex parsing under a
    /// user-supplied descriptor.
    pub fn with_descriptor(descriptor: AgentDescriptor) -> Self {
        Self { descriptor }
    }
}

impl Default for CodexAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentAdapter for CodexAdapter {
    fn descriptor(&self) -> &AgentDescriptor {
        &self.descriptor
    }

    fn sessions_with_mtime(&self) -> Vec<(PathBuf, SystemTime)> {
        codex::codex_sessions_with_mtime()
    }

    fn latest_session_file(&self, cwd: &str) -> Option<PathBuf> {
        codex::latest_codex_session_file(cwd).or_else(codex::latest_codex_session_any)
    }

    fn parse_usage(&self, text: &str) -> Option<AiUsage> {
        codex::parse_codex_session(text)
    }

    fn update_usage(&self, text: &str, prev: AiUsage) -> AiUsage {
        codex::update_codex_session(text, prev)
    }

    fn parse_transcript(&self, text: &str) -> Vec<tn_agent::TranscriptEntry> {
        codex::parse_codex_transcript(text)
    }

    fn is_subscription(&self) -> bool {
        detect::codex_is_subscription()
    }
}

/// A seed registry containing Claude + Codex telemetry adapters. The default app
/// does **not** install this automatically; it starts with `AgentRegistry::new()`
/// and registers user manifests, proving Agent Host works without built-ins.
pub fn builtin_registry() -> AgentRegistry {
    AgentRegistry::new()
        .with(Arc::new(ClaudeAdapter::new()))
        .with(Arc::new(CodexAdapter::new()))
}

/// If a user `[[agents]]` manifest names a built-in agent (its id/aliases match
/// Claude's or Codex's command), return a telemetry adapter that uses **Tn's
/// built-in usage parser** but **the manifest's own presentation** (accent /
/// label / cursor, with `usage` enabled). So "add an agent whose command is
/// `claude`" gets a real usage ring for free — no sidecar, no built-in registered
/// by default — while keeping the user's chosen color/name. `None` for an unknown
/// command → the caller registers a generic (no-telemetry) agent instead.
///
/// This is the supported "user re-adds Claude/Codex telemetry" path: the platform
/// (`tn-agent`) stays agent-agnostic; only this `tn-ai` function knows the names.
pub fn builtin_adapter_for_manifest(m: &tn_config::AgentManifest) -> Option<Arc<dyn AgentAdapter>> {
    // The command tokens this manifest would be identified by.
    let probes: Vec<String> = if m.aliases.is_empty() {
        vec![m.id.to_ascii_lowercase()]
    } else {
        m.aliases.iter().map(|a| a.to_ascii_lowercase()).collect()
    };
    let seed = builtin_registry();
    let id = probes.iter().find_map(|p| seed.match_command(p))?;
    let seed_descriptor = seed.get(&id)?;
    // Keep the user's descriptor (color/label/Ink) but force `usage` + `transcript`
    // on, since the built-in parser supplies both (usage ring + Tn's own scrollable
    // history). Without forcing `transcript`, a config-declared Claude/Codex would
    // never start the transcript poller → the history overlay stays empty.
    let mut descriptor = AgentDescriptor::from_manifest(m);
    descriptor.default_args = seed_descriptor.default_args.clone();
    descriptor.default_env = seed_descriptor.default_env.clone();
    descriptor.capabilities.usage = true;
    descriptor.capabilities.transcript = true;
    match id.as_str() {
        "claude" => Some(Arc::new(ClaudeAdapter::with_descriptor(descriptor))),
        "codex" => Some(Arc::new(CodexAdapter::with_descriptor(descriptor))),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAUDE_SAMPLE: &str = r#"{"type":"assistant","message":{"model":"claude-opus-4-7","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":10,"cache_read_input_tokens":1000}}}"#;

    #[test]
    fn descriptors_carry_identity_and_quirks() {
        let c = ClaudeAdapter::new();
        assert_eq!(c.descriptor().id, AgentId::new("claude"));
        assert_eq!(c.descriptor().label, "Claude Code");
        assert_eq!(c.descriptor().short, "Claude");
        assert!(c.descriptor().manages_own_cursor); // Ink TUI
        // Native full-screen agents: no inline-mode injection (history comes from
        // the Tn-owned transcript, not terminal scrollback).
        assert!(c.descriptor().default_env.is_empty());
        assert!(c.descriptor().default_args.is_empty());
        assert!(c.descriptor().capabilities.usage);

        let x = CodexAdapter::new();
        assert_eq!(x.descriptor().id, AgentId::new("codex"));
        assert!(x.descriptor().default_args.is_empty());
        assert!(!x.descriptor().manages_own_cursor);
        assert!(x.descriptor().capabilities.usage);
    }

    #[test]
    fn adapter_parse_matches_underlying_parser() {
        // The adapter must produce exactly what the direct parser does — it's a
        // thin wrapper, not a reimplementation.
        let via_adapter = ClaudeAdapter::new().parse_usage(CLAUDE_SAMPLE);
        let direct = claude::parse_claude_session(CLAUDE_SAMPLE);
        assert_eq!(via_adapter, direct);
        assert!(via_adapter.is_some());
        // Codex text fed to the Claude adapter yields nothing.
        assert!(ClaudeAdapter::new().parse_usage("garbage").is_none());
    }

    #[test]
    fn builtin_registry_resolves_both_agents() {
        let reg = builtin_registry();
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
        // Descriptors are reachable by id with their default accents.
        assert_eq!(
            reg.get(&AgentId::new("claude")).unwrap().accent,
            Some(Color::new(0xF0, 0x91, 0x6D))
        );
        assert_eq!(
            reg.get(&AgentId::new("codex")).unwrap().accent,
            Some(Color::new(0x73, 0xDA, 0xCA))
        );
    }

    #[test]
    fn builtin_adapter_for_manifest_uses_builtin_parser_with_user_presentation() {
        // A user manifest whose command is `claude` → real Claude usage parsing,
        // but the user's own id / label / accent kept (usage forced on).
        let m = tn_config::AgentManifest {
            id: "cluade".into(),
            label: Some("我的 Claude".into()),
            short: None,
            aliases: vec!["claude".into()],
            accent: Some(Color::new(0xE0, 0xAF, 0x68)),
            glyph: None,
            manages_own_cursor: true,
            capabilities: vec![], // not declared → still forced on for a built-in
            runtime_support: vec![],
            allow_network: false,
            sidecar: None,
        };
        let a = builtin_adapter_for_manifest(&m).expect("claude command matched a built-in");
        let d = a.descriptor();
        assert_eq!(d.id, AgentId::new("cluade")); // user's id, not "claude"
        assert_eq!(d.label, "我的 Claude"); // user's label
        assert_eq!(d.accent, Some(Color::new(0xE0, 0xAF, 0x68))); // user's accent
        assert!(
            d.default_env.is_empty(),
            "manifest-backed Claude inherits the built-in's (now empty) launch env"
        );
        assert!(d.capabilities.usage); // built-in supplies usage → ring unlocked
        assert!(d.capabilities.transcript); // built-in supplies history → overlay unlocked
        assert!(d.supports_runtime(AgentRuntimeKind::LocalPty)); // still PTY-launchable
                                                                 // And it actually parses Claude logs (built-in behavior, not generic).
        assert_eq!(
            a.parse_usage(CLAUDE_SAMPLE),
            claude::parse_claude_session(CLAUDE_SAMPLE)
        );

        // An unknown command → None (caller registers a generic, no-telemetry agent).
        let unknown = tn_config::AgentManifest {
            id: "gemini".into(),
            aliases: vec!["gemini".into()],
            ..m
        };
        assert!(builtin_adapter_for_manifest(&unknown).is_none());
    }

    #[test]
    fn builtin_adapter_for_codex_manifest_has_no_inline_default_arg() {
        let m = tn_config::AgentManifest {
            id: "codex".into(),
            label: Some("Codex".into()),
            short: None,
            aliases: vec!["codex".into()],
            accent: None,
            glyph: None,
            manages_own_cursor: false,
            capabilities: vec![],
            runtime_support: vec![],
            allow_network: false,
            sidecar: None,
        };
        let a = builtin_adapter_for_manifest(&m).expect("codex command matched a built-in");
        assert!(a.descriptor().default_args.is_empty());
    }
}
