//! [`LaunchSpec`]: turning a config profile into a spawnable pane.
//!
//! One launch path per `ProfileKind` (WSL / SSH / native pwsh / hosted command);
//! see [`LaunchSpec::from_profile`]. Pure data + selection logic, no GPUI — the
//! view ([`super::TerminalView::new`]) consumes a `LaunchSpec` to pick a backend.

use tn_ai::AgentKind;

use super::AGENT_EXIT_SENTINEL;

/// Which filesystem namespace a pane's cwd belongs to. The terminal may display
/// any cwd string, but only explicitly-mapped namespaces may drive host file I/O.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FileNamespace {
    Host,
    Wsl { distro: Option<String> },
    Ssh,
}

impl FileNamespace {
    pub fn host_process_path_from_cwd(&self, cwd: &str) -> Option<std::path::PathBuf> {
        match self {
            FileNamespace::Host => host_process_path(cwd),
            FileNamespace::Wsl { .. } | FileNamespace::Ssh => None,
        }
    }

    pub fn browsable_path_from_cwd(&self, cwd: &str) -> Option<std::path::PathBuf> {
        match self {
            FileNamespace::Host => host_process_path(cwd),
            FileNamespace::Wsl { distro } => wsl_unc_path(distro.as_deref()?, cwd),
            FileNamespace::Ssh => None,
        }
    }
}

pub fn is_host_process_path(path: &std::path::Path) -> bool {
    let s = path.to_string_lossy();
    host_process_path(&s).is_some() && !is_wsl_unc(&s)
}

fn host_process_path(cwd: &str) -> Option<std::path::PathBuf> {
    let s = cwd.trim();
    if s.len() >= 3 {
        let b = s.as_bytes();
        if b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/') {
            return Some(std::path::PathBuf::from(s));
        }
    }
    if (s.starts_with(r"\\") || s.starts_with("//")) && !is_wsl_unc(s) {
        return Some(std::path::PathBuf::from(s));
    }
    None
}

fn is_wsl_unc(path: &str) -> bool {
    let s = path.replace('/', "\\").to_ascii_lowercase();
    s.starts_with(r"\\wsl$\") || s.starts_with(r"\\wsl.localhost\")
}

fn wsl_unc_path(distro: &str, cwd: &str) -> Option<std::path::PathBuf> {
    let cwd = cwd.trim();
    if cwd.is_empty() || !cwd.starts_with('/') {
        return None;
    }
    let rel = cwd.trim_start_matches('/').replace('/', "\\");
    let prefix = format!(r"\\wsl$\{}", distro);
    Some(if rel.is_empty() {
        std::path::PathBuf::from(prefix)
    } else {
        std::path::PathBuf::from(format!(r"{prefix}\{rel}"))
    })
}

/// Which shell-integration flavour to inject at spawn time.
/// `None` means no shell-integration markers are injected (plain WSL/SSH without
/// the extra hooks, or a hosted agent pane that has no shell prompt to annotate).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellIntegration {
    Pwsh,
    Bash,
}

/// How to launch a pane's process: program + args + whether to inject shell-
/// integration scripts. Built from a `tn_config::Profile` (command-bearing
/// shell/agent profiles), or the default local PowerShell via [`LaunchSpec::pwsh`].
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    pub program: String,
    pub args: Vec<String>,
    /// When true, the render reserves 212 px for the activity rail from the start
    /// so the terminal never resizes when sync_shell_agent promotes a shell to an
    /// agent, avoiding input lag. Derived from [`Self::shell_integration`] being set.
    pub integrate_pwsh: bool,
    /// When set, the pane's shell is instrumented with OSC 133/633 markers so
    /// command blocks, cwd tracking, and agent detection work automatically.
    pub shell_integration: Option<ShellIntegration>,
    /// Which agent this pane hosts (launch-intent signal for per-pane usage).
    /// `None` for a plain shell — usage is then auto-detected by log freshness.
    pub agent: Option<AgentKind>,
    /// When set, this pane is a remote SSH session (M2): the view spawns an
    /// `SshBackend` instead of a local ConPTY, and `program`/`args` are unused
    /// (`program` is just the `user@host` label).
    pub ssh: Option<tn_pty::SshConfig>,
    /// Working directory for the spawned process. When None at spawn time,
    /// Workspace::spawn_pane_with fills it from the explorer root so the
    /// process inherits the file-tree directory.
    pub cwd: Option<std::path::PathBuf>,
    pub file_namespace: FileNamespace,
}

impl LaunchSpec {
    /// Default local PowerShell pane, with OSC 133 shell integration.
    pub fn pwsh() -> Self {
        Self {
            program: "powershell.exe".into(),
            args: vec!["-NoLogo".into()],
            ssh: None,
            shell_integration: Some(ShellIntegration::Pwsh),
            integrate_pwsh: true,
            agent: None,
            cwd: None,
            file_namespace: FileNamespace::Host,
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

    /// WSL (M2): host the distro's login shell via `wsl.exe -d <distro>`.
    /// ConPTY runs `wsl.exe` like any program, so no special backend is needed.
    /// Now includes bash shell integration (OSC 133/633) via `--rcfile` injection
    /// so command blocks, cwd tracking, and agent detection work automatically.
    fn launch_wsl(p: &tn_config::Profile) -> Self {
        let mut args = Vec::new();
        if let Some(distro) = p.distro.as_deref().filter(|d| !d.is_empty()) {
            args.push("-d".to_string());
            args.push(distro.to_string());
        }
        args.push("--cd".to_string());
        args.push("~".to_string());
        Self {
            program: "wsl.exe".into(),
            args,
            shell_integration: Some(ShellIntegration::Bash),
            integrate_pwsh: true,
            agent: None,
            ssh: None,
            cwd: None,
            file_namespace: FileNamespace::Wsl {
                distro: p.distro.clone(),
            },
        }
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
            shell_integration: None,
            integrate_pwsh: false,
            agent: None,
            cwd: None,
            ssh: Some(cfg),
            file_namespace: FileNamespace::Ssh,
        })
    }

    /// Native PowerShell: run directly with OSC 133 integration. Empty args
    /// default to `-NoLogo`.
    fn launch_pwsh(command: String, profile_args: &[String], agent: Option<AgentKind>) -> Self {
        let mut args = profile_args.to_vec();
        if args.is_empty() {
            args.push("-NoLogo".into());
        }
        Self {
            program: command,
            args,
            shell_integration: Some(ShellIntegration::Pwsh),
            integrate_pwsh: true,
            agent,
            ssh: None,
            cwd: None,
            file_namespace: FileNamespace::Host,
        }
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
        // After a *persistent* agent exits, the surviving `-NoExit` pwsh runs this
        // and sets a sentinel title; the reader sees it and clears the agent so the
        // pane reverts to a plain shell instead of keeping a stale agent header.
        // (Ephemeral panes exit pwsh outright → `ProcessExited`, so no sentinel.)
        if agent.is_some() && persist {
            invoke.push_str(&format!(
                "; $Host.UI.RawUI.WindowTitle = '{AGENT_EXIT_SENTINEL}'"
            ));
        }
        let mut args = vec!["-NoLogo".to_string()];
        if persist {
            args.push("-NoExit".into());
        }
        args.push("-Command".into());
        args.push(invoke);
        Self {
            program: "powershell.exe".into(),
            args,
            shell_integration: None,
            integrate_pwsh: false,
            agent,
            ssh: None,
            cwd: None,
            file_namespace: FileNamespace::Host,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_namespace_rejects_unix_paths() {
        assert!(FileNamespace::Host
            .browsable_path_from_cwd(r"C:\Users\Gua")
            .is_some());
        assert!(FileNamespace::Host
            .browsable_path_from_cwd("/home/gua/project")
            .is_none());
        assert!(FileNamespace::Ssh
            .browsable_path_from_cwd("/Users/gua/project")
            .is_none());
    }

    #[test]
    fn wsl_namespace_maps_to_wsl_unc_not_windows_drive() {
        let ns = FileNamespace::Wsl {
            distro: Some("Ubuntu".into()),
        };
        let p = ns.browsable_path_from_cwd("/home/gua/project").unwrap();
        let s = p.to_string_lossy().replace('/', "\\");
        assert!(s.starts_with(r"\\wsl$\Ubuntu\home\gua\project"));
        assert!(ns.host_process_path_from_cwd("/home/gua/project").is_none());
    }

    #[test]
    fn host_process_path_rejects_wsl_unc() {
        assert!(is_host_process_path(std::path::Path::new(r"D:\coder\Tn")));
        assert!(!is_host_process_path(std::path::Path::new(
            r"\\wsl$\Ubuntu\home\gua"
        )));
    }
}
