//! Built-in [`AgentAdapter`]s for Claude Code and Codex — the platform's two
//! seed providers. Each pairs a static [`AgentDescriptor`] (identity, accent,
//! capabilities, cursor quirk) with the existing `claude`/`codex` log parsers.
//!
//! These are **removable**: tn-app builds the registry from [`builtin_registry`],
//! and the P6 end state swaps that for an empty registry (pure shell host) +
//! manually-registered agents. The platform (`tn-agent`) has no knowledge of
//! either agent — all the Claude/Codex specifics live right here.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use tn_agent::{
    AgentAdapter, AgentCapabilities, AgentDescriptor, AgentId, AgentRegistry, AgentRuntimeKind,
    AiUsage,
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

/// Terminal + usage + cwd sync + git-diff rail — the full built-in agent surface.
fn full_capabilities() -> AgentCapabilities {
    AgentCapabilities {
        terminal: true,
        usage: true,
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
        default_args: Vec::new(),
        capabilities: full_capabilities(),
        runtime_support: pty_runtimes(),
        // Claude Code is an Ink TUI that paints its own cursor → hide ours.
        manages_own_cursor: true,
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
        default_args: Vec::new(),
        capabilities: full_capabilities(),
        runtime_support: pty_runtimes(),
        manages_own_cursor: false,
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

    fn is_subscription(&self) -> bool {
        detect::codex_is_subscription()
    }
}

/// The default built-in registry: Claude + Codex. tn-app wires this at startup.
/// **Removing the built-ins (P6) = returning `AgentRegistry::new()` instead** —
/// the platform stays identical, only the seed providers change.
pub fn builtin_registry() -> AgentRegistry {
    AgentRegistry::new()
        .with(Arc::new(ClaudeAdapter::new()))
        .with(Arc::new(CodexAdapter::new()))
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
        assert!(c.descriptor().capabilities.usage);

        let x = CodexAdapter::new();
        assert_eq!(x.descriptor().id, AgentId::new("codex"));
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
}
