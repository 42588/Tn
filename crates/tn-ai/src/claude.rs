//! Claude Code usage: parse `~/.claude/projects/<proj>/<session>.jsonl`.
//!
//! Each line is one JSON object. Assistant turns look like
//! `{"type":"assistant","message":{"model":"…","usage":{"input_tokens":…,
//! "output_tokens":…,"cache_creation_input_tokens":…,"cache_read_input_tokens":…}}}`.
//! We sum tokens over the session and take the **last** turn's total input as
//! the current context size (what `/context` reports). No agent cooperation
//! needed — this is the same source `ccusage` reads.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::{preview, pricing, push_collapsed, AiUsage, TranscriptEntry, TranscriptRole};

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
        // Latest turn's total input = the live context size. Anthropic splits
        // input into three ADDITIVE buckets — plain `input_tokens`,
        // `cache_creation_input_tokens`, `cache_read_input_tokens` (a real turn
        // reads 47K cached with input_tokens=2) — so sum all three to match
        // what `/context` reports.
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

/// Incrementally update an existing `AiUsage` with new lines appended to the session.
pub fn update_claude_session(jsonl: &str, mut prev: AiUsage) -> AiUsage {
    let Some(delta) = parse_claude_session(jsonl) else {
        return prev;
    };
    prev.input += delta.input;
    prev.output += delta.output;
    prev.cache_create += delta.cache_create;
    prev.cache_read += delta.cache_read;
    prev.turns += delta.turns;
    prev.context_used = delta.context_used;
    prev.context_max = delta.context_max;
    if !delta.model.is_empty() {
        prev.model = delta.model;
    }
    let p = pricing::pricing_for(&prev.model);
    prev.cost_usd = p.cost(prev.input, prev.output, prev.cache_create, prev.cache_read);
    prev
}

/// Parse Claude session JSONL into ordered transcript entries (Tn's own history
/// surface — see [`tn_agent::TranscriptEntry`]). Each line is independent, so this
/// works on a full file or an appended delta. Non-conversation records
/// (`queue-operation`, `attachment`, `file-history-snapshot`, `ai-title`,
/// `last-prompt`, …) and empty/`thinking` blocks are skipped. A single assistant
/// line may carry several content blocks (text + tool_use), each its own entry.
pub fn parse_claude_transcript(jsonl: &str) -> Vec<TranscriptEntry> {
    let mut out = Vec::new();
    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let role = match v.get("type").and_then(|t| t.as_str()) {
            Some("user") => TranscriptRole::User,
            Some("assistant") => TranscriptRole::Assistant,
            _ => continue, // not a conversation turn
        };
        let Some(content) = v.get("message").and_then(|m| m.get("content")) else {
            continue;
        };
        push_claude_blocks(content, role, &mut out);
    }
    out
}

/// Claude logs slash commands and bash-tool plumbing as **user** text blocks
/// wrapped in metadata tags (`<command-name>/model</command-name>`,
/// `<local-command-stdout>…`, `<local-command-caveat>…`, `<bash-input>…`). These
/// aren't real user turns — they'd otherwise show up as bogus "You" entries in
/// the history (the "多个会话混在一起" the user saw). Skip a text block that is
/// one of these standalone metadata blocks.
fn is_claude_noise_text(text: &str) -> bool {
    const NOISE_PREFIXES: &[&str] = &[
        "<local-command-caveat>",
        "<local-command-stdout>",
        "<local-command-stderr>",
        "<command-name>",
        "<command-message>",
        "<command-args>",
        "<command-contents>",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
    ];
    let t = text.trim_start();
    NOISE_PREFIXES.iter().any(|p| t.starts_with(p))
}

/// Append entries for one message's `content` (a plain string, or an array of
/// `{type:"text"|"tool_use"|"tool_result"|"thinking", …}` blocks). `tool_use` /
/// `tool_result` always become `Tool` entries regardless of the outer role;
/// `text` keeps the outer role; `thinking` (often empty/encrypted) is skipped.
fn push_claude_blocks(content: &Value, role: TranscriptRole, out: &mut Vec<TranscriptEntry>) {
    if let Some(s) = content.as_str() {
        let s = s.trim();
        if !s.is_empty() && !is_claude_noise_text(s) {
            push_collapsed(out, TranscriptEntry::message(role, s));
        }
        return;
    }
    let Some(blocks) = content.as_array() else {
        return;
    };
    for block in blocks {
        match block.get("type").and_then(|t| t.as_str()) {
            Some("text") => {
                if let Some(t) = block.get("text").and_then(|t| t.as_str()) {
                    let t = t.trim();
                    if !t.is_empty() && !is_claude_noise_text(t) {
                        push_collapsed(out, TranscriptEntry::message(role, t));
                    }
                }
            }
            Some("tool_use") => {
                let name = block.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                let summary = claude_tool_summary(block.get("input"));
                push_collapsed(out, TranscriptEntry::tool_call(name, summary));
            }
            Some("tool_result") => {
                let text = claude_result_text(block.get("content"));
                push_collapsed(out, TranscriptEntry::tool_result(None, preview(&text, 600)));
            }
            _ => {} // thinking / unknown → skip
        }
    }
}

/// A one-glance preview of a `tool_use` input: the most telling field if present
/// (command / path / pattern / query / url), else the compact JSON.
fn claude_tool_summary(input: Option<&Value>) -> String {
    let Some(input) = input else {
        return String::new();
    };
    for k in ["command", "file_path", "path", "pattern", "query", "url"] {
        if let Some(s) = input.get(k).and_then(|v| v.as_str()) {
            return preview(s, 200);
        }
    }
    preview(&input.to_string(), 200)
}

/// Flatten a `tool_result` `content` (a string, or an array of `{text}` blocks)
/// into one string.
fn claude_result_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(s) = content.as_str() {
        return s.to_string();
    }
    let Some(arr) = content.as_array() else {
        return String::new();
    };
    let mut s = String::new();
    for b in arr {
        if let Some(t) = b.get("text").and_then(|t| t.as_str()) {
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(t);
        }
    }
    s
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
        .map(|c| {
            if matches!(c, ':' | '\\' | '/') {
                '-'
            } else {
                c
            }
        })
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

/// Every Claude session log across all projects, as `(path, mtime)`. The
/// pane-binding logic (see `detect::resolve_session_for_pane`) keys on mtime —
/// an agent **resumes** an old session file as often as it creates one, so file
/// creation time can't identify "this pane's session"; activity (mtime) can.
pub fn claude_sessions_with_mtime() -> Vec<(PathBuf, std::time::SystemTime)> {
    let mut out = Vec::new();
    let Some(projects) = claude_projects_dir() else {
        return out;
    };
    let Ok(projs) = std::fs::read_dir(&projects) else {
        return out;
    };
    for proj in projs.flatten() {
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
            if let Some(mtime) = entry.metadata().ok().and_then(|m| m.modified().ok()) {
                out.push((path, mtime));
            }
        }
    }
    out
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
        assert_eq!(
            (u.input, u.output, u.cache_create, u.cache_read),
            (300, 130, 10, 3000)
        );
        // context = LAST turn total input = 200 + 0 + 2000.
        assert_eq!(u.context_used, 2200);
        // claude-opus-4-7 is a current-gen 1M model (was wrongly 200K before).
        assert_eq!(u.context_max, 1_000_000);
        // input/cache_create/cache_read are separate additive buckets, each billed at its
        // own rate. claude-opus-4-7 is current-gen Opus → $5/$25/$6.25/$0.50 per MTok.
        let expect =
            300.0 / 1e6 * 5.0 + 130.0 / 1e6 * 25.0 + 10.0 / 1e6 * 6.25 + 3000.0 / 1e6 * 0.50;
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
        assert!((u.context_frac() - 2200.0 / 1_000_000.0).abs() < 1e-6);
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

    // Trimmed real-log shapes: a user turn, an assistant turn with thinking +
    // text + a tool_use, a tool_result user turn, and non-conversation noise.
    const TRANSCRIPT_SAMPLE: &str = r#"
{"type":"queue-operation","operation":"enqueue"}
{"type":"user","message":{"role":"user","content":[{"type":"text","text":"  修一下滚动  "}]}}
{"type":"ai-title","aiTitle":"Fix scrolling"}
{"type":"assistant","message":{"model":"claude-opus-4-8","content":[{"type":"thinking","thinking":""},{"type":"text","text":"On it."},{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"cargo test","description":"run tests"}}]}}
{"type":"user","message":{"role":"user","content":[{"tool_use_id":"t1","type":"tool_result","content":"test result: ok. 44 passed"}]}}
{"type":"assistant","message":{"content":"done"}}
"#;

    #[test]
    fn transcript_maps_roles_blocks_and_skips_noise() {
        let t = parse_claude_transcript(TRANSCRIPT_SAMPLE);
        assert_eq!(t.len(), 5, "1 user + (text + tool_use) + tool_result + final assistant");

        assert_eq!(t[0].role, TranscriptRole::User);
        assert_eq!(t[0].kind, crate::TranscriptKind::Message);
        assert_eq!(t[0].text, "修一下滚动"); // trimmed

        // thinking block skipped; assistant text kept.
        assert_eq!(t[1].role, TranscriptRole::Assistant);
        assert_eq!(t[1].text, "On it.");

        // tool_use → Tool/ToolCall with the command as the summary.
        assert_eq!(t[2].role, TranscriptRole::Tool);
        assert_eq!(t[2].kind, crate::TranscriptKind::ToolCall);
        assert_eq!(t[2].tool.as_deref(), Some("Bash"));
        assert_eq!(t[2].text, "cargo test");

        // tool_result → Tool/ToolResult.
        assert_eq!(t[3].kind, crate::TranscriptKind::ToolResult);
        assert_eq!(t[3].text, "test result: ok. 44 passed");

        // string content also works.
        assert_eq!(t[4].role, TranscriptRole::Assistant);
        assert_eq!(t[4].text, "done");
    }

    #[test]
    fn transcript_skips_slash_command_metadata_noise() {
        // Real Claude logs slash commands as user text blocks wrapped in metadata
        // tags — these must NOT appear as "You" turns.
        let s = r#"
{"type":"user","message":{"content":[{"type":"text","text":"<local-command-caveat>Caveat: …</local-command-caveat>"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"<command-name>/model</command-name>"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"<local-command-stdout>Set model to Haiku</local-command-stdout>"}]}}
{"type":"user","message":{"content":[{"type":"text","text":"你好"}]}}
"#;
        let t = parse_claude_transcript(s);
        assert_eq!(t.len(), 1, "only the real user message survives, got {t:?}");
        assert_eq!(t[0].text, "你好");
    }

    #[test]
    fn transcript_delta_parses_independently() {
        // One appended assistant line parses on its own (the incremental tail step).
        let delta = r#"{"type":"assistant","message":{"content":[{"type":"text","text":"hi"}]}}"#;
        let t = parse_claude_transcript(delta);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].text, "hi");
    }

    #[test]
    fn observed_context_above_window_infers_1m() {
        // A 200K-default model (Haiku) whose observed turn exceeds 200K → widen to
        // 1M so the ring never reads >100%. (Current-gen Opus/Sonnet are already 1M
        // from the model id, so the widening net only matters for 200K families.)
        let s = r#"{"type":"assistant","message":{"model":"claude-haiku-4-5-20251001","usage":{"input_tokens":250000,"output_tokens":10,"cache_creation_input_tokens":0,"cache_read_input_tokens":50000}}}"#;
        let u = parse_claude_session(s).unwrap();
        assert_eq!(u.context_used, 300_000);
        assert_eq!(u.context_max, 1_000_000);
        assert!(u.context_frac() < 1.0);
    }
}
