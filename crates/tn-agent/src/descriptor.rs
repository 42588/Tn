//! [`AgentDescriptor`] — an agent's static identity, presentation, and declared
//! capabilities. The UI reads **only** this for display; it never matches on a
//! concrete agent. A config-declared agent with no built-in adapter gets a
//! [`AgentDescriptor::generic`] (terminal-only, no usage).

use tn_config::Color;

use crate::AgentId;

/// Where an agent process/protocol runs — **not** where its files live. Kept
/// strictly separate from `FileNamespace` (the UI's file-I/O namespace): an
/// SSH-runtime agent still can't drive Explorer/Quick Look until a remote FS is
/// wired.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentRuntimeKind {
    LocalPty,
    WslPty,
    SshPty,
    /// A local/remote sidecar daemon reached over stdio or a local socket.
    RemoteDaemon,
    /// HTTP(S) agent runtime. Network use is denied by default unless a manifest
    /// explicitly allows it and the user confirms at the host layer.
    Http,
    /// WebSocket agent runtime. Same network-permission model as [`Http`](Self::Http).
    WebSocket,
    /// A structured, non-terminal protocol endpoint (Tn Agent Protocol / JSON-RPC).
    Structured,
}

impl AgentRuntimeKind {
    pub fn is_pty(self) -> bool {
        matches!(self, Self::LocalPty | Self::WslPty | Self::SshPty)
    }

    pub fn is_networked(self) -> bool {
        matches!(self, Self::Http | Self::WebSocket | Self::RemoteDaemon)
    }
}

/// Network access policy for non-PTY runtimes declared by a manifest. The safe
/// default is [`Deny`](Self::Deny). [`Ask`](Self::Ask) means the descriptor may
/// be used for networked runtimes only after the host has shown a user
/// confirmation; it is never a silent allow.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentNetworkPolicy {
    Deny,
    Ask,
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
    /// Extra args inserted when launching, before the profile's own args.
    /// This lets top-level flags (for example "keep native scrollback") precede
    /// subcommands such as `resume`.
    pub default_args: Vec<String>,
    /// Environment variables applied when launching this agent in a local PTY.
    pub default_env: Vec<(String, String)>,
    pub capabilities: AgentCapabilities,
    pub runtime_support: Vec<AgentRuntimeKind>,
    pub network_policy: AgentNetworkPolicy,
    /// The agent paints/owns its own cursor (Ink TUIs like Claude); the terminal
    /// must hide its block cursor. Replaces the old Claude-only `force_hide_cursor`.
    pub manages_own_cursor: bool,
    /// Argv for a stdio/JSONL telemetry **sidecar** (from manifest `sidecar`), if
    /// any. The host spawns it per-pane as an `ExternalProcessAdapter` to get
    /// realtime events without a built-in adapter. `None` = log-only / generic.
    pub realtime_command: Option<Vec<String>>,
}

/// How the host should treat a descriptor's sidecar at launch time — the result
/// of the default-deny network policy. Pure/headless so the launch decision is
/// unit-testable without spawning anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidecarLaunch {
    /// No sidecar declared → nothing to spawn.
    None,
    /// A local stdio sidecar → spawn it now.
    SpawnNow,
    /// A network-reaching sidecar (`allow_network`) → ask the user first.
    Confirm,
}

impl AgentDescriptor {
    /// Does `command` name this agent? Any alias as a substring of the lowercased
    /// command (mirrors the old `agent_kind_for_command`: program name or the
    /// `& 'cmd'` pwsh-hosting form).
    pub fn matches_command(&self, command: &str) -> bool {
        let lc = command.to_ascii_lowercase();
        self.command_aliases.iter().any(|a| lc.contains(a.as_str()))
    }

    pub fn supports_runtime(&self, runtime: AgentRuntimeKind) -> bool {
        self.runtime_support.contains(&runtime)
    }

    pub fn requires_network_confirmation(&self, runtime: AgentRuntimeKind) -> bool {
        runtime.is_networked() && self.network_policy == AgentNetworkPolicy::Ask
    }

    pub fn runtime_allowed_after_confirmation(&self, runtime: AgentRuntimeKind) -> bool {
        self.supports_runtime(runtime)
            && (!runtime.is_networked() || self.network_policy == AgentNetworkPolicy::Ask)
    }

    /// What the host should do with this descriptor's sidecar at launch (the
    /// default-deny network gate, in one place): nothing if no sidecar; a user
    /// confirmation when the sidecar may reach the network (`allow_network` →
    /// `network_policy == Ask`); otherwise spawn the local stdio sidecar now.
    ///
    /// Keyed on `network_policy` (the **sidecar's** network property), **not** on
    /// `runtime_support` — the latter is where the *agent itself* runs (its PTY),
    /// independent of whether its telemetry sidecar networks. (Conflating them
    /// made a `claude` agent with a networked sidecar look non-PTY → refused →
    /// fell back to a plain shell.)
    pub fn sidecar_launch(&self) -> SidecarLaunch {
        if self.realtime_command.is_none() {
            SidecarLaunch::None
        } else if self.network_policy == AgentNetworkPolicy::Ask {
            SidecarLaunch::Confirm
        } else {
            SidecarLaunch::SpawnNow
        }
    }

    /// Build a descriptor from a user config manifest (`[[agents]]`) — the
    /// config-level access tier. `terminal` + `cwd_sync` + `git_diff` are always
    /// on (the baseline a hosted agent gets); `manifest.capabilities` lists extra
    /// slots to enable (unknown names ignored). Aliases default to `[id]`.
    pub fn from_manifest(m: &tn_config::AgentManifest) -> Self {
        let label = m.label.clone().unwrap_or_else(|| m.id.clone());
        let short = m.short.clone().unwrap_or_else(|| label.clone());
        let mut capabilities = AgentCapabilities::terminal_only();
        for c in &m.capabilities {
            match c.as_str() {
                "terminal" => capabilities.terminal = true,
                "usage" => capabilities.usage = true,
                "transcript" => capabilities.transcript = true,
                "cwd_sync" => capabilities.cwd_sync = true,
                "git_diff" => capabilities.git_diff = true,
                "permission_prompts" => capabilities.permission_prompts = true,
                "tool_calls" => capabilities.tool_calls = true,
                "checkpoints" => capabilities.checkpoints = true,
                _ => {} // unknown capability name → ignored
            }
        }
        let command_aliases = if m.aliases.is_empty() {
            vec![m.id.to_ascii_lowercase()]
        } else {
            m.aliases.iter().map(|a| a.to_ascii_lowercase()).collect()
        };
        let runtime_support = runtimes_from_manifest(&m.runtime_support);
        Self {
            id: AgentId::new(m.id.clone()),
            label,
            short,
            command_aliases,
            accent: m.accent,
            glyph: m.glyph.clone(),
            default_args: Vec::new(),
            default_env: Vec::new(),
            capabilities,
            runtime_support,
            network_policy: if m.allow_network {
                AgentNetworkPolicy::Ask
            } else {
                AgentNetworkPolicy::Deny
            },
            manages_own_cursor: m.manages_own_cursor,
            realtime_command: parse_sidecar(m.sidecar.as_deref()),
        }
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
            default_env: Vec::new(),
            capabilities: AgentCapabilities::terminal_only(),
            runtime_support: pty_runtimes(),
            network_policy: AgentNetworkPolicy::Deny,
            manages_own_cursor: false,
            realtime_command: None,
        }
    }
}

/// Split a manifest `sidecar = "..."` string into argv (whitespace-separated).
/// `None`/blank → `None` (no sidecar). Quoting isn't handled yet — a path with
/// spaces would need a future array form; the common `prog --flag` case works.
fn parse_sidecar(raw: Option<&str>) -> Option<Vec<String>> {
    let parts: Vec<String> = raw?.split_whitespace().map(String::from).collect();
    (!parts.is_empty()).then_some(parts)
}

pub fn pty_runtimes() -> Vec<AgentRuntimeKind> {
    vec![
        AgentRuntimeKind::LocalPty,
        AgentRuntimeKind::WslPty,
        AgentRuntimeKind::SshPty,
    ]
}

fn runtimes_from_manifest(raw: &[String]) -> Vec<AgentRuntimeKind> {
    if raw.is_empty() {
        return pty_runtimes();
    }
    let mut out = Vec::new();
    for r in raw {
        let Some(kind) = runtime_from_str(r) else {
            continue;
        };
        if !out.contains(&kind) {
            out.push(kind);
        }
    }
    if out.is_empty() {
        pty_runtimes()
    } else {
        out
    }
}

fn runtime_from_str(s: &str) -> Option<AgentRuntimeKind> {
    match s.trim().to_ascii_lowercase().replace('-', "_").as_str() {
        "local" | "local_pty" | "pty" => Some(AgentRuntimeKind::LocalPty),
        "wsl" | "wsl_pty" => Some(AgentRuntimeKind::WslPty),
        "ssh" | "ssh_pty" => Some(AgentRuntimeKind::SshPty),
        "daemon" | "remote_daemon" | "sidecar" => Some(AgentRuntimeKind::RemoteDaemon),
        "http" | "https" => Some(AgentRuntimeKind::Http),
        "websocket" | "ws" | "wss" => Some(AgentRuntimeKind::WebSocket),
        "structured" | "protocol" | "tn_agent_protocol" => Some(AgentRuntimeKind::Structured),
        _ => None,
    }
}
