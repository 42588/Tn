//! [`TranscriptEntry`] — one normalized, render-ready item of an agent's
//! conversation history, parsed from its session log by an [`AgentAdapter`]
//! (`tn-ai` maps Claude `*.jsonl` / Codex rollout records into these).
//!
//! This is the data behind Tn's **own** scrollable transcript surface: TUI
//! agents (Claude/Codex) only repaint the visible viewport, so the terminal's
//! scrollback never holds the full conversation. Instead Tn tails the session
//! log and renders these entries itself — full history, reachable on resume,
//! independent of how the agent was launched. See the
//! `2026-06-13-agent自管transcript设计` task doc.
//!
//! Entries are intentionally **flat and presentation-light**: a role, a coarse
//! kind, an optional tool name, and plain text (markdown *source*, not yet
//! rendered). The UI decides how to lay them out.

/// Who produced an entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptRole {
    User,
    Assistant,
    /// A tool call or its result (the agent's tool use, e.g. shell/edit/search).
    Tool,
    /// Host/system context (rare; e.g. a compaction marker).
    System,
}

/// The coarse shape of an entry, so the UI can style/collapse it without
/// re-parsing. `Message` is ordinary prose; `ToolCall`/`ToolResult` carry a tool
/// name in [`TranscriptEntry::tool`]; `Reasoning` is model thinking (usually
/// collapsed or hidden).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TranscriptKind {
    Message,
    Reasoning,
    ToolCall,
    ToolResult,
}

/// One conversation item. `text` is the readable body (a tool call's is a short
/// summary of name + arguments; a tool result's is its output). For
/// `ToolCall`/`ToolResult`, `tool` names the tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptEntry {
    pub role: TranscriptRole,
    pub kind: TranscriptKind,
    pub tool: Option<String>,
    pub text: String,
}

impl TranscriptEntry {
    /// A plain prose message from `role`.
    pub fn message(role: TranscriptRole, text: impl Into<String>) -> Self {
        Self {
            role,
            kind: TranscriptKind::Message,
            tool: None,
            text: text.into(),
        }
    }

    /// A tool invocation: `tool` is the tool name, `summary` a one-glance preview.
    pub fn tool_call(tool: impl Into<String>, summary: impl Into<String>) -> Self {
        Self {
            role: TranscriptRole::Tool,
            kind: TranscriptKind::ToolCall,
            tool: Some(tool.into()),
            text: summary.into(),
        }
    }

    /// A tool's result/output. `tool` is the tool name when known.
    pub fn tool_result(tool: Option<String>, output: impl Into<String>) -> Self {
        Self {
            role: TranscriptRole::Tool,
            kind: TranscriptKind::ToolResult,
            tool,
            text: output.into(),
        }
    }
}

/// Push `entry` unless it exactly equals the last one already in `out`, so
/// consecutive duplicates collapse to one. Agents re-emit identical records on
/// resends / forks / resume — notably Codex logs the same `user_message` event
/// several times — and the history surface should show that turn once.
pub fn push_collapsed(out: &mut Vec<TranscriptEntry>, entry: TranscriptEntry) {
    if out.last() != Some(&entry) {
        out.push(entry);
    }
}

/// Trim a possibly-huge tool/result blob to a bounded preview for the transcript
/// list, collapsing leading/trailing blank lines and capping length. The full
/// text lives in the log; the history surface only needs a readable glance.
pub fn preview(text: &str, max_chars: usize) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= max_chars {
        return trimmed.to_string();
    }
    let mut out: String = trimmed.chars().take(max_chars).collect();
    out.push('…');
    out
}
