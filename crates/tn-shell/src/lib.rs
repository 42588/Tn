//! Shell integration (M3).
//!
//! alacritty's VT engine doesn't surface OSC 133 / 633 / 7 shell-integration
//! sequences, so we run a **bypass** [`vte::Parser`] over the same PTY output
//! bytes that feed the grid, and extract the semantic markers as [`BlockEvent`]s.
//! The raw stream still drives `tn-core`; this is a pure side-channel that
//! `tn-blocks` turns into Warp-style command blocks.
//!
//! Recognized sequences:
//! - **OSC 133** (FinalTerm / FTCS): `A` prompt start, `B` command start,
//!   `C` output start, `D[;exit]` command finished.
//! - **OSC 633** (VS Code): `A`/`B`/`C`/`D[;exit]` as above, plus `E;<cmdline>`
//!   (command line) and `P;Cwd=<path>` (properties).
//! - **OSC 7**: `file://host/path` working directory.

use vte::{Parser, Perform};

mod integration;
pub use integration::Integration;

/// A semantic shell-integration event extracted from the PTY byte stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BlockEvent {
    /// Prompt drawing begins (FTCS `A` / 633 `A`).
    PromptStart,
    /// Prompt end, command input begins (FTCS `B` / 633 `B`).
    CommandStart,
    /// Command executed, output begins (FTCS `C` / 633 `C`).
    OutputStart,
    /// Command finished, with the exit code if reported (FTCS `D` / 633 `D`).
    CommandFinished { exit: Option<i32> },
    /// The command line text (633 `E`).
    CommandLine(String),
    /// Working directory (OSC 7, or 633 `P;Cwd=`).
    Cwd(String),
}

/// Bypass parser: feed it PTY output, get back the shell-integration events.
/// Only OSC is interpreted; CSI / printable / control bytes are ignored (the
/// real grid is updated separately by `tn-core` from the same bytes).
pub struct ShellParser {
    parser: Parser,
}

impl Default for ShellParser {
    fn default() -> Self {
        Self::new()
    }
}

impl ShellParser {
    /// A fresh parser with no buffered escape-sequence state.
    pub fn new() -> Self {
        Self { parser: Parser::new() }
    }

    /// Feed PTY bytes; returns the shell-integration events found within them.
    pub fn advance(&mut self, bytes: &[u8]) -> Vec<BlockEvent> {
        let mut sink = Sink { events: Vec::new() };
        self.parser.advance(&mut sink, bytes);
        sink.events
    }
}

/// `vte::Perform` that only collects OSC shell-integration markers.
struct Sink {
    events: Vec<BlockEvent>,
}

impl Perform for Sink {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some(&kind) = params.first() else {
            return;
        };
        if kind == b"133" {
            self.ftcs(&params[1..]);
        } else if kind == b"633" {
            self.vscode(&params[1..]);
        } else if kind == b"7" {
            if let Some(p) = params.get(1).and_then(|u| parse_file_uri(u)) {
                self.events.push(BlockEvent::Cwd(p));
            }
        }
    }
}

impl Sink {
    /// OSC 133 (FTCS) and the A/B/C/D subset shared with 633.
    fn ftcs(&mut self, rest: &[&[u8]]) {
        let Some(&tag) = rest.first() else {
            return;
        };
        let ev = if tag == b"A" {
            BlockEvent::PromptStart
        } else if tag == b"B" {
            BlockEvent::CommandStart
        } else if tag == b"C" {
            BlockEvent::OutputStart
        } else if tag == b"D" {
            BlockEvent::CommandFinished {
                exit: rest.get(1).and_then(|c| parse_i32(c)),
            }
        } else {
            return;
        };
        self.events.push(ev);
    }

    /// OSC 633 (VS Code): A/B/C/D plus E (command line) and P (properties).
    fn vscode(&mut self, rest: &[&[u8]]) {
        let Some(&tag) = rest.first() else {
            return;
        };
        if tag == b"E" {
            if let Some(cmd) = rest.get(1).and_then(|c| std::str::from_utf8(c).ok()) {
                self.events.push(BlockEvent::CommandLine(unescape_633(cmd)));
            }
        } else if tag == b"P" {
            for prop in &rest[1..] {
                if let Some(cwd) = std::str::from_utf8(prop)
                    .ok()
                    .and_then(|s| s.strip_prefix("Cwd="))
                {
                    self.events.push(BlockEvent::Cwd(cwd.to_string()));
                }
            }
        } else {
            self.ftcs(rest); // A/B/C/D are identical to FTCS
        }
    }
}

fn parse_i32(b: &[u8]) -> Option<i32> {
    std::str::from_utf8(b).ok()?.trim().parse().ok()
}

/// Decode an OSC 7 `file://host/path` URI to a filesystem path: strip the
/// authority, percent-decode, and drop a Windows leading slash before a drive
/// (`/C:/x` → `C:/x`). Best-effort.
fn parse_file_uri(b: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(b).ok()?;
    let rest = s.strip_prefix("file://").unwrap_or(s);
    let path = match rest.find('/') {
        Some(i) => &rest[i..],
        None => rest,
    };
    let decoded = percent_decode(path);
    let win_drive = decoded
        .strip_prefix('/')
        .is_some_and(|p| p.as_bytes().get(1) == Some(&b':'));
    Some(if win_drive {
        decoded[1..].to_string()
    } else {
        decoded
    })
}

/// Minimal `%XX` percent-decoding.
fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(h) = hex2(b[i + 1], b[i + 2]) {
                out.push(h);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// VS Code OSC 633 escapes control chars / `;` / `\` as `\xHH`. Reverse that.
fn unescape_633(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && (b[i + 1] == b'x' || b[i + 1] == b'X') {
            if let Some(h) = hex2(b[i + 2], b[i + 3]) {
                out.push(h);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex2(a: u8, b: u8) -> Option<u8> {
    let nib = |c: u8| match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        b'A'..=b'F' => Some(c - b'A' + 10),
        _ => None,
    };
    Some((nib(a)? << 4) | nib(b)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(bytes: &[u8]) -> Vec<BlockEvent> {
        ShellParser::new().advance(bytes)
    }

    #[test]
    fn ftcs_prompt_and_command_markers() {
        // BEL-terminated OSC.
        assert_eq!(
            run(b"\x1b]133;A\x07\x1b]133;B\x07"),
            vec![BlockEvent::PromptStart, BlockEvent::CommandStart]
        );
    }

    #[test]
    fn ftcs_finished_exit_code() {
        // ST-terminated (ESC \\).
        assert_eq!(
            run(b"\x1b]133;D;0\x1b\\"),
            vec![BlockEvent::CommandFinished { exit: Some(0) }]
        );
        assert_eq!(
            run(b"\x1b]133;D;130\x07"),
            vec![BlockEvent::CommandFinished { exit: Some(130) }]
        );
        assert_eq!(
            run(b"\x1b]133;D\x07"),
            vec![BlockEvent::CommandFinished { exit: None }]
        );
    }

    #[test]
    fn full_command_cycle() {
        let s = b"\x1b]133;A\x07PS C:\\> \x1b]133;B\x07dir\x1b]133;C\x07<output>\x1b]133;D;0\x07";
        assert_eq!(
            run(s),
            vec![
                BlockEvent::PromptStart,
                BlockEvent::CommandStart,
                BlockEvent::OutputStart,
                BlockEvent::CommandFinished { exit: Some(0) },
            ]
        );
    }

    #[test]
    fn vscode_command_line_and_cwd() {
        assert_eq!(
            run(b"\x1b]633;E;git status\x07"),
            vec![BlockEvent::CommandLine("git status".into())]
        );
        assert_eq!(
            run(b"\x1b]633;P;Cwd=/home/me\x07"),
            vec![BlockEvent::Cwd("/home/me".into())]
        );
        // 633 A/B/C/D behave like FTCS.
        assert_eq!(run(b"\x1b]633;A\x07"), vec![BlockEvent::PromptStart]);
    }

    #[test]
    fn vscode_escaped_command_line() {
        // "echo a;b" with the ';' escaped as \x3b.
        assert_eq!(
            run(b"\x1b]633;E;echo a\\x3bb\x07"),
            vec![BlockEvent::CommandLine("echo a;b".into())]
        );
    }

    #[test]
    fn osc7_cwd_unix_and_windows() {
        assert_eq!(
            run(b"\x1b]7;file://host/home/me\x07"),
            vec![BlockEvent::Cwd("/home/me".into())]
        );
        assert_eq!(
            run(b"\x1b]7;file:///C:/Users/Gua\x07"),
            vec![BlockEvent::Cwd("C:/Users/Gua".into())]
        );
        // percent-encoded space.
        assert_eq!(
            run(b"\x1b]7;file://host/home/my%20dir\x07"),
            vec![BlockEvent::Cwd("/home/my dir".into())]
        );
    }

    #[test]
    fn ignores_other_osc_and_plain_text() {
        // OSC 0 (title) and plain text are not block events.
        assert_eq!(
            run(b"hello\x1b]0;some title\x07world\x1b]133;A\x07"),
            vec![BlockEvent::PromptStart]
        );
    }

    #[test]
    fn split_across_advance_calls() {
        // The parser is stateful: a sequence split across two feeds still parses.
        let mut p = ShellParser::new();
        assert_eq!(p.advance(b"\x1b]133;"), vec![]);
        assert_eq!(p.advance(b"D;42\x07"), vec![BlockEvent::CommandFinished { exit: Some(42) }]);
    }
}
