//! [`AgentDescriptor`] — an agent's static identity, presentation, and declared
//! capabilities. The UI reads **only** this for display; it never matches on a
//! concrete agent. A config-declared agent with no built-in adapter gets a
//! [`AgentDescriptor::generic`] (terminal-only, no usage).

use tn_config::Color;

use crate::AgentId;

/// Where an agent process/protocol runs — **not** where its files live. Kept
/// strictly separate from `FileNamespace` (the UI's file-I/O namespace): an
/// SSH-runtime agent still can't drive Explorer/Quick Look until a remote FS is
/// wired. Only the PTY family is implemented this round; the rest are reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRuntimeKind {
    LocalPty,
    WslPty,
    SshPty,
    // Reserved for future non-PTY runtimes (RemoteDaemon / Http / WebSocket /
    // Structured); not implemented this round — the enum is open so the UI's
    // "agent is a local process" assumption can be lifted later without churn.
}

/// Capability flags → which Universal Agent Surface slots render for this agent
/// (P4). An agent without `usage` simply hides the usage ring instead of showing
/// an empty one; a plain hosted CLI still works as a terminal session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AgentCapabilities {
    pub terminal: bool,
    pub usage: bool,
    pub transcript: bool,
    pub cwd_sync: bool,
    pub git_diff: bool,
    pub permission_prompts: bool,
    pub tool_calls: bool,
    pub checkpoints: bool,
}

impl AgentCapabilities {
    /// A plain hosted CLI agent: a terminal + cwd sync + the git-diff rail,
    /// nothing agent-private. The baseline for config-level (no-adapter) agents.
    pub fn terminal_only() -> Self {
        Self {
            terminal: true,
            cwd_sync: true,
            git_diff: true,
            ..Self::default()
        }
    }
}

/// Static identity + presentation + capability declaration for an agent provider.
#[derive(Clone, Debug)]
pub struct AgentDescriptor {
    pub id: AgentId,
    /// Full name for the agent header (e.g. "Claude Code").
    pub label: String,
    /// Short label for tab chrome / status bar (e.g. "Claude").
    pub short: String,
    /// Lowercased command substrings that identify this agent (replaces the old
    /// `agent_kind_for_command`). First registered match wins in the registry.
    pub command_aliases: Vec<String>,
    /// Default identity accent; a theme `[agents.<id>]` entry overrides it.
    pub accent: Option<Color>,
    /// Optional embedded SVG icon name for launch tiles / header.
    pub glyph: Option<String>,
    /// Extra args appended when launching (beyond the profile's own args).
    pub default_args: Vec<String>,
    pub capabilities: AgentCapabilities,
    pub runtime_support: Vec<AgentRuntimeKind>,
    /// The agent paints/owns its own cursor (Ink TUIs like Claude); the terminal
    /// must hide its block cursor. Replaces the old Claude-only `force_hide_cursor`.
    pub manages_own_cursor: bool,
}

impl AgentDescriptor {
    /// Does `command` name this agent? Any alias as a substring of the lowercased
    /// command (mirrors the old `agent_kind_for_command`: program name or the
    /// `& 'cmd'` pwsh-hosting form).
    pub fn matches_command(&self, command: &str) -> bool {
        let lc = command.to_ascii_lowercase();
        self.command_aliases.iter().any(|a| lc.contains(a.as_str()))
    }

    /// A generic descriptor for a config-declared agent with no built-in adapter:
    /// terminal-only capabilities, name-derived labels, no usage, all PTY runtimes.
    /// This is the "declare a command in TOML → it appears as an agent" path.
    pub fn generic(id: AgentId, label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            short: label.clone(),
            command_aliases: vec![id.as_str().to_ascii_lowercase()],
            id,
            label,
            accent: None,
            glyph: None,
            default_args: Vec::new(),
            capabilities: AgentCapabilities::terminal_only(),
            runtime_support: vec![
                AgentRuntimeKind::LocalPty,
                AgentRuntimeKind::WslPty,
                AgentRuntimeKind::SshPty,
            ],
            manages_own_cursor: false,
        }
    }
}
