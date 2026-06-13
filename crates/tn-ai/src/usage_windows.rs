//! Windowed usage aggregation (额度面板 · Tier A).
//!
//! Scan recent Claude session logs and bucket token spend into rolling windows —
//! 5-hour, day, week — with cost via [`tn_agent::pricing`]. This is the honest,
//! offline "你用了多少". The **authoritative** cap / remaining / reset (Tier B)
//! only lives at `claude.ai`; see the live-quota module. ai-limit splits the same
//! way: consumption from local `~/.claude/projects/**/*.jsonl`, quota from the web.
//!
//! Each assistant line carries a top-level ISO-8601 `timestamp` and
//! `message.usage` token buckets — all we need to window. Parsing is
//! dependency-free (fixed `YYYY-MM-DDTHH:MM:SSZ` UTC → unix epoch).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tn_agent::pricing::pricing_for;

const FIVE_HOURS: Duration = Duration::from_secs(5 * 3600);
const ONE_DAY: Duration = Duration::from_secs(24 * 3600);
const ONE_WEEK: Duration = Duration::from_secs(7 * 24 * 3600);

/// One billed assistant turn: when, which model, and its token buckets.
#[derive(Clone, Debug, PartialEq)]
pub struct Turn {
    pub at: SystemTime,
    pub model: String,
    pub input: u64,
    pub output: u64,
    pub cache_create: u64,
    pub cache_read: u64,
}

impl Turn {
    fn cost(&self) -> f64 {
        pricing_for(&self.model).cost(self.input, self.output, self.cache_create, self.cache_read)
    }
    /// All buckets — what the spend "weighs" (input + output + both cache tiers).
    fn tokens(&self) -> u64 {
        self.input + self.output + self.cache_create + self.cache_read
    }
}

/// Aggregate of one time window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Window {
    pub tokens: u64,
    pub cost_usd: f64,
    pub turns: u32,
    /// Earliest in-window turn (drives the 5h reset estimate).
    pub first_at: Option<SystemTime>,
}

/// Rolling 5h / day / week aggregates ending at `now`.
#[derive(Clone, Debug, Default)]
pub struct UsageWindows {
    pub five_hour: Window,
    pub day: Window,
    pub week: Window,
    /// Estimated end of the active 5-hour window = oldest in-window turn + 5h.
    /// `None` with no recent activity. Anthropic's real reset comes from Tier B —
    /// this is a rolling-window estimate, label it as such in the UI.
    pub reset_5h: Option<SystemTime>,
}

/// Aggregate `turns` into rolling windows ending at `now`. Pure → tested.
pub fn aggregate(turns: &[Turn], now: SystemTime) -> UsageWindows {
    let (cut5, cutd, cutw) = (
        now.checked_sub(FIVE_HOURS),
        now.checked_sub(ONE_DAY),
        now.checked_sub(ONE_WEEK),
    );
    let mut w = UsageWindows::default();
    let add = |win: &mut Window, t: &Turn| {
        win.tokens += t.tokens();
        win.cost_usd += t.cost();
        win.turns += 1;
        win.first_at = Some(win.first_at.map_or(t.at, |f| f.min(t.at)));
    };
    for t in turns {
        if t.at > now {
            continue; // clock-skew future stamp → ignore
        }
        if cutw.is_none_or(|c| t.at >= c) {
            add(&mut w.week, t);
        }
        if cutd.is_none_or(|c| t.at >= c) {
            add(&mut w.day, t);
        }
        if cut5.is_none_or(|c| t.at >= c) {
            add(&mut w.five_hour, t);
        }
    }
    w.reset_5h = w.five_hour.first_at.map(|f| f + FIVE_HOURS);
    w
}

/// Parse the per-turn `(timestamp, model, usage)` from one Claude session JSONL.
/// Skips non-assistant / malformed lines. Pure → tested.
pub fn turns_from_jsonl(jsonl: &str) -> Vec<Turn> {
    let mut out = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let Some(at) = v
            .get("timestamp")
            .and_then(|t| t.as_str())
            .and_then(parse_iso8601_utc)
        else {
            continue;
        };
        let Some(msg) = v.get("message") else { continue };
        let Some(usage) = msg.get("usage").filter(|u| u.is_object()) else {
            continue;
        };
        let g = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        out.push(Turn {
            at,
            model: msg
                .get("model")
                .and_then(|m| m.as_str())
                .unwrap_or("")
                .to_string(),
            input: g("input_tokens"),
            output: g("output_tokens"),
            cache_create: g("cache_creation_input_tokens"),
            cache_read: g("cache_read_input_tokens"),
        });
    }
    out
}

/// Read every Claude session touched within the last week and collect its turns.
/// Bounded by mtime so we don't re-read the entire history each refresh. Blocking
/// (file IO) — call off the UI thread.
pub fn collect_recent_turns(now: SystemTime) -> Vec<Turn> {
    let cutoff = now.checked_sub(ONE_WEEK);
    let mut turns = Vec::new();
    for (path, mtime) in crate::claude::claude_sessions_with_mtime() {
        if cutoff.is_some_and(|c| mtime < c) {
            continue; // untouched all week → can't hold an in-window turn
        }
        if let Ok(s) = std::fs::read_to_string(&path) {
            turns.extend(turns_from_jsonl(&s));
        }
    }
    turns
}

/// Convenience: aggregate the last week of Claude activity as of `now`. Blocking.
pub fn current_windows(now: SystemTime) -> UsageWindows {
    aggregate(&collect_recent_turns(now), now)
}

/// Parse `YYYY-MM-DDTHH:MM:SS(.fff)?Z` (always UTC) to a [`SystemTime`]. Fixed-field
/// — no datetime crate. `None` on a malformed prefix.
fn parse_iso8601_utc(s: &str) -> Option<SystemTime> {
    let f = |a: usize, z: usize| -> Option<i64> { s.get(a..z)?.parse().ok() };
    // Require at least `YYYY-MM-DDTHH:MM:SS`.
    if s.len() < 19 || s.as_bytes()[10] != b'T' {
        return None;
    }
    let (y, mo, d) = (f(0, 4)?, f(5, 7)?, f(8, 10)?);
    let (h, mi, se) = (f(11, 13)?, f(14, 16)?, f(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let secs = days_from_civil(y, mo, d) * 86_400 + h * 3600 + mi * 60 + se;
    if secs < 0 {
        return None;
    }
    Some(UNIX_EPOCH + Duration::from_secs(secs as u64))
}

/// Days since 1970-01-01 for a civil (proleptic Gregorian) date — Howard Hinnant's
/// algorithm. Valid for any year; we only ever feed it recent dates.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = (if y >= 0 { y } else { y - 399 }) / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(at: SystemTime, tokens: u64, model: &str) -> Turn {
        // Put all tokens in `input` so cost/tokens are easy to reason about.
        Turn {
            at,
            model: model.to_string(),
            input: tokens,
            output: 0,
            cache_create: 0,
            cache_read: 0,
        }
    }

    #[test]
    fn iso8601_parses_to_epoch() {
        // 2026-06-13T04:33:53Z — verify against an independent epoch computation.
        let st = parse_iso8601_utc("2026-06-13T04:33:53.541Z").unwrap();
        let secs = st.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, days_from_civil(2026, 6, 13) as u64 * 86_400 + 4 * 3600 + 33 * 60 + 53);
        // Unix epoch itself.
        assert_eq!(
            parse_iso8601_utc("1970-01-01T00:00:00Z").unwrap(),
            UNIX_EPOCH
        );
        // Malformed → None.
        assert!(parse_iso8601_utc("not-a-date").is_none());
        assert!(parse_iso8601_utc("2026-13-01T00:00:00Z").is_none()); // month 13
    }

    #[test]
    fn windows_bucket_by_recency_and_estimate_reset() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let ago = |secs: u64| now - Duration::from_secs(secs);
        let turns = vec![
            t(ago(60), 100, "claude-opus-4-8"),       // in 5h, day, week
            t(ago(3 * 3600), 200, "claude-opus-4-8"), // in 5h, day, week
            t(ago(10 * 3600), 400, "claude-opus-4-8"), // in day, week (not 5h)
            t(ago(3 * 86400), 800, "claude-opus-4-8"), // in week only
            t(ago(30 * 86400), 999, "claude-opus-4-8"), // outside all windows
        ];
        let w = aggregate(&turns, now);
        assert_eq!(w.five_hour.tokens, 300);
        assert_eq!(w.five_hour.turns, 2);
        assert_eq!(w.day.tokens, 700);
        assert_eq!(w.week.tokens, 1500);
        // Reset estimate = oldest in-5h turn (3h ago) + 5h = 2h from now.
        assert_eq!(w.reset_5h, Some(ago(3 * 3600) + FIVE_HOURS));
        // Cost: 300 input tokens on Opus 4.8 ($5/MTok) = $0.0015.
        assert!((w.five_hour.cost_usd - 300.0 / 1e6 * 5.0).abs() < 1e-12);
    }

    #[test]
    fn empty_has_no_reset() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let w = aggregate(&[], now);
        assert_eq!(w.week.turns, 0);
        assert!(w.reset_5h.is_none());
    }

    #[test]
    fn turns_from_jsonl_parses_assistant_usage_only() {
        let s = r#"
{"type":"user","timestamp":"2026-06-13T04:00:00Z","message":{"role":"user"}}
{"type":"assistant","timestamp":"2026-06-13T04:33:53.541Z","message":{"model":"claude-opus-4-8","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":10,"cache_read_input_tokens":2000}}}
{"type":"assistant","message":{"model":"x","usage":{"input_tokens":1}}}
"#;
        let turns = turns_from_jsonl(s);
        assert_eq!(turns.len(), 1, "user line + timestampless assistant skipped");
        assert_eq!(turns[0].input, 100);
        assert_eq!(turns[0].cache_read, 2000);
        assert_eq!(turns[0].model, "claude-opus-4-8");
        assert_eq!(turns[0].at, parse_iso8601_utc("2026-06-13T04:33:53.541Z").unwrap());
    }
}
