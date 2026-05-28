//! File / diff viewer (M4 chrome) — the mockup's right column.
//!
//! Shows a file the user clicked in the explorer: the **File** tab renders it
//! with line numbers and a light, best-effort syntax tint; the **Diff** tab
//! runs `git diff` for that path and renders the unified hunks with `+`/`-`
//! styling. It's a Calm Glass panel (chrome, not a split-tree node). Content is
//! read once on open / tab-switch and cached, so it does no work per frame.

use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use gpui::{
    div, linear_color_stop, linear_gradient, prelude::*, px, rgba, Context, FocusHandle,
    MouseButton, Rgba, SharedString,
};
use tn_config::Loaded;

use crate::style::{col, cola, HOVER, RIM, UI_SANS};

/// Max lines rendered (a viewer is a glance, not a pager).
const MAX_LINES: usize = 500;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    File,
    Diff,
}

/// A syntax tint class (best-effort, language-agnostic-ish).
#[derive(Clone, Copy)]
enum Tint {
    Plain,
    Keyword,
    Type,
    Str,
    Comment,
    Call,
    Num,
}

const KEYWORDS: &[&str] = &[
    "fn", "let", "mut", "pub", "impl", "for", "in", "if", "else", "match", "struct", "enum", "use",
    "return", "self", "Self", "mod", "trait", "where", "as", "move", "async", "await", "const",
    "static", "ref", "type", "crate", "super", "dyn", "while", "loop", "break", "continue", "true",
    "false", "unsafe", "extern", "default",
];

fn classify(word: &str, is_call: bool) -> Tint {
    if KEYWORDS.contains(&word) {
        Tint::Keyword
    } else if is_call {
        Tint::Call
    } else if word.chars().next().is_some_and(|c| c.is_ascii_uppercase()) {
        Tint::Type
    } else {
        Tint::Plain
    }
}

/// Tokenize one line into (text, tint) runs. A tiny hand scanner: line comments,
/// double-quoted strings, words (keyword / type / call / ident), numbers, and
/// runs of punctuation. Not a real parser — just enough to read like code.
fn highlight(line: &str) -> Vec<(String, Tint)> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        // line comment to end
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            out.push((chars[i..].iter().collect(), Tint::Comment));
            break;
        }
        // string literal
        if c == '"' {
            let mut j = i + 1;
            while j < n {
                if chars[j] == '\\' {
                    j += 2;
                    continue;
                }
                if chars[j] == '"' {
                    j += 1;
                    break;
                }
                j += 1;
            }
            let end = j.min(n);
            out.push((chars[i..end].iter().collect(), Tint::Str));
            i = end;
            continue;
        }
        // word
        if c.is_alphabetic() || c == '_' {
            let mut j = i;
            while j < n && (chars[j].is_alphanumeric() || chars[j] == '_') {
                j += 1;
            }
            let w: String = chars[i..j].iter().collect();
            let is_call = j < n && chars[j] == '(';
            let t = classify(&w, is_call);
            out.push((w, t));
            i = j;
            continue;
        }
        // number
        if c.is_ascii_digit() {
            let mut j = i;
            while j < n && (chars[j].is_ascii_digit() || chars[j] == '.' || chars[j] == '_') {
                j += 1;
            }
            out.push((chars[i..j].iter().collect(), Tint::Num));
            i = j;
            continue;
        }
        // run of other characters (punctuation / whitespace)
        let mut j = i;
        while j < n {
            let d = chars[j];
            if d.is_alphanumeric() || d == '_' || d == '"' || (d == '/' && j + 1 < n && chars[j + 1] == '/') {
                break;
            }
            j += 1;
        }
        out.push((chars[i..j].iter().collect(), Tint::Plain));
        i = j;
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DiffKind {
    Ctx,
    Add,
    Del,
    Hunk,
}

struct DiffLine {
    kind: DiffKind,
    new_no: Option<u32>,
    text: String,
}

pub struct ViewerView {
    config: Arc<Loaded>,
    root: PathBuf,
    path: Option<PathBuf>,
    tab: Tab,
    file_lines: Vec<String>,
    file_truncated: bool,
    diff: Vec<DiffLine>,
    focus_handle: FocusHandle,
}

impl ViewerView {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            config,
            root,
            path: None,
            tab: Tab::File,
            file_lines: Vec::new(),
            file_truncated: false,
            diff: Vec::new(),
            focus_handle: cx.focus_handle(),
        }
    }

    /// `(filename, language)` for the open file — drives the status bar's
    /// "element.rs · Rust" segment.
    pub fn status(&self) -> Option<(String, &'static str)> {
        let p = self.path.as_ref()?;
        let name = p.file_name()?.to_string_lossy().to_string();
        let lang = match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "rs" => "Rust",
            "toml" => "TOML",
            "md" => "Markdown",
            "json" => "JSON",
            "js" | "mjs" | "cjs" => "JavaScript",
            "ts" | "tsx" => "TypeScript",
            "py" => "Python",
            "html" | "htm" => "HTML",
            "css" => "CSS",
            "sh" | "bash" => "Shell",
            "ps1" => "PowerShell",
            "yml" | "yaml" => "YAML",
            "lock" => "Lock",
            "txt" => "Text",
            _ => "Plain",
        };
        Some((name, lang))
    }

    /// Open `path`: read its text + compute its git diff, default to the File tab.
    pub fn open(&mut self, path: PathBuf) {
        self.path = Some(path.clone());
        self.tab = Tab::File;
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let all: Vec<String> = text.lines().map(str::to_string).collect();
        self.file_truncated = all.len() > MAX_LINES;
        self.file_lines = all.into_iter().take(MAX_LINES).collect();
        self.diff = self.compute_diff(&path);
    }

    /// `git diff` for `path`, parsed into renderable lines (tracking new-file
    /// line numbers from each hunk header). Empty when not a repo / no changes.
    fn compute_diff(&self, path: &PathBuf) -> Vec<DiffLine> {
        let rel = path.strip_prefix(&self.root).unwrap_or(path);
        let output = Command::new("git")
            .arg("-C")
            .arg(&self.root)
            .arg("diff")
            .arg("--no-color")
            .arg("--")
            .arg(rel)
            .output();
        let Ok(out) = output else { return Vec::new() };
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = Vec::new();
        let mut new_no = 0u32;
        for line in text.lines() {
            if line.starts_with("diff ")
                || line.starts_with("index ")
                || line.starts_with("--- ")
                || line.starts_with("+++ ")
                || line.starts_with("old mode")
                || line.starts_with("new mode")
                || line.starts_with("similarity")
                || line.starts_with("rename ")
            {
                continue;
            }
            if let Some(rest) = line.strip_prefix("@@") {
                // @@ -a,b +c,d @@  → start tracking at c
                if let Some(plus) = rest.split('+').nth(1) {
                    let num: String = plus.chars().take_while(|c| c.is_ascii_digit()).collect();
                    new_no = num.parse().unwrap_or(new_no);
                }
                lines.push(DiffLine { kind: DiffKind::Hunk, new_no: None, text: line.to_string() });
                continue;
            }
            let (kind, text) = match line.chars().next() {
                Some('+') => (DiffKind::Add, line[1..].to_string()),
                Some('-') => (DiffKind::Del, line[1..].to_string()),
                _ => (DiffKind::Ctx, line.strip_prefix(' ').unwrap_or(line).to_string()),
            };
            let no = if kind == DiffKind::Del {
                None
            } else {
                let n = new_no;
                new_no += 1;
                Some(n)
            };
            lines.push(DiffLine { kind, new_no: no, text });
            if lines.len() >= MAX_LINES {
                break;
            }
        }
        lines
    }

    fn tint_color(&self, t: Tint) -> Rgba {
        let th = &self.config.theme;
        match t {
            Tint::Plain => col(th.ui.foreground),
            Tint::Keyword => col(th.ui.accent_alt),
            Tint::Type => col(th.ansi.cyan),
            Tint::Str => col(th.ansi.green),
            Tint::Comment => col(th.ui.muted),
            Tint::Call => col(th.ui.accent),
            Tint::Num => col(th.ansi.yellow),
        }
    }

    fn render_file(&self) -> gpui::Div {
        let th = &self.config.theme;
        let rows = self.file_lines.iter().enumerate().map(|(i, line)| {
            let spans = highlight(line).into_iter().map(|(text, tint)| {
                div().text_color(self.tint_color(tint)).child(SharedString::from(text))
            });
            div()
                .flex()
                .flex_row()
                .child(
                    div()
                        .w(px(40.))
                        .flex_none()
                        .pr_2()
                        .text_color(col(th.ui.muted))
                        .child(SharedString::from(format!("{}", i + 1))),
                )
                .child(div().flex().flex_row().children(spans))
        });
        div().flex().flex_col().children(rows)
    }

    fn render_diff(&self) -> gpui::Div {
        let th = &self.config.theme;
        if self.diff.is_empty() {
            return div()
                .p_3()
                .text_color(col(th.ui.muted))
                .child("无改动 · git working tree clean");
        }
        let rows = self.diff.iter().map(|d| {
            let (bg, mark, mark_col, txt_col) = match d.kind {
                // mockup .cl.add/.del:bg=绿/红 @ .09;.ln/.mk 同色;正文不暗化(del 不 muted)
                DiffKind::Add => (cola(th.ansi.green, 0.09), "+", col(th.ansi.green), col(th.ui.foreground)),
                DiffKind::Del => (cola(th.ansi.red, 0.09), "-", col(th.ansi.red), col(th.ui.foreground)),
                DiffKind::Hunk => (rgba(0x00000000), " ", col(th.ui.accent_alt), col(th.ui.accent_alt)),
                DiffKind::Ctx => (rgba(0x00000000), " ", col(th.ui.muted), col(th.ui.foreground)),
            };
            let no = d.new_no.map(|n| format!("{n}")).unwrap_or_default();
            div()
                .flex()
                .flex_row()
                .bg(bg)
                // mockup .cl .ln:width 38 · faint #474E72 · 11px · 右对齐 · margin-right 14
                .child(div().w(px(38.)).flex_none().mr(px(14.)).text_right().text_size(px(11.)).text_color(gpui::rgb(0x474E72)).child(SharedString::from(no)))
                .child(div().w(px(14.)).flex_none().text_color(mark_col).child(mark))
                .child(div().text_color(txt_col).child(SharedString::from(d.text.clone())))
        });
        div().flex().flex_col().children(rows)
    }

    fn render_header(&self) -> gpui::Div {
        let th = &self.config.theme;
        let rel = self
            .path
            .as_ref()
            .and_then(|p| p.strip_prefix(&self.root).ok().or(Some(p.as_path())))
            .map(|p| p.to_string_lossy().replace('\\', "/"))
            .unwrap_or_default();
        let (dir, name) = match rel.rfind('/') {
            Some(i) => (rel[..=i].to_string(), rel[i + 1..].to_string()),
            None => (String::new(), rel.clone()),
        };
        let tab_chip = |label: &'static str, on: bool| {
            div()
                .px_2()
                .py(px(2.))
                .rounded(px(7.))
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight::BOLD)
                .text_color(col(if on { th.ui.foreground } else { th.ui.muted }))
                .when(on, |d| d.bg(rgba(HOVER)))
                .child(label)
        };
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .h(px(32.))
            .px_3()
            .flex_none()
            .font_family(UI_SANS) // header chrome = sans (code stays mono)
            .text_size(px(11.5))
            .child(crate::assets::icon("file", 14.).text_color(col(th.ui.accent)))
            .child(div().text_color(col(th.ui.muted)).child(SharedString::from(dir)))
            .child(
                div()
                    .text_color(col(th.ui.accent))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(SharedString::from(name)),
            )
            .child(div().flex_1())
            .when(!self.diff.is_empty(), |d| {
                d.child(crate::assets::icon("pen", 13.).text_color(col(th.agents.claude)))
                    .child(div().text_size(px(11.)).text_color(col(th.agents.claude)).child("已改动"))
            })
            .child(tab_chip("Diff", self.tab == Tab::Diff))
            .child(tab_chip("File", self.tab == Tab::File))
    }
}

impl Render for ViewerView {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let th = &self.config.theme;
        // Tab chips are clickable: wire them via overlay listeners on the header
        // children would be awkward, so the header builds plain chips and we add
        // click targets here by wrapping. Simpler: rebuild header with listeners.
        let body = if self.path.is_none() {
            div()
                .flex_1()
                .flex()
                .items_center()
                .justify_center()
                .text_color(col(th.ui.muted))
                .child("在左侧选择文件查看")
        } else if self.tab == Tab::File {
            let mut v = div().flex_1().min_h(px(0.)).overflow_hidden().p_2().child(self.render_file());
            if self.file_truncated {
                v = v.child(
                    div()
                        .px_2()
                        .py_1()
                        .text_color(col(th.ui.muted))
                        .child(SharedString::from(format!("… 仅显示前 {MAX_LINES} 行"))),
                );
            }
            v
        } else {
            div().flex_1().min_h(px(0.)).overflow_hidden().p_2().child(self.render_diff())
        };

        // Clickable Diff/File toggles (cover the header chips).
        let to_file = cx.listener(|this, _e, _w, cx| {
            this.tab = Tab::File;
            cx.notify();
        });
        let to_diff = cx.listener(|this, _e, _w, cx| {
            this.tab = Tab::Diff;
            cx.notify();
        });

        div()
            .track_focus(&self.focus_handle)
            .size_full()
            .flex()
            .flex_col()
            .min_h(px(0.))
            .overflow_hidden()
            .rounded(px(14.))
            .border_1()
            .border_color(rgba(RIM))
            // mockup .viewer 是 .pane:用 g1 玻璃渐变(与其它面板一致)
            .bg(linear_gradient(
                180.,
                linear_color_stop(rgba(0x2a2e446b), 0.), // rgba(42,46,68,.42)
                linear_color_stop(rgba(0x1a1c2c85), 1.), // rgba(26,28,44,.52)
            ))
            .font_family(SharedString::from(self.config.font().family.clone()))
            .text_size(px(12.5)) // mockup .code font-size:12.5px
            .child(
                // header + invisible click targets aligned to the right tab chips
                div()
                    .relative()
                    .child(self.render_header())
                    .child(
                        div()
                            .absolute()
                            .top(px(0.))
                            .right(px(0.))
                            .flex()
                            .flex_row()
                            .h(px(32.))
                            .items_center()
                            .gap_2()
                            .pr_3()
                            .child(div().w(px(34.)).h(px(20.)).on_mouse_down(MouseButton::Left, to_diff))
                            .child(div().w(px(34.)).h(px(20.)).on_mouse_down(MouseButton::Left, to_file)),
                    ),
            )
            .child(body)
    }
}
