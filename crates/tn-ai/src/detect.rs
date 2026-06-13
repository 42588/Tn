//! Per-pane session binding + subscription detection for the built-in adapters.
//!
//! Binding a pane to *its* session log is [`resolve_pane_session`]: it picks the
//! session that went **stale→fresh after the pane launched** (created anew or
//! resumed), ignoring one already active at launch (a concurrent dev agent).
//! Agent-agnostic — it works off any [`AgentAdapter`]'s session list, so a new
//! agent needs no new arm here. The richer signals (process tree / OSC title /
//! banner) are deferred — activity-after-launch covers the dogfooding case.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use tn_agent::{AgentAdapter, SessionRef};

/// A session whose mtime sat within this window *before* launch was already
/// being written when the pane started → it's a **concurrently-running** session
/// (e.g. a dev Claude editing this repo while Tn runs from it), not this pane's.
/// Our session goes from stale/absent at launch to freshly written *after*.
const SESSION_ACTIVE_MARGIN: Duration = Duration::from_secs(120);

/// Pick the session this pane activated from a `(path, mtime)` list + the
/// launch-time `baseline`. Two tiers, both requiring activity after launch
/// (`mtime ≥ launched_at`):
///
/// 1. **Freshly created** — a path **absent** from baseline (the file didn't
///    exist at launch). This unambiguously belongs to this pane, so it wins even
///    over a more-active session. Critical when a concurrent dev agent edits the
///    same repo (Tn dogfooding): that session is busier (newer mtime) but it
///    existed at launch, so a brand-new `claude`/`codex` pane must not bind to it.
/// 2. **Resumed** — a path **present** at baseline but **stale** then (baseline
///    mtime older than `launched_at − SESSION_ACTIVE_MARGIN`), now written again.
///    Only consulted when no freshly-created session qualifies. Ambiguous against
///    a concurrent session with a >margin gap, but resume is the rarer case.
///
/// Splitting this out keeps it unit-testable with synthetic timestamps.
fn pick_pane_session(
    sessions: Vec<(PathBuf, SystemTime)>,
    launched_at: SystemTime,
    baseline: &HashMap<PathBuf, SystemTime>,
) -> Option<PathBuf> {
    // Tier 1: a file that did not exist at launch is unambiguously this pane's.
    let fresh = sessions
        .iter()
        .filter(|(path, mtime)| *mtime >= launched_at && !baseline.contains_key(path))
        .max_by_key(|(_, mtime)| *mtime)
        .map(|(path, _)| path.clone());
    if fresh.is_some() {
        return fresh;
    }
    // Tier 2: a pre-existing session that was stale at launch and is active again
    // (a resume). Excludes one that was concurrently writing at launch.
    let stale_before = launched_at
        .checked_sub(SESSION_ACTIVE_MARGIN)
        .unwrap_or(launched_at);
    sessions
        .into_iter()
        .filter(|(path, mtime)| {
            *mtime >= launched_at && baseline.get(path).is_some_and(|&b| b < stale_before)
        })
        .max_by_key(|(_, mtime)| *mtime)
        .map(|(path, _)| path)
}

/// The launch baseline an adapter's pane captures: a `(path, mtime)` snapshot of
/// every known session at launch, keyed for the stale→fresh test in
/// [`resolve_pane_session`]. Sessions already fresh here are someone else's.
pub fn adapter_session_mtimes(adapter: &dyn AgentAdapter) -> HashMap<PathBuf, SystemTime> {
    adapter.sessions_with_mtime().into_iter().collect()
}

/// The session **this pane activated** — whether it created a fresh log or
/// **resumed** an old one. Binds by *activity after launch*, not file creation
/// (agents reuse session files, e.g. `claude --continue`). `baseline` =
/// [`adapter_session_mtimes`] captured at launch; a session already fresh then is
/// a **concurrent** one (a dev agent editing this repo) and is excluded. Returns
/// a [`tn_agent::SessionRef`] (path + mtime); the owning agent is implied by the
/// adapter the caller passed. `None` until the agent writes — honest, not a guess.
pub fn resolve_pane_session(
    adapter: &dyn AgentAdapter,
    launched_at: SystemTime,
    baseline: &HashMap<PathBuf, SystemTime>,
) -> Option<SessionRef> {
    let path = pick_pane_session(adapter.sessions_with_mtime(), launched_at, baseline)?;
    let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
    Some(SessionRef { path, mtime })
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
}

/// Claude Code writes `~/.claude/.credentials.json` with a `claudeAiOauth` object
/// carrying `subscriptionType` (e.g. `"pro"`) when logged in as a member; an
/// API-key user has no such OAuth block. Used by [`crate::ClaudeAdapter`].
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
/// key, otherwise a ChatGPT (subscription) login. Used by [`crate::CodexAdapter`].
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn fresh_session_wins_over_more_active_concurrent_dev() {
        // The dogfooding bug: a dev agent edits this very repo (its session existed
        // before launch, went briefly stale, then writes furiously → newest mtime).
        // The user opens a fresh pane (a NEW session file, absent from baseline).
        // We MUST bind to the user's fresh file, not the busier dev session — or the
        // pane shows another session's history.
        let launch = secs(1_000_000);
        let mine = PathBuf::from("mine.jsonl"); // created after launch (absent at baseline)
        let dev = PathBuf::from("dev.jsonl"); // pre-existing, stale at launch, now very active
        let mut baseline = HashMap::new();
        baseline.insert(dev.clone(), secs(1_000_000 - 3600)); // stale gap > margin
        let sessions = vec![
            (mine.clone(), secs(1_000_000 + 10)),
            (dev.clone(), secs(1_000_000 + 99)), // newer mtime, but NOT ours
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
