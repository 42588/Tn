//! Agent detection + session resolution.
//!
//! A pane's agent is known from **launch intent** (we launched `claude` /
//! `codex`), which is the strongest signal — see [`agent_kind_for_command`].
//!
//! Binding a pane to *its* session log is [`resolve_session_for_pane`]: it picks
//! the session that went **stale→fresh after the pane launched** (created anew or
//! resumed), ignoring one already active at launch (a concurrent dev agent). The
//! older cwd/freshness resolver ([`resolve_session`]) remains for callers that
//! only have a cwd. The richer signals (process tree / OSC title / banner) are
//! deferred — activity-after-launch covers the dogfooding case without them.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tn_agent::{AgentAdapter, SessionRef as AgentSessionRef};

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
        // Prefer the cwd-specific session; fall back to the newest session of
        // that agent overall — a freshly-launched agent pane's session is the
        // newest, and its cwd often differs from the app cwd (e.g. Codex in ~).
        Some(AgentKind::ClaudeCode) => {
            let path =
                claude::latest_session_file(cwd).or_else(claude::latest_claude_session_any)?;
            session_ref(AgentKind::ClaudeCode, path)
        }
        Some(AgentKind::Codex) => {
            let path =
                codex::latest_codex_session_file(cwd).or_else(codex::latest_codex_session_any)?;
            session_ref(AgentKind::Codex, path)
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

/// A session whose mtime sat within this window *before* launch was already
/// being written when the pane started → it's a **concurrently-running** session
/// (e.g. a dev Claude editing this repo while Tn runs from it), not this pane's.
/// Our session goes from stale/absent at launch to freshly written *after*.
const SESSION_ACTIVE_MARGIN: Duration = Duration::from_secs(120);

/// Snapshot every `kind` session's mtime — the **baseline** a pane captures at
/// launch. Sessions already fresh in this snapshot are someone else's; the pane's
/// own is the one that transitions stale→fresh afterward (see
/// [`resolve_session_for_pane`]).
pub fn session_mtimes(kind: AgentKind) -> HashMap<PathBuf, SystemTime> {
    let sessions = match kind {
        AgentKind::ClaudeCode => claude::claude_sessions_with_mtime(),
        AgentKind::Codex => codex::codex_sessions_with_mtime(),
    };
    sessions.into_iter().collect()
}

/// Pick the session this pane activated from a `(path, mtime)` list + the
/// launch-time `baseline`: the newest-mtime one that (a) is absent from baseline
/// (created after launch) or was **stale** then (baseline mtime older than
/// `launched_at − SESSION_ACTIVE_MARGIN`), **and** (b) has since been written
/// (`mtime ≥ launched_at`). Splitting this out keeps it unit-testable with
/// synthetic timestamps (no filesystem).
fn pick_pane_session(
    sessions: Vec<(PathBuf, SystemTime)>,
    launched_at: SystemTime,
    baseline: &HashMap<PathBuf, SystemTime>,
) -> Option<PathBuf> {
    let stale_before = launched_at
        .checked_sub(SESSION_ACTIVE_MARGIN)
        .unwrap_or(launched_at);
    sessions
        .into_iter()
        .filter(|(path, mtime)| {
            *mtime >= launched_at && baseline.get(path).is_none_or(|&b| b < stale_before)
        })
        .max_by_key(|(_, mtime)| *mtime)
        .map(|(path, _)| path)
}

/// The session **this pane activated** — whether it created a fresh log or
/// **resumed** an old one. Binds by *activity after launch*, not file creation:
/// agents reuse session files (`claude --continue`, or simply appending to the
/// project's latest), so a brand-new pane that resumes yesterday's session must
/// still show that session's usage. `baseline` = [`session_mtimes`] captured at
/// launch; a session already fresh then is a **concurrent** one (a dev Claude
/// editing this repo) and is excluded, fixing "a fresh pane shows another
/// session's numbers". `None` until the agent writes — honest, not a guess.
pub fn resolve_session_for_pane(
    kind: AgentKind,
    launched_at: SystemTime,
    baseline: &HashMap<PathBuf, SystemTime>,
) -> Option<SessionRef> {
    let sessions = match kind {
        AgentKind::ClaudeCode => claude::claude_sessions_with_mtime(),
        AgentKind::Codex => codex::codex_sessions_with_mtime(),
    };
    let path = pick_pane_session(sessions, launched_at, baseline)?;
    session_ref(kind, path)
}

/// Agent-agnostic launch baseline (the adapter form of [`session_mtimes`]): the
/// `(path, mtime)` snapshot a pane captures at launch, keyed for stale→fresh.
pub fn adapter_session_mtimes(adapter: &dyn AgentAdapter) -> HashMap<PathBuf, SystemTime> {
    adapter.sessions_with_mtime().into_iter().collect()
}

/// Agent-agnostic pane session binder (the adapter form of
/// [`resolve_session_for_pane`]). Same stale→fresh [`pick_pane_session`]
/// algorithm, but the session list comes from the adapter instead of a `match`
/// on [`AgentKind`] — so a third agent needs no new arm here. Returns a
/// [`tn_agent::SessionRef`] (path + mtime); the owning agent is implied by the
/// adapter the caller passed.
pub fn resolve_pane_session(
    adapter: &dyn AgentAdapter,
    launched_at: SystemTime,
    baseline: &HashMap<PathBuf, SystemTime>,
) -> Option<AgentSessionRef> {
    let path = pick_pane_session(adapter.sessions_with_mtime(), launched_at, baseline)?;
    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
    Some(AgentSessionRef { path, mtime })
}

/// Parse a session file's text for the given agent.
pub fn parse_session(kind: AgentKind, text: &str) -> Option<AiUsage> {
    match kind {
        AgentKind::ClaudeCode => claude::parse_claude_session(text),
        AgentKind::Codex => codex::parse_codex_session(text),
    }
}

/// Incrementally update an existing `AiUsage` with new lines appended to the session.
pub fn update_session(kind: AgentKind, text: &str, prev: AiUsage) -> AiUsage {
    match kind {
        AgentKind::ClaudeCode => claude::update_claude_session(text, prev),
        AgentKind::Codex => codex::update_codex_session(text, prev),
    }
}

/// Read + parse usage for `cwd`, returning which agent it belongs to. `hint`
/// is the launch-intent agent (or `None` to auto-detect by freshness).
pub fn usage_for_cwd(cwd: &str, hint: Option<AgentKind>) -> Option<(AgentKind, AiUsage)> {
    let sref = resolve_session(cwd, hint)?;
    let text = std::fs::read_to_string(&sref.path).ok()?;
    Some((sref.kind, parse_session(sref.kind, &text)?))
}

/// Best-effort: is this agent signed in via a **subscription** (Claude Pro/Max,
/// ChatGPT) rather than a metered **API key**? Drives the `Auto` usage-display
/// default — members see context %, API users see $. Reads the agent's local
/// auth file; anything unreadable/unknown → `false` (assume API, the money view).
pub fn detect_subscription(kind: AgentKind) -> bool {
    match kind {
        AgentKind::ClaudeCode => claude_is_subscription(),
        AgentKind::Codex => codex_is_subscription(),
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Claude Code writes `~/.claude/.credentials.json` with a `claudeAiOauth` object
/// carrying `subscriptionType` (e.g. `"pro"`) when logged in as a member; an
/// API-key user has no such OAuth block.
pub(crate) fn claude_is_subscription() -> bool {
    let Some(home) = home_dir() else { return false };
    let path = home.join(".claude").join(".credentials.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    v.get("claudeAiOauth")
        .and_then(|o| o.get("subscriptionType"))
        .and_then(|s| s.as_str())
        .is_some_and(|s| !s.is_empty())
}

/// Codex writes `~/.codex/auth.json` with `auth_mode`: `"ApiKey"` for a metered
/// key, otherwise a ChatGPT (subscription) login.
pub(crate) fn codex_is_subscription() -> bool {
    let Some(home) = home_dir() else { return false };
    let path = home.join(".codex").join("auth.json");
    let Ok(text) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
        return false;
    };
    v.get("auth_mode")
        .and_then(|s| s.as_str())
        .is_some_and(|s| !s.is_empty() && !s.eq_ignore_ascii_case("apikey"))
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
        assert_eq!(
            agent_kind_for_command("claude"),
            Some(AgentKind::ClaudeCode)
        );
        assert_eq!(
            agent_kind_for_command("C:/npm/codex.cmd"),
            Some(AgentKind::Codex)
        );
        assert_eq!(
            agent_kind_for_command("& 'claude' '--resume'"),
            Some(AgentKind::ClaudeCode)
        );
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

    // Helpers for pick_pane_session timestamp math.
    fn secs(n: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(n)
    }

    #[test]
    fn binds_to_resumed_session_not_concurrent_dev() {
        // launch at T; our session was stale (modified 1h before), the dev session
        // was active 5s before launch and keeps writing (newer mtime). We must pick
        // OURS once it's written after launch — never the concurrent dev session,
        // even though the dev session's mtime is newer.
        let launch = secs(1_000_000);
        let mine = PathBuf::from("mine.jsonl");
        let dev = PathBuf::from("dev.jsonl");
        let mut baseline = HashMap::new();
        baseline.insert(mine.clone(), secs(1_000_000 - 3600)); // stale at launch
        baseline.insert(dev.clone(), secs(1_000_000 - 5)); // concurrent at launch
        let sessions = vec![
            (mine.clone(), secs(1_000_000 + 30)), // written after launch
            (dev.clone(), secs(1_000_000 + 31)),  // newer, but concurrent → excluded
        ];
        assert_eq!(pick_pane_session(sessions, launch, &baseline), Some(mine));
    }

    #[test]
    fn binds_to_freshly_created_session() {
        // A session file that didn't exist at launch (absent from baseline) and is
        // written after launch is ours.
        let launch = secs(1_000_000);
        let fresh = PathBuf::from("fresh.jsonl");
        let baseline = HashMap::new();
        let sessions = vec![(fresh.clone(), secs(1_000_000 + 10))];
        assert_eq!(pick_pane_session(sessions, launch, &baseline), Some(fresh));
    }

    #[test]
    fn idle_pane_binds_to_nothing() {
        // Only a concurrent dev session exists (active at launch, still writing).
        // An idle pane (ours not yet written) must show NOTHING, not the dev one.
        let launch = secs(1_000_000);
        let dev = PathBuf::from("dev.jsonl");
        let mut baseline = HashMap::new();
        baseline.insert(dev.clone(), secs(1_000_000 - 2)); // concurrent
        let sessions = vec![(dev.clone(), secs(1_000_000 + 50))];
        assert_eq!(pick_pane_session(sessions, launch, &baseline), None);
    }
}
