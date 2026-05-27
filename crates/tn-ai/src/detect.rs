//! Agent detection + per-cwd session resolution.
//!
//! A pane's agent is known from **launch intent** (we launched `claude` /
//! `codex`), which is the strongest signal — see [`agent_kind_for_command`].
//! When that's unknown (a plain shell where the user typed an agent by hand),
//! we fall back to **freshness**: whichever agent wrote the most recently
//! modified session log for this `cwd` is the one on screen. The richer signals
//! (process tree / OSC title / banner) are deferred; freshness covers the
//! dogfooding case (running `claude` in a pwsh pane) without any of them.

use std::path::PathBuf;
use std::time::SystemTime;

use crate::{claude, codex, AgentKind, AiUsage};

/// A resolved session log: which agent wrote it, where, and when last touched.
#[derive(Clone, Debug)]
pub struct SessionRef {
    pub kind: AgentKind,
    pub path: PathBuf,
    pub mtime: SystemTime,
}

fn session_ref(kind: AgentKind, path: PathBuf) -> Option<SessionRef> {
    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
    Some(SessionRef { kind, path, mtime })
}

/// The newest session log to read for a pane in `cwd`: an explicit `hint` (from
/// launch intent) wins; otherwise pick the agent whose latest session for `cwd`
/// was modified most recently. `None` when neither agent has a session here.
pub fn resolve_session(cwd: &str, hint: Option<AgentKind>) -> Option<SessionRef> {
    match hint {
        Some(AgentKind::ClaudeCode) => {
            session_ref(AgentKind::ClaudeCode, claude::latest_session_file(cwd)?)
        }
        Some(AgentKind::Codex) => {
            session_ref(AgentKind::Codex, codex::latest_codex_session_file(cwd)?)
        }
        None => {
            let c = claude::latest_session_file(cwd)
                .and_then(|p| session_ref(AgentKind::ClaudeCode, p));
            let x = codex::latest_codex_session_file(cwd)
                .and_then(|p| session_ref(AgentKind::Codex, p));
            match (c, x) {
                (Some(a), Some(b)) => Some(if a.mtime >= b.mtime { a } else { b }),
                (a, b) => a.or(b),
            }
        }
    }
}

/// Parse a session file's text for the given agent.
pub fn parse_session(kind: AgentKind, text: &str) -> Option<AiUsage> {
    match kind {
        AgentKind::ClaudeCode => claude::parse_claude_session(text),
        AgentKind::Codex => codex::parse_codex_session(text),
    }
}

/// Read + parse usage for `cwd`, returning which agent it belongs to. `hint`
/// is the launch-intent agent (or `None` to auto-detect by freshness).
pub fn usage_for_cwd(cwd: &str, hint: Option<AgentKind>) -> Option<(AgentKind, AiUsage)> {
    let sref = resolve_session(cwd, hint)?;
    let text = std::fs::read_to_string(&sref.path).ok()?;
    Some((sref.kind, parse_session(sref.kind, &text)?))
}

/// Classify a launch command into an agent kind (the launch-intent signal).
/// Matches the program name or the `& 'cmd'` form used to host agents in pwsh.
pub fn agent_kind_for_command(command: &str) -> Option<AgentKind> {
    let lc = command.to_ascii_lowercase();
    if lc.contains("claude") {
        Some(AgentKind::ClaudeCode)
    } else if lc.contains("codex") {
        Some(AgentKind::Codex)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_classification() {
        assert_eq!(agent_kind_for_command("claude"), Some(AgentKind::ClaudeCode));
        assert_eq!(agent_kind_for_command("C:/npm/codex.cmd"), Some(AgentKind::Codex));
        assert_eq!(agent_kind_for_command("& 'claude' '--resume'"), Some(AgentKind::ClaudeCode));
        assert_eq!(agent_kind_for_command("powershell.exe"), None);
    }

    #[test]
    fn parse_session_dispatches_by_kind() {
        let claude = r#"{"type":"assistant","message":{"model":"claude-opus-4-7","usage":{"input_tokens":10,"output_tokens":5,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
        let u = parse_session(AgentKind::ClaudeCode, claude).unwrap();
        assert_eq!(u.model, "claude-opus-4-7");
        // Codex text fed to the Claude parser yields nothing (and vice versa).
        assert!(parse_session(AgentKind::Codex, claude).is_none());
    }
}
