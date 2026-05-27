//! [`LaunchSpec`]: turning a config profile into a spawnable pane.
//!
//! One launch path per `ProfileKind` (WSL / SSH / native pwsh / hosted command);
//! see [`LaunchSpec::from_profile`]. Pure data + selection logic, no GPUI — the
//! view ([`super::TerminalView::new`]) consumes a `LaunchSpec` to pick a backend.

use tn_ai::AgentKind;

/// How to launch a pane's process: program + args + whether to inject the pwsh
/// shell-integration script. Built from a `tn_config::Profile` (command-bearing
/// shell/agent profiles), or the default local PowerShell via [`LaunchSpec::pwsh`].
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    pub program: String,
    pub args: Vec<String>,
    pub integrate_pwsh: bool,
    /// Which agent this pane hosts (launch-intent signal for per-pane usage).
    /// `None` for a plain shell — usage is then auto-detected by log freshness.
    pub agent: Option<AgentKind>,
    /// When set, this pane is a remote SSH session (M2): the view spawns an
    /// `SshBackend` instead of a local ConPTY, and `program`/`args` are unused
    /// (`program` is just the `user@host` label).
    pub ssh: Option<tn_pty::SshConfig>,
}

impl LaunchSpec {
    /// Default local PowerShell pane, with OSC 133 shell integration.
    pub fn pwsh() -> Self {
        Self {
            program: "powershell.exe".into(),
            args: vec!["-NoLogo".into()],
            ssh: None,
            integrate_pwsh: true,
            agent: None,
        }
    }

    /// Derive from a config profile if it carries a command (shell + agent).
    /// WSL/SSH profiles (no command yet, M2) return `None`.
    ///
    /// Native pwsh runs directly (with integration). Any other command (Claude /
    /// Codex / scripts) is **hosted inside pwsh** via `-NoExit -Command "& '…'"`,
    /// because on Windows those are extensionless npm shims that `CreateProcessW`
    /// can't execute directly — pwsh resolves them via PATH + PATHEXT, and the
    /// shell survives the agent's exit (back to a prompt).
    pub fn from_profile(p: &tn_config::Profile) -> Option<Self> {
        Self::from_profile_inner(p, true)
    }

    /// Like [`from_profile`](Self::from_profile), but the pwsh hosting a non-pwsh
    /// agent omits `-NoExit`, so exiting the agent exits the PTY. The quick
    /// terminal uses this so "exit claude" returns to its launcher instead of
    /// leaving a lingering pwsh prompt under a stale agent header.
    pub fn from_profile_ephemeral(p: &tn_config::Profile) -> Option<Self> {
        Self::from_profile_inner(p, false)
    }

    fn from_profile_inner(p: &tn_config::Profile, persist: bool) -> Option<Self> {
        // One launch path per profile kind (待优化清单 §6.3). WSL/SSH ignore the
        // command field; everything else needs a command, then forks on whether
        // it's a native pwsh (run directly + integrated) or another program
        // (hosted inside pwsh, since Windows can't `CreateProcessW` an npm shim).
        match p.kind {
            tn_config::ProfileKind::Wsl => Some(Self::launch_wsl(p)),
            tn_config::ProfileKind::Ssh => Self::launch_ssh(p),
            _ => {
                let command = p.command.clone()?;
                // Agent identity: an explicit `agent = "..."` wins, else infer from
                // the command (`claude` / `codex`). This launch-intent signal is
                // what the status bar reads, so a Codex pane never shows Claude's
                // usage.
                let agent = p
                    .agent
                    .as_deref()
                    .and_then(tn_ai::agent_kind_for_command)
                    .or_else(|| tn_ai::agent_kind_for_command(&command));
                let lc = command.to_ascii_lowercase();
                if lc.contains("powershell") || lc.contains("pwsh") {
                    Some(Self::launch_pwsh(command, &p.args, agent))
                } else {
                    Some(Self::launch_hosted(command, &p.args, agent, persist))
                }
            }
        }
    }

    /// WSL (M2): host the distro's login shell via `wsl.exe -d <distro>`. ConPTY
    /// runs `wsl.exe` like any program, so no special backend is needed; no pwsh
    /// integration (the distro runs bash/zsh). An empty/absent distro launches
    /// WSL's default distro.
    fn launch_wsl(p: &tn_config::Profile) -> Self {
        let mut args = Vec::new();
        if let Some(distro) = p.distro.as_deref().filter(|d| !d.is_empty()) {
            args.push("-d".to_string());
            args.push(distro.to_string());
        }
        Self { program: "wsl.exe".into(), args, integrate_pwsh: false, agent: None, ssh: None }
    }

    /// SSH (M2b): a remote session over russh. `host` (optionally `host:port`) +
    /// `user` build an `SshConfig`; the view spawns an `SshBackend`. `None` if the
    /// profile carries no host.
    fn launch_ssh(p: &tn_config::Profile) -> Option<Self> {
        let host = p.host.clone()?;
        let cfg = tn_pty::SshConfig::parse(&host, p.user.as_deref());
        Some(Self {
            program: format!("{}@{}", cfg.user, cfg.host),
            args: Vec::new(),
            integrate_pwsh: false,
            agent: None,
            ssh: Some(cfg),
        })
    }

    /// Native PowerShell: run directly with OSC 133 integration. Empty args
    /// default to `-NoLogo`.
    fn launch_pwsh(command: String, profile_args: &[String], agent: Option<AgentKind>) -> Self {
        let mut args = profile_args.to_vec();
        if args.is_empty() {
            args.push("-NoLogo".into());
        }
        Self { program: command, args, integrate_pwsh: true, agent, ssh: None }
    }

    /// Host a non-pwsh command inside pwsh (single-quote-escaped call operator),
    /// because Windows can't `CreateProcessW` an extensionless npm shim. With
    /// `persist` we keep `-NoExit` so the shell survives the agent's exit (back to
    /// a prompt); without it, pwsh exits when the agent does (the quick terminal's
    /// "exit claude → launcher" path).
    fn launch_hosted(
        command: String,
        profile_args: &[String],
        agent: Option<AgentKind>,
        persist: bool,
    ) -> Self {
        let mut invoke = format!("& '{}'", command.replace('\'', "''"));
        for a in profile_args {
            invoke.push_str(&format!(" '{}'", a.replace('\'', "''")));
        }
        let mut args = vec!["-NoLogo".to_string()];
        if persist {
            args.push("-NoExit".into());
        }
        args.push("-Command".into());
        args.push(invoke);
        Self { program: "powershell.exe".into(), args, integrate_pwsh: false, agent, ssh: None }
    }
}
