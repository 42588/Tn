//! Recent + favorite SSH connections (A1).
//!
//! Every *successful* SSH connect (see `tn_pty::PtyEvent::Connected`) is recorded
//! here, so the connector overlay can offer one-keystroke re-connect instead of
//! re-typing `user@host:port` each time. Favorites (⭐) pin to the top; the rest
//! are most-recent-first and capped. Persisted as JSON at
//! `%APPDATA%\Tn\ssh_recents.json` (same pattern as [`crate::layout`]).

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// How a recorded connection last authenticated — drives the connector badge.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthBadge {
    #[default]
    Unknown,
    Key,
    Password,
}

impl AuthBadge {
    /// Map the method the backend reported into a coarse key/password badge
    /// (keyboard-interactive is a password flavour, so it badges as password).
    pub fn from_pty(k: tn_pty::AuthKind) -> Self {
        match k {
            tn_pty::AuthKind::PublicKey => AuthBadge::Key,
            tn_pty::AuthKind::Password | tn_pty::AuthKind::KeyboardInteractive => AuthBadge::Password,
        }
    }
}

/// Non-favorite recents kept on disk; favorites are always kept.
const MAX_RECENTS: usize = 12;

/// One remembered SSH endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshRecent {
    pub host: String,
    pub user: String,
    pub port: u16,
    #[serde(default)]
    pub auth: AuthBadge,
    /// Unix seconds of the last successful connect.
    #[serde(default)]
    pub last_used: u64,
    #[serde(default)]
    pub favorite: bool,
}

impl SshRecent {
    /// The reconnect target string `user@host[:port]` (port shown only when ≠ 22).
    pub fn target(&self) -> String {
        let up = if self.user.is_empty() {
            self.host.clone()
        } else {
            format!("{}@{}", self.user, self.host)
        };
        if self.port == 22 {
            up
        } else {
            format!("{up}:{}", self.port)
        }
    }

    /// Case-insensitive substring match over host / user / target — the connector
    /// uses the input box as a live filter.
    pub fn matches(&self, q: &str) -> bool {
        if q.is_empty() {
            return true;
        }
        let q = q.to_ascii_lowercase();
        self.host.to_ascii_lowercase().contains(&q)
            || self.user.to_ascii_lowercase().contains(&q)
            || self.target().to_ascii_lowercase().contains(&q)
    }

    /// Same endpoint = same host (case-insensitive) + user + port.
    fn same_endpoint(&self, host: &str, user: &str, port: u16) -> bool {
        self.port == port
            && self.user == user
            && self.host.eq_ignore_ascii_case(host)
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// Compact relative time for the connector's "last used" column.
pub fn rel_time(last_used: u64) -> String {
    if last_used == 0 {
        return String::new();
    }
    let now = now_secs();
    let d = now.saturating_sub(last_used);
    match d {
        0..=59 => "刚刚".to_string(),
        60..=3599 => format!("{} 分前", d / 60),
        3600..=86_399 => format!("{} 时前", d / 3600),
        86_400..=172_799 => "昨天".to_string(),
        172_800..=604_799 => format!("{} 天前", d / 86_400),
        _ => format!("{} 周前", d / 604_800),
    }
}

/// The on-disk recents table (`ssh_recents.json`).
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct SshRecents {
    #[serde(default)]
    pub entries: Vec<SshRecent>,
}

impl SshRecents {
    fn path() -> Option<PathBuf> {
        tn_config::config_dir().map(|d| d.join("ssh_recents.json"))
    }

    /// Load from disk (missing / unparsable → empty).
    pub fn load() -> Self {
        Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<SshRecents>(&s).ok())
            .unwrap_or_default()
    }

    /// Persist to disk (best-effort; logs on failure).
    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::error!(path = %path.display(), error = %e, "save ssh recents failed");
                }
            }
            Err(e) => tracing::error!(error = %e, "serialize ssh recents failed"),
        }
    }

    /// Record a *successful* connect: upsert by endpoint, bump `last_used`, update
    /// the auth badge, and keep any existing favorite flag. Trims old non-favorites.
    pub fn record(&mut self, host: &str, user: &str, port: u16, auth: AuthBadge) {
        let now = now_secs();
        if let Some(e) = self
            .entries
            .iter_mut()
            .find(|e| e.same_endpoint(host, user, port))
        {
            e.last_used = now;
            e.auth = auth;
        } else {
            self.entries.push(SshRecent {
                host: host.to_string(),
                user: user.to_string(),
                port,
                auth,
                last_used: now,
                favorite: false,
            });
        }
        self.trim();
    }

    /// Flip the favorite flag of the matching endpoint (no-op if absent).
    pub fn toggle_favorite(&mut self, host: &str, user: &str, port: u16) {
        if let Some(e) = self
            .entries
            .iter_mut()
            .find(|e| e.same_endpoint(host, user, port))
        {
            e.favorite = !e.favorite;
        }
    }

    /// Display order: favorites first, then most-recent-first.
    pub fn sorted(&self) -> Vec<&SshRecent> {
        let mut v: Vec<&SshRecent> = self.entries.iter().collect();
        v.sort_by(|a, b| {
            b.favorite
                .cmp(&a.favorite)
                .then(b.last_used.cmp(&a.last_used))
        });
        v
    }

    /// [`sorted`](Self::sorted) filtered by a live query.
    pub fn filtered(&self, q: &str) -> Vec<&SshRecent> {
        self.sorted().into_iter().filter(|e| e.matches(q)).collect()
    }

    /// Keep every favorite + the [`MAX_RECENTS`] most-recent non-favorites.
    fn trim(&mut self) {
        if self.entries.len() <= MAX_RECENTS {
            return;
        }
        // Newest non-favorites to keep.
        let mut non_fav: Vec<&SshRecent> =
            self.entries.iter().filter(|e| !e.favorite).collect();
        non_fav.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        let keep: std::collections::HashSet<(String, String, u16)> = non_fav
            .into_iter()
            .take(MAX_RECENTS)
            .map(|e| (e.host.to_ascii_lowercase(), e.user.clone(), e.port))
            .collect();
        self.entries.retain(|e| {
            e.favorite || keep.contains(&(e.host.to_ascii_lowercase(), e.user.clone(), e.port))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(host: &str, user: &str, port: u16) -> SshRecent {
        SshRecent { host: host.into(), user: user.into(), port, auth: AuthBadge::Unknown, last_used: 0, favorite: false }
    }

    #[test]
    fn target_omits_default_port() {
        assert_eq!(rec("h", "u", 22).target(), "u@h");
        assert_eq!(rec("h", "u", 2222).target(), "u@h:2222");
        let mut r = rec("h", "", 22);
        r.user.clear();
        assert_eq!(r.target(), "h");
    }

    #[test]
    fn record_upserts_and_preserves_favorite() {
        let mut s = SshRecents::default();
        s.record("Host", "root", 2222, AuthBadge::Key);
        assert_eq!(s.entries.len(), 1);
        s.toggle_favorite("host", "root", 2222); // case-insensitive host
        assert!(s.entries[0].favorite);
        // Re-record same endpoint (different case) → still one entry, favorite kept.
        s.record("HOST", "root", 2222, AuthBadge::Password);
        assert_eq!(s.entries.len(), 1);
        assert!(s.entries[0].favorite);
        assert_eq!(s.entries[0].auth, AuthBadge::Password);
    }

    #[test]
    fn sorted_favorites_first_then_recency() {
        let mut s = SshRecents::default();
        s.entries = vec![
            SshRecent { last_used: 10, ..rec("old", "u", 22) },
            SshRecent { last_used: 30, ..rec("new", "u", 22) },
            SshRecent { last_used: 5, favorite: true, ..rec("fav", "u", 22) },
        ];
        let order: Vec<&str> = s.sorted().iter().map(|e| e.host.as_str()).collect();
        assert_eq!(order, vec!["fav", "new", "old"]);
    }

    #[test]
    fn filter_matches_substring() {
        let mut s = SshRecents::default();
        s.entries = vec![rec("alma.local", "root", 22), rec("10.0.0.5", "ubuntu", 22)];
        assert_eq!(s.filtered("alma").len(), 1);
        assert_eq!(s.filtered("ubuntu").len(), 1);
        assert_eq!(s.filtered("root@alma").len(), 1);
        assert_eq!(s.filtered("").len(), 2);
        assert_eq!(s.filtered("zzz").len(), 0);
    }

    #[test]
    fn trim_keeps_favorites_over_cap() {
        let mut s = SshRecents::default();
        // One pinned favorite + many recents over the cap.
        s.entries.push(SshRecent { favorite: true, last_used: 1, ..rec("fav", "u", 22) });
        for i in 0..(MAX_RECENTS as u64 + 5) {
            s.entries.push(SshRecent { last_used: 100 + i, ..rec(&format!("h{i}"), "u", 22) });
        }
        s.trim();
        assert!(s.entries.iter().any(|e| e.host == "fav")); // favorite survived
        let non_fav = s.entries.iter().filter(|e| !e.favorite).count();
        assert_eq!(non_fav, MAX_RECENTS); // capped
    }

    #[test]
    fn json_roundtrips() {
        let mut s = SshRecents::default();
        s.record("h", "u", 2222, AuthBadge::Key);
        s.entries[0].favorite = true;
        let json = serde_json::to_string(&s).unwrap();
        let back: SshRecents = serde_json::from_str(&json).unwrap();
        assert_eq!(back.entries.len(), 1);
        assert!(back.entries[0].favorite);
        assert_eq!(back.entries[0].auth, AuthBadge::Key);
    }
}
