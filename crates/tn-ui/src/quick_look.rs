//! Quick Look 速览浮层(prototype ③ 侧栏 · 速览编辑)— the redesigned file viewer.
//!
//! Selecting a file in the explorer pops a **floating glass overlay** hugging the
//! tree's right edge and floating *over* the terminal (it no longer docks as a
//! permanent right column — that ate split space). The **File** tab renders the
//! file with line numbers + a light syntax tint; the **Diff** tab runs `git diff`
//! and renders the unified hunks with `+`/`-` styling. A left **seam** (accent
//! vertical line) points back at the selected file in the tree. Content is read
//! once on open / tab-switch and cached, so it does no work per frame.
//!
//! Keyboard nav (Space toggle · ↑↓ change file · Enter edit) + real in-place
//! editing (Ctrl+S) are the prototype's full model but are ⏳ deferred (need
//! explorer keyboard focus + an editable text buffer); this is the visual overlay
//! + click-to-open + Diff/File toggle. See docs/架构蓝图 §8 ①.

use std::path::PathBuf;
use std::process::Command;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{
    div, linear_color_stop, linear_gradient, prelude::*, px, rgba, uniform_list, Context,
    FocusHandle, MouseButton, Rgba, SharedString, UniformListScrollHandle,
};
use tn_config::Loaded;

use crate::style::{
    col, cola, icon, quicklook_fill, quicklook_frame, specular_top, HOVER, INSET, R_PANEL, UI_SANS,
};

/// Cap the lines read/stored on open (bounds one-time work; the list itself is
/// virtualized via `uniform_list`, so only visible rows ever lay out / highlight).
const MAX_LINES: usize = 4000;

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

pub struct QuickLook {
    config: Arc<Loaded>,
    root: PathBuf,
    path: Option<PathBuf>,
    tab: Tab,
    // Rc so the `'static` uniform_list closure can capture them cheaply each frame.
    file_lines: Rc<Vec<String>>,
    file_truncated: bool,
    diff: Rc<Vec<DiffLine>>,
    /// Virtualized code list scroll position (kept across frames per gpui).
    scroll: UniformListScrollHandle,
    focus_handle: FocusHandle,
}

impl QuickLook {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        Self {
            config,
            root,
            path: None,
            tab: Tab::File,
            file_lines: Rc::new(Vec::new()),
            file_truncated: false,
            diff: Rc::new(Vec::new()),
            scroll: UniformListScrollHandle::default(),
            focus_handle: cx.focus_handle(),
        }
    }

    /// Whether a file is currently loaded (the workspace shows the overlay only
    /// when there is one — an empty overlay would float over nothing).
    pub fn has_file(&self) -> bool {
        self.path.is_some()
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
        self.file_lines = Rc::new(all.into_iter().take(MAX_LINES).collect());
        self.diff = Rc::new(self.compute_diff(&path));
        self.scroll = UniformListScrollHandle::default(); // new file → scroll to top
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

}

fn tint_color(config: &Loaded, t: Tint) -> Rgba {
    let th = &config.theme;
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

/// One code row (`.cl`): a faint line-number gutter (`.ln`, width 38, mr 14)
/// + a marker column (`.mk`, width 14) + the tinted source. Free fn so the
/// `'static` uniform_list closure can build rows without borrowing the view.
fn code_row(no: String, mark: &'static str, mark_col: Rgba, spans: Vec<gpui::Div>) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .pr(px(12.)) // mockup .cl padding-right 12
        // mockup .cl .ln:width 38 · faint #474E72 · 11px · 右对齐 · margin-right 14
        .child(
            div()
                .w(px(38.))
                .flex_none()
                .mr(px(14.))
                .text_right()
                .text_size(px(11.))
                .text_color(gpui::rgb(0x474E72)) // faint(无主题 token,字面量)
                .child(SharedString::from(no)),
        )
        // mockup .cl .mk:width 14 居中
        .child(div().w(px(14.)).flex_none().text_center().text_color(mark_col).child(mark))
        .child(div().flex().flex_row().children(spans))
}

/// Build one File-tab row `i` (1-based line number + syntax-tinted source).
fn file_row(config: &Loaded, lines: &[String], i: usize) -> gpui::Div {
    let spans: Vec<gpui::Div> = highlight(&lines[i])
        .into_iter()
        .map(|(text, tint)| div().text_color(tint_color(config, tint)).child(SharedString::from(text)))
        .collect();
    code_row(format!("{}", i + 1), "", col(config.theme.ui.muted), spans)
}

/// Build one Diff-tab row `i` (hunk/context/add/del with `+`/`-` styling).
fn diff_row(config: &Loaded, diff: &[DiffLine], i: usize) -> gpui::Div {
    let th = &config.theme;
    let d = &diff[i];
    let (bg, mark, mark_col, txt_col) = match d.kind {
        // mockup .cl.add/.del:bg=绿/红 @ .09;.ln/.mk 同色;正文不暗化(del 不 muted)
        DiffKind::Add => (cola(th.ansi.green, 0.09), "+", col(th.ansi.green), col(th.ui.foreground)),
        DiffKind::Del => (cola(th.ansi.red, 0.09), "-", col(th.ansi.red), col(th.ui.foreground)),
        DiffKind::Hunk => (rgba(0x00000000), " ", col(th.ui.accent_alt), col(th.ui.accent_alt)),
        DiffKind::Ctx => (rgba(0x00000000), " ", col(th.ui.muted), col(th.ui.foreground)),
    };
    let no = d.new_no.map(|n| format!("{n}")).unwrap_or_default();
    let spans = vec![div().text_color(txt_col).child(SharedString::from(d.text.clone()))];
    code_row(no, mark, mark_col, spans).bg(bg)
}

impl Render for QuickLook {
    fn render(&mut self, _window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let ui = &self.config.theme.ui;
        let th = &self.config.theme;

        // ── .vh header:file icon + path(dir muted / name accent) + 已改动 badge + Diff/File tabset ──
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

        // Diff/File pill (`.tg` / `.tg.on`), clickable to switch tabs.
        let pill = |label: &'static str, on: bool, to: Tab| {
            div()
                .px(px(10.))
                .py(px(2.))
                .rounded(px(7.)) // §16 .vh .tg radius 7
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight(640.)) // §16 .vh .tg weight 640
                .text_color(col(if on { ui.foreground } else { ui.muted }))
                .when(on, |d| d.bg(rgba(HOVER))) // .tg.on bg = g3
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        this.tab = to;
                        cx.notify();
                    }),
                )
                .child(label)
        };
        let tabset = div()
            .flex()
            .flex_row()
            .gap(px(2.)) // §16 .vh .tabset gap 2
            .p(px(2.))
            .rounded(px(9.)) // §16 .vh .tabset radius 9
            .bg(rgba(INSET)) // .tabset bg = g2
            .child(pill("Diff", self.tab == Tab::Diff, Tab::Diff))
            .child(pill("File", self.tab == Tab::File, Tab::File));

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(9.)) // §16 .vh gap 9
            .h(px(36.)) // §16 .vh height 36
            .px(px(13.)) // §16 .vh padding 0 13
            .flex_none()
            .font_family(UI_SANS) // header chrome = sans (code stays mono)
            .text_size(px(11.5))
            .font_weight(gpui::FontWeight(560.)) // §16 .vh weight 560
            // mockup .vh bg:accent @ .06 → transparent 72%
            .bg(linear_gradient(
                180.,
                linear_color_stop(cola(ui.accent, 0.06), 0.),
                linear_color_stop(rgba(0x00000000), 0.72),
            ))
            .child(icon("file", 14., ui.accent))
            // mockup .vh .path:dir = fg-dim(#A6AFD4 字面量),name = accent bold;mono
            .child(
                div()
                    .font_family(SharedString::from(self.config.font().family.clone()))
                    .text_color(gpui::rgb(0xA6AFD4))
                    .child(SharedString::from(dir)),
            )
            .child(
                div()
                    .font_family(SharedString::from(self.config.font().family.clone()))
                    .text_color(col(ui.accent))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(SharedString::from(name)),
            )
            .child(div().flex_1())
            // mockup .vh .by:已改动 badge(claude)—— 仅文件有未提交改动时显
            .when(!self.diff.is_empty(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(5.)) // §16 .vh .by gap 5
                        .text_size(px(11.))
                        .text_color(col(th.agents.claude))
                        .child(icon("pen", 13., th.agents.claude))
                        .child("已改动"),
                )
            })
            .child(tabset);

        // ── .code body:**虚拟化**列表(uniform_list 只渲染可见行 → 大文件不再卡死整窗)──
        let config = self.config.clone(); // Arc clone for the 'static closure
        let lines = self.file_lines.clone(); // Rc clone (cheap)
        let diff = self.diff.clone();
        let tab = self.tab;
        let count = match tab {
            Tab::File => lines.len(),
            Tab::Diff => diff.len(),
        };
        let body = if tab == Tab::Diff && diff.is_empty() {
            div()
                .flex_1()
                .min_h(px(0.))
                .px(px(14.))
                .py(px(8.))
                .text_color(col(ui.muted))
                .child("无改动 · git working tree clean")
        } else {
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .overflow_hidden()
                .pt(px(8.)) // mockup .code padding 8px 0(顶部留白;余下走列表内滚动)
                .child(
                    uniform_list("ql-code", count, move |range, _window, _cx| {
                        range
                            .map(|i| match tab {
                                Tab::File => file_row(&config, &lines, i),
                                Tab::Diff => diff_row(&config, &diff, i),
                            })
                            .collect::<Vec<_>>()
                    })
                    .flex_1()
                    .min_h(px(0.))
                    .track_scroll(self.scroll.clone()),
                )
                .when(self.file_truncated && tab == Tab::File, |d| {
                    d.child(
                        div()
                            .flex_none()
                            .px(px(14.))
                            .py_1()
                            .text_color(col(ui.muted))
                            .child(SharedString::from(format!("… 仅显示前 {MAX_LINES} 行"))),
                    )
                })
        };

        // ── .qlfoot footer:键帽 + 操作提示(预览态)──
        let kcap = |label: &'static str| {
            div()
                .px(px(6.))
                .py(px(1.))
                .rounded(px(5.)) // §16 .qlfoot .k radius 5
                .font_family(SharedString::from(self.config.font().family.clone()))
                .text_size(px(10.))
                .text_color(gpui::rgb(0xA6AFD4)) // fg-dim
                .bg(rgba(INSET)) // .k bg = g2
                .child(label)
        };
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.)) // §16 .qlfoot gap 7
            .px(px(14.)) // mockup .qlfoot padding 7px 14px
            .py(px(7.))
            .flex_none()
            .font_family(UI_SANS)
            .text_size(px(10.5))
            .text_color(col(ui.muted))
            .border_t_1()
            .border_color(rgba(0xffffff0d)) // mockup .qlfoot border-top 白 .05 = round(.05×255)=13=0x0d
            .child(kcap("↑↓"))
            .child("换文件 ·")
            .child(kcap("⇥"))
            .child("切 File ·")
            .child(kcap("Enter"))
            .child("编辑")
            .child(div().flex_1())
            .child("Diff 只读审阅 ·")
            .child(kcap("Esc"))
            .child("关闭");

        // ── 左缘 accent 竖线(.seam):指向树中选中文件的「连接感」;末位子 = 画在最上层 ──
        let seam = div()
            .absolute()
            .left(px(0.))
            .top(px(16.)) // mockup .seam top 16 bottom 16
            .bottom(px(16.))
            .w(px(2.))
            .rounded_r(px(2.))
            .bg(cola(ui.accent, 0.55)); // mockup .seam accent @ .55

        let inner = div()
            .track_focus(&self.focus_handle)
            .size_full()
            .relative() // anchor specular / seam absolute layers
            .flex()
            .flex_col()
            .min_h(px(0.))
            .overflow_hidden()
            .rounded(px(R_PANEL - 1.)) // 1px tighter so the gradient-edge ring shows (quicklook_frame)
            // mockup .quicklook 底层暗玻璃,baked opaque(浮终端上须压住后字)
            .bg(quicklook_fill(ui.chrome_bg))
            .font_family(SharedString::from(self.config.font().family.clone()))
            .text_size(px(12.5)) // mockup .code font-size 12.5
            .child(specular_top()) // 顶部柔光洗(白 .03~.04)
            .child(header)
            .child(body)
            .child(footer)
            .child(seam);

        // mockup .quicklook::before 冷能量描边 + 更深的浮起投影
        quicklook_frame(inner, ui.accent)
    }
}
