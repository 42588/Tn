//! Claude Code usage: parse `~/.claude/projects/<proj>/<session>.jsonl`.
//!
//! Each line is one JSON object. Assistant turns look like
//! `{"type":"assistant","message":{"model":"…","usage":{"input_tokens":…,
//! "output_tokens":…,"cache_creation_input_tokens":…,"cache_read_input_tokens":…}}}`.
//! We sum tokens over the session and take the **last** turn's total input as
//! the current context size (what `/context` reports). No agent cooperation
//! needed — this is the same source `ccusage` reads.

use std::path::{Path, PathBuf};

use crate::{pricing, AiUsage};

/// Parse a Claude session JSONL into [`AiUsage`]. Returns `None` if no
/// assistant turn with usage is present (malformed lines are skipped).
pub fn parse_claude_session(jsonl: &str) -> Option<AiUsage> {
    let mut model = String::new();
    let (mut input, mut output, mut cache_create, mut cache_read) = (0u64, 0u64, 0u64, 0u64);
    let mut context_used = 0u32;
    let mut turns = 0u32;

    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let msg = match v.get("message") {
            Some(m) => m,
            None => continue,
        };
        let usage = match msg.get("usage") {
            Some(u) if u.is_object() => u,
            _ => continue,
        };
        let g = |k: &str| usage.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
        let (it, ot, cc, cr) = (
            g("input_tokens"),
            g("output_tokens"),
            g("cache_creation_input_tokens"),
            g("cache_read_input_tokens"),
        );
        input += it;
        output += ot;
        cache_create += cc;
        cache_read += cr;
        // Latest turn's total input = the live context size.
        context_used = (it + cc + cr).min(u32::MAX as u64) as u32;
        if let Some(m) = msg.get("model").and_then(|m| m.as_str()) {
            if !m.is_empty() {
                model = m.to_string();
            }
        }
        turns += 1;
    }

    if turns == 0 {
        return None;
    }
    let p = pricing::pricing_for(&model);
    // Long-context variants (1M) aren't always marked in the model id; if an
    // observed turn already exceeds the table window, widen it to the 1M tier
    // so the ring never reads >100%.
    let context_max = if context_used > p.context_window {
        p.context_window.max(1_000_000)
    } else {
        p.context_window
    };
    Some(AiUsage {
        cost_usd: p.cost(input, output, cache_create, cache_read),
        context_max,
        model,
        input,
        output,
        cache_create,
        cache_read,
        context_used,
        turns,
    })
}

/// `~/.claude/projects`, if it exists.
pub fn claude_projects_dir() -> Option<PathBuf> {
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
    let dir = Path::new(&home).join(".claude").join("projects");
    dir.is_dir().then_some(dir)
}

/// Encode a working directory to Claude's project-folder name: `:` `\` `/` → `-`
/// (e.g. `d:\coder\Tn` → `d--coder-Tn`).
pub fn encode_project_dir(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if matches!(c, ':' | '\\' | '/') { '-' } else { c })
        .collect()
}

/// Newest `*.jsonl` session file for `cwd`, if any. Falls back to a
/// case-insensitive folder match (Windows drive-letter casing varies).
pub fn latest_session_file(cwd: &str) -> Option<PathBuf> {
    let projects = claude_projects_dir()?;
    let want = encode_project_dir(cwd);
    let dir = if projects.join(&want).is_dir() {
        projects.join(&want)
    } else {
        std::fs::read_dir(&projects)
            .ok()?
            .flatten()
            .map(|e| e.path())
            .find(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(&want))
            })?
    };

    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(&dir).ok()?.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let Some(mtime) = entry.metadata().ok().and_then(|m| m.modified().ok()) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
            newest = Some((mtime, path));
        }
    }
    newest.map(|(_, p)| p)
}

/// Newest Claude session `*.jsonl` across **all** projects (any cwd). Fallback
/// for an agent pane whose session cwd doesn't match the app cwd.
pub fn latest_claude_session_any() -> Option<PathBuf> {
    let projects = claude_projects_dir()?;
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for proj in std::fs::read_dir(&projects).ok()?.flatten() {
        if !proj.path().is_dir() {
            continue;
        }
        let Ok(files) = std::fs::read_dir(proj.path()) else {
            continue;
        };
        for entry in files.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Some(mtime) = entry.metadata().ok().and_then(|m| m.modified().ok()) else {
                continue;
            };
            if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
                newest = Some((mtime, path));
            }
        }
    }
    newest.map(|(_, p)| p)
}

/// Read + parse the newest Claude session for `cwd`.
pub fn usage_for_cwd(cwd: &str) -> Option<AiUsage> {
    let text = std::fs::read_to_string(latest_session_file(cwd)?).ok()?;
    parse_claude_session(&text)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
{"type":"user","message":{"role":"user","content":"hi"}}
{"type":"assistant","message":{"model":"claude-opus-4-7","role":"assistant","usage":{"input_tokens":100,"output_tokens":50,"cache_creation_input_tokens":10,"cache_read_input_tokens":1000}}}
{"type":"assistant","message":{"model":"claude-opus-4-7","role":"assistant","usage":{"input_tokens":200,"output_tokens":80,"cache_creation_input_tokens":0,"cache_read_input_tokens":2000}}}
"#;

    #[test]
    fn parses_tokens_context_and_cost() {
        let u = parse_claude_session(SAMPLE).expect("usage");
        assert_eq!(u.model, "claude-opus-4-7");
        assert_eq!(u.turns, 2);
        assert_eq!((u.input, u.output, u.cache_create, u.cache_read), (300, 130, 10, 3000));
        // context = LAST turn total input = 200 + 0 + 2000.
        assert_eq!(u.context_used, 2200);
        assert_eq!(u.context_max, 200_000);
        let expect = 300.0 / 1e6 * 15.0 + 130.0 / 1e6 * 75.0 + 10.0 / 1e6 * 18.75 + 3000.0 / 1e6 * 1.5;
        assert!((u.cost_usd - expect).abs() < 1e-9);
    }

    #[test]
    fn no_assistant_turn_is_none() {
        let s = "not json\n{\"type\":\"user\",\"message\":{}}\n";
        assert!(parse_claude_session(s).is_none());
    }

    #[test]
    fn context_frac_and_totals() {
        let u = parse_claude_session(SAMPLE).unwrap();
        assert_eq!(u.total_tokens(), 300 + 130 + 10 + 3000);
        assert!((u.context_frac() - 2200.0 / 200_000.0).abs() < 1e-6);
    }

    #[test]
    fn encode_project_dir_matches_claude() {
        assert_eq!(encode_project_dir("d:\\coder\\Tn"), "d--coder-Tn");
        assert_eq!(encode_project_dir("C:\\Users\\Gua"), "C--Users-Gua");
    }

    #[test]
    fn one_m_variant_widens_context() {
        let s = r#"{"type":"assistant","message":{"model":"claude-sonnet-4-6-1m","usage":{"input_tokens":1,"output_tokens":1,"cache_creation_input_tokens":0,"cache_read_input_tokens":0}}}"#;
        assert_eq!(parse_claude_session(s).unwrap().context_max, 1_000_000);
    }

    #[test]
    fn observed_context_above_window_infers_1m() {
        // Opus id with no "1m" marker, but a turn larger than 200K → widen to 1M
        // (real `claude-opus-4-7` 1M sessions look exactly like this).
        let s = r#"{"type":"assistant","message":{"model":"claude-opus-4-7","usage":{"input_tokens":250000,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":50000}}}"#;
        let u = parse_claude_session(s).unwrap();
        assert_eq!(u.context_used, 300_000);
        assert_eq!(u.context_max, 1_000_000);
        assert!(u.context_frac() < 1.0);
    }
}
