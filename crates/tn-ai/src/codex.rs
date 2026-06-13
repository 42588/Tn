//! Codex usage: parse `$CODEX_HOME/sessions/<YYYY>/<MM>/<DD>/rollout-*.jsonl`.
//!
//! Each line is one JSON object. The interesting ones are:
//!   - `session_meta` — `payload.cwd` (so we can match a session to a project),
//!   - `turn_context` — `payload.model` (the model id),
//!   - `event_msg` with `payload.type == "token_count"` — carries cumulative
//!     `info.total_token_usage`, the current-turn `info.last_token_usage`, and
//!     `info.model_context_window` (Codex logs the real window, so we use it
//!     directly instead of guessing from the pricing table).
//!
//! Codex's `input_tokens` *includes* `cached_input_tokens`, so we split them
//! (uncached input vs cache read) to feed the same [`AiUsage`] shape Claude uses.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use serde_json::Value;

use crate::{preview, pricing, push_collapsed, AiUsage, TranscriptEntry, TranscriptRole};

/// One `*_token_usage` block from a `token_count` event.
#[derive(Clone, Copy, Default)]
struct TokenUsage {
    input: u64,
    cached_input: u64,
    output: u64,
    total: u64,
}

fn parse_token_usage(v: Option<&Value>) -> Option<TokenUsage> {
    let v = v?;
    if !v.is_object() {
        return None;
    }
    let g = |k: &str| v.get(k).and_then(|x| x.as_u64()).unwrap_or(0);
    Some(TokenUsage {
        input: g("input_tokens"),
        cached_input: g("cached_input_tokens"),
        output: g("output_tokens"),
        total: g("total_tokens"),
    })
}

/// Parse a Codex rollout JSONL into [`AiUsage`]. Returns `None` if no
/// `token_count` event is present (malformed lines are skipped).
pub fn parse_codex_session(jsonl: &str) -> Option<AiUsage> {
    let mut model = String::new();
    let mut context_window = 0u32;
    let mut total: Option<TokenUsage> = None; // cumulative over the session
    let mut last: Option<TokenUsage> = None; // current turn (= live context size)
    let mut turns = 0u32;

    for line in jsonl.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let payload = v.get("payload");
        match ty {
            "turn_context" => {
                if let Some(m) = payload
                    .and_then(|p| p.get("model"))
                    .and_then(|m| m.as_str())
                {
                    if !m.is_empty() {
                        model = m.to_string();
                    }
                }
            }
            "event_msg" => {
                let Some(p) = payload else { continue };
                match p.get("type").and_then(|t| t.as_str()) {
                    Some("task_started") => {
                        if let Some(w) = p.get("model_context_window").and_then(|x| x.as_u64()) {
                            context_window = w.min(u32::MAX as u64) as u32;
                        }
                    }
                    Some("token_count") => {
                        let Some(info) = p.get("info").filter(|i| i.is_object()) else {
                            continue;
                        };
                        if let Some(w) = info.get("model_context_window").and_then(|x| x.as_u64()) {
                            context_window = w.min(u32::MAX as u64) as u32;
                        }
                        if let Some(t) = parse_token_usage(info.get("total_token_usage")) {
                            total = Some(t);
                        }
                        if let Some(l) = parse_token_usage(info.get("last_token_usage")) {
                            last = Some(l);
                        }
                        turns += 1;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    let total = total?;
    // Codex folds cached input into `input_tokens`; split it back out so the
    // uncached-input vs cache-read costs are billed at the right rates.
    let cache_read = total.cached_input.min(total.input);
    let input = total.input - cache_read;
    let output = total.output;
    // Live context = the last turn's input side (all input, cached + uncached).
    let context_used = last
        .map(|l| l.input.max(l.total.saturating_sub(l.output)))
        .unwrap_or(total.input)
        .min(u32::MAX as u64) as u32;

    let p = pricing::pricing_for(&model);
    // Codex logs the real window; fall back to the pricing table only if absent.
    let mut context_max = if context_window > 0 {
        context_window
    } else {
        p.context_window
    };
    if context_used > context_max {
        context_max = context_used; // never read >100%
    }

    Some(AiUsage {
        cost_usd: p.cost(input, output, 0, cache_read),
        context_max,
        model,
        input,
        output,
        cache_create: 0,
        cache_read,
        context_used,
        turns,
    })
}

/// Incrementally update an existing `AiUsage` with new lines appended to the session.
pub fn update_codex_session(jsonl: &str, mut prev: AiUsage) -> AiUsage {
    let Some(delta) = parse_codex_session(jsonl) else {
        return prev;
    };
    prev.input = delta.input;
    prev.output = delta.output;
    prev.cache_create = delta.cache_create;
    prev.cache_read = delta.cache_read;
    prev.turns += delta.turns;
    prev.context_used = delta.context_used;
    prev.context_max = delta.context_max;
    if !delta.model.is_empty() {
        prev.model = delta.model;
    }
    prev.cost_usd = delta.cost_usd;
    prev
}

/// Parse a Codex rollout JSONL into ordered transcript entries (Tn's own history
/// surface — see [`tn_agent::TranscriptEntry`]). Each line is independent, so this
/// works on a full file or an appended delta.
///
/// The clean conversation pair is `event_msg/user_message` and
/// `event_msg/agent_message` (the `response_item/message` records also carry
/// `developer`/`environment_context` injections and a duplicate of the assistant
/// text, so we don't read those). Tool use comes from `response_item`
/// `function_call`(`_output`) and `custom_tool_call`(`_output`). Encrypted
/// `reasoning` is skipped.
pub fn parse_codex_transcript(jsonl: &str) -> Vec<TranscriptEntry> {
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
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let Some(p) = v.get("payload") else { continue };
        let pt = p.get("type").and_then(|t| t.as_str()).unwrap_or("");
        match (ty, pt) {
            ("event_msg", "user_message") => {
                if let Some(m) = nonempty_str(p.get("message")) {
                    push_collapsed(&mut out, TranscriptEntry::message(TranscriptRole::User, m));
                }
            }
            ("event_msg", "agent_message") => {
                if let Some(m) = nonempty_str(p.get("message")) {
                    push_collapsed(
                        &mut out,
                        TranscriptEntry::message(TranscriptRole::Assistant, m),
                    );
                }
            }
            ("response_item", "function_call") | ("response_item", "custom_tool_call") => {
                let name = p.get("name").and_then(|n| n.as_str()).unwrap_or("tool");
                push_collapsed(&mut out, TranscriptEntry::tool_call(name, codex_tool_summary(p)));
            }
            ("response_item", "function_call_output")
            | ("response_item", "custom_tool_call_output") => {
                let output = p
                    .get("output")
                    .and_then(|o| o.as_str().map(str::to_string).or_else(|| Some(o.to_string())))
                    .unwrap_or_default();
                push_collapsed(&mut out, TranscriptEntry::tool_result(None, preview(&output, 600)));
            }
            _ => {} // reasoning (encrypted) / lifecycle events / context items → skip
        }
    }
    out
}

/// The trimmed string at `v`, or `None` if absent/blank.
fn nonempty_str(v: Option<&Value>) -> Option<&str> {
    let s = v?.as_str()?.trim();
    (!s.is_empty()).then_some(s)
}

/// A one-glance preview of a Codex tool call: `function_call.arguments` is a JSON
/// string (prefer its `command`); `custom_tool_call.input` is the raw payload
/// (e.g. an apply_patch body).
fn codex_tool_summary(p: &Value) -> String {
    if let Some(args) = p.get("arguments").and_then(|a| a.as_str()) {
        if let Ok(parsed) = serde_json::from_str::<Value>(args) {
            if let Some(cmd) = parsed.get("command").and_then(|c| c.as_str()) {
                return preview(cmd, 200);
            }
        }
        return preview(args, 200);
    }
    if let Some(input) = p.get("input").and_then(|i| i.as_str()) {
        return preview(input, 200);
    }
    String::new()
}

/// `$CODEX_HOME/sessions` (default `~/.codex/sessions`), if it exists.
pub fn codex_sessions_dir() -> Option<PathBuf> {
    let base = match std::env::var_os("CODEX_HOME") {
        Some(h) => PathBuf::from(h),
        None => {
            let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"))?;
            Path::new(&home).join(".codex")
        }
    };
    let dir = base.join("sessions");
    dir.is_dir().then_some(dir)
}

/// Normalize a path for case/separator-insensitive comparison (`D:/x/` ==
/// `d:\x`). Codex stores the launch cwd in each session's `session_meta`.
fn norm_path(p: &str) -> String {
    p.trim()
        .trim_end_matches(['/', '\\'])
        .replace('/', "\\")
        .to_ascii_lowercase()
}

/// Read just the first line of a file (the `session_meta` record).
fn first_line(path: &Path) -> Option<String> {
    let mut r = BufReader::new(std::fs::File::open(path).ok()?);
    let mut s = String::new();
    r.read_line(&mut s).ok()?;
    Some(s)
}

/// The `cwd` recorded in a `session_meta` line, if it is one.
fn session_meta_cwd(line: &str) -> Option<String> {
    let v: Value = serde_json::from_str(line.trim()).ok()?;
    if v.get("type").and_then(|t| t.as_str()) != Some("session_meta") {
        return None;
    }
    v.get("payload")?.get("cwd")?.as_str().map(str::to_string)
}

/// Collect every `rollout-*.jsonl` under `dir` (recursively), with its mtime.
fn collect_rollouts(dir: &Path, out: &mut Vec<(SystemTime, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            collect_rollouts(&path, out);
        } else if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(".jsonl"))
        {
            if let Some(mtime) = entry.metadata().ok().and_then(|m| m.modified().ok()) {
                out.push((mtime, path));
            }
        }
    }
}

/// Newest Codex rollout whose `session_meta.cwd` matches `cwd`. Scans rollouts
/// newest-first and reads only their first line, so the cost is bounded even
/// with a deep session history (capped at the most recent [`SCAN_CAP`]).
pub fn latest_codex_session_file(cwd: &str) -> Option<PathBuf> {
    /// Don't read first lines of more than this many rollouts looking for a match.
    const SCAN_CAP: usize = 80;
    let dir = codex_sessions_dir()?;
    let want = norm_path(cwd);
    let mut rollouts = Vec::new();
    collect_rollouts(&dir, &mut rollouts);
    rollouts.sort_by(|a, b| b.0.cmp(&a.0)); // newest first
    rollouts
        .into_iter()
        .take(SCAN_CAP)
        .find(|(_, path)| {
            first_line(path)
                .as_deref()
                .and_then(session_meta_cwd)
                .map(|c| norm_path(&c) == want)
                .unwrap_or(false)
        })
        .map(|(_, p)| p)
}

/// Newest Codex rollout overall (any cwd). Fallback for an agent pane whose
/// session cwd doesn't match the app cwd (Codex often runs in `~`).
pub fn latest_codex_session_any() -> Option<PathBuf> {
    let dir = codex_sessions_dir()?;
    let mut rollouts = Vec::new();
    collect_rollouts(&dir, &mut rollouts);
    rollouts.sort_by(|a, b| b.0.cmp(&a.0));
    rollouts.into_iter().next().map(|(_, p)| p)
}

/// Every Codex rollout, as `(path, mtime)`. Keyed on mtime by the pane-binding
/// logic (see the Claude analogue + `detect::resolve_session_for_pane`): a
/// resumed session reuses an old file, so creation time can't identify it.
pub fn codex_sessions_with_mtime() -> Vec<(PathBuf, SystemTime)> {
    let Some(dir) = codex_sessions_dir() else {
        return Vec::new();
    };
    let mut rollouts = Vec::new();
    collect_rollouts(&dir, &mut rollouts); // pushes (mtime, path)
    rollouts
        .into_iter()
        .map(|(mtime, path)| (path, mtime))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Trimmed shape of a real rollout: meta (cwd) + turn_context (model) +
    // task_started (window) + two token_count events.
    const SAMPLE: &str = r#"
{"type":"session_meta","payload":{"id":"abc","cwd":"C:\\Users\\Gua","cli_version":"0.133.0"}}
{"type":"event_msg","payload":{"type":"task_started","model_context_window":950000}}
{"type":"turn_context","payload":{"turn_id":"t1","model":"gpt-5-codex","cwd":"C:\\Users\\Gua"}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":100,"reasoning_output_tokens":0,"total_tokens":1100},"last_token_usage":{"input_tokens":1000,"cached_input_tokens":800,"output_tokens":100,"reasoning_output_tokens":0,"total_tokens":1100},"model_context_window":950000}}}
{"type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":5000,"cached_input_tokens":4000,"output_tokens":300,"reasoning_output_tokens":50,"total_tokens":5300},"last_token_usage":{"input_tokens":4000,"cached_input_tokens":3800,"output_tokens":200,"reasoning_output_tokens":50,"total_tokens":4200},"model_context_window":950000}}}
"#;

    #[test]
    fn parses_tokens_context_window_and_split() {
        let u = parse_codex_session(SAMPLE).expect("usage");
        assert_eq!(u.model, "gpt-5-codex");
        assert_eq!(u.turns, 2);
        // cumulative total = last token_count's total_token_usage (5000/4000/300)
        assert_eq!(u.cache_read, 4000);
        assert_eq!(u.input, 1000); // 5000 input - 4000 cached
        assert_eq!(u.output, 300);
        // live context = last turn's input side = 4000
        assert_eq!(u.context_used, 4000);
        // window taken straight from the log, not the pricing table
        assert_eq!(u.context_max, 950_000);
        // gpt/codex pricing: 2.5 / 10 / 0 / 0.25 per MTok
        let expect = 1000.0 / 1e6 * 2.5 + 300.0 / 1e6 * 10.0 + 4000.0 / 1e6 * 0.25;
        assert!((u.cost_usd - expect).abs() < 1e-9);
        assert!(u.context_frac() < 0.01);
    }

    #[test]
    fn unknown_model_reports_tokens_without_cost() {
        // A custom provider id ("moonbridge") isn't in the pricing table: tokens
        // and context still parse, but cost stays 0 (never a wrong dollar figure).
        let s = SAMPLE.replace("gpt-5-codex", "moonbridge");
        let u = parse_codex_session(&s).unwrap();
        assert_eq!(u.model, "moonbridge");
        assert_eq!(u.context_max, 950_000);
        assert_eq!(u.cost_usd, 0.0);
    }

    #[test]
    fn no_token_count_is_none() {
        let s = r#"{"type":"session_meta","payload":{"cwd":"C:\\x"}}"#;
        assert!(parse_codex_session(s).is_none());
    }

    // Trimmed real rollout shapes: user_message + agent_message events, a
    // function_call + its output, a custom_tool_call, plus records we skip
    // (developer context message, reasoning, token_count).
    const TRANSCRIPT_SAMPLE: &str = r#"
{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"<permissions instructions>"}]}}
{"type":"event_msg","payload":{"type":"user_message","message":"  你好  ","images":[]}}
{"type":"response_item","payload":{"type":"reasoning","summary":[],"encrypted_content":"gAAA"}}
{"type":"event_msg","payload":{"type":"agent_message","message":"hi there"}}
{"type":"response_item","payload":{"type":"function_call","name":"shell_command","arguments":"{\"command\":\"Get-Content x\",\"workdir\":\"C:\\\\x\"}"}}
{"type":"response_item","payload":{"type":"function_call_output","call_id":"c1","output":"Exit code: 0\nOutput:\nok"}}
{"type":"response_item","payload":{"type":"custom_tool_call","name":"apply_patch","input":"*** Begin Patch"}}
{"type":"event_msg","payload":{"type":"token_count","info":{}}}
"#;

    #[test]
    fn transcript_uses_event_messages_and_tool_items_skips_noise() {
        let t = parse_codex_transcript(TRANSCRIPT_SAMPLE);
        assert_eq!(
            t.len(),
            5,
            "user + assistant + function_call + function_output + custom_tool_call"
        );

        assert_eq!(t[0].role, TranscriptRole::User);
        assert_eq!(t[0].text, "你好"); // trimmed; developer/context message skipped

        assert_eq!(t[1].role, TranscriptRole::Assistant);
        assert_eq!(t[1].text, "hi there");

        // function_call → Tool/ToolCall; arguments JSON parsed → command preview.
        assert_eq!(t[2].role, TranscriptRole::Tool);
        assert_eq!(t[2].kind, crate::TranscriptKind::ToolCall);
        assert_eq!(t[2].tool.as_deref(), Some("shell_command"));
        assert_eq!(t[2].text, "Get-Content x");

        assert_eq!(t[3].kind, crate::TranscriptKind::ToolResult);
        assert!(t[3].text.contains("Exit code: 0"));

        // custom_tool_call (apply_patch) → ToolCall with the raw input preview.
        assert_eq!(t[4].tool.as_deref(), Some("apply_patch"));
        assert_eq!(t[4].text, "*** Begin Patch");
    }

    #[test]
    fn transcript_collapses_consecutive_duplicate_user_messages() {
        // Codex re-emits the same user_message on resends/forks (real logs show a
        // bare `你好`/`你好` rollout). Consecutive exact dups collapse to one.
        let s = r#"
{"type":"event_msg","payload":{"type":"user_message","message":"你好"}}
{"type":"event_msg","payload":{"type":"user_message","message":"你好"}}
"#;
        let t = parse_codex_transcript(s);
        assert_eq!(t.len(), 1);
        assert_eq!(t[0].text, "你好");
    }

    #[test]
    fn norm_path_is_case_and_sep_insensitive() {
        assert_eq!(norm_path("D:/coder/Tn/"), norm_path("d:\\coder\\tn"));
    }

    #[test]
    fn session_meta_cwd_extracts_cwd() {
        let line = r#"{"type":"session_meta","payload":{"cwd":"C:\\Users\\Gua"}}"#;
        assert_eq!(session_meta_cwd(line).as_deref(), Some("C:\\Users\\Gua"));
        // a non-meta line yields nothing
        assert!(session_meta_cwd(r#"{"type":"turn_context","payload":{}}"#).is_none());
    }
}
