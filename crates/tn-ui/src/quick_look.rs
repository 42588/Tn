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
//! + click-to-open + Diff/File toggle. See docs/架构/编辑器与快速预览.md.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use gpui::{
    canvas, div, fill, point, prelude::*, px, rgba, size, uniform_list, App, AsyncApp, Bounds,
    ClipboardItem, ContentMask, Context, ElementInputHandler, EntityInputHandler, FocusHandle,
    Hsla, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    RenderImage, Rgba, ScrollStrategy, ScrollWheelEvent, SharedString, Subscription, TextRun,
    UTF16Selection, UniformListScrollHandle, WeakEntity, Window,
};
use tn_config::Loaded;
use tn_pty::remote_cmd::SshCommandService;
use tn_pty::remote_fs::{
    remote_path_to_virtual_path, RemoteFileService, RemoteFileStat, RemoteId, SftpFileService,
    REMOTE_READ_LIMIT,
};

use crate::editor::motion::{
    inserted_char_from_text, large_file_motion_gate, motion_snapshot, visual_col_for_prefix,
    CaretMotionInput, CaretMotionState, MotionSnapshot, MotionTrigger,
};
use crate::editor::session::DocumentSession;
use crate::style::{col, cola, float_panel, icon, R_PANEL, UI_SANS};
#[cfg(test)]
use tn_editor::{
    char_to_byte, op_backspace, op_delete, op_delete_range, op_insert, op_insert_multiline,
    op_move, op_newline, op_page,
};
use tn_editor::{line_chars, LineLayout, TextRange, VisualLine, WrapMode};

/// A (row, char-col) position in the edit buffer.
type Pos = (usize, usize);

type QuickLookEditState = DocumentSession;

/// Cap the lines read/stored on open (bounds one-time work; the list itself is
/// virtualized via `uniform_list`, so only visible rows ever lay out / highlight).
const MAX_LINES: usize = 4000;

/// Max file size for text preview (2 MB). Larger files get a size-exceeded placeholder
/// instead — reading them would spike memory and blocking-IO time.
const MAX_FILE_SIZE: u64 = 2 * 1024 * 1024;

/// Peek at the first N bytes of a file to decide binary vs text (via content_inspector,
/// not a null-byte test — that wrongly flagged UTF-16/BOM text; 优化 10).
const PEEK_SIZE: usize = 8192;

/// Format a byte count as a short human-readable string ("1.2 KB", "3.4 MB").
fn human_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".into();
    }
    let units = ["B", "KB", "MB", "GB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < units.len() {
        v /= 1024.0;
        i += 1;
    }
    if v < 10.0 {
        format!("{:.1} {}", v, units[i])
    } else {
        format!("{:.0} {}", v, units[i])
    }
}

/// Code font size (px) — mockup `.code` font-size (also the mouse char-width probe).
const CODE_FS: f32 = 12.5;
/// 代码区底 = L3 浮板面(SHEET 03 `.code{background:var(--l3)}`):浮层正文与
/// 浮层同海拔,File/Diff 两态同底。曾用 L1「凹井」,被二轮像素复审判为海拔断裂
/// (差异总结 3-5/3-6:头 L4 + 正文 L1 差两级,且 Diff 态又是另一色)。
const CODE_BG: u32 = crate::style::L3;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    File,
    Diff,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum PendingLeave {
    Close,
    Nav(i32),
    Tab(Tab),
    LocalOpen(PathBuf),
    RemoteOpen {
        cfg: tn_pty::SshConfig,
        id: RemoteId,
        size: Option<u64>,
    },
    LocalOpenForEdit(PathBuf),
    LocalOpenDiff(PathBuf),
    RemoteOpenDiff(crate::remote_git::RemoteGitFile),
    Quit,
}

impl PendingLeave {
    fn prompt(self) -> &'static str {
        match self {
            Self::Close => "关闭速览前保存更改？",
            Self::Nav(_) | Self::LocalOpen(_) | Self::RemoteOpen { .. } => "切换文件前保存更改？",
            Self::Tab(_) | Self::LocalOpenDiff(_) | Self::RemoteOpenDiff(_) => {
                "切换视图前保存更改？"
            }
            Self::LocalOpenForEdit(_) => "打开编辑器前保存更改？",
            Self::Quit => "退出 Tn 前保存更改？",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeaveDecision {
    Continue,
    Confirm,
}

fn dirty_leave_decision(
    dirty: bool,
    pending: &mut Option<PendingLeave>,
    action: PendingLeave,
) -> LeaveDecision {
    if dirty {
        *pending = Some(action);
        LeaveDecision::Confirm
    } else {
        *pending = None;
        LeaveDecision::Continue
    }
}

/// QuickLook data-fetch state machine — render-pure: zero I/O inside `render()`.
/// Mirrors the activity rail's `RailState` pattern.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum LoadingState {
    /// File content / binary peek is being read off-thread.
    Loading,
    /// Data has arrived — render real content (or the binary placeholder).
    Ready,
}

/// A syntax tint class (best-effort, language-agnostic-ish).
#[derive(Clone, Copy, PartialEq, Eq)]
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

/// Map a text file's extension to a human-readable language label.
fn text_label(ext: &str) -> &'static str {
    match ext {
        "rs" => "Rust",
        "toml" => "TOML",
        "md" => "Markdown",
        "json" => "JSON",
        "js" | "mjs" | "cjs" => "JavaScript",
        "ts" | "tsx" => "TypeScript",
        "jsx" => "JSX",
        "py" | "pyw" => "Python",
        "html" | "htm" => "HTML",
        "css" => "CSS",
        "scss" | "sass" => "SCSS",
        "sh" | "bash" | "zsh" => "Shell",
        "ps1" | "psm1" | "psd1" => "PowerShell",
        "yml" | "yaml" => "YAML",
        "lock" => "Lock",
        "gitignore" | "gitattributes" => "Git",
        "env" | "envrc" => "Env",
        "cfg" | "conf" | "ini" | "config" => "Config",
        "txt" | "log" => "Text",
        "csv" => "CSV",
        "xml" | "svg" => "XML",
        "sql" => "SQL",
        "c" | "h" => "C",
        "cpp" | "cxx" | "cc" | "hpp" | "hxx" => "C++",
        "java" => "Java",
        "go" => "Go",
        "rb" => "Ruby",
        "php" => "PHP",
        "swift" => "Swift",
        "kt" | "kts" => "Kotlin",
        "lua" => "Lua",
        "r" => "R",
        "dart" => "Dart",
        "ex" | "exs" => "Elixir",
        "hs" => "Haskell",
        "scala" | "sc" => "Scala",
        "clj" | "cljs" | "edn" => "Clojure",
        "zig" => "Zig",
        "nim" => "Nim",
        "dockerfile" | "dockerignore" => "Docker",
        "patch" | "diff" => "Diff",
        "bat" | "cmd" => "Batch",
        "makefile" | "mk" => "Makefile",
        "vue" | "svelte" => "Web",
        _ => "Plain",
    }
}

/// Map a binary file's extension to a short format label (shown in the
/// "can't preview" placeholder).
fn binary_label(ext: &str) -> &'static str {
    match ext {
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "ico" | "tiff" | "tif" => "Image",
        "mp3" | "wav" | "ogg" | "flac" | "aac" | "wma" | "m4a" => "Audio",
        "mp4" | "mkv" | "avi" | "mov" | "wmv" | "webm" | "flv" => "Video",
        "pdf" => "PDF",
        "zip" | "tar" | "gz" | "xz" | "bz2" | "7z" | "rar" | "zst" => "Archive",
        "exe" | "dll" | "so" | "dylib" | "bin" => "Binary",
        "ttf" | "otf" | "woff" | "woff2" => "Font",
        "wasm" => "WebAssembly",
        "class" => "Java Bytecode",
        "pyc" | "pyo" => "Python Bytecode",
        "obj" | "o" | "a" | "lib" => "Object",
        "pdb" => "Debug Symbols",
        "db" | "sqlite" | "sqlite3" => "Database",
        "doc" | "docx" => "Word",
        "xls" | "xlsx" => "Excel",
        "ppt" | "pptx" => "PowerPoint",
        _ => "Binary",
    }
}

/// Tokenize one line into (text, tint) runs. A tiny hand scanner: line comments,
/// double-quoted strings, words (keyword / type / call / ident), numbers, and
/// runs of punctuation. Not a real parser — just enough to read like code.
fn highlight(line: &str) -> Vec<(smol_str::SmolStr, Tint)> {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    let mut out = Vec::new();
    let mut i = 0;
    while i < n {
        let c = chars[i];
        // line comment to end
        if c == '/' && i + 1 < n && chars[i + 1] == '/' {
            let s: String = chars[i..].iter().collect();
            out.push((smol_str::SmolStr::new(s), Tint::Comment));
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
            let s: String = chars[i..end].iter().collect();
            out.push((smol_str::SmolStr::new(s), Tint::Str));
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
            out.push((smol_str::SmolStr::new(w), t));
            i = j;
            continue;
        }
        // number
        if c.is_ascii_digit() {
            let mut j = i;
            while j < n && (chars[j].is_ascii_digit() || chars[j] == '.' || chars[j] == '_') {
                j += 1;
            }
            let s: String = chars[i..j].iter().collect();
            out.push((smol_str::SmolStr::new(s), Tint::Num));
            i = j;
            continue;
        }
        // run of other characters (punctuation / whitespace)
        let mut j = i;
        while j < n {
            let d = chars[j];
            if d.is_alphanumeric()
                || d == '_'
                || d == '"'
                || (d == '/' && j + 1 < n && chars[j + 1] == '/')
            {
                break;
            }
            j += 1;
        }
        // CRITICAL: never stall. A char that `is_alphanumeric()` but is neither
        // `is_alphabetic()` (word branch) nor `is_ascii_digit()` (number branch) —
        // e.g. `①`/`②`/`½` (Unicode "No"/numeric) — enters none of those branches,
        // falls here, and breaks the loop at `j == i` → `i` never advances →
        // INFINITE LOOP pushing empty tokens → OOM (froze on a `①` in the HTML; see
        // 踩过的坑). Consume the offending char so the scanner always progresses.
        if j == i {
            j = i + 1;
        }
        let s: String = chars[i..j].iter().collect();
        out.push((smol_str::SmolStr::new(s), Tint::Plain));
        i = j;
    }
    out
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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
    /// For `Hunk` rows on a **remote** diff: the 0-based hunk index (matching
    /// [`crate::remote_git::parse_file_diff`]), so the accept/reject buttons can
    /// build the patch for exactly this hunk. `None` for non-hunk rows.
    hunk_index: Option<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DiffRenderRow {
    kind: crate::editor::DiffRowKind,
    new_no: Option<u32>,
    text: String,
    hunk_index: Option<usize>,
}

impl DiffRenderRow {
    fn gutter(&self) -> char {
        self.kind.gutter()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DiffFileJump {
    row: usize,
    highlight_row: usize,
}

fn diff_render_rows(diff: &[DiffLine]) -> Vec<DiffRenderRow> {
    diff.iter()
        .map(|line| DiffRenderRow {
            kind: match line.kind {
                DiffKind::Ctx => crate::editor::DiffRowKind::Context,
                DiffKind::Add => crate::editor::DiffRowKind::Addition,
                DiffKind::Del => crate::editor::DiffRowKind::Deletion,
                DiffKind::Hunk => crate::editor::DiffRowKind::HunkHeader,
            },
            new_no: line.new_no,
            text: line.text.clone(),
            hunk_index: line.hunk_index,
        })
        .collect()
}

fn normalized_range(a: Pos, b: Pos) -> Option<(Pos, Pos)> {
    if a == b {
        None
    } else if a <= b {
        Some((a, b))
    } else {
        Some((b, a))
    }
}

fn diff_row_chars(rows: &[DiffRenderRow], row: usize) -> usize {
    rows.get(row).map(|r| r.text.chars().count()).unwrap_or(0)
}

fn diff_selected_text(rows: &[DiffRenderRow], s: Pos, e: Pos) -> String {
    let Some((mut s, mut e)) = normalized_range(s, e) else {
        return String::new();
    };
    if rows.is_empty() {
        return String::new();
    }
    let last = rows.len() - 1;
    s.0 = s.0.min(last);
    e.0 = e.0.min(last);
    s.1 = s.1.min(diff_row_chars(rows, s.0));
    e.1 = e.1.min(diff_row_chars(rows, e.0));
    if s == e {
        return String::new();
    }
    if s.0 == e.0 {
        return rows[s.0]
            .text
            .chars()
            .skip(s.1)
            .take(e.1.saturating_sub(s.1))
            .collect();
    }
    let mut out: String = rows[s.0].text.chars().skip(s.1).collect();
    for row in rows.iter().take(e.0).skip(s.0 + 1) {
        out.push('\n');
        out.push_str(&row.text);
    }
    out.push('\n');
    out.push_str(&rows[e.0].text.chars().take(e.1).collect::<String>());
    out
}

fn diff_cursor_from_point(
    rows: &[DiffRenderRow],
    row: usize,
    x_in_viewport: f32,
    char_w: f32,
    hscroll: f32,
) -> Pos {
    if rows.is_empty() {
        return (0, 0);
    }
    let row = row.min(rows.len() - 1);
    let rel = x_in_viewport - CODE_GUTTER + hscroll;
    let col = caret_col_at_x(&rows[row].text, rel, char_w).min(diff_row_chars(rows, row));
    (row, col)
}

fn diff_drag_cursor_from_point(
    rows: &[DiffRenderRow],
    anchor: Pos,
    row: usize,
    x_in_viewport: f32,
    char_w: f32,
    hscroll: f32,
) -> Pos {
    if rows.is_empty() {
        return (0, 0);
    }
    let row = row.min(rows.len() - 1);
    let rel = x_in_viewport - CODE_GUTTER + hscroll;
    let hover = hover_char_at_x(&rows[row].text, rel, char_w);
    let col = if (row, hover) >= anchor {
        hover + 1
    } else {
        hover
    }
    .min(diff_row_chars(rows, row));
    (row, col)
}

fn diff_selection_span_cols(
    rows: &[DiffRenderRow],
    row: usize,
    range: (Pos, Pos),
) -> Option<(usize, usize)> {
    let (s, e) = normalized_range(range.0, range.1)?;
    let line = rows.get(row).map(|r| r.text.as_str())?;
    if row < s.0 || row > e.0 {
        return None;
    }
    let nchars = line.chars().count();
    let ss = if row == s.0 { s.1.min(nchars) } else { 0 };
    let ee = if row == e.0 { e.1.min(nchars) } else { nchars };
    if ss >= ee {
        return None;
    }
    Some((
        crate::editor::geometry::prefix_cols(line, ss),
        crate::editor::geometry::prefix_cols(line, ee),
    ))
}

fn parse_diff_hunk_new_start(text: &str) -> Option<u32> {
    let rest = text.strip_prefix("@@")?;
    let plus = rest.split('+').nth(1)?;
    let num: String = plus.chars().take_while(|c| c.is_ascii_digit()).collect();
    num.parse().ok()
}

fn diff_target_new_line(rows: &[DiffRenderRow], row: usize) -> Option<u32> {
    if rows.is_empty() {
        return None;
    }
    let row = row.min(rows.len() - 1);
    if let Some(no) = rows[row].new_no {
        return Some(no);
    }
    if rows[row].kind == crate::editor::DiffRowKind::HunkHeader {
        return parse_diff_hunk_new_start(&rows[row].text);
    }
    let mut prev = None;
    for i in (0..row).rev() {
        if let Some(no) = rows[i].new_no {
            prev = Some((i, no));
            break;
        }
        if rows[i].kind == crate::editor::DiffRowKind::HunkHeader {
            prev = parse_diff_hunk_new_start(&rows[i].text).map(|no| (i, no));
            break;
        }
    }
    let mut next = None;
    for (i, item) in rows.iter().enumerate().skip(row + 1) {
        if item.kind == crate::editor::DiffRowKind::HunkHeader {
            break;
        }
        if let Some(no) = item.new_no {
            next = Some((i, no));
            break;
        }
    }
    match (prev, next) {
        (Some((pi, pn)), Some((ni, nn))) => {
            if row.saturating_sub(pi) <= ni.saturating_sub(row) {
                Some(pn)
            } else {
                Some(nn)
            }
        }
        (Some((_, no)), None) | (None, Some((_, no))) => Some(no),
        (None, None) => None,
    }
}

fn diff_target_file_row(rows: &[DiffRenderRow], row: usize) -> Option<usize> {
    diff_target_new_line(rows, row).map(|line| line.saturating_sub(1) as usize)
}

fn diff_file_jump_target(rows: &[DiffRenderRow], row: usize) -> Option<DiffFileJump> {
    let row = diff_target_file_row(rows, row)?;
    Some(DiffFileJump {
        row,
        highlight_row: row,
    })
}

fn diff_file_jump_target_for_file_len(
    rows: &[DiffRenderRow],
    row: usize,
    file_len: usize,
) -> Option<DiffFileJump> {
    if file_len == 0 {
        return None;
    }
    let mut jump = diff_file_jump_target(rows, row)?;
    let last = file_len - 1;
    jump.row = jump.row.min(last);
    jump.highlight_row = jump.highlight_row.min(last);
    Some(jump)
}

fn diff_hunk_jump_row(rows: &[DiffRenderRow], from: usize, forward: bool) -> Option<usize> {
    let kinds: Vec<_> = rows.iter().map(|row| row.kind).collect();
    let headers = crate::editor::hunk_header_rows(&kinds);
    if forward {
        crate::editor::next_hunk(&headers, from)
    } else {
        crate::editor::prev_hunk(&headers, from)
    }
}

fn should_self_paint_diff(
    el_render: bool,
    editing: bool,
    tab: Tab,
    file_data: &QuickLookData,
) -> bool {
    el_render && !editing && tab == Tab::Diff && matches!(file_data, QuickLookData::Text { .. })
}

#[derive(Clone, Debug, PartialEq)]
struct QuickLookFileLayout {
    layout: LineLayout,
    pre: crate::editor::prepaint::ReadOnlyPrepaint,
    wrap_mode: WrapMode,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CaretPaintRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CaretVisualRect {
    x: f32,
    y: f32,
    width: f32,
    height: f32,
    radius: f32,
}

const QUICKLOOK_CARET_RADIUS: f32 = 1.0;
const QUICKLOOK_CARET_FONT_PAD_Y: f32 = 4.0;

fn caret_visual_height(row_h: f32, code_fs: f32) -> f32 {
    (code_fs + QUICKLOOK_CARET_FONT_PAD_Y).min(row_h).max(1.0)
}

fn caret_visual_rect(
    cell_x: f32,
    row_y: f32,
    cell_w: f32,
    row_h: f32,
    code_fs: f32,
    scale_x: f32,
    scale_y: f32,
    dx: f32,
    dy: f32,
) -> CaretVisualRect {
    let base_w = cell_w.max(1.0);
    let base_h = caret_visual_height(row_h, code_fs);
    let scale_x = scale_x.max(0.55);
    let scale_y = scale_y.max(0.55);
    let width = base_w * scale_x;
    let height = base_h * scale_y;
    CaretVisualRect {
        x: cell_x + dx - (width - base_w) * 0.5,
        y: row_y + dy + (row_h - base_h) * 0.5 - (height - base_h) * 0.5,
        width,
        height,
        radius: QUICKLOOK_CARET_RADIUS,
    }
}

fn soft_wrap_extension(ext: &str) -> bool {
    matches!(
        ext,
        "md" | "markdown"
            | "mdown"
            | "mkd"
            | "txt"
            | "text"
            | "log"
            | "rst"
            | "adoc"
            | "asciidoc"
            | "org"
            | "tex"
            | "csv"
            | "tsv"
    )
}

fn soft_wrap_file_name(name: &str) -> bool {
    matches!(
        name,
        "readme"
            | "license"
            | "licence"
            | "copying"
            | "notice"
            | "changelog"
            | "changes"
            | "authors"
            | "contributors"
    )
}

fn file_wrap_mode_for_path(path: &std::path::Path, width_cols: usize) -> WrapMode {
    let width_cols = width_cols.max(1);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if soft_wrap_extension(&ext) || soft_wrap_file_name(&stem) {
        WrapMode::Word { width_cols }
    } else {
        WrapMode::None
    }
}

fn wrap_width_cols(viewport_w: f32, char_w: f32) -> usize {
    if viewport_w <= CODE_GUTTER || char_w <= 0.0 {
        return 1;
    }
    ((viewport_w - CODE_GUTTER) / char_w).floor().max(1.0) as usize
}

fn visual_line_text(line: &str, visual: VisualLine) -> String {
    line.chars()
        .skip(visual.char_start)
        .take(visual.len())
        .collect()
}

fn visual_prefix_cols(line: &str, visual: VisualLine, local_col: usize) -> usize {
    crate::editor::geometry::prefix_cols(line, visual.char_start + local_col).saturating_sub(
        crate::editor::geometry::prefix_cols(line, visual.char_start),
    )
}

fn quicklook_file_layout(
    lines: &[String],
    path: &std::path::Path,
    viewport_w: f32,
    viewport_h: f32,
    offset_y: f32,
    h_offset: f32,
    char_w: f32,
) -> QuickLookFileLayout {
    use crate::editor::geometry::{content_width, max_cols, max_h_offset, Metrics};
    use crate::editor::prepaint::{visible_row_indices, ReadOnlyPrepaint};

    let m = Metrics::new(char_w);
    let wrap_mode = file_wrap_mode_for_path(path, wrap_width_cols(viewport_w, char_w));
    let layout = LineLayout::build(lines, wrap_mode);
    let visual_count = layout.visual_count();
    let rows = visible_row_indices(offset_y, viewport_h, m.row_h, visual_count);
    let (content_w, max_off, h_offset, thumb, content_x) = match wrap_mode {
        WrapMode::None => {
            let content_w = content_width(max_cols(lines), m, viewport_w);
            let max_off = max_h_offset(content_w, viewport_w);
            let h_offset = h_offset.clamp(0.0, max_off);
            let thumb = (max_off > 8.0 && viewport_w > 0.0).then(|| {
                crate::editor::geometry::h_scroll_thumb(content_w, viewport_w, h_offset, max_off)
            });
            (content_w, max_off, h_offset, thumb, m.gutter - h_offset)
        }
        WrapMode::Word { .. } => (viewport_w.max(0.0), 0.0, 0.0, None, m.gutter),
    };
    QuickLookFileLayout {
        layout,
        pre: ReadOnlyPrepaint {
            content_w,
            max_off,
            h_offset,
            rows,
            thumb,
            content_x,
        },
        wrap_mode,
    }
}

fn quicklook_caret_paint_rect(
    lines: &[String],
    path: &std::path::Path,
    cursor: Pos,
    viewport_w: f32,
    viewport_h: f32,
    scroll_y: f32,
    hscroll: f32,
    char_w: f32,
) -> Option<CaretPaintRect> {
    if lines.is_empty() || char_w <= 0.0 || viewport_w <= 0.0 || viewport_h <= 0.0 {
        return None;
    }
    let layout = quicklook_file_layout(
        lines, path, viewport_w, viewport_h, scroll_y, hscroll, char_w,
    );
    let row = cursor.0.min(lines.len().saturating_sub(1));
    let col = cursor.1.min(line_chars(lines, row));
    let (visual_row, local_col) = layout.layout.logical_to_visual((row, col));
    let visual = layout.layout.visual_line(visual_row)?;
    let line = lines.get(row).map(String::as_str).unwrap_or("");
    let x = layout.pre.content_x
        + visual_prefix_cols(line, visual, local_col.min(visual.len())) as f32 * char_w;
    let y = crate::editor::prepaint::row_top(visual_row, scroll_y, ROW_H);
    Some(CaretPaintRect {
        x,
        y,
        width: char_w.max(1.0),
        height: ROW_H,
    })
}

fn quicklook_visual_vertical_cursor(
    lines: &[String],
    path: &std::path::Path,
    cursor: Pos,
    dir: i32,
    viewport_w: f32,
    viewport_h: f32,
    scroll_y: f32,
    hscroll: f32,
    char_w: f32,
) -> Option<Pos> {
    if lines.is_empty() || dir == 0 || viewport_w <= 0.0 || viewport_h <= 0.0 || char_w <= 0.0 {
        return None;
    }
    let file_layout = quicklook_file_layout(
        lines, path, viewport_w, viewport_h, scroll_y, hscroll, char_w,
    );
    if file_layout.wrap_mode == WrapMode::None {
        return None;
    }
    let row = cursor.0.min(lines.len().saturating_sub(1));
    let col = cursor.1.min(line_chars(lines, row));
    let (visual_row, local_col) = file_layout.layout.logical_to_visual((row, col));
    let target_visual = if dir < 0 {
        visual_row.checked_sub(1)?
    } else {
        let next = visual_row + 1;
        (next < file_layout.layout.visual_count()).then_some(next)?
    };
    let current_visual = file_layout.layout.visual_line(visual_row)?;
    let current_line = lines.get(current_visual.logical_row)?;
    let target_x_cols = visual_prefix_cols(current_line, current_visual, local_col);
    Some(
        file_layout
            .layout
            .hit_test(lines, target_visual, target_x_cols as f32 * char_w, char_w),
    )
}

fn should_center_after_text_commit(el_render: bool) -> bool {
    !el_render
}

fn text_motion_trigger(text: &str, before: Pos, after: Pos) -> Option<MotionTrigger> {
    let inserted = inserted_char_from_text(text)?;
    (before != after).then_some(MotionTrigger::Insert {
        from: before,
        to: after,
        inserted: Some(inserted),
    })
}

fn delete_motion_trigger(before: Pos, after: Pos) -> Option<MotionTrigger> {
    (before != after).then_some(MotionTrigger::Delete {
        from: before,
        to: after,
    })
}

/// Emitted to the workspace for the few cross-entity needs (keyboard focus lives
/// on the overlay while it's open; these are the things it can't do alone).
pub enum QuickLookEvent {
    /// `↑↓` preview nav: move to the prev(-1)/next(+1) **file** in the tree.
    Nav(i32),
    /// `Esc`/`Space` in preview: close the overlay (give space back to the terminal).
    Close,
    /// User confirmed a dirty close, so the workspace should hide the overlay.
    CloseConfirmed,
    /// User confirmed app quit while a dirty Quick Look document was open.
    QuitConfirmed,
    /// `Ctrl+S` wrote this file to disk — the workspace refreshes any agent pane's
    /// activity rail (本次改动) **synchronously**, instead of waiting on the file
    /// watcher (which can miss the edit: file outside the watched cwd, debounce, etc.).
    FileSaved(std::path::PathBuf),
    /// A remote per-hunk accept/reject (`git apply`) changed the remote working
    /// tree → refresh every agent pane's「本次改动」(remote panes recompute via
    /// `changes_for_remote`; the file watcher can't see remote FS edits).
    RemoteChangesDirty,
}

#[derive(Clone)]
enum QuickLookData {
    None,
    Text {
        lines: Arc<Vec<String>>,
        truncated: bool,
    },
    Pdf {
        pages: Arc<std::sync::Mutex<Vec<Option<Arc<RenderImage>>>>>,
        page_count: usize,
    },
    Image {
        img: Arc<RenderImage>,
    },
    Binary {
        size: u64,
    },
}

#[derive(Clone)]
struct PreviewPayload {
    data: QuickLookData,
    format: Option<TextFormat>,
    guard: Option<FileGuard>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct RemoteSource {
    cfg: tn_pty::SshConfig,
    id: RemoteId,
}

enum RemoteSaveResult {
    Saved {
        guard: FileGuard,
        lines: Vec<String>,
    },
    Conflict(Conflict),
    Error(String),
}

enum LocalSaveResult {
    Saved {
        guard: FileGuard,
        lines: Vec<String>,
    },
    Conflict(Conflict),
    Error(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SaveStateUpdate {
    dirty: bool,
    diff_dirty: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TextEncoding {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Gbk,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NewlineStyle {
    Lf,
    Crlf,
}

impl NewlineStyle {
    fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "\n",
            Self::Crlf => "\r\n",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TextFormat {
    encoding: TextEncoding,
    newline: NewlineStyle,
    final_newline: bool,
}

impl Default for TextFormat {
    fn default() -> Self {
        Self {
            encoding: TextEncoding::Utf8,
            newline: NewlineStyle::Lf,
            final_newline: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct DecodedText {
    lines: Vec<String>,
    format: TextFormat,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct FileGuard {
    mtime: SystemTime,
    size: u64,
    hash: u64,
}

impl FileGuard {
    fn from_parts(mtime: SystemTime, size: u64, hash: u64) -> Self {
        Self { mtime, size, hash }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Conflict {
    Clean,
    ModifiedOnDisk,
    MissingOnDisk,
    Unknown,
}

impl Conflict {
    fn label(self) -> &'static str {
        match self {
            Self::Clean => "无冲突",
            Self::ModifiedOnDisk => "文件已被其他进程修改",
            Self::MissingOnDisk => "文件已不存在",
            Self::Unknown => "无法确认文件状态",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveGuardMode {
    Check,
    Force,
}

fn detect_conflict(opened: Option<&FileGuard>, disk: Option<&FileGuard>) -> Conflict {
    match (opened, disk) {
        (None, _) => Conflict::Unknown,
        (Some(_), None) => Conflict::MissingOnDisk,
        (Some(opened), Some(disk)) if opened == disk => Conflict::Clean,
        (Some(_), Some(_)) => Conflict::ModifiedOnDisk,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExternalReloadDecision {
    Unchanged,
    Reload,
    Conflict(Conflict),
}

fn external_reload_decision(
    editing: bool,
    dirty: bool,
    opened: Option<&FileGuard>,
    disk: Option<&FileGuard>,
) -> ExternalReloadDecision {
    if editing {
        return ExternalReloadDecision::Unchanged;
    }
    match detect_conflict(opened, disk) {
        Conflict::Clean => ExternalReloadDecision::Unchanged,
        conflict => {
            if dirty {
                ExternalReloadDecision::Conflict(conflict)
            } else {
                ExternalReloadDecision::Reload
            }
        }
    }
}

fn remote_save_conflict(
    opened: Option<&FileGuard>,
    current: Option<&FileGuard>,
    mode: SaveGuardMode,
) -> Conflict {
    match mode {
        SaveGuardMode::Check => detect_conflict(opened, current),
        SaveGuardMode::Force => Conflict::Clean,
    }
}

fn remote_file_guard(stat: &RemoteFileStat, bytes: &[u8]) -> FileGuard {
    FileGuard::from_parts(
        std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(stat.mtime.unwrap_or(0)),
        stat.size.unwrap_or(bytes.len() as u64),
        file_sample_hash(bytes),
    )
}

fn save_state_after_success(is_remote: bool, has_remote_diff: bool) -> SaveStateUpdate {
    SaveStateUpdate {
        dirty: false,
        diff_dirty: !is_remote || has_remote_diff,
    }
}

fn file_sample_hash(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
    const SAMPLE: usize = 64 * 1024;

    fn feed(mut hash: u64, bytes: &[u8]) -> u64 {
        for &b in bytes {
            hash ^= b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    let mut hash = FNV_OFFSET;
    hash = feed(hash, &(bytes.len() as u64).to_le_bytes());
    if bytes.len() <= SAMPLE * 2 {
        feed(hash, bytes)
    } else {
        hash = feed(hash, &bytes[..SAMPLE]);
        hash = feed(hash, b"tn-quicklook-sample-tail");
        feed(hash, &bytes[bytes.len() - SAMPLE..])
    }
}

fn split_text_lines(text: &str) -> (Vec<String>, NewlineStyle, bool) {
    if text.is_empty() {
        return (Vec::new(), NewlineStyle::Lf, false);
    }
    let newline = if text.contains("\r\n") {
        NewlineStyle::Crlf
    } else {
        NewlineStyle::Lf
    };
    let final_newline = text.ends_with('\n') || text.ends_with('\r');
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines: Vec<String> = normalized.split('\n').map(str::to_string).collect();
    if final_newline {
        lines.pop();
    }
    (lines, newline, final_newline)
}

fn decode_text_bytes(bytes: &[u8], _ext: &str) -> Option<DecodedText> {
    let (text, encoding) = if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        (
            String::from_utf8_lossy(&bytes[3..]).into_owned(),
            TextEncoding::Utf8Bom,
        )
    } else if bytes.starts_with(&[0xFF, 0xFE]) {
        let (cow, _, _) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
        (cow.into_owned(), TextEncoding::Utf16Le)
    } else if bytes.starts_with(&[0xFE, 0xFF]) {
        let (cow, _, _) = encoding_rs::UTF_16BE.decode(&bytes[2..]);
        (cow.into_owned(), TextEncoding::Utf16Be)
    } else if let Ok(utf8) = std::str::from_utf8(bytes) {
        (utf8.to_string(), TextEncoding::Utf8)
    } else {
        let (cow, _, _) = encoding_rs::GBK.decode(bytes);
        (cow.into_owned(), TextEncoding::Gbk)
    };
    let (lines, newline, final_newline) = split_text_lines(&text);
    Some(DecodedText {
        lines,
        format: TextFormat {
            encoding,
            newline,
            final_newline,
        },
    })
}

fn encode_text_lines(lines: &[String], format: TextFormat) -> Vec<u8> {
    let sep = format.newline.as_str();
    let mut text = lines.join(sep);
    if format.final_newline {
        text.push_str(sep);
    }
    match format.encoding {
        TextEncoding::Utf8 => text.into_bytes(),
        TextEncoding::Utf8Bom => {
            let mut out = vec![0xEF, 0xBB, 0xBF];
            out.extend_from_slice(text.as_bytes());
            out
        }
        TextEncoding::Utf16Le => {
            let mut out = vec![0xFF, 0xFE];
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_le_bytes());
            }
            out
        }
        TextEncoding::Utf16Be => {
            let mut out = vec![0xFE, 0xFF];
            for unit in text.encode_utf16() {
                out.extend_from_slice(&unit.to_be_bytes());
            }
            out
        }
        TextEncoding::Gbk => {
            let (cow, _, _) = encoding_rs::GBK.encode(&text);
            cow.into_owned()
        }
    }
}

fn local_file_guard(path: &std::path::Path) -> Option<FileGuard> {
    let bytes = std::fs::read(path).ok()?;
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    Some(FileGuard::from_parts(
        mtime,
        meta.len(),
        file_sample_hash(&bytes),
    ))
}

fn save_local_text(
    path: &std::path::Path,
    lines: &[String],
    format: TextFormat,
    opened_guard: Option<&FileGuard>,
    guard_mode: SaveGuardMode,
) -> LocalSaveResult {
    let current_guard = local_file_guard(path);
    let conflict = match guard_mode {
        SaveGuardMode::Check => detect_conflict(opened_guard, current_guard.as_ref()),
        SaveGuardMode::Force => Conflict::Clean,
    };
    if conflict != Conflict::Clean {
        return LocalSaveResult::Conflict(conflict);
    }

    let bytes = encode_text_lines(lines, format);
    match std::fs::write(path, &bytes) {
        Ok(()) => {
            let guard = local_file_guard(path).unwrap_or_else(|| {
                FileGuard::from_parts(
                    SystemTime::UNIX_EPOCH,
                    bytes.len() as u64,
                    file_sample_hash(&bytes),
                )
            });
            LocalSaveResult::Saved {
                guard,
                lines: lines.to_vec(),
            }
        }
        Err(e) => LocalSaveResult::Error(e.to_string()),
    }
}

fn preview_payload_from_bytes(
    bytes: Vec<u8>,
    ext: &str,
    declared_size: Option<u64>,
    remote_stat: Option<&RemoteFileStat>,
) -> PreviewPayload {
    let guard = remote_stat.map(|stat| remote_file_guard(stat, &bytes));
    let size = declared_size.unwrap_or(bytes.len() as u64);
    if size > MAX_FILE_SIZE || bytes.len() as u64 > MAX_FILE_SIZE {
        return PreviewPayload {
            data: QuickLookData::Binary { size },
            format: None,
            guard,
        };
    }
    let peek_len = PEEK_SIZE.min(bytes.len());
    if content_inspector::inspect(&bytes[..peek_len]).is_binary() {
        return PreviewPayload {
            data: QuickLookData::Binary { size },
            format: None,
            guard,
        };
    }
    let Some(decoded) = decode_text_bytes(&bytes, ext) else {
        return PreviewPayload {
            data: QuickLookData::Binary { size },
            format: None,
            guard,
        };
    };
    let mut line_iter = decoded.lines.into_iter();
    let mut lines = Vec::with_capacity(MAX_LINES.min(1000));
    for line in (&mut line_iter).take(MAX_LINES) {
        lines.push(line);
    }
    let truncated = line_iter.next().is_some();
    PreviewPayload {
        data: QuickLookData::Text {
            lines: Arc::new(lines),
            truncated,
        },
        format: Some(decoded.format),
        guard,
    }
}

fn preview_is_editable(path: &std::path::Path, data: &QuickLookData, _is_remote: bool) -> bool {
    match data {
        QuickLookData::Text {
            truncated: false, ..
        } => {}
        _ => return false,
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    !matches!(
        ext.as_str(),
        "docx" | "doc" | "xlsx" | "xls" | "ods" | "pptx" | "ppt" | "odp" | "odt" | "pdf"
    )
}

fn evict_render_image(img: &Arc<RenderImage>, cx: &mut App) {
    let windows = cx.windows();
    if let Some(win_handle) = windows.first() {
        let _ = cx.update_window(*win_handle, |_, window, cx| {
            cx.drop_image(img.clone(), Some(window));
        });
    }
}

fn resize_image_to_fit(img: image::DynamicImage, max_w: u32, max_h: u32) -> image::DynamicImage {
    let (width, height) = (img.width(), img.height());
    if width <= max_w && height <= max_h {
        return img;
    }

    let ratio = width as f32 / height as f32;
    let (new_w, new_h) = if width > height {
        let new_w = max_w.min(width);
        let new_h = (new_w as f32 / ratio).round() as u32;
        (new_w, new_h)
    } else {
        let new_h = max_h.min(height);
        let new_w = (new_h as f32 * ratio).round() as u32;
        (new_w, new_h)
    };

    let new_w = new_w.max(1);
    let new_h = new_h.max(1);

    match img {
        image::DynamicImage::ImageRgb8(rgb_img) => {
            let src_image = fast_image_resize::images::Image::from_vec_u8(
                width.max(1),
                height.max(1),
                rgb_img.into_raw(),
                fast_image_resize::PixelType::U8x3,
            ).unwrap();

            let mut dst_image = fast_image_resize::images::Image::new(
                new_w,
                new_h,
                src_image.pixel_type(),
            );

            let mut resizer = fast_image_resize::Resizer::new();
            resizer.resize(&src_image, &mut dst_image, &fast_image_resize::ResizeOptions::new()).unwrap();

            let dst_raw = dst_image.into_vec();
            let dst_buffer = image::ImageBuffer::from_raw(new_w, new_h, dst_raw).unwrap();
            image::DynamicImage::ImageRgb8(dst_buffer)
        }
        image::DynamicImage::ImageRgba8(rgba_img) => {
            let src_image = fast_image_resize::images::Image::from_vec_u8(
                width.max(1),
                height.max(1),
                rgba_img.into_raw(),
                fast_image_resize::PixelType::U8x4,
            ).unwrap();

            let mut dst_image = fast_image_resize::images::Image::new(
                new_w,
                new_h,
                src_image.pixel_type(),
            );

            let mut resizer = fast_image_resize::Resizer::new();
            resizer.resize(&src_image, &mut dst_image, &fast_image_resize::ResizeOptions::new()).unwrap();

            let dst_raw = dst_image.into_vec();
            let dst_buffer = image::ImageBuffer::from_raw(new_w, new_h, dst_raw).unwrap();
            image::DynamicImage::ImageRgba8(dst_buffer)
        }
        other => {
            let rgba_img = other.into_rgba8();
            let src_image = fast_image_resize::images::Image::from_vec_u8(
                width.max(1),
                height.max(1),
                rgba_img.into_raw(),
                fast_image_resize::PixelType::U8x4,
            ).unwrap();

            let mut dst_image = fast_image_resize::images::Image::new(
                new_w,
                new_h,
                src_image.pixel_type(),
            );

            let mut resizer = fast_image_resize::Resizer::new();
            resizer.resize(&src_image, &mut dst_image, &fast_image_resize::ResizeOptions::new()).unwrap();

            let dst_raw = dst_image.into_vec();
            let dst_buffer = image::ImageBuffer::from_raw(new_w, new_h, dst_raw).unwrap();
            image::DynamicImage::ImageRgba8(dst_buffer)
        }
    }
}

fn dynamic_image_to_render_image(img: image::DynamicImage) -> RenderImage {
    let mut rgba = img.into_rgba8();
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    RenderImage::new(vec![image::Frame::new(rgba)])
}

pub struct QuickLook {
    config: Arc<Loaded>,
    root: PathBuf,
    path: Option<PathBuf>,
    tab: Tab,
    file_data: QuickLookData,
    diff: Rc<Vec<DiffLine>>,
    /// `git diff` is computed **lazily** (only when the Diff tab is shown) — it's a
    /// blocking subprocess and was freezing the UI when run eagerly on every file
    /// open / navigation. `true` = the cached `diff` is stale and must be recomputed
    /// the next time the Diff tab is viewed. (See 踩坑记录 + docs/架构/编辑器与快速预览.md.)
    diff_dirty: bool,
    /// Edit state for our own small modeless editor.
    editing: bool,
    /// Document-backed editable state. The old renderer still reads an `Rc<Vec<_>>`
    /// snapshot from this shell until `EditorElement` lands.
    edit: QuickLookEditState,
    /// Preview cursor/selection head for read-only File tab drag-select. Edit mode
    /// reads cursor/selection from `edit.document` instead.
    cursor: Pos,
    /// Preview selection anchor (head = `cursor`); `None` = no selection.
    sel_anchor: Option<Pos>,
    /// True while a left-drag text selection is in progress in the editor (mouse down
    /// on a row starts it, per-row mouse move extends it). Cleared on mouse up / when
    /// the button is found released (drag can end outside the panel — same bounds
    /// caveat as the horizontal scrollbar).
    edit_drag: bool,
    /// Unsaved edits since the last write (drives the "编辑中 ●" badge).
    dirty: bool,
    /// IME composition (preedit) text while editing, set by the platform input
    /// handler. `Some` ⇒ composing (中文): gpui routes keys to the IME and we don't
    /// touch the buffer until commit, when the result is inserted at the cursor.
    /// Without an input handler the editor couldn't accept IME-composed text.
    ime_marked: Option<String>,
    /// Monospace advance width (px) at the code font size — for mouse → column.
    char_w: f32,
    /// Code-area content bounds (window space), captured each paint by a canvas —
    /// lets a click map to a column.
    code_bounds: Rc<RefCell<Bounds<Pixels>>>,
    /// Find / replace bar state.
    find_open: bool,
    replacing: bool,
    find_query: String,
    replace_query: String,
    /// Which find field typing goes to (false = find, true = replace).
    find_field_replace: bool,
    // 编辑态高亮**不缓存**:可见行仅 ~30,每帧直接算够快;按行号缓存会在删除/撤销后
    // 显示陈旧内容(审查⑫)。仅预览态(只读、内容不变)缓存,行号 key 安全。
    file_highlight_cache: std::rc::Rc<
        std::cell::RefCell<std::collections::HashMap<usize, Vec<(smol_str::SmolStr, Tint)>>>,
    >,
    /// One-shot visual target after jumping from Diff back to File. It does not use
    /// selection state, so Ctrl+C and subsequent drag-select keep normal semantics.
    file_jump_highlight: Option<usize>,
    /// Virtualized code list scroll position (kept across frames per gpui).
    scroll: UniformListScrollHandle,
    /// Grab focus in the next render (focusing in an event/open callback doesn't
    /// land — the overlay isn't rendered yet; see 踩过的坑).
    needs_focus: bool,
    /// RAIL 读数:从活动栏打开时的 `(当前序号, 总数)`(0-based idx);`None` = 从
    /// 文件树打开,footer 不显示 `RAIL · n/N`。workspace 每帧同步(SHEET 03 footer)。
    rail_pos: Option<(usize, usize)>,
    focus_handle: FocusHandle,
    // ── Async-loading control (render-pure: zero I/O in render()) ──
    loading_state: LoadingState,
    generation: usize,
    /// True when `path` is only a display/virtual path for an SSH remote file.
    /// Remote text saves go through SFTP guards; local git/disk paths are never
    /// used against the virtual `ssh://` display path.
    is_remote_source: bool,
    remote_fs: Arc<dyn RemoteFileService>,
    remote_source: Option<RemoteSource>,
    remote_diff_file: Option<crate::remote_git::RemoteGitFile>,
    text_format: Option<TextFormat>,
    opened_guard: Option<FileGuard>,
    save_conflict: Option<Conflict>,
    save_error: Option<String>,
    save_in_flight: bool,
    pending_leave: Option<PendingLeave>,
    /// Deferred-edit flag: if `open_for_edit` is called while the file is still
    /// loading, this is set so the async completion handler enters edit afterwards.
    edit_on_ready: bool,
    /// Independent loading track for the `git diff` path (separate from file I/O).
    diff_loading: bool,
    diff_generation: usize,
    /// A remote per-hunk accept/reject (`git apply`) is in flight — buttons are
    /// disabled meanwhile so a double-click can't fire two conflicting patches.
    hunk_busy: bool,
    /// Last remote hunk apply/reject failure, surfaced as a dismissible banner.
    hunk_error: Option<String>,
    /// Token used to cancel background tasks (e.g. image decoding, pdf parsing) when a new file is opened.
    cancel_token: Arc<std::sync::atomic::AtomicBool>,
    /// Preview 横向滚动(px)。自绘底部滚动条,**不用** `overflow.x = Scroll`(那会让滚轮
    /// 横纵同滚 → 整页斜移,owner 实测否)。`hscroll_drag` = 拖 thumb 时光标相对 thumb 左缘
    /// 的偏移;`hscroll_content_w` = render 缓存的内容宽(拖动回调里没有行列表,靠它算可滚范围)。
    hscroll_px: f32,
    hscroll_drag: Option<f32>,
    hscroll_content_w: f32,
    caret_motion: CaretMotionState,
    motion_cleanup_pending: bool,
    /// 编辑态横向 caret-follow 的去抖:只在光标**变化**时跟随一次,否则手动拖横滚条会被
    /// 每帧的 follow 立刻拉回(=「光标固定后拖不动」)。`None` ⇒ 下一帧无条件跟随一次。
    last_follow_cursor: Option<(usize, usize)>,
    /// TnE-09: `TN_QL_ELEMENT=1` 门控的只读自绘 File 渲染(默认关 = 旧 `uniform_list`)。
    /// 用 `editor::{geometry,prepaint}` 模型自绘行号 / 文本 / 横滚条;一键回旧路 = 不设 env。
    el_render: bool,
    /// 自绘 File 预览的纵向滚动偏移(px,≤0 向下滚)。仅自绘路径用。
    el_scroll_y: f32,
    /// TnE-12:查找/替换栏激活字段输入框的窗口坐标(每帧由 find_bar 里的占位
    /// canvas 写入)。`bounds_for_range` 在 `find_open` 时据此把 IME 候选框定位到
    /// 查找框旁,而非代码区光标(中文搜索时候选框才不会飘到正文)。
    find_field_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _release_subscription: Subscription,
}

impl QuickLook {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        // Measure the monospace advance once (mouse click → column). Mirrors
        // terminal_view's cell-width probe; falls back to a 0.6 ratio.
        let font_id = cx
            .text_system()
            .resolve_font(&gpui::font(&config.font().family));
        let char_w = cx
            .text_system()
            .advance(font_id, px(CODE_FS), 'm')
            .map(|s| f32::from(s.width))
            .unwrap_or(CODE_FS * 0.6);
        let _release_subscription = cx.on_release(|view, cx| {
            view.evict_assets_internal(cx);
        });
        Self {
            config,
            root,
            path: None,
            tab: Tab::File,
            file_data: QuickLookData::None,
            diff: Rc::new(Vec::new()),
            diff_dirty: true,
            editing: false,
            edit: QuickLookEditState::default(),
            cursor: (0, 0),
            sel_anchor: None,
            edit_drag: false,
            dirty: false,
            char_w,
            ime_marked: None,
            code_bounds: Rc::new(RefCell::new(Bounds::default())),
            find_open: false,
            replacing: false,
            find_query: String::new(),
            replace_query: String::new(),
            find_field_replace: false,
            file_highlight_cache: std::rc::Rc::new(std::cell::RefCell::new(
                std::collections::HashMap::new(),
            )),
            file_jump_highlight: None,
            scroll: UniformListScrollHandle::default(),
            needs_focus: false,
            rail_pos: None,
            focus_handle: cx.focus_handle(),
            loading_state: LoadingState::Ready,
            generation: 0,
            is_remote_source: false,
            remote_fs: SftpFileService::shared(),
            remote_source: None,
            remote_diff_file: None,
            text_format: None,
            opened_guard: None,
            save_conflict: None,
            save_error: None,
            save_in_flight: false,
            pending_leave: None,
            edit_on_ready: false,
            diff_loading: false,
            diff_generation: 0,
            hunk_busy: false,
            hunk_error: None,
            cancel_token: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hscroll_px: 0.0,
            hscroll_drag: None,
            hscroll_content_w: 0.0,
            caret_motion: CaretMotionState::default(),
            motion_cleanup_pending: false,
            last_follow_cursor: None,
            // TnE-10 收尾:自绘 File 预览/编辑器现为默认路径;`TN_QL_LEGACY=1`
            // 强制回退旧 `uniform_list`(紧急逃生口)。
            el_render: std::env::var("TN_QL_LEGACY").is_err(),
            el_scroll_y: 0.0,
            find_field_bounds: Rc::new(Cell::new(None)),
            _release_subscription,
        }
    }

    /// Whether a file is currently loaded (the workspace shows the overlay only
    /// when there is one — an empty overlay would float over nothing).
    pub fn has_file(&self) -> bool {
        self.path.is_some()
    }

    /// workspace 同步 RAIL 读数(从活动栏打开 = `Some((idx, total))`,文件树 = `None`)。
    /// 只在变化时 notify,避免每帧重渲。
    pub(crate) fn set_rail_pos(&mut self, pos: Option<(usize, usize)>, cx: &mut Context<Self>) {
        if self.rail_pos != pos {
            self.rail_pos = pos;
            cx.notify();
        }
    }

    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    pub fn refresh_after_external_change(&mut self, cx: &mut Context<Self>) {
        if self.is_remote_source || self.remote_source.is_some() || self.save_in_flight {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let disk_guard = local_file_guard(&path);
        match external_reload_decision(
            self.editing,
            self.dirty,
            self.opened_guard.as_ref(),
            disk_guard.as_ref(),
        ) {
            ExternalReloadDecision::Unchanged => {}
            ExternalReloadDecision::Reload => {
                self.path = None;
                self.open(path, cx);
            }
            ExternalReloadDecision::Conflict(conflict) => {
                self.save_conflict = Some(conflict);
                self.save_error = None;
                cx.notify();
            }
        }
    }

    pub fn request_close(&mut self, cx: &mut Context<Self>) -> bool {
        match self.request_leave(PendingLeave::Close, cx) {
            LeaveDecision::Continue => {
                self.close(cx);
                true
            }
            LeaveDecision::Confirm => false,
        }
    }

    pub fn request_quit(&mut self, cx: &mut Context<Self>) -> bool {
        matches!(
            self.request_leave(PendingLeave::Quit, cx),
            LeaveDecision::Continue
        )
    }

    fn request_leave(&mut self, action: PendingLeave, cx: &mut Context<Self>) -> LeaveDecision {
        let decision = dirty_leave_decision(self.dirty, &mut self.pending_leave, action);
        if decision == LeaveDecision::Confirm {
            self.save_conflict = None;
            self.save_error = None;
            cx.notify();
        }
        decision
    }

    /// Whether the currently loaded file can be opened in the text editor.
    /// PDF, image, binary, and Office files (docx/xlsx/ppt/etc.) are view-only
    /// and should not show the "Enter 编辑" hint in the footer.
    fn is_editable(&self) -> bool {
        self.path
            .as_ref()
            .is_some_and(|path| preview_is_editable(path, &self.file_data, self.is_remote_source))
    }

    /// `(filename, language)` for the open file — drives the status bar's
    /// "element.rs · Rust" segment.
    pub fn status(&self) -> Option<(String, &'static str)> {
        let p = self.path.as_ref()?;
        let name = p.file_name()?.to_string_lossy().to_string();
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        let lang = match &self.file_data {
            QuickLookData::Binary { .. } => binary_label(ext),
            QuickLookData::Pdf { .. } => "PDF",
            _ => text_label(ext),
        };
        Some((name, lang))
    }

    fn evict_assets_internal(&self, cx: &mut App) {
        match &self.file_data {
            QuickLookData::Image { img } => {
                evict_render_image(img, cx);
            }
            QuickLookData::Pdf { pages, .. } => {
                if let Ok(lock) = pages.lock() {
                    for page in lock.iter().flatten() {
                        evict_render_image(page, cx);
                    }
                }
            }
            _ => {}
        }
    }

    /// Explicitly close QuickLook, evicting any GPUI caches and freeing memory capacity
    /// for HashMaps and large vectors to prevent "ghost" memory leaks when hidden.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        // --- EXPLICIT GPUI CACHE EVICTION ---
        self.evict_assets_internal(cx);

        // --- MEMORY CAPACITY RELEASE ---
        self.path = None;
        self.file_data = QuickLookData::None;
        self.edit = QuickLookEditState::default();
        self.diff = Rc::new(Vec::new());
        self.ime_marked = None;

        // Replace HashMaps entirely to return their capacity to the OS!
        self.file_highlight_cache =
            std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));
        self.file_jump_highlight = None;
        self.snap_caret_motion();

        // Cancel any pending async tasks.
        self.cancel_token
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.is_remote_source = false;
        self.remote_source = None;
        self.remote_diff_file = None;
        self.text_format = None;
        self.opened_guard = None;
        self.save_conflict = None;
        self.save_error = None;
        self.save_in_flight = false;
        self.pending_leave = None;
        self.hunk_busy = false;
        self.hunk_error = None;

        cx.notify();
    }

    fn reset_for_open(&mut self, path: PathBuf, is_remote: bool, cx: &mut Context<Self>) {
        // --- EXPLICIT GPUI CACHE EVICTION ---
        self.evict_assets_internal(cx);

        self.path = Some(path.clone());
        self.root = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        self.tab = Tab::File;
        self.editing = false;
        self.sel_anchor = None; // 清上个文件残留的(预览)选区
        self.cursor = (0, 0);
        self.edit = QuickLookEditState::default();
        self.edit_drag = false;
        self.hscroll_px = 0.0; // 新文件从最左开始
        self.el_scroll_y = 0.0; // 自绘路径:新文件从顶部开始
        self.last_follow_cursor = None;
        self.dirty = false;
        self.file_data = QuickLookData::None;
        self.diff = Rc::new(Vec::new());
        self.diff_dirty = !is_remote;
        self.diff_loading = false;
        self.scroll = UniformListScrollHandle::default();
        self.needs_focus = true;
        self.find_open = false;
        self.file_highlight_cache.borrow_mut().clear();
        self.file_jump_highlight = None;
        self.snap_caret_motion();
        self.is_remote_source = is_remote;
        self.remote_source = None;
        self.remote_diff_file = None;
        self.text_format = None;
        self.opened_guard = None;
        self.save_conflict = None;
        self.save_error = None;
        self.save_in_flight = false;
        self.pending_leave = None;
        self.hunk_busy = false;
        self.hunk_error = None;

        self.cancel_token
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.cancel_token = Arc::new(std::sync::atomic::AtomicBool::new(false));
    }

    /// Open `path`: read its text off the **background** thread, default to the File
    /// tab (preview). Binary files (null bytes) or files exceeding [`MAX_FILE_SIZE`]
    /// are detected early — instead of garbled/empty content, the overlay shows file
    /// info with size and a "can't preview" note.
    ///
    /// ## Async + stale-result prevention
    /// The file read and binary peek are dispatched to `cx.background_executor()`;
    /// the UI switches to `LoadingState::Loading` immediately (skeleton renders).
    /// A monotonic `generation` counter prevents out-of-order completion from
    /// overwriting a newer open that was triggered while this one was in flight.
    pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.path.as_ref() == Some(&path) && self.loading_state == LoadingState::Ready {
            return; // unchanged, don't re-trigger async loading
        }
        if self.request_leave(PendingLeave::LocalOpen(path.clone()), cx) == LeaveDecision::Confirm {
            return;
        }

        self.reset_for_open(path.clone(), false, cx);
        let cancel_token = self.cancel_token.clone();

        // ── Async: bump generation + switch to Loading → skeleton renders ──
        self.generation = self.generation.wrapping_add(1);
        let gen = self.generation;
        self.loading_state = LoadingState::Loading;
        self.edit_on_ready = false;
        cx.notify();

        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let path_clone = path.clone();
            let ext = path_clone
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_lowercase();

            if ext == "pdf" {
                let (tx, mut rx) = futures::channel::mpsc::unbounded();
                let pdf_cancel = cancel_token.clone();
                let exec_debounce = exec.clone();
                exec.spawn(async move {
                    if pdf_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    exec_debounce.timer(std::time::Duration::from_millis(100)).await;
                    if pdf_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    use pdfium_render::prelude::*;
                    static PDFIUM: std::sync::OnceLock<Option<Pdfium>> = std::sync::OnceLock::new();
                    let pdfium_lock = PDFIUM.get_or_init(|| {
                        // pdfium.dll is staged beside the exe by tn-app's build.rs
                        // (dev: target/<profile>; installed: the install dir). Prefer
                        // the bundled copy — its ABI is matched to pdfium-render — and
                        // fall back to a system-installed pdfium only if that's absent.
                        let beside_exe = std::env::current_exe()
                            .ok()
                            .and_then(|p| p.parent().map(|d| d.join("pdfium.dll")));
                        let bind_result = match &beside_exe {
                            Some(dll) => Pdfium::bind_to_library(dll)
                                .or_else(|_| Pdfium::bind_to_system_library()),
                            None => Pdfium::bind_to_system_library(),
                        };
                        bind_result.ok().map(|bind| Pdfium::new(bind))
                    });

                    let pdfium = match pdfium_lock {
                        Some(p) => p,
                        None => {
                            let _ = tx.unbounded_send(Err("PDF 引擎初始化失败".to_string()));
                            return;
                        }
                    };

                    if pdf_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }

                    match pdfium.load_pdf_from_file(&path_clone, None) {
                        Ok(document) => {
                            let page_count = document.pages().len() as usize;
                            let limit = page_count.min(100); // 宽容到 100 页
                            let _ = tx.unbounded_send(Ok((limit, None)));

                            // 1000px 对速览足够清晰,比 1200 省 ~30% JPEG 字节/页内存(审查⑪)。
                            let render_config = PdfRenderConfig::new().set_target_width(1000);
                            for (i, page) in document.pages().iter().take(limit).enumerate() {
                                if pdf_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                    break;
                                }
                                if let Ok(bitmap) = page.render_with_config(&render_config) {
                                    if let Ok(img) = bitmap.as_image() {
                                        let render_img = dynamic_image_to_render_image(img);
                                        let _ = tx.unbounded_send(Ok((
                                            limit,
                                            Some((i, Arc::new(render_img))),
                                        )));
                                    }
                                }
                            }
                        }
                        Err(_) => {
                            let _ = tx.unbounded_send(Err("无法解析此 PDF 文件".to_string()));
                        }
                    }
                })
                .detach();

                use futures::StreamExt;
                let mut pages_arc: Option<Arc<std::sync::Mutex<Vec<Option<Arc<RenderImage>>>>>> =
                    None;

                while let Some(msg) = rx.next().await {
                    match msg {
                        Ok((limit, None)) => {
                            let arc = Arc::new(std::sync::Mutex::new(vec![None; limit]));
                            pages_arc = Some(arc.clone());
                            let _ = this.update(cx, |v, cx| {
                                if v.generation != gen {
                                    return;
                                }
                                v.file_data = QuickLookData::Pdf {
                                    pages: arc,
                                    page_count: limit,
                                };
                                v.loading_state = LoadingState::Ready;
                                cx.notify();
                            });
                        }
                        Ok((_, Some((i, img)))) => {
                            if let Some(arc) = &pages_arc {
                                if let Ok(mut lock) = arc.lock() {
                                    lock[i] = Some(img.clone());
                                }
                                let _ = this.update(cx, |v, cx| {
                                    if v.generation != gen {
                                        evict_render_image(&img, cx);
                                    } else {
                                        cx.notify();
                                    }
                                });
                            }
                        }
                        Err(e) => {
                            let _ = this.update(cx, |v, cx| {
                                if v.generation != gen {
                                    return;
                                }
                                v.file_data = QuickLookData::Text {
                                    lines: Arc::new(vec![e]),
                                    truncated: false,
                                };
                                v.loading_state = LoadingState::Ready;
                                cx.notify();
                            });
                            break;
                        }
                    }
                }
                return;
            }

            if matches!(
                ext.as_str(),
                "png" | "jpg" | "jpeg" | "webp" | "bmp" | "gif"
            ) {
                let path_for_bg = path_clone.clone();
                let img_cancel = cancel_token.clone();
                let exec_debounce = cx.background_executor().clone();
                let bytes_res: Result<RenderImage, anyhow::Error> = cx
                    .background_executor()
                    .spawn(async move {
                        if img_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err(anyhow::anyhow!("Cancelled"));
                        }
                        exec_debounce.timer(std::time::Duration::from_millis(100)).await;
                        if img_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err(anyhow::anyhow!("Cancelled"));
                        }
                        let dynamic_img = image::ImageReader::open(&path_for_bg)?
                            .with_guessed_format()?
                            .decode()?;
                        if img_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err(anyhow::anyhow!("Cancelled"));
                        }
                        let dynamic_img = resize_image_to_fit(dynamic_img, 2048, 2048);
                        let render_img = dynamic_image_to_render_image(dynamic_img);
                        Ok(render_img)
                    })
                    .await;

                if cancel_token.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }

                if let Ok(img) = bytes_res {
                    let _ = this.update(cx, |v, cx| {
                        if v.generation != gen {
                            evict_render_image(&Arc::new(img), cx);
                            return;
                        }
                        v.file_data = QuickLookData::Image { img: Arc::new(img) };
                        v.loading_state = LoadingState::Ready;
                        cx.notify();
                    });
                    return;
                }

                let _ = this.update(cx, |v, cx| {
                    if v.generation != gen {
                        return;
                    }
                    v.file_data = QuickLookData::Binary {
                        size: std::fs::metadata(&path_clone).map(|m| m.len()).unwrap_or(0),
                    };
                    v.loading_state = LoadingState::Ready;
                    cx.notify();
                });
                return;
            }

            let txt_cancel = cancel_token.clone();
            let exec_debounce = exec.clone();
            let res = exec
                .spawn(async move {
                    if txt_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return PreviewPayload {
                            data: QuickLookData::None,
                            format: None,
                            guard: None,
                        };
                    }
                    exec_debounce.timer(std::time::Duration::from_millis(100)).await;
                    if txt_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return PreviewPayload {
                            data: QuickLookData::None,
                            format: None,
                            guard: None,
                        };
                    }
                    let meta = std::fs::metadata(&path).ok();
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);

                    let mut peek_buf = vec![0u8; PEEK_SIZE.min(size as usize)];
                    let is_binary = if size > 0 {
                        let n = std::fs::File::open(&path)
                            .ok()
                            .and_then(|mut f| std::io::Read::read(&mut f, &mut peek_buf).ok())
                            .unwrap_or(0);
                        content_inspector::inspect(&peek_buf[..n]).is_binary()
                    } else {
                        false
                    };

                    if matches!(ext.as_str(), "docx" | "xlsx" | "xls" | "ods") {
                        if ext == "docx" {
                            use dotext::MsDoc;
                            if let Ok(mut doc) = dotext::Docx::open(&path) {
                                use std::io::Read;
                                let mut text = String::new();
                                let _ = doc.read_to_string(&mut text);
                                let mut line_iter = text.lines();
                                let mut lines = Vec::with_capacity(MAX_LINES.min(1000));
                                for line in (&mut line_iter).take(MAX_LINES) {
                                    if txt_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                        return PreviewPayload {
                                            data: QuickLookData::None,
                                            format: None,
                                            guard: None,
                                        };
                                    }
                                    lines.push(line.to_string());
                                }
                                let truncated = line_iter.next().is_some();
                                return PreviewPayload {
                                    data: QuickLookData::Text {
                                        lines: Arc::new(lines),
                                        truncated,
                                    },
                                    format: None,
                                    guard: None,
                                };
                            }
                        } else {
                            use calamine::{open_workbook_auto, Data, Reader};
                            if let Ok(mut workbook) = open_workbook_auto(&path) {
                                // Two-pass alignment (审查㉑): collect cells first, then
                                // `align_table` pads each column to its widest cell so the
                                // text table reads cleanly (was a ragged `join(" | ")`).
                                let mut cells: Vec<Vec<String>> = Vec::new();
                                let mut truncated = false;
                                if let Some(Ok(range)) = workbook.worksheet_range_at(0) {
                                    let mut row_iter = range.rows();
                                    for row in (&mut row_iter).take(MAX_LINES) {
                                        if txt_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                                            return PreviewPayload {
                                                data: QuickLookData::None,
                                                format: None,
                                                guard: None,
                                            };
                                        }
                                        cells.push(
                                            row.iter()
                                                .map(|c| match c {
                                                    Data::String(s) => s.to_string(),
                                                    Data::Float(f) => f.to_string(),
                                                    Data::Int(i) => i.to_string(),
                                                    Data::Bool(b) => b.to_string(),
                                                    _ => String::new(),
                                                })
                                                .collect::<Vec<_>>(),
                                        );
                                    }
                                    truncated = row_iter.next().is_some();
                                }
                                return PreviewPayload {
                                    data: QuickLookData::Text {
                                        lines: Arc::new(align_table(&cells)),
                                        truncated,
                                    },
                                    format: None,
                                    guard: None,
                                };
                            }
                        }
                    }

                    if size > MAX_FILE_SIZE || is_binary {
                        return PreviewPayload {
                            data: QuickLookData::Binary { size },
                            format: None,
                            guard: None,
                        };
                    }

                    if txt_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return PreviewPayload {
                            data: QuickLookData::None,
                            format: None,
                            guard: None,
                        };
                    }

                    if let Ok(bytes) = std::fs::read(&path) {
                        let mut payload =
                            preview_payload_from_bytes(bytes.clone(), &ext, Some(size), None);
                        payload.guard =
                            meta.as_ref().and_then(|m| m.modified().ok()).map(|mtime| {
                                FileGuard::from_parts(mtime, size, file_sample_hash(&bytes))
                            });
                        return payload;
                    }
                    PreviewPayload {
                        data: QuickLookData::Binary { size },
                        format: None,
                        guard: None,
                    }
                })
                .await;

            if cancel_token.load(std::sync::atomic::Ordering::Relaxed) {
                return;
            }

            let _ = this.update(cx, |v, cx| {
                // ── Stale guard: drop if a newer open() was dispatched ──
                if v.generation != gen {
                    return;
                }
                v.file_data = res.data;
                v.text_format = res.format;
                v.opened_guard = res.guard;
                v.loading_state = LoadingState::Ready;

                // Deferred edit: `open_for_edit` was called while loading.
                if v.edit_on_ready {
                    v.enter_edit();
                    v.edit_on_ready = false;
                }

                // If the user is already on the Diff tab (e.g. clicked a card),
                // kick off the async diff now that the file is ready.
                if v.tab == Tab::Diff {
                    v.ensure_diff(cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Open a remote SSH file through the remote filesystem service. Reads are
    /// bounded; text saves use SFTP stat/hash guards and never invoke local
    /// `git diff`, image/PDF decoders, or disk writes against the virtual
    /// `ssh://` display path.
    pub fn open_remote(
        &mut self,
        cfg: tn_pty::SshConfig,
        id: RemoteId,
        size: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        let display_path = remote_path_to_virtual_path(&id);
        if self.path.as_ref() == Some(&display_path)
            && self.loading_state == LoadingState::Ready
            && self.is_remote_source
        {
            return;
        }
        if self.request_leave(
            PendingLeave::RemoteOpen {
                cfg: cfg.clone(),
                id: id.clone(),
                size,
            },
            cx,
        ) == LeaveDecision::Confirm
        {
            return;
        }
        let ext = display_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_lowercase();
        let remote_fs = self.remote_fs.clone();
        let remote_path = id.path.clone();
        let source = RemoteSource {
            cfg: cfg.clone(),
            id: id.clone(),
        };
        self.reset_for_open(display_path, true, cx);
        self.remote_source = Some(source);
        self.generation = self.generation.wrapping_add(1);
        let gen = self.generation;
        self.loading_state = LoadingState::Loading;
        self.edit_on_ready = false;
        let cancel_token = self.cancel_token.clone();
        cx.notify();

        let exec = cx.background_executor().clone();
        let exec_debounce = exec.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let data = exec
                .spawn(async move {
                    if cancel_token.load(std::sync::atomic::Ordering::Relaxed) {
                        return PreviewPayload {
                            data: QuickLookData::None,
                            format: None,
                            guard: None,
                        };
                    }
                    exec_debounce.timer(std::time::Duration::from_millis(100)).await;
                    if cancel_token.load(std::sync::atomic::Ordering::Relaxed) {
                        return PreviewPayload {
                            data: QuickLookData::None,
                            format: None,
                            guard: None,
                        };
                    }
                    let stat = remote_fs.stat_file(&cfg, &remote_path).ok();
                    let declared_size = size.or_else(|| stat.as_ref().and_then(|s| s.size));
                    let bytes = remote_fs.read_file(&cfg, &remote_path, REMOTE_READ_LIMIT);
                    if cancel_token.load(std::sync::atomic::Ordering::Relaxed) {
                        return PreviewPayload {
                            data: QuickLookData::None,
                            format: None,
                            guard: None,
                        };
                    }
                    match bytes {
                        Ok(bytes) => {
                            preview_payload_from_bytes(bytes, &ext, declared_size, stat.as_ref())
                        }
                        Err(e) => PreviewPayload {
                            data: QuickLookData::Text {
                                lines: Arc::new(vec![format!("Remote preview failed: {e}")]),
                                truncated: false,
                            },
                            format: None,
                            guard: None,
                        },
                    }
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.generation != gen {
                    return;
                }
                v.file_data = data.data;
                v.text_format = data.format;
                v.opened_guard = data.guard;
                v.loading_state = LoadingState::Ready;
                v.diff_dirty = false;
                v.diff_loading = false;
                cx.notify();
            });
        })
        .detach();
    }

    /// Open `path` straight into the editor (app menu「设置」opens config.toml here).
    /// If the file is still loading (skeleton shown), the edit is deferred — the
    /// async completion handler enters edit once the content arrives.
    pub fn open_for_edit(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.request_leave(PendingLeave::LocalOpenForEdit(path.clone()), cx)
            == LeaveDecision::Confirm
        {
            return;
        }
        self.open(path.clone(), cx);
        if self.loading_state == LoadingState::Ready {
            self.enter_edit();
        } else {
            self.edit_on_ready = true;
        }
    }

    /// Open `path` straight on the Diff tab — the agent activity-rail card click
    /// ("点卡片 = 速览全 diff") lands here.
    pub fn open_diff(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if self.request_leave(PendingLeave::LocalOpenDiff(path.clone()), cx)
            == LeaveDecision::Confirm
        {
            return;
        }
        self.open(path, cx);
        self.select_tab_now(Tab::Diff, cx);
    }

    pub(crate) fn open_remote_diff(
        &mut self,
        file: crate::remote_git::RemoteGitFile,
        cx: &mut Context<Self>,
    ) {
        if self.request_leave(PendingLeave::RemoteOpenDiff(file.clone()), cx)
            == LeaveDecision::Confirm
        {
            return;
        }
        let id = RemoteId::new(&file.cfg, file.remote_path().as_str());
        self.open_remote(file.cfg.clone(), id, None, cx);
        self.remote_diff_file = Some(file);
        self.diff_dirty = true;
        self.select_tab_now(Tab::Diff, cx);
    }

    /// Recompute `diff` **asynchronously** — dispatched to the background executor.
    /// Stale-protected by an independent `diff_generation` counter so rapid
    /// tab-toggling / file navigation never shows an old diff on a new file.
    fn ensure_diff(&mut self, cx: &mut Context<Self>) {
        if !self.diff_dirty || self.diff_loading {
            return;
        }
        if let Some(file) = self.remote_diff_file.clone() {
            self.diff_generation = self.diff_generation.wrapping_add(1);
            let gen = self.diff_generation;
            self.diff_loading = true;
            cx.notify();

            let exec = cx.background_executor().clone();
            let diff_cancel = self.cancel_token.clone();
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let diff_lines = exec
                    .spawn(async move {
                        if diff_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return vec![];
                        }
                        let service = SshCommandService::shared();
                        let text = crate::remote_git::diff_for_remote_file(service.as_ref(), &file)
                            .unwrap_or_default();
                        if diff_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return vec![];
                        }
                        parse_diff(&text)
                    })
                    .await;

                let _ = this.update(cx, |v, cx| {
                    if v.diff_generation == gen {
                        v.diff = Rc::new(diff_lines);
                        v.diff_dirty = false;
                        v.diff_loading = false;
                        cx.notify();
                    }
                });
            })
            .detach();
            return;
        }
        if self.is_remote_source {
            self.diff = Rc::new(Vec::new());
            self.diff_dirty = false;
            self.diff_loading = false;
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };

        self.diff_generation = self.diff_generation.wrapping_add(1);
        let gen = self.diff_generation;
        self.diff_loading = true;
        cx.notify();

        let exec = cx.background_executor().clone();
        let root = self.root.clone();
        let diff_cancel = self.cancel_token.clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let diff_lines = exec
                .spawn(async move {
                    if diff_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return vec![];
                    }
                    let rel = path
                        .strip_prefix(&root)
                        .unwrap_or(&path)
                        .to_string_lossy()
                        .into_owned();
                    let text = crate::gitutil::capture_bounded(
                        &root,
                        &["diff", "--no-color", "--", &rel],
                        std::time::Duration::from_millis(1500),
                    );
                    if diff_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return vec![];
                    }
                    parse_diff(text.as_deref().unwrap_or(""))
                })
                .await;

            let _ = this.update(cx, |v, cx| {
                if v.diff_generation == gen {
                    v.diff = Rc::new(diff_lines);
                    v.diff_dirty = false;
                    v.diff_loading = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Accept (`--cached`) or reject (`--reverse`) a single remote hunk via
    /// `git apply` over SSH, then refresh the diff + agent rails. Only valid on a
    /// remote Diff tab (`remote_diff_file` set). The patch is rebuilt from a
    /// **freshly fetched** diff so it always applies against the current tree (the
    /// rendered `DiffLine`s have lost the raw hunk body needed for a patch).
    fn apply_hunk(
        &mut self,
        hunk_index: usize,
        action: crate::remote_git::HunkAction,
        cx: &mut Context<Self>,
    ) {
        let Some(file) = self.remote_diff_file.clone() else {
            return;
        };
        if self.hunk_busy {
            return;
        }
        self.hunk_busy = true;
        self.hunk_error = None;
        cx.notify();

        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result: Result<(), String> = exec
                .spawn(async move {
                    let service = SshCommandService::shared();
                    let text = crate::remote_git::diff_for_remote_file(service.as_ref(), &file)
                        .map_err(|e| e.to_string())?;
                    let parsed = crate::remote_git::parse_file_diff(&file.path, &text);
                    crate::remote_git::apply_remote_hunk(
                        service.as_ref(),
                        &file,
                        &parsed,
                        hunk_index,
                        action,
                    )
                    .map_err(|e| e.to_string())
                })
                .await;

            let _ = this.update(cx, |v, cx| {
                v.hunk_busy = false;
                match result {
                    Ok(()) => {
                        // Re-fetch the diff so the applied/reverted hunk drops out,
                        // and let the workspace refresh remote「本次改动」rails.
                        v.diff_dirty = true;
                        v.ensure_diff(cx);
                        cx.emit(QuickLookEvent::RemoteChangesDirty);
                    }
                    Err(msg) => v.hunk_error = Some(msg),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn dismiss_hunk_error(&mut self, cx: &mut Context<Self>) {
        self.hunk_error = None;
        cx.notify();
    }

    /// Switch tabs; computing the diff lazily (async) when entering the Diff tab.
    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        if self.request_leave(PendingLeave::Tab(tab), cx) == LeaveDecision::Confirm {
            return;
        }
        self.select_tab_now(tab, cx);
    }

    fn select_tab_now(&mut self, tab: Tab, cx: &mut Context<Self>) {
        self.tab = tab;
        self.editing = false;
        self.sel_anchor = None;
        self.edit_drag = false;
        self.hscroll_px = 0.0; // File↔Diff 内容宽不同,切换从最左开始,不残留横滚
        self.file_jump_highlight = None;
        if tab == Tab::Diff {
            self.ensure_diff(cx);
        }
    }

    fn continue_pending_leave(&mut self, cx: &mut Context<Self>) {
        let Some(action) = self.pending_leave.take() else {
            return;
        };
        self.dirty = false;
        self.save_conflict = None;
        self.save_error = None;
        match action {
            PendingLeave::Close => {
                self.close(cx);
                cx.emit(QuickLookEvent::CloseConfirmed);
            }
            PendingLeave::Nav(delta) => cx.emit(QuickLookEvent::Nav(delta)),
            PendingLeave::Tab(tab) => self.select_tab_now(tab, cx),
            PendingLeave::LocalOpen(path) => self.open(path, cx),
            PendingLeave::RemoteOpen { cfg, id, size } => self.open_remote(cfg, id, size, cx),
            PendingLeave::LocalOpenForEdit(path) => self.open_for_edit(path, cx),
            PendingLeave::LocalOpenDiff(path) => self.open_diff(path, cx),
            PendingLeave::RemoteOpenDiff(file) => self.open_remote_diff(file, cx),
            PendingLeave::Quit => cx.emit(QuickLookEvent::QuitConfirmed),
        }
        cx.notify();
    }

    fn save_pending_leave(&mut self, cx: &mut Context<Self>) {
        if self.pending_leave.is_none() {
            return;
        }
        self.save(cx);
        if !self.save_in_flight
            && !self.dirty
            && self.save_conflict.is_none()
            && self.save_error.is_none()
        {
            self.continue_pending_leave(cx);
        }
    }

    fn discard_pending_leave(&mut self, cx: &mut Context<Self>) {
        if self.pending_leave.is_none() {
            return;
        }
        self.continue_pending_leave(cx);
    }

    fn cancel_pending_leave(&mut self, cx: &mut Context<Self>) {
        self.pending_leave = None;
        self.save_conflict = None;
        self.save_error = None;
        cx.notify();
    }

    /// Enter edit mode: copy the file into the editable buffer, cursor at (0,0).
    fn enter_edit(&mut self) {
        if self.dirty && self.edit.line_count() > 0 {
            self.editing = true;
            self.sync_edit_mirror();
            self.snap_caret_motion();
            return;
        }
        let lines = if let QuickLookData::Text { lines, .. } = &self.file_data {
            lines.as_ref().clone()
        } else {
            Vec::new()
        };
        self.edit = QuickLookEditState::from_lines(lines);
        self.cursor = (0, 0);
        self.sel_anchor = None;
        self.hscroll_px = 0.0; // 进编辑从最左开始
        self.last_follow_cursor = None;
        self.editing = true;
        self.dirty = false;
        self.edit.mark_clean();
        self.snap_caret_motion();
    }

    /// After leaving edit mode back to the File preview, mirror the (possibly
    /// unsaved) edit buffer into `file_data` so the preview shows what the user
    /// just typed — the preview renders from `file_data`, which editing doesn't
    /// touch, so without this it shows stale pre-edit content until the file is
    /// reopened. Display-only: does **not** write to disk or clear the dirty flag
    /// (so the save-conflict / dirty-close guards still see unsaved changes). The
    /// per-row highlight cache is keyed by row index and now stale, so drop it.
    fn sync_preview_from_edit(&mut self) {
        if !self.dirty {
            return; // clean buffer already equals file_data (fresh open or post-save)
        }
        if let QuickLookData::Text { truncated, .. } = &self.file_data {
            let truncated = *truncated;
            let lines = self.edit.lines().borrow().clone();
            self.file_data = QuickLookData::Text {
                lines: Arc::new(lines),
                truncated,
            };
            self.file_highlight_cache.borrow_mut().clear();
        }
    }

    /// TnE-11: caret-follow for the self-paint editor. Only when the cursor *changes*
    /// (de-bounced via `last_follow_cursor`) — otherwise it would fight the user's
    /// manual wheel/thumb scroll every frame (踩过的坑). Mutates `el_scroll_y`
    /// (vertical) + `hscroll_px` (horizontal) so a freshly-moved caret is visible.
    fn el_follow_caret(&mut self) {
        if !self.editing {
            return;
        }
        let cursor = self.cursor;
        if self.last_follow_cursor == Some(cursor) {
            return;
        }
        let (vw, vh) = {
            let b = self.code_bounds.borrow();
            (f32::from(b.size.width), f32::from(b.size.height))
        };
        if vw <= 0.0 || vh <= 0.0 {
            return;
        }
        self.last_follow_cursor = Some(cursor);
        let char_w = self.char_w;
        let lines_ref = self.edit.lines();
        let lines = lines_ref.borrow();
        let wrap_mode = self.file_wrap_mode_for_viewport(vw);
        let layout = LineLayout::build(&lines, wrap_mode);
        let (visual_row, _) = layout.logical_to_visual(cursor);
        let line = lines.get(cursor.0).map(String::as_str).unwrap_or("");
        match wrap_mode {
            WrapMode::None => {
                let caret_x = CODE_GUTTER
                    + crate::editor::geometry::prefix_cols(line, cursor.1) as f32 * char_w;
                let max_disp = lines.iter().map(|l| disp_width(l)).max().unwrap_or(0);
                let content_w = (CODE_GUTTER + (max_disp as f32 + 1.0) * char_w).max(vw);
                let max_off = (content_w - vw).max(0.0);
                self.hscroll_px = crate::editor::geometry::follow_h_offset(
                    caret_x,
                    self.hscroll_px,
                    vw,
                    max_off,
                    char_w * 4.0,
                );
            }
            WrapMode::Word { .. } => {
                self.hscroll_px = 0.0;
            }
        }
        // Vertical: scroll the caret visual row into view if it left the window.
        let first = (-self.el_scroll_y / ROW_H).floor().max(0.0) as usize;
        let rows = (vh / ROW_H).floor() as usize;
        let last = first + rows.saturating_sub(1);
        if visual_row < first {
            self.el_scroll_y = -(visual_row as f32) * ROW_H;
        } else if visual_row > last {
            self.el_scroll_y = -((visual_row + 1) as f32 * ROW_H - vh);
        }
        let content_h = layout.visual_count() as f32 * ROW_H;
        let vmin = (vh - content_h).min(0.0);
        self.el_scroll_y = self.el_scroll_y.clamp(vmin, 0.0);
    }

    /// TnE-09: read-only self-painted File preview element (env-gated). A scrollable
    /// container whose `canvas` paints via [`paint_file_preview`]; vertical scroll is
    /// `el_scroll_y` driven by the wheel here (clamped to the content), horizontal is
    /// the shared `hscroll_px`. Default off — `render` only calls this when
    /// `TN_QL_ELEMENT` is set, so the `uniform_list` path stays the one-key fallback.
    fn file_element(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let config = self.config.clone();
        let char_w = self.char_w;
        let scroll_y = self.el_scroll_y;
        let hscroll = self.hscroll_px;
        let sel = self.sel_range();
        let editing = self.editing;
        let caret = self.cursor;
        let motion = motion_snapshot(&mut self.caret_motion, Instant::now());
        let file_jump_highlight = (!editing).then_some(self.file_jump_highlight).flatten();
        // While the find bar is open the IME preedit belongs to the query field, not
        // the editor caret — suppress the body preedit so it isn't drawn twice.
        let find_open = self.find_open;
        let ime = if find_open {
            None
        } else {
            self.ime_marked.clone()
        };
        let focus = self.focus_handle.clone();
        let entity = cx.entity();
        let bounds_cell = self.code_bounds.clone();
        let wrap_path = self.path.clone().unwrap_or_else(|| PathBuf::from(""));

        // Line source: the edit-buffer mirror while editing (borrowed in paint — no
        // per-frame clone), else the read-only `file_data` lines.
        let edit_mirror = editing.then(|| self.edit.lines());
        let view_lines = match &self.file_data {
            QuickLookData::Text { lines, .. } if !editing => lines.clone(),
            _ => Arc::new(Vec::new()),
        };

        // Find highlights: every occurrence of the live query (突出显示), tinted under
        // the current match's selection. Empty while the find bar is closed/empty.
        let matches: Vec<((usize, usize), (usize, usize))> =
            if editing && find_open && !self.find_query.is_empty() {
                if let Some(rc) = &edit_mirror {
                    all_matches(&rc.borrow(), &self.find_query)
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };

        // Row under a pointer y (from the stashed viewport bounds + vertical scroll),
        // clamped to the document. Column comes from `row_text` + a CJK-aware hit-test
        // (`rel + hscroll_px`), exactly like the old `uniform_list` per-row handlers.
        div()
            .flex_1()
            .min_h(px(0.))
            .relative()
            .overflow_hidden()
            .bg(gpui::rgb(CODE_BG))
            .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _w, cx| {
                let (vw, vh) = {
                    let b = this.code_bounds.borrow();
                    (f32::from(b.size.width), f32::from(b.size.height))
                };
                let d = ev.delta.pixel_delta(px(ROW_H));
                let (dx, dy) = (f32::from(d.x), f32::from(d.y));
                // Horizontal: Shift+wheel (no native x axis) or a trackpad x delta.
                // Content width mirrors the renderer (gutter + longest line + 1 col).
                let lines_ref;
                let lines: &[String] = if editing {
                    lines_ref = this.edit.lines();
                    &lines_ref.borrow()
                } else {
                    match &this.file_data {
                        QuickLookData::Text { lines, .. } => lines.as_slice(),
                        _ => &[],
                    }
                };
                let layout = quicklook_file_layout(
                    lines,
                    this.file_wrap_path(),
                    vw,
                    vh,
                    this.el_scroll_y,
                    this.hscroll_px,
                    char_w,
                );
                let hmax = layout.pre.max_off;
                if ev.modifiers.shift && hmax > 0.0 {
                    this.hscroll_px = (this.hscroll_px - dy).clamp(0.0, hmax);
                } else if dx != 0.0 && hmax > 0.0 {
                    this.hscroll_px = (this.hscroll_px - dx).clamp(0.0, hmax);
                } else {
                    let content_h = layout.layout.visual_count() as f32 * ROW_H;
                    let vmin = (vh - content_h).min(0.0); // ≤ 0; 0 when content fits
                    this.el_scroll_y = (this.el_scroll_y + dy).clamp(vmin, 0.0);
                    if matches!(layout.wrap_mode, WrapMode::Word { .. }) {
                        this.hscroll_px = 0.0;
                    }
                }
                this.snap_caret_motion();
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                    let (vw, vh, left) = {
                        let b = this.code_bounds.borrow();
                        (
                            f32::from(b.size.width),
                            f32::from(b.size.height),
                            f32::from(b.origin.x),
                        )
                    };
                    let lines_ref;
                    let lines: &[String] = if editing {
                        lines_ref = this.edit.lines();
                        &lines_ref.borrow()
                    } else {
                        match &this.file_data {
                            QuickLookData::Text { lines, .. } => lines.as_slice(),
                            _ => &[],
                        }
                    };
                    let layout = quicklook_file_layout(
                        lines,
                        this.file_wrap_path(),
                        vw,
                        vh,
                        this.el_scroll_y,
                        this.hscroll_px,
                        char_w,
                    );
                    let row =
                        el_row_at(this, f32::from(ev.position.y), layout.layout.visual_count());
                    let rel = f32::from(ev.position.x) - left - CODE_GUTTER + layout.pre.h_offset;
                    let (logical_row, col) = layout.layout.hit_test(lines, row, rel, char_w);
                    this.file_jump_highlight = None;
                    this.place_cursor(logical_row, col, ev.modifiers.shift);
                    this.edit_drag = true;
                    this.snap_caret_motion();
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _w, cx| {
                // Left-drag → extend selection; release-elsewhere fallback ends the drag
                // (mouse_up can be missed if the pointer leaves the overlay — 踩过的坑).
                if ev.pressed_button != Some(MouseButton::Left) {
                    if this.edit_drag {
                        this.edit_drag = false;
                    }
                    if this.hscroll_drag.is_some() {
                        this.hscroll_drag = None;
                    }
                    return;
                }
                // Dragging the horizontal scrollbar thumb takes precedence over text drag.
                if let Some(grab) = this.hscroll_drag {
                    let (left, vw) = {
                        let b = this.code_bounds.borrow();
                        (f32::from(b.origin.x), f32::from(b.size.width))
                    };
                    let lines_ref;
                    let lines: &[String] = if editing {
                        lines_ref = this.edit.lines();
                        &lines_ref.borrow()
                    } else {
                        match &this.file_data {
                            QuickLookData::Text { lines, .. } => lines.as_slice(),
                            _ => &[],
                        }
                    };
                    let layout = quicklook_file_layout(
                        lines,
                        this.file_wrap_path(),
                        vw,
                        0.0,
                        this.el_scroll_y,
                        this.hscroll_px,
                        char_w,
                    );
                    this.hscroll_px = crate::editor::geometry::h_offset_from_drag(
                        f32::from(ev.position.x),
                        left,
                        grab,
                        layout.pre.content_w,
                        vw,
                        layout.pre.max_off,
                    );
                    this.snap_caret_motion();
                    cx.notify();
                    return;
                }
                if !this.edit_drag {
                    return;
                }
                let (vw, vh, left) = {
                    let b = this.code_bounds.borrow();
                    (
                        f32::from(b.size.width),
                        f32::from(b.size.height),
                        f32::from(b.origin.x),
                    )
                };
                let lines_ref;
                let lines: &[String] = if editing {
                    lines_ref = this.edit.lines();
                    &lines_ref.borrow()
                } else {
                    match &this.file_data {
                        QuickLookData::Text { lines, .. } => lines.as_slice(),
                        _ => &[],
                    }
                };
                let layout = quicklook_file_layout(
                    lines,
                    this.file_wrap_path(),
                    vw,
                    vh,
                    this.el_scroll_y,
                    this.hscroll_px,
                    char_w,
                );
                let row = el_row_at(this, f32::from(ev.position.y), layout.layout.visual_count());
                let rel = f32::from(ev.position.x) - left - CODE_GUTTER + layout.pre.h_offset;
                let hover = layout.layout.hit_test(lines, row, rel, char_w);
                let anchor = this.sel_anchor.unwrap_or(this.cursor);
                // Include the hovered char when dragging right (selection is half-open).
                let target = if hover >= anchor {
                    let line_len = lines.get(hover.0).map(|l| l.chars().count()).unwrap_or(0);
                    (hover.0, (hover.1 + 1).min(line_len))
                } else {
                    hover
                };
                this.place_cursor(target.0, target.1, true);
                this.snap_caret_motion();
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _ev: &MouseUpEvent, _w, cx| {
                    if this.edit_drag || this.hscroll_drag.is_some() {
                        this.edit_drag = false;
                        this.hscroll_drag = None;
                        this.snap_caret_motion();
                        cx.notify();
                    }
                }),
            )
            .child(
                canvas(
                    move |bounds, _window, _app| {
                        // Stash the viewport bounds so wheel + hit-test can read it.
                        *bounds_cell.borrow_mut() = bounds;
                    },
                    move |bounds, _prepaint, window, cx| {
                        // Register the IME/text input handler whenever editing — the
                        // handler routes committed/composed text to the buffer, or to
                        // the find query when the find bar is open (中文 search), so
                        // 中文 composition + WM_CHAR always land in `replace_text_in_range`.
                        if editing {
                            window.handle_input(
                                &focus,
                                ElementInputHandler::new(bounds, entity.clone()),
                                cx,
                            );
                        }
                        let guard;
                        let lines: &[String] = match &edit_mirror {
                            Some(rc) => {
                                guard = rc.borrow();
                                guard.as_slice()
                            }
                            None => view_lines.as_slice(),
                        };
                        paint_file_preview(
                            bounds,
                            lines,
                            &wrap_path,
                            char_w,
                            scroll_y,
                            hscroll,
                            sel,
                            &matches,
                            file_jump_highlight,
                            editing,
                            caret,
                            ime.as_deref(),
                            motion,
                            &config,
                            window,
                            cx,
                        );
                    },
                )
                .size_full(),
            )
            // Horizontal scrollbar hit strip (transparent, ~14px tall, bottom). The
            // visible thin thumb is painted in the canvas; this strip makes it
            // draggable. Down on the thumb → grab; down on the track → jump there.
            // Bubbles to the container (no stop) when there's no overflow, so a click
            // in the bottom row still selects text.
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(14.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                            let (left, vw) = {
                                let b = this.code_bounds.borrow();
                                (f32::from(b.origin.x), f32::from(b.size.width))
                            };
                            let lines_ref;
                            let lines: &[String] = if editing {
                                lines_ref = this.edit.lines();
                                &lines_ref.borrow()
                            } else {
                                match &this.file_data {
                                    QuickLookData::Text { lines, .. } => lines.as_slice(),
                                    _ => &[],
                                }
                            };
                            let layout = quicklook_file_layout(
                                lines,
                                this.file_wrap_path(),
                                vw,
                                0.0,
                                this.el_scroll_y,
                                this.hscroll_px,
                                char_w,
                            );
                            let content_w = layout.pre.content_w;
                            let max_off = layout.pre.max_off;
                            if max_off <= 0.0 {
                                return; // no overflow → let it bubble to selection
                            }
                            let thumb = crate::editor::geometry::h_scroll_thumb(
                                content_w,
                                vw,
                                this.hscroll_px,
                                max_off,
                            );
                            let rel = f32::from(ev.position.x) - left; // x within the track
                            let grab =
                                if rel >= thumb.thumb_x && rel <= thumb.thumb_x + thumb.thumb_w {
                                    rel - thumb.thumb_x // grab the thumb where clicked
                                } else {
                                    // Click on the empty track → jump so the thumb centers here.
                                    thumb.thumb_w / 2.0
                                };
                            this.hscroll_px = crate::editor::geometry::h_offset_from_drag(
                                f32::from(ev.position.x),
                                left,
                                grab,
                                content_w,
                                vw,
                                max_off,
                            );
                            this.hscroll_drag = Some(grab);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    ),
            )
    }

    /// Self-painted read-only Diff tab. This keeps Diff on the same canvas/prepaint
    /// path as File preview while leaving hunk accept/reject as normal GPUI controls
    /// layered over visible hunk rows for remote diffs.
    fn diff_element(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let config = self.config.clone();
        let char_w = self.char_w;
        let scroll_y = self.el_scroll_y;
        let hscroll = self.hscroll_px;
        let rows = Rc::new(diff_render_rows(&self.diff));
        let total = rows.len();
        let max_disp = rows.iter().map(|r| disp_width(&r.text)).max().unwrap_or(0);
        let bounds_cell = self.code_bounds.clone();
        let entity = cx.entity().downgrade();
        let is_remote_diff = self.remote_diff_file.is_some();
        let hunk_busy = self.hunk_busy;
        let sel = self.sel_range();
        let rows_for_down = rows.clone();
        let rows_for_move = rows.clone();

        let mut root = div()
            .flex_1()
            .min_h(px(0.))
            .relative()
            .overflow_hidden()
            .bg(gpui::rgb(CODE_BG))
            .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _w, cx| {
                let (vw, vh) = {
                    let b = this.code_bounds.borrow();
                    (f32::from(b.size.width), f32::from(b.size.height))
                };
                let d = ev.delta.pixel_delta(px(ROW_H));
                let (dx, dy) = (f32::from(d.x), f32::from(d.y));
                let content_w = (CODE_GUTTER + (max_disp as f32 + 1.0) * char_w).max(vw);
                let hmax = (content_w - vw).max(0.0);
                if ev.modifiers.shift && hmax > 0.0 {
                    this.hscroll_px = (this.hscroll_px - dy).clamp(0.0, hmax);
                } else if dx != 0.0 && hmax > 0.0 {
                    this.hscroll_px = (this.hscroll_px - dx).clamp(0.0, hmax);
                } else {
                    let content_h = total as f32 * ROW_H;
                    let vmin = (vh - content_h).min(0.0);
                    this.el_scroll_y = (this.el_scroll_y + dy).clamp(vmin, 0.0);
                }
                cx.notify();
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                    if rows_for_down.is_empty() {
                        return;
                    }
                    let row = el_row_at(this, f32::from(ev.position.y), rows_for_down.len());
                    let left = f32::from(this.code_bounds.borrow().origin.x);
                    let x = f32::from(ev.position.x) - left;
                    let cursor = diff_cursor_from_point(
                        rows_for_down.as_slice(),
                        row,
                        x,
                        char_w,
                        this.hscroll_px,
                    );
                    this.place_cursor(cursor.0, cursor.1, ev.modifiers.shift);
                    this.edit_drag = true;
                    if ev.click_count >= 2 {
                        this.goto_diff_target(cx);
                    }
                    cx.notify();
                    cx.stop_propagation();
                }),
            )
            .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _w, cx| {
                if ev.pressed_button != Some(MouseButton::Left) {
                    let mut changed = false;
                    if this.edit_drag {
                        this.edit_drag = false;
                        changed = true;
                    }
                    if this.hscroll_drag.is_some() {
                        this.hscroll_drag = None;
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                    return;
                }
                if let Some(grab) = this.hscroll_drag {
                    let (left, vw) = {
                        let b = this.code_bounds.borrow();
                        (f32::from(b.origin.x), f32::from(b.size.width))
                    };
                    let content_w = (CODE_GUTTER + (max_disp as f32 + 1.0) * char_w).max(vw);
                    let max_off = (content_w - vw).max(0.0);
                    this.hscroll_px = crate::editor::geometry::h_offset_from_drag(
                        f32::from(ev.position.x),
                        left,
                        grab,
                        content_w,
                        vw,
                        max_off,
                    );
                    cx.notify();
                    return;
                }
                if !this.edit_drag || rows_for_move.is_empty() {
                    return;
                }
                let row = el_row_at(this, f32::from(ev.position.y), rows_for_move.len());
                let left = f32::from(this.code_bounds.borrow().origin.x);
                let x = f32::from(ev.position.x) - left;
                let anchor = this.sel_anchor.unwrap_or(this.cursor);
                let cursor = diff_drag_cursor_from_point(
                    rows_for_move.as_slice(),
                    anchor,
                    row,
                    x,
                    char_w,
                    this.hscroll_px,
                );
                this.place_cursor(cursor.0, cursor.1, true);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, _w, cx| {
                    let mut changed = this.hscroll_drag.take().is_some();
                    if this.edit_drag {
                        this.edit_drag = false;
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                }),
            )
            .child(
                canvas(
                    move |bounds, _window, _app| {
                        *bounds_cell.borrow_mut() = bounds;
                    },
                    {
                        let rows = rows.clone();
                        move |bounds, _prepaint, window, cx| {
                            paint_diff_preview(
                                bounds,
                                rows.as_slice(),
                                char_w,
                                scroll_y,
                                hscroll,
                                sel,
                                &config,
                                window,
                                cx,
                            );
                        }
                    },
                )
                .size_full(),
            )
            .child(
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .bottom_0()
                    .h(px(14.))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                            let (left, vw) = {
                                let b = this.code_bounds.borrow();
                                (f32::from(b.origin.x), f32::from(b.size.width))
                            };
                            let content_w =
                                (CODE_GUTTER + (max_disp as f32 + 1.0) * char_w).max(vw);
                            let max_off = (content_w - vw).max(0.0);
                            if max_off <= 0.0 {
                                return;
                            }
                            let thumb = crate::editor::geometry::h_scroll_thumb(
                                content_w,
                                vw,
                                this.hscroll_px,
                                max_off,
                            );
                            let rel = f32::from(ev.position.x) - left;
                            let grab =
                                if rel >= thumb.thumb_x && rel <= thumb.thumb_x + thumb.thumb_w {
                                    rel - thumb.thumb_x
                                } else {
                                    thumb.thumb_w / 2.0
                                };
                            this.hscroll_px = crate::editor::geometry::h_offset_from_drag(
                                f32::from(ev.position.x),
                                left,
                                grab,
                                content_w,
                                vw,
                                max_off,
                            );
                            this.hscroll_content_w = content_w;
                            this.hscroll_drag = Some(grab);
                            cx.notify();
                            cx.stop_propagation();
                        }),
                    ),
            );

        if is_remote_diff {
            let viewport_h = f32::from(self.code_bounds.borrow().size.height);
            for (i, row) in rows.iter().enumerate() {
                let Some(hunk_index) = row.hunk_index else {
                    continue;
                };
                let y = scroll_y + i as f32 * ROW_H;
                if viewport_h > 0.0 && (y < -ROW_H || y > viewport_h) {
                    continue;
                }
                let th = &self.config.theme;
                let hbtn = |label: &'static str, c: tn_config::Color| {
                    div()
                        .px(px(7.))
                        .py(px(1.))
                        .rounded(px(6.))
                        .flex_none()
                        .text_size(px(crate::style::FS_MICRO))
                        .font_weight(gpui::FontWeight(640.))
                        .text_color(if hunk_busy { col(th.ui.muted) } else { col(c) })
                        .bg(if hunk_busy {
                            gpui::rgb(crate::style::L2)
                        } else {
                            cola(c, 0.12)
                        })
                        .border_1()
                        .border_color(cola(c, 0.30))
                        .child(label)
                };
                let entity_a = entity.clone();
                let entity_r = entity.clone();
                root = root.child(
                    div()
                        .absolute()
                        .top(px(y))
                        .right(px(8.))
                        .h(px(ROW_H))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(6.))
                        .child(hbtn("接受", th.ansi.green).on_mouse_down(
                            MouseButton::Left,
                            move |_e: &MouseDownEvent, _w, app| {
                                if hunk_busy {
                                    return;
                                }
                                let _ = entity_a.update(app, |this, cx| {
                                    this.apply_hunk(
                                        hunk_index,
                                        crate::remote_git::HunkAction::Apply,
                                        cx,
                                    );
                                });
                                app.stop_propagation();
                            },
                        ))
                        .child(hbtn("拒绝", th.ansi.red).on_mouse_down(
                            MouseButton::Left,
                            move |_e: &MouseDownEvent, _w, app| {
                                if hunk_busy {
                                    return;
                                }
                                let _ = entity_r.update(app, |this, cx| {
                                    this.apply_hunk(
                                        hunk_index,
                                        crate::remote_git::HunkAction::Reject,
                                        cx,
                                    );
                                });
                                app.stop_propagation();
                            },
                        )),
                );
            }
        }

        root
    }

    /// Write the edit buffer back to disk, then refresh the preview + diff.
    /// The `write` is sync (typically <1ms for reasonable files), but the
    /// diff recomputation is dispatched off-thread via `ensure_diff`.
    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if self.save_in_flight {
            return;
        }
        if let Some(source) = self.remote_source.clone() {
            self.save_remote(path, source, SaveGuardMode::Check, cx);
            return;
        }
        let format = self.text_format.unwrap_or_default();
        let lines = self.edit.lines();
        match save_local_text(
            &path,
            &lines.borrow(),
            format,
            self.opened_guard.as_ref(),
            SaveGuardMode::Check,
        ) {
            LocalSaveResult::Saved { guard, lines } => {
                let update = save_state_after_success(false, false);
                self.dirty = update.dirty;
                self.edit.mark_clean();
                self.opened_guard = Some(guard);
                self.text_format = Some(format);
                self.file_data = QuickLookData::Text {
                    lines: Arc::new(lines),
                    truncated: false,
                };
                // The diff is now stale; recompute lazily (only if the Diff tab is
                // currently showing — otherwise just mark it dirty so Ctrl+S stays
                // fast and never blocks on `git diff`).
                self.diff_dirty = update.diff_dirty;
                if self.tab == Tab::Diff {
                    self.ensure_diff(cx);
                }
                // Tell the workspace so it refreshes any agent pane's「本次改动」rail
                // now — don't rely on the file watcher (debounce / cwd coverage gaps).
                cx.emit(QuickLookEvent::FileSaved(path.clone()));
                if self.pending_leave.is_some() {
                    self.continue_pending_leave(cx);
                }
            }
            LocalSaveResult::Conflict(conflict) => {
                self.save_conflict = Some(conflict);
            }
            LocalSaveResult::Error(e) => {
                tracing::error!(path = %path.display(), error = %e, "quick look save failed");
                self.save_error = Some(e);
            }
        }
        cx.notify();
    }

    fn force_local_save(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        if self.save_in_flight {
            return;
        }
        if self.remote_source.is_some() {
            self.force_remote_save(cx);
            return;
        }
        let format = self.text_format.unwrap_or_default();
        let lines = self.edit.lines();
        match save_local_text(
            &path,
            &lines.borrow(),
            format,
            self.opened_guard.as_ref(),
            SaveGuardMode::Force,
        ) {
            LocalSaveResult::Saved { guard, lines } => {
                let update = save_state_after_success(false, false);
                self.dirty = update.dirty;
                self.edit.mark_clean();
                self.opened_guard = Some(guard);
                self.text_format = Some(format);
                self.file_data = QuickLookData::Text {
                    lines: Arc::new(lines),
                    truncated: false,
                };
                self.diff_dirty = update.diff_dirty;
                self.diff_loading = false;
                self.save_conflict = None;
                self.save_error = None;
                if self.tab == Tab::Diff {
                    self.ensure_diff(cx);
                }
                cx.emit(QuickLookEvent::FileSaved(path));
                if self.pending_leave.is_some() {
                    self.continue_pending_leave(cx);
                }
            }
            LocalSaveResult::Conflict(conflict) => {
                self.save_conflict = Some(conflict);
            }
            LocalSaveResult::Error(e) => {
                tracing::error!(path = %path.display(), error = %e, "quick look force save failed");
                self.save_error = Some(e);
            }
        }
        cx.notify();
    }

    fn save_remote(
        &mut self,
        path: PathBuf,
        source: RemoteSource,
        guard_mode: SaveGuardMode,
        cx: &mut Context<Self>,
    ) {
        let format = self.text_format.unwrap_or_default();
        let edit_lines = self.edit.lines();
        let edit_lines = edit_lines.borrow();
        let bytes = encode_text_lines(&edit_lines, format);
        let lines = edit_lines.clone();
        let opened_guard = self.opened_guard.clone();
        let remote_fs = self.remote_fs.clone();
        let cfg = source.cfg.clone();
        let remote_path = source.id.path.clone();
        self.save_in_flight = true;
        self.save_conflict = None;
        self.save_error = None;
        cx.notify();

        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = exec
                .spawn(async move {
                    let current_stat = match remote_fs.stat_file(&cfg, &remote_path) {
                        Ok(stat) => stat,
                        Err(e) => {
                            return RemoteSaveResult::Error(format!("Remote stat failed: {e}"))
                        }
                    };
                    let current_bytes =
                        match remote_fs.read_file(&cfg, &remote_path, REMOTE_READ_LIMIT) {
                            Ok(bytes) => bytes,
                            Err(e) => {
                                return RemoteSaveResult::Error(format!(
                                    "Remote conflict check failed: {e}"
                                ))
                            }
                        };
                    let current_guard = remote_file_guard(&current_stat, &current_bytes);
                    match remote_save_conflict(
                        opened_guard.as_ref(),
                        Some(&current_guard),
                        guard_mode,
                    ) {
                        Conflict::Clean => {}
                        conflict => return RemoteSaveResult::Conflict(conflict),
                    }

                    match remote_fs.write_file(&cfg, &remote_path, &bytes) {
                        Ok(stat) => RemoteSaveResult::Saved {
                            guard: remote_file_guard(&stat, &bytes),
                            lines,
                        },
                        Err(e) => RemoteSaveResult::Error(format!("Remote save failed: {e}")),
                    }
                })
                .await;

            let _ = this.update(cx, |v, cx| {
                if v.path.as_ref() != Some(&path) {
                    return;
                }
                v.save_in_flight = false;
                match result {
                    RemoteSaveResult::Saved { guard, lines } => {
                        let update = save_state_after_success(true, v.remote_diff_file.is_some());
                        v.dirty = update.dirty;
                        v.edit.mark_clean();
                        v.opened_guard = Some(guard);
                        v.file_data = QuickLookData::Text {
                            lines: Arc::new(lines),
                            truncated: false,
                        };
                        v.diff_dirty = update.diff_dirty;
                        v.diff_loading = false;
                        v.save_conflict = None;
                        v.save_error = None;
                        if v.tab == Tab::Diff {
                            v.ensure_diff(cx);
                        }
                        cx.emit(QuickLookEvent::FileSaved(path.clone()));
                        if v.pending_leave.is_some() {
                            v.continue_pending_leave(cx);
                        }
                    }
                    RemoteSaveResult::Conflict(conflict) => {
                        v.save_conflict = Some(conflict);
                    }
                    RemoteSaveResult::Error(error) => {
                        v.save_error = Some(error);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn force_remote_save(&mut self, cx: &mut Context<Self>) {
        if self.save_in_flight {
            return;
        }
        let Some(path) = self.path.clone() else {
            return;
        };
        let Some(source) = self.remote_source.clone() else {
            return;
        };
        self.save_remote(path, source, SaveGuardMode::Force, cx);
    }

    fn force_save_current_source(&mut self, cx: &mut Context<Self>) {
        if self.remote_source.is_some() {
            self.force_remote_save(cx);
        } else {
            self.force_local_save(cx);
        }
    }

    fn cancel_save_conflict(&mut self, cx: &mut Context<Self>) {
        self.save_conflict = None;
        self.save_error = None;
        cx.notify();
    }

    fn reload_current_source(&mut self, cx: &mut Context<Self>) {
        self.pending_leave = None;
        self.dirty = false;
        if let Some(source) = self.remote_source.clone() {
            self.loading_state = LoadingState::Loading;
            self.open_remote(source.cfg, source.id, None, cx);
            return;
        }
        if let Some(path) = self.path.clone() {
            // `open()` skips when the same file is already ready. Clear the current
            // path so conflict resolution can intentionally discard the edit buffer
            // and reload the disk version.
            self.path = None;
            self.open(path, cx);
        }
    }

    // ── selection / undo helpers ──

    fn sync_edit_mirror(&mut self) {
        if self.editing {
            self.cursor = self.edit.cursor();
            self.sel_anchor = self.edit.selection_anchor();
            self.dirty = self.edit.is_dirty();
        }
    }

    /// Active selection range (normalized `start ≤ end`), or `None` when collapsed.
    fn sel_range(&self) -> Option<(Pos, Pos)> {
        if self.editing {
            return self.edit.sel_range();
        }
        normalized_range(self.sel_anchor?, self.cursor)
    }

    fn undo(&mut self) {
        if self.edit.undo() {
            self.sync_edit_mirror();
            self.snap_caret_motion();
        }
    }

    fn redo(&mut self) {
        if self.edit.redo() {
            self.sync_edit_mirror();
            self.snap_caret_motion();
        }
    }

    // ── editor ops (selection-aware; buffer math in pure `op_*` fns, unit-tested) ──

    fn type_char(&mut self, ch: &str) {
        let before = self.cursor;
        let had_selection = self.sel_range().is_some();
        self.edit.type_char(ch);
        self.sync_edit_mirror();
        if had_selection {
            self.snap_caret_motion();
        } else if let Some(trigger) = text_motion_trigger(ch, before, self.cursor) {
            self.record_caret_motion(trigger, before);
        } else {
            self.snap_caret_motion();
        }
    }

    fn newline(&mut self) {
        self.edit.newline();
        self.sync_edit_mirror();
        self.snap_caret_motion();
    }

    fn indent(&mut self) {
        self.edit.indent();
        self.sync_edit_mirror();
        self.snap_caret_motion();
    }

    fn backspace(&mut self) {
        let before = self.cursor;
        let had_selection = self.sel_range().is_some();
        self.edit.backspace();
        self.sync_edit_mirror();
        if had_selection {
            self.snap_caret_motion();
        } else if let Some(trigger) = delete_motion_trigger(before, self.cursor) {
            self.record_caret_motion(trigger, before);
        } else {
            self.snap_caret_motion();
        }
    }

    fn delete_forward(&mut self) {
        let before = self.cursor;
        let had_selection = self.sel_range().is_some();
        self.edit.delete_forward();
        self.sync_edit_mirror();
        if had_selection {
            self.snap_caret_motion();
        } else if let Some(trigger) = delete_motion_trigger(before, self.cursor) {
            self.record_caret_motion(trigger, before);
        } else {
            self.snap_caret_motion();
        }
    }

    /// Move the cursor; `extend` keeps/starts the selection (Shift held).
    fn move_cursor(&mut self, key: &str, extend: bool) {
        let visual_target = if matches!(key, "up" | "down") && self.el_render {
            let lines = self.edit.lines();
            let lines = lines.borrow();
            let b = self.code_bounds.borrow();
            quicklook_visual_vertical_cursor(
                &lines,
                self.file_wrap_path(),
                self.cursor,
                if key == "up" { -1 } else { 1 },
                f32::from(b.size.width),
                f32::from(b.size.height),
                self.el_scroll_y,
                self.hscroll_px,
                self.char_w,
            )
        } else {
            None
        };
        if let Some(target) = visual_target {
            self.edit.place_cursor(target.0, target.1, extend);
            self.sync_edit_mirror();
        } else {
            self.edit.move_cursor(key, extend);
            self.sync_edit_mirror();
        }
        self.snap_caret_motion();
    }

    fn page(&mut self, dir: i32, extend: bool) {
        self.edit.page(dir, extend);
        self.sync_edit_mirror();
        self.snap_caret_motion();
    }

    fn select_all(&mut self) {
        let (last, last_len) = if self.editing {
            self.edit.select_all();
            self.sync_edit_mirror();
            return;
        } else if self.tab == Tab::Diff {
            let rows = diff_render_rows(&self.diff);
            let last = rows.len().saturating_sub(1);
            (last, diff_row_chars(&rows, last))
        } else if let QuickLookData::Text { lines, .. } = &self.file_data {
            let last = lines.len().saturating_sub(1);
            (
                last,
                lines.get(last).map(|l| l.chars().count()).unwrap_or(0),
            )
        } else {
            return;
        };
        self.sel_anchor = Some((0, 0));
        self.cursor = (last, last_len);
    }

    /// Place the cursor at (row, col) on click; `extend` = Shift-click selects.
    fn place_cursor(&mut self, row: usize, col: usize, extend: bool) {
        if !self.editing && self.tab == Tab::File {
            self.file_jump_highlight = None;
        }
        // 行/列 clamp 的来源:编辑态是 `buf`,预览态(只读拖选)是 file_data 的 lines。
        let (r, c) = if self.editing {
            let r = row.min(self.edit.line_count().saturating_sub(1));
            (r, col.min(self.edit.line_chars(r)))
        } else if self.tab == Tab::Diff {
            let rows = diff_render_rows(&self.diff);
            if rows.is_empty() {
                return;
            }
            let r = row.min(rows.len().saturating_sub(1));
            (r, col.min(diff_row_chars(&rows, r)))
        } else if let QuickLookData::Text { lines, .. } = &self.file_data {
            let r = row.min(lines.len().saturating_sub(1));
            (
                r,
                col.min(lines.get(r).map(|l| l.chars().count()).unwrap_or(0)),
            )
        } else {
            return; // 非文本预览(图片 / PDF)不可选
        };
        if self.editing {
            self.edit.place_cursor(r, c, extend);
            self.sync_edit_mirror();
            self.snap_caret_motion();
            return;
        }
        if extend {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
        } else {
            self.sel_anchor = None;
        }
        self.cursor = (r, c);
        self.snap_caret_motion();
    }

    /// Text of display row `row` for mouse hit-testing: editing → live `buf`,
    /// read-only preview → the `Text` file lines. `None` for non-text previews.
    fn row_text(&self, row: usize) -> Option<String> {
        if self.editing {
            self.edit.row_text(row)
        } else if self.tab == Tab::Diff {
            self.diff.get(row).map(|line| line.text.clone())
        } else if let QuickLookData::Text { lines, .. } = &self.file_data {
            lines.get(row).cloned()
        } else {
            None
        }
    }

    fn file_text_line_count(&self) -> usize {
        match &self.file_data {
            QuickLookData::Text { lines, .. } => lines.len(),
            _ => 0,
        }
    }

    fn file_wrap_path(&self) -> &std::path::Path {
        self.path
            .as_deref()
            .unwrap_or_else(|| std::path::Path::new(""))
    }

    fn file_wrap_mode_for_viewport(&self, viewport_w: f32) -> WrapMode {
        file_wrap_mode_for_path(
            self.file_wrap_path(),
            wrap_width_cols(viewport_w, self.char_w),
        )
    }

    fn motion_policy(&self) -> tn_config::EffectiveMotion {
        let high_load = self.motion_high_load();
        self.config
            .config
            .editor
            .effective_motion(crate::platform::reduced_motion_enabled(), high_load)
    }

    fn motion_high_load(&self) -> bool {
        std::env::var_os("TN_QL_MOTION_SNAP").is_some()
            || !self.el_render
            || large_file_motion_gate(self.edit.line_count())
    }

    fn visual_cursor_for_motion(&self, cursor: Pos) -> Option<(usize, usize)> {
        let lines = self.edit.lines();
        let lines = lines.borrow();
        let b = self.code_bounds.borrow();
        let vw = f32::from(b.size.width).max(1.0);
        let vh = f32::from(b.size.height).max(1.0);
        let layout = quicklook_file_layout(
            &lines,
            self.file_wrap_path(),
            vw,
            vh,
            self.el_scroll_y,
            self.hscroll_px,
            self.char_w,
        );
        let (visual_row, local_col) = layout.layout.logical_to_visual(cursor);
        let visual = layout.layout.visual_line(visual_row)?;
        let line = lines.get(cursor.0).map(String::as_str).unwrap_or("");
        Some((
            visual_row,
            visual_col_for_prefix(line, visual.char_start + local_col)
                .saturating_sub(visual_col_for_prefix(line, visual.char_start)),
        ))
    }

    fn record_caret_motion(&mut self, trigger: MotionTrigger, before: Pos) {
        let after = self.cursor;
        let visual_from = self.visual_cursor_for_motion(before);
        let visual_to = self.visual_cursor_for_motion(after);
        self.caret_motion.record(
            trigger,
            Instant::now(),
            CaretMotionInput {
                policy: self.motion_policy(),
                high_load: self.motion_high_load(),
                ime_active: self.ime_marked.as_ref().is_some_and(|s| !s.is_empty()),
                selecting: self.sel_anchor.is_some() || self.edit_drag,
                visual_from,
                visual_to,
                char_w: self.char_w,
                line_h: ROW_H,
                large_file: large_file_motion_gate(self.edit.line_count()),
            },
        );
    }

    fn snap_caret_motion(&mut self) {
        self.caret_motion.snap();
        self.motion_cleanup_pending = false;
    }

    fn schedule_motion_cleanup(&mut self, cx: &mut Context<Self>) {
        if self.motion_cleanup_pending {
            return;
        }
        self.motion_cleanup_pending = true;
        let exec = cx.background_executor().clone();
        cx.spawn(
            async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
                exec.timer(Duration::from_millis(16)).await;
                let again = this.update(cx, |v, cx| {
                    let active = v.caret_motion.is_animating(Instant::now());
                    if !active {
                        v.snap_caret_motion();
                    }
                    cx.notify();
                    active
                });
                if !matches!(again, Ok(true)) {
                    break;
                }
            },
        )
        .detach();
    }

    /// Drag the bottom horizontal scrollbar thumb → update `hscroll_px`. `cursor_x`
    /// is the absolute mouse X; `hscroll_content_w` (cached in render) gives the
    /// scrollable width without needing the line list here.
    fn on_hscroll_move(&mut self, cursor_x: f32, cx: &mut Context<Self>) {
        let Some(grab) = self.hscroll_drag else {
            return;
        };
        let (viewport_w, track_left) = {
            let b = self.code_bounds.borrow();
            (f32::from(b.size.width), f32::from(b.origin.x))
        };
        let content_w = self.hscroll_content_w;
        let max_off = (content_w - viewport_w).max(0.0);
        if max_off <= 0.0 || viewport_w <= 0.0 {
            return;
        }
        // 与 render 的 thumb 几何一致(左右内缘 6px、thumb 最小 36px)。
        let inset = 6.0_f32;
        let track_w = (viewport_w - inset * 2.0).max(1.0);
        let thumb_w = (track_w / content_w * track_w).clamp(36.0, track_w);
        let usable = (track_w - thumb_w).max(1.0);
        let thumb_left = (cursor_x - track_left - inset - grab).clamp(0.0, usable);
        self.hscroll_px = thumb_left / usable * max_off;
        self.snap_caret_motion();
        cx.notify();
    }

    // ── clipboard ──

    fn copy(&mut self, cx: &mut Context<Self>) {
        if self.editing {
            let text = self.edit.selected_text().unwrap_or_else(|| {
                format!(
                    "{}\n",
                    self.edit.row_text(self.edit.cursor().0).unwrap_or_default()
                )
            });
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        } else if self.tab == Tab::Diff {
            if let Some((s, e)) = self.sel_range() {
                let rows = diff_render_rows(&self.diff);
                let text = diff_selected_text(&rows, s, e);
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }
        } else if let QuickLookData::Text { lines, .. } = &self.file_data {
            // 预览态(只读):仅在有选区时复制选中文本(基于 lines,不碰 buf)。
            if let Some((s, e)) = self.sel_range() {
                cx.write_to_clipboard(ClipboardItem::new_string(selected_text(lines, s, e)));
            }
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.edit.selected_text() {
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.edit.insert_text("");
        } else {
            let line = self
                .edit
                .row_text(self.edit.cursor().0)
                .unwrap_or_default()
                .to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(format!("{line}\n")));
            self.edit.delete_current_line();
        }
        self.sync_edit_mirror();
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.edit.insert_text(&text);
        self.sync_edit_mirror();
    }

    // ── find / replace ──

    fn open_find(&mut self, replacing: bool) {
        self.find_open = true;
        self.replacing = replacing;
        self.find_field_replace = false;
        // Prefill the query from a single-line selection.
        if let Some((s, e)) = self.sel_range() {
            if s.0 == e.0 {
                self.find_query = self.edit.selected_text().unwrap_or_default();
            }
        }
    }

    /// Move to the next(`forward`)/prev match of the query (wraps), selecting it.
    fn find_next(&mut self, forward: bool) {
        if let Some(range) = self.edit.find_next(&self.find_query, forward) {
            self.sync_edit_mirror();
            self.scroll
                .scroll_to_item(range.start.0, ScrollStrategy::Center);
            // Self-paint path: center the match row + reveal its column (long lines
            // need a horizontal scroll), then pin the de-bounced follow so the
            // render-time `el_follow_caret` keeps it (won't edge-snap it away).
            if self.el_render {
                self.el_center_file_cursor(range.start);
                self.el_reveal_col(range.start.0, range.start.1);
                self.last_follow_cursor = Some(self.cursor);
            }
        }
    }

    /// Self-paint: scroll so visual `row` sits at the viewport's vertical center
    /// (clamped to the content). Used by find-jump so a match never lands flush
    /// against an edge. `el_scroll_y ≤ 0`.
    fn el_center_visual_row(&mut self, row: usize, total: usize) {
        let vh = f32::from(self.code_bounds.borrow().size.height);
        if vh <= 0.0 {
            return;
        }
        let target = -(row as f32 * ROW_H) + (vh * 0.5 - ROW_H * 0.5);
        let content_h = total as f32 * ROW_H;
        let vmin = (vh - content_h).min(0.0);
        self.el_scroll_y = target.clamp(vmin, 0.0);
    }

    /// Self-paint: scroll so a logical File/Edit cursor sits at the viewport's
    /// vertical center. Soft-wrapped prose maps the logical cursor to a visual row.
    fn el_center_file_cursor(&mut self, cursor: Pos) {
        let vw = f32::from(self.code_bounds.borrow().size.width);
        if vw <= 0.0 {
            return;
        }
        if self.editing {
            let lines_ref = self.edit.lines();
            let lines = lines_ref.borrow();
            let layout = LineLayout::build(&lines, self.file_wrap_mode_for_viewport(vw));
            let (visual_row, _) = layout.logical_to_visual(cursor);
            self.el_center_visual_row(visual_row, layout.visual_count());
        } else if let QuickLookData::Text { lines, .. } = &self.file_data {
            let layout = LineLayout::build(lines, self.file_wrap_mode_for_viewport(vw));
            let (visual_row, _) = layout.logical_to_visual(cursor);
            self.el_center_visual_row(visual_row, layout.visual_count());
        }
    }

    /// Self-paint: scroll so logical `row` sits at the viewport's vertical center.
    /// Diff is one visual row per render row; File/Edit uses `el_center_file_cursor`.
    fn el_center_row(&mut self, row: usize) {
        let total = if self.tab == Tab::Diff {
            self.diff.len()
        } else if self.editing {
            self.edit.line_count()
        } else {
            match &self.file_data {
                QuickLookData::Text { lines, .. } => lines.len(),
                _ => 0,
            }
        };
        self.el_center_visual_row(row, total);
    }

    /// Self-paint: horizontally scroll so char `col` on `row` is comfortably in view —
    /// centered when it would otherwise be off-screen (long lines need this for find-
    /// jump). Leaves `hscroll_px` alone when the column is already within a margin.
    fn el_reveal_col(&mut self, row: usize, col: usize) {
        let vw = f32::from(self.code_bounds.borrow().size.width);
        if vw <= 0.0 {
            return;
        }
        if (self.editing || self.tab == Tab::File)
            && matches!(self.file_wrap_mode_for_viewport(vw), WrapMode::Word { .. })
        {
            self.hscroll_px = 0.0;
            return;
        }
        let char_w = self.char_w;
        let line = self.row_text(row).unwrap_or_default();
        let caret_x =
            CODE_GUTTER + crate::editor::geometry::prefix_cols(&line, col) as f32 * char_w;
        let max_disp = if self.editing {
            self.edit
                .lines()
                .borrow()
                .iter()
                .map(|l| disp_width(l))
                .max()
                .unwrap_or(0)
        } else {
            match &self.file_data {
                QuickLookData::Text { lines, .. } => {
                    lines.iter().map(|l| disp_width(l)).max().unwrap_or(0)
                }
                _ if self.tab == Tab::Diff => self
                    .diff
                    .iter()
                    .map(|l| disp_width(&l.text))
                    .max()
                    .unwrap_or(0),
                _ => 0,
            }
        };
        let content_w = (CODE_GUTTER + (max_disp as f32 + 1.0) * char_w).max(vw);
        let max_off = (content_w - vw).max(0.0);
        let margin = char_w * 4.0;
        // caret_x is in content coords (gutter + cols, pre-scroll); the visible band is
        // [hscroll_px, hscroll_px + vw]. Re-center only when it falls outside the margin.
        if caret_x < self.hscroll_px + margin || caret_x > self.hscroll_px + vw - margin {
            self.hscroll_px = (caret_x - vw * 0.5).clamp(0.0, max_off);
        }
    }

    fn jump_diff_hunk(&mut self, forward: bool) -> bool {
        if self.tab != Tab::Diff {
            return false;
        }
        let rows = diff_render_rows(&self.diff);
        let Some(row) = diff_hunk_jump_row(&rows, self.cursor.0, forward) else {
            return false;
        };
        self.sel_anchor = None;
        self.cursor = (row, 0);
        self.edit_drag = false;
        self.el_center_row(row);
        self.el_reveal_col(row, 0);
        true
    }

    fn goto_diff_target(&mut self, cx: &mut Context<Self>) -> bool {
        if self.tab != Tab::Diff {
            return false;
        }
        let rows = diff_render_rows(&self.diff);
        let Some(jump) =
            diff_file_jump_target_for_file_len(&rows, self.cursor.0, self.file_text_line_count())
        else {
            return false;
        };
        self.select_tab_now(Tab::File, cx);
        self.place_cursor(jump.row, 0, false);
        self.file_jump_highlight = Some(jump.highlight_row);
        self.edit_drag = false;
        self.scroll.scroll_to_item(jump.row, ScrollStrategy::Center);
        self.el_center_file_cursor((jump.row, 0));
        self.el_reveal_col(jump.row, 0);
        true
    }

    fn replace_current(&mut self) {
        if self.find_query.is_empty() {
            return;
        }
        if self
            .edit
            .replace_current(&self.find_query, &self.replace_query)
        {
            self.sync_edit_mirror();
        }
        self.find_next(true);
    }

    fn replace_all(&mut self) {
        if self.find_query.is_empty() {
            return;
        }
        let n = self.edit.replace_all(&self.find_query, &self.replace_query);
        if n > 0 {
            self.sync_edit_mirror();
        }
    }

    /// Find-bar named/control keys (the bar owns these). Returns `true` when the key
    /// was consumed. **Printable text is NOT handled here** — it returns `false` and
    /// the caller defers it to the IME input handler (→ `find_query`/`replace_query`),
    /// so 中文 composition works (typing it here + stop_propagation would make gpui
    /// skip `translate_message` and the IME could never compose — see 踩过的坑).
    fn find_key(&mut self, key: &str, shift: bool) -> bool {
        match key {
            "escape" => {
                self.find_open = false;
                self.ime_marked = None;
                true
            }
            "enter" => {
                if self.replacing && self.find_field_replace {
                    self.replace_current();
                } else {
                    self.find_next(!shift); // Enter = next, Shift+Enter = prev
                }
                true
            }
            "tab" => {
                if self.replacing {
                    self.find_field_replace = !self.find_field_replace;
                }
                true
            }
            "backspace" => {
                // No WM_CHAR is emitted for backspace, so handle it here (during IME
                // composition the keyfix subclass routes it to the IME instead).
                if self.find_field_replace {
                    self.replace_query.pop();
                } else {
                    self.find_query.pop();
                }
                true
            }
            _ => false,
        }
    }

    /// Append IME-committed / WM_CHAR text into the active find-bar field.
    fn find_input(&mut self, text: &str) {
        if self.find_field_replace {
            self.replace_query.push_str(text);
        } else {
            self.find_query.push_str(text);
        }
    }

    /// Keyboard while the overlay is focused. Edit mode → our editor; preview mode
    /// → nav (`↑↓` file / `⇥` tab / `Enter` edit / `Esc`·`Space` close).
    fn on_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = &ks.modifiers;
        let key = ks.key.as_str();
        if self.editing {
            // Ctrl/Cmd combos (editing): save / undo / redo / clipboard / find.
            if m.control || m.platform {
                let handled = match key {
                    "enter" if self.find_open && self.replacing => {
                        self.replace_all(); // Ctrl+Enter in replace = replace all
                        true
                    }
                    "s" if !m.alt => {
                        self.save(cx);
                        true
                    }
                    "z" if m.shift => {
                        self.redo();
                        true
                    }
                    "z" => {
                        self.undo();
                        true
                    }
                    "y" => {
                        self.redo();
                        true
                    }
                    "c" => {
                        self.copy(cx);
                        true
                    }
                    "x" => {
                        self.cut(cx);
                        true
                    }
                    "v" => {
                        self.paste(cx);
                        true
                    }
                    "a" => {
                        self.select_all();
                        true
                    }
                    "f" => {
                        self.open_find(false);
                        true
                    }
                    "h" => {
                        self.open_find(true);
                        true
                    }
                    _ => false,
                };
                if handled {
                    self.scroll
                        .scroll_to_item(self.cursor.0, ScrollStrategy::Center);
                    cx.stop_propagation();
                    cx.notify();
                }
                return;
            }
            if m.alt {
                return;
            }
            // The find bar captures named/control input while it's open; printable
            // text falls through (handled==false) to the IME input handler so it
            // composes into the query (中文) instead of being eaten here.
            if self.find_open {
                if self.find_key(key, m.shift) {
                    cx.stop_propagation();
                    cx.notify();
                }
                return;
            }
            let shift = m.shift;
            let mut handled = true;
            match key {
                "escape" => {
                    self.sel_anchor = None;
                    self.editing = false; // exit edit → preview (stay focused)
                    self.sync_preview_from_edit(); // reflect unsaved edits in the preview
                    self.hscroll_px = 0.0; // 预览不继承编辑态的横滚位置(否则停在很右=大片留白)
                    self.last_follow_cursor = None;
                }
                "backspace" => self.backspace(),
                "delete" => self.delete_forward(),
                "enter" => self.newline(),
                "tab" => self.indent(),
                // NOTE: `space` is intentionally NOT handled here — it falls to
                // `_ => handled = false` and defers to the IME input handler (the IME
                // commit key; a real WM_CHAR 0x20 → `replace_text_in_range` → typed when
                // not composing). `backspace` IS handled here (encoded): the platform
                // emits no WM_CHAR for it, so deferring would drop it (same as terminal).
                "left" | "right" | "up" | "down" | "home" | "end" => self.move_cursor(key, shift),
                "pageup" => self.page(-1, shift),
                "pagedown" => self.page(1, shift),
                // Plain text → **defer to the IME input handler** (registered while
                // editing & no find bar): English via WM_CHAR, 中文 via composition,
                // both land in `replace_text_in_range` → `type_char`. Typing it here
                // (+stop_propagation) would make gpui skip `translate_message` and the
                // IME could never start composing (the "编辑器无法输入中文" bug). Named
                // keys above stay handled here (they don't start composition).
                _ => handled = false,
            }
            if handled {
                if self.editing {
                    self.scroll
                        .scroll_to_item(self.cursor.0, ScrollStrategy::Center);
                }
                cx.stop_propagation();
                cx.notify();
            }
        } else {
            if m.control || m.alt || m.platform {
                // 预览态只读:放行 Ctrl+C(复制选中) / Ctrl+A(全选),其余控制键忽略。
                if m.control && !m.alt && !m.platform {
                    match key {
                        "c" => {
                            self.copy(cx);
                            cx.stop_propagation();
                        }
                        "a" => {
                            self.select_all();
                            cx.stop_propagation();
                            cx.notify();
                        }
                        _ => {}
                    }
                }
                return;
            }
            match key {
                "enter" if self.tab == Tab::Diff => {
                    self.goto_diff_target(cx);
                    cx.stop_propagation();
                    cx.notify();
                }
                "pageup" if self.tab == Tab::Diff => {
                    self.jump_diff_hunk(false);
                    cx.stop_propagation();
                    cx.notify();
                }
                "pagedown" if self.tab == Tab::Diff => {
                    self.jump_diff_hunk(true);
                    cx.stop_propagation();
                    cx.notify();
                }
                "up" => {
                    if self.request_leave(PendingLeave::Nav(-1), cx) == LeaveDecision::Continue {
                        cx.emit(QuickLookEvent::Nav(-1));
                    }
                    cx.stop_propagation();
                }
                "down" => {
                    if self.request_leave(PendingLeave::Nav(1), cx) == LeaveDecision::Continue {
                        cx.emit(QuickLookEvent::Nav(1));
                    }
                    cx.stop_propagation();
                }
                "tab" => {
                    let next_tab = if self.tab == Tab::File {
                        Tab::Diff
                    } else {
                        Tab::File
                    };
                    self.select_tab(next_tab, cx);
                    cx.stop_propagation();
                    cx.notify(); // diff 已缓存时 ensure_diff 不会 notify，需显式触发重渲染
                }
                "enter" if self.is_editable() => {
                    self.enter_edit();
                    self.scroll.scroll_to_item(0, ScrollStrategy::Top);
                    cx.stop_propagation();
                    cx.notify();
                }
                "escape" | "space" => {
                    cx.emit(QuickLookEvent::Close);
                    cx.stop_propagation();
                }
                _ => {}
            }
        }
    }
}

/// Display width of `s` in monospace columns: ASCII = 1, others (CJK etc.) ≈ 2.
/// Approximate — the code area's CJK fallback font isn't exactly 2× the ASCII
/// advance (踩过的坑: CJK 步进 ≠ cell_width), so alignment is exact for ASCII
/// data and near-aligned when cells contain wide chars. Pure → headless tested.
fn disp_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// Map a horizontal pixel offset (relative to the glyph start = after the gutter)
/// to the **char index under the pointer** (floor), accounting for CJK double-width
/// glyphs with the same 1/2-col model as [`disp_width`]. `char_w` = single-column
/// advance. A naive `rel/char_w` runs ~2× ahead on a CJK line (each 汉字 takes two
/// columns), which is why the drag selection desynced from the mouse on Chinese
/// text. Pure → headless tested.
fn hover_char_at_x(line: &str, rel_x: f32, char_w: f32) -> usize {
    if rel_x <= 0.0 || char_w <= 0.0 {
        return 0;
    }
    let target = rel_x / char_w; // distance in single-width columns
    let mut acc = 0.0f32;
    for (idx, c) in line.chars().enumerate() {
        let w = if c.is_ascii() { 1.0 } else { 2.0 };
        if target < acc + w {
            return idx;
        }
        acc += w;
    }
    line.chars().count()
}

/// Like [`hover_char_at_x`] but rounds to the nearest char **boundary** (caret
/// position) — past a glyph's midpoint the caret lands to its right. Used for
/// click-to-place-cursor; [`hover_char_at_x`] (floor) is used for drag extent.
fn caret_col_at_x(line: &str, rel_x: f32, char_w: f32) -> usize {
    if rel_x <= 0.0 || char_w <= 0.0 {
        return 0;
    }
    let target = rel_x / char_w;
    let mut acc = 0.0f32;
    for (idx, c) in line.chars().enumerate() {
        let w = if c.is_ascii() { 1.0 } else { 2.0 };
        if target < acc + w {
            return if target < acc + w / 2.0 { idx } else { idx + 1 };
        }
        acc += w;
    }
    line.chars().count()
}

/// Render spreadsheet cells (`.xlsx`/`.xls`/`.ods`) as a left-aligned monospace
/// table: each column padded to its widest cell's [`disp_width`], joined with
/// ` | `. The row's last cell isn't padded (no trailing-space churn). Rows may
/// have differing column counts. Pure → headless unit-tested. (审查㉑: replaced a
/// naive `join(" | ")` that left columns ragged — the two-pass alignment the log
/// claimed but that never actually landed in code.)
fn align_table(rows: &[Vec<String>]) -> Vec<String> {
    // Pass 1: per-column max display width.
    let mut widths: Vec<usize> = Vec::new();
    for row in rows {
        for (i, cell) in row.iter().enumerate() {
            let w = disp_width(cell);
            match widths.get_mut(i) {
                Some(cur) => *cur = (*cur).max(w),
                None => widths.push(w),
            }
        }
    }
    // Pass 2: pad every cell but the row's last to its column width, then join.
    rows.iter()
        .map(|row| {
            let last = row.len().saturating_sub(1);
            row.iter()
                .enumerate()
                .map(|(i, cell)| {
                    if i == last {
                        cell.clone()
                    } else {
                        let pad = widths[i].saturating_sub(disp_width(cell));
                        format!("{}{}", cell, " ".repeat(pad))
                    }
                })
                .collect::<Vec<_>>()
                .join(" | ")
        })
        .collect()
}

fn is_diff_metadata_line(line: &str) -> bool {
    line.starts_with("diff ")
        || line.starts_with("index ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("old mode")
        || line.starts_with("new mode")
        || line.starts_with("new file")
        || line.starts_with("deleted file")
        || line.starts_with("similarity")
        || line.starts_with("dissimilarity")
        || line.starts_with("rename ")
        || line.starts_with("copy ")
        || line.starts_with("Binary files")
        || line.starts_with('\\')
}

/// Parse `git diff --no-color` output into renderable lines (tracking new-file line
/// numbers from each hunk header). Pure → unit-testable headless.
fn parse_diff(text: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut new_no = 0u32;
    let mut in_hunk = false;
    // 0-based hunk counter — kept in lockstep with `remote_git::parse_file_diff`
    // (both skip the same header lines, count `@@` in order) so a clicked hunk
    // header maps back to the right `FileDiff` hunk for accept/reject.
    let mut hunk_no = 0usize;
    for line in text.lines() {
        if line.starts_with('\\') {
            continue;
        }
        if line.starts_with("diff ") {
            in_hunk = false;
            continue;
        }
        if !in_hunk && is_diff_metadata_line(line) {
            continue;
        }
        if let Some(rest) = line.strip_prefix("@@") {
            in_hunk = true;
            // @@ -a,b +c,d @@  → start tracking at c
            if let Some(plus) = rest.split('+').nth(1) {
                let num: String = plus.chars().take_while(|c| c.is_ascii_digit()).collect();
                new_no = num.parse().unwrap_or(new_no);
            }
            lines.push(DiffLine {
                kind: DiffKind::Hunk,
                new_no: None,
                text: line.to_string(),
                hunk_index: Some(hunk_no),
            });
            hunk_no += 1;
            continue;
        }
        let (kind, text) = match line.chars().next() {
            Some('+') => (DiffKind::Add, line[1..].to_string()),
            Some('-') => (DiffKind::Del, line[1..].to_string()),
            _ => (
                DiffKind::Ctx,
                line.strip_prefix(' ').unwrap_or(line).to_string(),
            ),
        };
        let no = if kind == DiffKind::Del {
            None
        } else {
            let n = new_no;
            new_no += 1;
            Some(n)
        };
        lines.push(DiffLine {
            kind,
            new_no: no,
            text,
            hunk_index: None,
        });
        if lines.len() >= MAX_LINES {
            break;
        }
    }
    lines
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

/// Fixed code-row height (≈ 12.5px × 1.62 line-height). Explicit so every row is
/// **uniform** — required by `uniform_list` (it measures row 0 and assumes the
/// rest match) and keeps the edit caret aligned regardless of which row it's on.
const ROW_H: f32 = 20.0;
/// Code-row left gutter width: line-number `.ln`(38) + margin(14) + marker `.mk`(14).
/// Single-sourced so the mouse→column hit-test and the IME caret-bounds agree.
const CODE_GUTTER: f32 = 66.0;

/// One code row (`.cl`): a faint line-number gutter (`.ln`, width 38, mr 14)
/// + a marker column (`.mk`, width 14) + the tinted source. Free fn so the
/// `'static` uniform_list closure can build rows without borrowing the view.
fn code_row(no: String, mark: &'static str, mark_col: Rgba, spans: Vec<gpui::Div>) -> gpui::Div {
    div()
        .flex()
        .flex_row()
        .items_center()
        .h(px(ROW_H)) // uniform height (see ROW_H)
        .pr(px(12.)) // mockup .cl padding-right 12
        // mockup .cl .ln:width 38 · faint #474E72 · 11px · 右对齐 · margin-right 14
        .child(
            div()
                .w(px(38.))
                .flex_none()
                .mr(px(14.))
                .text_right()
                .text_size(px(crate::style::FS_MICRO))
                .text_color(gpui::rgb(crate::style::T3)) // faint(无主题 token,字面量)
                .child(SharedString::from(no)),
        )
        // mockup .cl .mk:width 14 居中
        .child(
            div()
                .w(px(14.))
                .flex_none()
                .text_center()
                .text_color(mark_col)
                .child(mark),
        )
        .child(div().flex().flex_row().children(spans))
}

/// The selected text for the normalized range `[s, e)` (joins lines with `\n`).
fn selected_text(buf: &[String], s: (usize, usize), e: (usize, usize)) -> String {
    if s.0 == e.0 {
        return buf
            .get(s.0)
            .map(|l| l.chars().skip(s.1).take(e.1 - s.1).collect())
            .unwrap_or_default();
    }
    let mut out: String = buf[s.0].chars().skip(s.1).collect();
    for line in buf.iter().take(e.0).skip(s.0 + 1) {
        out.push('\n');
        out.push_str(line);
    }
    out.push('\n');
    out.push_str(&buf[e.0].chars().take(e.1).collect::<String>());
    out
}

/// All matches of `query` (single-line) in the buffer, as `(start, end)` char
/// positions, in document order.
fn all_matches(buf: &[String], query: &str) -> Vec<((usize, usize), (usize, usize))> {
    let mut out = Vec::new();
    if query.is_empty() {
        return out;
    }
    for (r, line) in buf.iter().enumerate() {
        // 增量累加 byte→char 偏移(从上个匹配处续数,而非每次从行首重数),使一行内多次
        // 命中总体 O(line) 而非 O(line×命中数)(审查⑬)。match_indices 按字节升序返回。
        let (mut last_byte, mut last_char) = (0usize, 0usize);
        for (byte_idx, matched_str) in line.match_indices(query) {
            last_char += line[last_byte..byte_idx].chars().count();
            last_byte = byte_idx;
            let len_chars = matched_str.chars().count();
            out.push(((r, last_char), (r, last_char + len_chars)));
        }
    }
    out
}

/// Replace every occurrence of `query` with `repl` (per line). Returns the count.
#[cfg(test)]
fn replace_all_in(buf: &mut Vec<String>, query: &str, repl: &str) -> usize {
    if query.is_empty() {
        return 0;
    }
    let mut count = 0;
    for line in buf.iter_mut() {
        let occ = line.matches(query).count();
        if occ > 0 {
            *line = line.replace(query, repl);
            count += occ;
        }
    }
    count
}

/// The edit caret: a thin insertion bar (style pass can switch to the prototype's
/// 7px block). Sits inline at the cursor column.
/// The edit caret as a terminal-style **solid block**. On a glyph it's drawn as an
/// inverse block inline in `edit_row_cached` (block bg + glyph repainted in the panel
/// bg color); at end-of-line (no glyph) this standalone block is pushed instead.
fn cursor_block(config: &Loaded) -> gpui::Div {
    div()
        .w(px(7.5))
        .h(px(caret_visual_height(ROW_H, CODE_FS)))
        .flex_none()
        .rounded(px(QUICKLOOK_CARET_RADIUS))
        .bg(col(config.theme.ui.foreground))
}

/// Per-char tint for `line` (expands `highlight()` runs to one tint per char).
fn tints_per_char(line: &str) -> Vec<Tint> {
    let mut tints = Vec::with_capacity(line.chars().count());
    for (text, t) in highlight(line) {
        for _ in text.chars() {
            tints.push(t);
        }
    }
    tints
}

fn edit_row_cached(
    config: &Loaded,
    chars: &[char],
    tints: &[Tint],
    i: usize,
    cursor: (usize, usize),
    sel: Option<((usize, usize), (usize, usize))>,
    char_w: f32,
) -> gpui::Div {
    let n = chars.len();
    if chars.len() > LONG_LINE_BYTES {
        let fg = col(config.theme.ui.foreground);
        let cc = if i == cursor.0 {
            cursor.1.min(n)
        } else {
            n + 1
        };
        let before: String = chars.iter().take(cc.min(n)).collect();
        let after: String = chars.iter().skip(cc.min(n)).collect();
        let mut row = div()
            .flex()
            .flex_row()
            .items_center()
            .child(div().text_color(fg).child(SharedString::from(before)));
        if i == cursor.0 {
            row = row.child(cursor_block(config));
        }
        row = row.child(div().text_color(fg).child(SharedString::from(after)));
        return code_row(
            format!("{}", i + 1),
            "",
            col(config.theme.ui.muted),
            vec![row],
        );
    }

    let tint_at = |k: usize| *tints.get(k).unwrap_or(&Tint::Plain);

    let (sel_s, sel_e) = match sel {
        Some((s, e)) if i >= s.0 && i <= e.0 => {
            let ss = if i == s.0 { s.1 } else { 0 };
            let ee = if i == e.0 { e.1 } else { n };
            (ss, ee)
        }
        _ => (0, 0),
    };
    let selected = |k: usize| k >= sel_s && k < sel_e;
    let caret_col = (i == cursor.0).then(|| cursor.1.min(n));
    let sel_bg = cola(config.theme.ui.accent, 0.22);

    // **固定单元格渲染**(同终端 row_runs):每个 ASCII 串成一个 `w(列数×char_w)` 定宽格、
    // 每个 CJK 字各成 `w(2×char_w)` 定宽格(`.overflow_hidden()` 裁字形到格内)。这样列↔像素
    // 严格等于 `disp_width×char_w` —— 光标 x / 鼠标 hit-test / 选区 / 横向内容宽全部精确对齐,
    // 不再因 CJK 实际字形步进 ≠ 2×char_w 而漂移(中文行光标乱飘/选区不跟手的根因)。
    //
    // 反相块光标(终端式):磷光填充 + 反相墨字(design `.cur`),就地反色成实心块,
    // 瞬时、精确(固定单元格下块 = 该字符格)、随字符列宽(中文 2 列宽、英文 1 列细)。
    let caret_bg = col(config.theme.ui.accent); // 磷光:唯一生命色
    let caret_fg = gpui::rgb(crate::style::PH_INK);
    let cell = |text: String, cols: f32| {
        div()
            .flex_none()
            .w(px(cols * char_w))
            .overflow_hidden()
            .child(SharedString::from(text))
    };
    let mut spans: Vec<gpui::Div> = Vec::new();
    let mut k = 0;
    while k < n {
        if caret_col == Some(k) {
            let c = chars[k];
            let cols = if c.is_ascii() { 1.0 } else { 2.0 };
            spans.push(cell(c.to_string(), cols).bg(caret_bg).text_color(caret_fg));
            k += 1;
            continue;
        }
        let c = chars[k];
        let s0 = selected(k);
        if c.is_ascii() {
            // ASCII 同 tint·同选区·非光标 连续合并成一个定宽格
            let t0 = tint_at(k);
            let mut j = k + 1;
            while j < n
                && chars[j].is_ascii()
                && caret_col != Some(j)
                && tint_at(j) == t0
                && selected(j) == s0
            {
                j += 1;
            }
            let text: String = chars[k..j].iter().collect();
            let mut sp = cell(text, (j - k) as f32).text_color(tint_color(config, t0));
            if s0 {
                sp = sp.bg(sel_bg);
            }
            spans.push(sp);
            k = j;
        } else {
            // CJK / 宽字符:独立 2 列定宽格
            let mut sp = cell(c.to_string(), 2.0).text_color(tint_color(config, tint_at(k)));
            if s0 {
                sp = sp.bg(sel_bg);
            }
            spans.push(sp);
            k += 1;
        }
    }
    if caret_col == Some(n) {
        spans.push(cursor_block(config));
    }
    let content = div().flex().flex_row().items_center().children(spans);
    code_row(
        format!("{}", i + 1),
        "",
        col(config.theme.ui.muted),
        vec![content],
    )
}

/// A line past this byte length is rendered as a single plain span (skip
/// tokenization) — bounds per-row work for minified / long-attribute lines.
const LONG_LINE_BYTES: usize = 2000;
/// Hard cap on token spans per row. Each span is a `div` that gpui lays out AND
/// shapes separately during paint; with font fallback (e.g. CJK in a code font),
/// many small spans per row × visible rows is what froze the HTML preview. The
/// remainder past the cap is collapsed into one plain span (no content lost).
const MAX_SPANS: usize = 48;

/// `(text, tint)` runs for one line, **bounded** (pure → unit-testable): long
/// lines → one plain span; otherwise `highlight()` tokens are **coalesced by tint**
/// (consecutive same-tint tokens merge — a markup line drops from ~30 tokens to a
/// handful) and capped at [`MAX_SPANS`] (tail collapsed, nothing dropped). Fewer
/// runs → fewer `div`s/shaped text runs per row → paint stays cheap (the HTML-
/// preview freeze was paint-time shaping of many small spans, see 踩过的坑).
fn coalesce_spans(line: &str) -> Vec<(smol_str::SmolStr, Tint)> {
    if line.len() > LONG_LINE_BYTES {
        return vec![(smol_str::SmolStr::new(line), Tint::Plain)];
    }
    let mut merged: Vec<(String, Tint)> = Vec::new();
    for (text, tint) in highlight(line) {
        match merged.last_mut() {
            Some((s, lt)) if *lt == tint => s.push_str(&text),
            _ => merged.push((text.to_string(), tint)),
        }
    }
    let mut out = Vec::with_capacity(merged.len().min(MAX_SPANS));
    if merged.len() > MAX_SPANS {
        for (s, t) in merged.drain(..MAX_SPANS - 1) {
            out.push((smol_str::SmolStr::new(s), t));
        }
        let tail: String = merged.into_iter().map(|(s, _)| s).collect();
        out.push((smol_str::SmolStr::new(tail), Tint::Plain));
    } else {
        for (s, t) in merged {
            out.push((smol_str::SmolStr::new(s), t));
        }
    }
    out
}

/// Document row under a viewport y for the self-paint File preview, clamped to the
/// document. Uses the stashed canvas bounds + vertical scroll (`el_scroll_y` ≤ 0).
fn el_row_at(ql: &QuickLook, pos_y: f32, total: usize) -> usize {
    let top = f32::from(ql.code_bounds.borrow().origin.y);
    let r = ((pos_y - top - ql.el_scroll_y) / ROW_H).floor();
    (r.max(0.0) as usize).min(total.saturating_sub(1))
}

/// TnE-09: read-only **self-painted** File preview (env-gated `TN_QL_ELEMENT`).
/// Draws line numbers + syntax-tinted text + a horizontal scrollbar via the shared
/// `editor::{geometry,prepaint}` model. Each glyph is positioned on the 1/2-col
/// grid (CJK = 2 cols) — ASCII runs shaped together, each CJK char placed at its
/// 2-col step — so columns stay aligned exactly like the `uniform_list` fixed-cell
/// path (no CJK drift, see 踩过的坑). `scroll_y` ≤ 0 (vertical), `hscroll` ≥ 0.
#[allow(clippy::too_many_arguments)]
fn paint_file_preview(
    bounds: Bounds<Pixels>,
    lines: &[String],
    wrap_path: &std::path::Path,
    char_w: f32,
    scroll_y: f32,
    hscroll: f32,
    sel: Option<((usize, usize), (usize, usize))>,
    matches: &[((usize, usize), (usize, usize))],
    file_jump_highlight: Option<usize>,
    editing: bool,
    caret: (usize, usize),
    ime: Option<&str>,
    motion: MotionSnapshot,
    config: &Loaded,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    use crate::editor::geometry::Metrics;
    use crate::editor::prepaint::{gutter_label, row_top};

    let m = Metrics::new(char_w);
    let vw = f32::from(bounds.size.width);
    let vh = f32::from(bounds.size.height);
    if vw <= 0.0 || vh <= 0.0 {
        return;
    }
    let file_layout = quicklook_file_layout(lines, wrap_path, vw, vh, scroll_y, hscroll, char_w);
    let layout = &file_layout.layout;
    let pre = &file_layout.pre;
    let sel_segments = sel
        .map(|(s, e)| layout.range_segments(TextRange::new(s, e)))
        .unwrap_or_default();
    let fs = px(CODE_FS);
    let line_h = px(ROW_H);
    let font = crate::style::with_cjk(&config.font().family);
    let ui = &config.theme.ui;
    let gutter_color: Hsla = col(ui.muted).into();
    let sel_bg: Hsla = cola(ui.accent, 0.22).into();
    // Find highlight (every occurrence) — a distinct hue from the selection so the
    // current match (selection, accent) stands out among the rest (accent_alt). When
    // a find is active each occurrence gets a clearly visible fill + a thin accent_alt
    // outline so it reads as "highlighted" even on busy syntax-colored lines.
    let match_bg: Hsla = cola(ui.accent_alt, 0.38).into();
    let match_border: Hsla = cola(ui.accent_alt, 0.85).into();
    // Reverse-block caret(磷光填充 + 反相墨字,design `.cur`)+ IME preedit colors.
    let caret_bg: Hsla = col(ui.accent).into();
    let caret_fg: Hsla = gpui::rgb(crate::style::PH_INK).into();
    let accent: Hsla = col(ui.accent).into();
    let jump_bg: Hsla = cola(ui.accent_alt, 0.16).into();
    let jump_bar: Hsla = cola(ui.accent_alt, 0.90).into();
    let view_bg: Hsla = gpui::rgb(CODE_BG).into();
    let left = f32::from(bounds.origin.x);
    let top = f32::from(bounds.origin.y);
    let gutter = m.gutter;

    let mk_run = |text: &str, color: Hsla| TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };

    // 当前行高亮(SHEET 03 板 B `.cl.cur-line`):编辑态光标行整行 L4 底,
    // 行号转磷光(见下方 gutter 着色)。画在查找命中/选区/文字之下。
    if editing {
        let cur_line_bg: Hsla = gpui::rgb(crate::style::L4).into();
        for visual_row in layout.visual_range_of_row(caret.0) {
            if !pre.rows.contains(&visual_row) {
                continue;
            }
            let y = px(top + row_top(visual_row, scroll_y, ROW_H));
            window.paint_quad(fill(
                Bounds {
                    origin: point(bounds.origin.x, y),
                    size: size(bounds.size.width, line_h),
                },
                cur_line_bg,
            ));
        }
    }

    if let Some(row) = file_jump_highlight {
        for visual_row in layout.visual_range_of_row(row) {
            if !pre.rows.contains(&visual_row) {
                continue;
            }
            let y = px(top + row_top(visual_row, scroll_y, ROW_H));
            window.paint_quad(fill(
                Bounds {
                    origin: point(bounds.origin.x, y),
                    size: size(bounds.size.width, line_h),
                },
                jump_bg,
            ));
            window.paint_quad(fill(
                Bounds {
                    origin: point(px(left + gutter - 3.0), y),
                    size: size(px(2.0), line_h),
                },
                jump_bar,
            ));
        }
    }

    // Text content, clipped to the area right of the gutter so horizontally-scrolled
    // glyphs never bleed into the gutter / line numbers.
    let text_area = Bounds {
        origin: point(px(left + gutter), bounds.origin.y),
        size: size(px((vw - gutter).max(0.0)), bounds.size.height),
    };
    window.with_content_mask(Some(ContentMask { bounds: text_area }), |window| {
        for visual_row in pre.rows.clone() {
            let Some(visual) = layout.visual_line(visual_row) else {
                continue;
            };
            let Some(logical_line) = lines.get(visual.logical_row) else {
                continue;
            };
            let line = visual_line_text(logical_line, visual);
            let y = px(top + row_top(visual_row, scroll_y, ROW_H));
            // Find highlights (突出显示): every query occurrence on this row, a clearly
            // visible fill + thin outline so it reads as highlighted on busy lines.
            // Painted under the text + selection (matches are single-line: s.0 == e.0).
            for (ms, me) in matches.iter().filter(|(s, _)| s.0 == visual.logical_row) {
                let start = ms.1.max(visual.char_start);
                let end = me.1.min(visual.char_end);
                if start < end {
                    let xs = left
                        + pre.content_x
                        + visual_prefix_cols(logical_line, visual, start - visual.char_start)
                            as f32
                            * char_w;
                    let xe = left
                        + pre.content_x
                        + visual_prefix_cols(logical_line, visual, end - visual.char_start) as f32
                            * char_w;
                    window.paint_quad(gpui::quad(
                        Bounds {
                            origin: point(px(xs), y),
                            size: size(px((xe - xs).max(0.0)), line_h),
                        },
                        px(2.0),
                        match_bg,
                        px(1.0),
                        match_border,
                        gpui::BorderStyle::Solid,
                    ));
                }
            }
            // Selection background (read-only preview drag-select). Per-row char span
            // [ss, ee) → display-col x range, painted under the text. Mirrors
            // `edit_row_cached`: full line for interior rows, accent @ 0.22.
            for &(_, ss, ee) in sel_segments.iter().filter(|(idx, _, _)| *idx == visual_row) {
                let xs = left
                    + pre.content_x
                    + visual_prefix_cols(logical_line, visual, ss) as f32 * char_w;
                let xe = left
                    + pre.content_x
                    + visual_prefix_cols(logical_line, visual, ee) as f32 * char_w;
                if xe > xs {
                    let rect = Bounds {
                        origin: point(px(xs), y),
                        size: size(px((xe - xs).max(0.0)), line_h),
                    };
                    window.paint_quad(fill(rect, sel_bg));
                }
            }
            let mut cols = 0.0f32; // display columns consumed so far on this row
            for (text, tint) in coalesce_spans(&line) {
                let color: Hsla = tint_color(config, tint).into();
                let chars: Vec<char> = text.chars().collect();
                let mut k = 0;
                while k < chars.len() {
                    if chars[k].is_ascii() {
                        let mut j = k + 1;
                        while j < chars.len() && chars[j].is_ascii() {
                            j += 1;
                        }
                        let seg: String = chars[k..j].iter().collect();
                        let x = px(left + pre.content_x + cols * char_w);
                        let run = mk_run(&seg, color);
                        let shaped = window
                            .text_system()
                            .shape_line(seg.into(), fs, &[run], None);
                        let _ = shaped.paint(point(x, y), line_h, window, cx);
                        cols += (j - k) as f32;
                        k = j;
                    } else {
                        let s = chars[k].to_string();
                        let x = px(left + pre.content_x + cols * char_w);
                        let run = mk_run(&s, color);
                        let shaped = window.text_system().shape_line(s.into(), fs, &[run], None);
                        let _ = shaped.paint(point(x, y), line_h, window, cx);
                        cols += 2.0;
                        k += 1;
                    }
                }
            }
            if let Some(settle) = motion.settle.filter(|s| s.row == visual.logical_row) {
                if settle.col >= visual.char_start && settle.col < visual.char_end {
                    let local_col = settle.col - visual.char_start;
                    let x = px(left
                        + pre.content_x
                        + visual_prefix_cols(logical_line, visual, local_col) as f32 * char_w);
                    let color: Hsla = cola(ui.accent, settle.alpha).into();
                    let s = settle.ch.to_string();
                    let run = mk_run(&s, color);
                    let shaped = window.text_system().shape_line(s.into(), fs, &[run], None);
                    let _ = shaped.paint(point(x, y), line_h, window, cx);
                }
            }
            // Reverse-block caret + IME preedit (editing only) on the caret row.
            if editing
                && caret.0 == visual.logical_row
                && caret.1 >= visual.char_start
                && caret.1 <= visual.char_end
            {
                let cc = caret.1.min(visual.char_end) - visual.char_start;
                let cx0 = left
                    + pre.content_x
                    + visual_prefix_cols(logical_line, visual, cc) as f32 * char_w;
                if let Some(pre_s) = ime.filter(|s| !s.is_empty()) {
                    // Composing: draw the preedit over the caret, covering following
                    // text so it stays readable, with an accent underline.
                    let pw = (disp_width(pre_s) as f32 * char_w).max(char_w);
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(px(cx0), y),
                            size: size(px(pw), line_h),
                        },
                        view_bg,
                    ));
                    let run = mk_run(pre_s, col(ui.foreground).into());
                    let shaped =
                        window
                            .text_system()
                            .shape_line(pre_s.to_string().into(), fs, &[run], None);
                    let _ = shaped.paint(point(px(cx0), y), line_h, window, cx);
                    window.paint_quad(fill(
                        Bounds {
                            origin: point(px(cx0), px(f32::from(y) + ROW_H - 2.0)),
                            size: size(px(pw), px(1.5)),
                        },
                        accent,
                    ));
                } else {
                    // Solid reverse block: foreground fill, char redrawn in bg color.
                    let ch = line.chars().nth(cc);
                    let cell_cols = ch
                        .map(|c| if c.is_ascii() { 1.0 } else { 2.0 })
                        .unwrap_or(1.0);
                    let cell_w = cell_cols * char_w;
                    let visual = caret_visual_rect(
                        cx0,
                        f32::from(y),
                        cell_w,
                        ROW_H,
                        CODE_FS,
                        motion.caret_scale_x,
                        motion.caret_scale_y,
                        motion.caret_dx,
                        motion.caret_dy,
                    );
                    window.paint_quad(gpui::quad(
                        Bounds {
                            origin: point(px(visual.x), px(visual.y)),
                            size: size(px(visual.width), px(visual.height)),
                        },
                        px(visual.radius),
                        caret_bg,
                        px(0.0),
                        rgba(0x00000000),
                        gpui::BorderStyle::Solid,
                    ));
                    if let Some(c) = ch {
                        let run = mk_run(&c.to_string(), caret_fg);
                        let shaped =
                            window
                                .text_system()
                                .shape_line(c.to_string().into(), fs, &[run], None);
                        let _ = shaped.paint(point(px(visual.x), y), line_h, window, cx);
                    }
                }
            }
        }
    });

    // Line numbers, right-aligned in the gutter (content is masked out of the gutter
    // region above, so they never overlap scrolled text).
    for visual_row in pre.rows.clone() {
        let Some(visual) = layout.visual_line(visual_row) else {
            continue;
        };
        let y = px(top + row_top(visual_row, scroll_y, ROW_H));
        let label = if visual.char_start == 0 {
            gutter_label(visual.logical_row)
        } else {
            String::new()
        };
        // 编辑态光标行行号 = 磷光(SHEET 03 `.cur-line .ln{color:var(--ph)}`)。
        let ln_color = if editing && visual.logical_row == caret.0 {
            accent
        } else {
            gutter_color
        };
        let run = mk_run(&label, ln_color);
        let shaped = window
            .text_system()
            .shape_line(label.into(), fs, &[run], None);
        let w = f32::from(shaped.width);
        let x = px(left + gutter - 14.0 - w); // 14px right pad (matches CODE_GUTTER mk)
        let _ = shaped.paint(point(x, y), line_h, window, cx);
    }

    // Horizontal scrollbar thumb (thin, near the bottom edge).
    if let Some(thumb) = pre.thumb {
        let thumb_color: Hsla = cola(ui.muted, 0.45).into();
        let rect = Bounds {
            origin: point(px(left + thumb.thumb_x), px(top + vh - 5.0)),
            size: size(px(thumb.thumb_w), px(3.0)),
        };
        window.paint_quad(fill(rect, thumb_color));
    }
}

fn paint_diff_preview(
    bounds: Bounds<Pixels>,
    rows: &[DiffRenderRow],
    char_w: f32,
    scroll_y: f32,
    hscroll: f32,
    sel: Option<(Pos, Pos)>,
    config: &Loaded,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    use crate::editor::prepaint::{prepaint_readonly, row_top};

    let vw = f32::from(bounds.size.width);
    let vh = f32::from(bounds.size.height);
    if vw <= 0.0 || vh <= 0.0 {
        return;
    }

    let lines: Vec<String> = rows.iter().map(|r| r.text.clone()).collect();
    let m = crate::editor::geometry::Metrics::new(char_w);
    let pre = prepaint_readonly(&lines, vw, vh, scroll_y, hscroll, m);
    let fs = px(CODE_FS);
    let line_h = px(ROW_H);
    let font = crate::style::with_cjk(&config.font().family);
    let th = &config.theme;
    let gutter_color: Hsla = gpui::rgb(crate::style::T3).into();
    let sel_bg: Hsla = cola(th.ui.accent, 0.22).into();
    let left = f32::from(bounds.origin.x);
    let top = f32::from(bounds.origin.y);
    let gutter = m.gutter;

    let mk_run = |text: &str, color: Hsla| TextRun {
        len: text.len(),
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    };
    let row_style = |kind: crate::editor::DiffRowKind| -> (Option<Hsla>, Hsla, Hsla) {
        match kind {
            crate::editor::DiffRowKind::Addition => (
                Some(cola(th.ansi.green, 0.09).into()),
                col(th.ansi.green).into(),
                col(th.ui.foreground).into(),
            ),
            crate::editor::DiffRowKind::Deletion => (
                Some(cola(th.ansi.red, 0.09).into()),
                col(th.ansi.red).into(),
                col(th.ui.foreground).into(),
            ),
            crate::editor::DiffRowKind::HunkHeader => (
                None,
                col(th.ui.accent_alt).into(),
                col(th.ui.accent_alt).into(),
            ),
            crate::editor::DiffRowKind::Context => {
                (None, col(th.ui.muted).into(), col(th.ui.foreground).into())
            }
            crate::editor::DiffRowKind::Meta => {
                (None, col(th.ui.muted).into(), col(th.ui.muted).into())
            }
        }
    };

    let text_area = Bounds {
        origin: point(px(left + gutter), bounds.origin.y),
        size: size(px((vw - gutter).max(0.0)), bounds.size.height),
    };
    window.with_content_mask(Some(ContentMask { bounds: text_area }), |window| {
        for row_ix in pre.rows.clone() {
            let Some(row) = rows.get(row_ix) else {
                continue;
            };
            let y = px(top + row_top(row_ix, scroll_y, ROW_H));
            let (bg, _marker_col, text_col) = row_style(row.kind);
            if let Some(bg) = bg {
                window.paint_quad(fill(
                    Bounds {
                        origin: point(bounds.origin.x, y),
                        size: size(bounds.size.width, line_h),
                    },
                    bg,
                ));
            }
            if let Some(range) = sel {
                if let Some((ss, ee)) = diff_selection_span_cols(rows, row_ix, range) {
                    let xs = left + pre.content_x + ss as f32 * char_w;
                    let xe = left + pre.content_x + ee as f32 * char_w;
                    if xe > xs {
                        window.paint_quad(fill(
                            Bounds {
                                origin: point(px(xs), y),
                                size: size(px(xe - xs), line_h),
                            },
                            sel_bg,
                        ));
                    }
                }
            }

            let mut cols = 0.0f32;
            let chars: Vec<char> = row.text.chars().collect();
            let mut k = 0;
            while k < chars.len() {
                if chars[k].is_ascii() {
                    let mut j = k + 1;
                    while j < chars.len() && chars[j].is_ascii() {
                        j += 1;
                    }
                    let seg: String = chars[k..j].iter().collect();
                    let x = px(left + pre.content_x + cols * char_w);
                    let run = mk_run(&seg, text_col);
                    let shaped = window
                        .text_system()
                        .shape_line(seg.into(), fs, &[run], None);
                    let _ = shaped.paint(point(x, y), line_h, window, cx);
                    cols += (j - k) as f32;
                    k = j;
                } else {
                    let seg = chars[k].to_string();
                    let x = px(left + pre.content_x + cols * char_w);
                    let run = mk_run(&seg, text_col);
                    let shaped = window
                        .text_system()
                        .shape_line(seg.into(), fs, &[run], None);
                    let _ = shaped.paint(point(x, y), line_h, window, cx);
                    cols += 2.0;
                    k += 1;
                }
            }
        }
    });

    for row_ix in pre.rows.clone() {
        let Some(row) = rows.get(row_ix) else {
            continue;
        };
        let y = px(top + row_top(row_ix, scroll_y, ROW_H));
        let (_bg, marker_col, _text_col) = row_style(row.kind);

        if let Some(no) = row.new_no {
            let label = no.to_string();
            let run = mk_run(&label, gutter_color);
            let shaped = window
                .text_system()
                .shape_line(label.into(), fs, &[run], None);
            let w = f32::from(shaped.width);
            let x = px(left + gutter - 14.0 - w);
            let _ = shaped.paint(point(x, y), line_h, window, cx);
        }

        let mark = row.gutter();
        if mark != ' ' {
            let label = mark.to_string();
            let run = mk_run(&label, marker_col);
            let shaped = window
                .text_system()
                .shape_line(label.into(), fs, &[run], None);
            let w = f32::from(shaped.width);
            let x = px(left + gutter - 14.0 + (14.0 - w) * 0.5);
            let _ = shaped.paint(point(x, y), line_h, window, cx);
        }
    }

    if let Some(thumb) = pre.thumb {
        let thumb_color: Hsla = cola(th.ui.muted, 0.45).into();
        let rect = Bounds {
            origin: point(px(left + thumb.thumb_x), px(top + vh - 5.0)),
            size: size(px(thumb.thumb_w), px(3.0)),
        };
        window.paint_quad(fill(rect, thumb_color));
    }
}

fn file_row_cached(
    config: &Loaded,
    cached_spans: &[(smol_str::SmolStr, Tint)],
    i: usize,
    char_w: f32,
) -> gpui::Div {
    // 同 edit_row_cached:固定单元格(ASCII 串定宽 / CJK 单字 2 列定宽),使列↔像素精确 →
    // 预览态拖选 hit-test / 横向滚动内容宽一致(否则 CJK 行选区/横滚也会漂)。
    let mut spans: Vec<gpui::Div> = Vec::new();
    for (text, tint) in cached_spans {
        let color = tint_color(config, *tint);
        let chars: Vec<char> = text.chars().collect();
        let n = chars.len();
        let mut k = 0;
        while k < n {
            if chars[k].is_ascii() {
                let mut j = k + 1;
                while j < n && chars[j].is_ascii() {
                    j += 1;
                }
                let t: String = chars[k..j].iter().collect();
                spans.push(
                    div()
                        .flex_none()
                        .w(px((j - k) as f32 * char_w))
                        .overflow_hidden()
                        .text_color(color)
                        .child(SharedString::from(t)),
                );
                k = j;
            } else {
                spans.push(
                    div()
                        .flex_none()
                        .w(px(2.0 * char_w))
                        .overflow_hidden()
                        .text_color(color)
                        .child(SharedString::from(chars[k].to_string())),
                );
                k += 1;
            }
        }
    }
    code_row(format!("{}", i + 1), "", col(config.theme.ui.muted), spans)
}

/// Build one Diff-tab row `i` (hunk/context/add/del with `+`/`-` styling).
fn diff_row(config: &Loaded, diff: &[DiffLine], i: usize) -> gpui::Div {
    let th = &config.theme;
    let d = &diff[i];
    let (bg, mark, mark_col, txt_col) = match d.kind {
        // mockup .cl.add/.del:bg=绿/红 @ .09;.ln/.mk 同色;正文不暗化(del 不 muted)
        DiffKind::Add => (
            cola(th.ansi.green, 0.09),
            "+",
            col(th.ansi.green),
            col(th.ui.foreground),
        ),
        DiffKind::Del => (
            cola(th.ansi.red, 0.09),
            "-",
            col(th.ansi.red),
            col(th.ui.foreground),
        ),
        DiffKind::Hunk => (
            rgba(0x00000000),
            " ",
            col(th.ui.accent_alt),
            col(th.ui.accent_alt),
        ),
        DiffKind::Ctx => (
            rgba(0x00000000),
            " ",
            col(th.ui.muted),
            col(th.ui.foreground),
        ),
    };
    let no = d.new_no.map(|n| format!("{n}")).unwrap_or_default();
    let spans = vec![div()
        .text_color(txt_col)
        .child(SharedString::from(d.text.clone()))];
    code_row(no, mark, mark_col, spans).bg(bg)
}

impl gpui::EventEmitter<QuickLookEvent> for QuickLook {}

/// IME / text input for the in-house editor (fixes "文件编辑界面无法切换中文输入").
/// Mirrors the terminal's approach: the only addressable "text" is the in-progress
/// composition (`ime_marked`); a commit (中文 result) is inserted at the cursor via
/// `type_char` (selection-aware + undo). Registered (in paint) **only while editing**.
/// ASCII still flows through `on_key` (which stops propagation → no duplicate WM_CHAR).
impl EntityInputHandler for QuickLook {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        adjusted: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let units: Vec<u16> = self
            .ime_marked
            .as_deref()
            .unwrap_or("")
            .encode_utf16()
            .collect();
        let start = range.start.min(units.len());
        let end = range.end.min(units.len());
        *adjusted = Some(start..end);
        Some(String::from_utf16_lossy(&units[start..end]))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = self
            .ime_marked
            .as_deref()
            .map(|s| s.encode_utf16().count())
            .unwrap_or(0);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.ime_marked
            .as_deref()
            .map(|s| 0..s.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ime_marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // IME commit (中文) or a printable WM_CHAR. While the find bar is open the
        // text belongs to the query field (中文 search), else it inserts at the cursor
        // like typed text. Empty `text` = composition cancel. (Backspace is encoded
        // in `on_key`, never routed here.)
        if !text.is_empty() {
            if self.find_open {
                self.find_input(text);
            } else {
                self.type_char(text);
                self.schedule_motion_cleanup(cx);
                if should_center_after_text_commit(self.el_render) {
                    self.scroll
                        .scroll_to_item(self.cursor.0, ScrollStrategy::Center);
                }
            }
        }
        self.ime_marked = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ime_marked = (!new_text.is_empty()).then(|| new_text.to_string());
        self.snap_caret_motion();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // 查找/替换栏开着时,IME 合成文本进的是查找框 → 候选框必须贴查找框,而非
        // 代码区光标(否则中文搜索时候选框飘到正文,与系统输入框心智不符)。find_bar
        // 里的占位 canvas 每帧把激活字段输入框的窗口坐标写进 `find_field_bounds`;
        // 取到则把候选框对齐到该框左缘、底缘(候选窗自然落在框下方)。
        if self.find_open {
            if let Some(b) = self.find_field_bounds.get() {
                return Some(Bounds {
                    origin: point(b.origin.x, b.origin.y + b.size.height),
                    size: size(px(self.char_w.max(1.0)), px(ROW_H)),
                });
            }
        }
        let lines = self.edit.lines();
        let lines = lines.borrow();
        let caret = quicklook_caret_paint_rect(
            &lines,
            self.file_wrap_path(),
            self.cursor,
            f32::from(element_bounds.size.width),
            f32::from(element_bounds.size.height),
            self.el_scroll_y,
            self.hscroll_px,
            self.char_w,
        )
        .unwrap_or(CaretPaintRect {
            x: CODE_GUTTER,
            y: 0.0,
            width: self.char_w.max(1.0),
            height: ROW_H,
        });
        Some(Bounds {
            origin: point(
                px(f32::from(element_bounds.origin.x) + caret.x),
                px(f32::from(element_bounds.origin.y) + caret.y + caret.height),
            ),
            size: size(px(caret.width.max(1.0)), px(caret.height)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for QuickLook {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) -> impl IntoElement {
        let config = self.config.clone();
        let file_cache = self.file_highlight_cache.clone();
        let _ui = &config.theme.ui;

        // Grab focus on first render after open (focusing in open() doesn't land —
        // the overlay isn't rendered yet; see 踩过的坑).
        if self.needs_focus {
            self.focus_handle.focus(window);
            self.needs_focus = false;
        }
        // Keep a freshly-moved caret visible in the self-paint editor (de-bounced).
        // Done here, before any immutable `&self` borrows below, so the `&mut self`
        // call doesn't conflict (it self-guards on `editing`).
        if self.el_render {
            self.el_follow_caret();
        }
        let ui = self.config.theme.ui;
        let ansi = self.config.theme.ansi;

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
        // SHEET 03 `.segs .seg`:分段开关 — on = L4 + 磷光字 + ph-dim 边。
        let pill = |label: &'static str, on: bool, to: Tab| {
            div()
                .px(px(13.))
                .py(px(2.))
                .rounded(px(3.))
                .font_family(SharedString::from(self.config.font().family.clone()))
                .text_size(px(crate::style::FS_MICRO))
                .text_color(if on {
                    gpui::rgb(crate::style::PH)
                } else {
                    gpui::rgb(crate::style::T2)
                })
                .when(on, |d| {
                    // 1px 边吃掉 1px padding,保证开关切换不抖(`.seg.on` 同款补偿)
                    d.bg(gpui::rgb(crate::style::L4))
                        .border_1()
                        .border_color(rgba(crate::style::PH_DIM))
                        .px(px(12.))
                        .py(px(1.))
                })
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e, _w, cx| {
                        this.select_tab(to, cx);
                    }),
                )
                .child(label)
        };
        let tabset = div()
            .flex()
            .flex_row()
            .gap(px(2.))
            .p(px(2.))
            .rounded(px(crate::style::R_CARD)) // `.segs`:L1 + 1px h1 + r4
            .bg(gpui::rgb(crate::style::L1))
            .border_1()
            .border_color(rgba(crate::style::H1))
            // SHEET 03:File 在左(默认预览态)· Diff 在右,与原型分段开关顺序一致。
            .child(pill("File", self.tab == Tab::File, Tab::File))
            .child(pill("Diff", self.tab == Tab::Diff, Tab::Diff));

        // 头部小 chip(SHEET 03:RS / UTF-8 · LF / 218 L 同款):mono 10 t1 + h1 边。
        let meta_chip = |text: String| {
            div()
                .px(px(8.))
                .py(px(2.))
                .rounded(px(crate::style::R_CHIP))
                .border_1()
                .border_color(rgba(crate::style::H1))
                .font_family(SharedString::from(self.config.font().family.clone()))
                .text_size(px(crate::style::FS_MICRO))
                .text_color(gpui::rgb(crate::style::T1))
                .child(SharedString::from(text))
        };
        // 文件元信息(预览态 chips):扩展名 / 编码·换行 / 行数(差异总结 3-13)。
        let ext_label = name
            .rsplit_once('.')
            .map(|(_, e)| e.to_ascii_uppercase())
            .filter(|e| !e.is_empty() && e.len() <= 8);
        let format_label = self.text_format.map(|f| {
            let enc = match f.encoding {
                TextEncoding::Utf8 => "UTF-8",
                TextEncoding::Utf8Bom => "UTF-8 BOM",
                TextEncoding::Utf16Le => "UTF-16 LE",
                TextEncoding::Utf16Be => "UTF-16 BE",
                TextEncoding::Gbk => "GBK",
            };
            let eol = match f.newline {
                NewlineStyle::Lf => "LF",
                NewlineStyle::Crlf => "CRLF",
            };
            format!("{enc} · {eol}")
        });
        let line_count_label = match &self.file_data {
            QuickLookData::Text { lines, .. } => Some(format!("{} L", lines.len())),
            _ => None,
        };
        let show_meta = !self.editing && self.tab == Tab::File;

        // SHEET 03 float-head:高 38 · L4 顶面 · 底 1px h1 · mono 12。
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(9.))
            .h(px(38.))
            .px(px(14.))
            .flex_none()
            .font_family(UI_SANS) // header chrome = sans (code stays mono)
            .text_size(px(11.5))
            .bg(col(ui.palette_selected)) // L4(不透明,契约 1)
            .border_b(px(1.))
            .border_color(rgba(crate::style::H1))
            // SHEET 03:头部左标 = 磷光 ▪(差异总结 3-11,不再用文件图标)
            .child(
                div()
                    .font_family(SharedString::from(self.config.font().family.clone()))
                    .text_color(gpui::rgb(crate::style::PH))
                    .child("▪"),
            )
            // `.crumb`:层级 t3 + ›,文件名 t0 bold(差异总结 3-12)
            .child(
                div()
                    .font_family(SharedString::from(self.config.font().family.clone()))
                    .text_color(gpui::rgb(crate::style::T3))
                    .child(SharedString::from(dir.replace('/', " › "))),
            )
            .child(
                div()
                    .font_family(SharedString::from(self.config.font().family.clone()))
                    .text_color(gpui::rgb(crate::style::T0))
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(SharedString::from(name.clone())),
            )
            // 编辑态:`EDIT` 磷光 chip 紧跟路径(SHEET 03 板 B;差异总结 3-14 —
            // 不再用 claude 橙「编辑中」)。
            .when(self.editing, |d| {
                d.child(
                    div()
                        .px(px(8.))
                        .py(px(2.))
                        .rounded(px(crate::style::R_CHIP))
                        .border_1()
                        .border_color(rgba(crate::style::PH_DIM))
                        .bg(rgba(crate::style::PH_SOFT))
                        .font_family(SharedString::from(self.config.font().family.clone()))
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(gpui::rgb(crate::style::PH))
                        .child("EDIT"),
                )
            })
            .child(div().flex_1())
            .when(show_meta, |mut d| {
                if let Some(e) = ext_label.clone() {
                    d = d.child(meta_chip(e));
                }
                if let Some(f) = format_label.clone() {
                    d = d.child(meta_chip(f));
                }
                if let Some(l) = line_count_label.clone() {
                    d = d.child(meta_chip(l));
                }
                d
            })
            // Diff 态:+N / −N 统计 chips(SHEET 03 板 C;差异总结 3-15)。
            .when(self.tab == Tab::Diff && !self.diff.is_empty(), |d| {
                let adds = self.diff.iter().filter(|r| r.kind == DiffKind::Add).count();
                let dels = self.diff.iter().filter(|r| r.kind == DiffKind::Del).count();
                let stat = |text: String, color: tn_config::Color| {
                    div()
                        .px(px(8.))
                        .py(px(2.))
                        .rounded(px(crate::style::R_CHIP))
                        .border_1()
                        .border_color(cola(color, 0.30))
                        .bg(cola(color, 0.12))
                        .font_family(SharedString::from(self.config.font().family.clone()))
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(col(color))
                        .child(SharedString::from(text))
                };
                d.child(stat(format!("+{adds}"), ansi.green))
                    .child(stat(format!("-{dels}"), ansi.red))
            })
            // mockup .vh .by:编辑态 = 「编辑中(●)」,预览态有未提交改动 = 「已改动」(claude)
            .when(
                self.editing
                    || !self.diff.is_empty()
                    || self.save_in_flight
                    || self.save_conflict.is_some()
                    || self.save_error.is_some(),
                |d| {
                    // 语义色按磷光表(差异总结 3-14):未保存/已改动 = warn 琥珀,
                    // 冲突/失败 = err;编辑态本身由左侧 EDIT 磷光 chip 表达。
                    let badge = if self.save_in_flight {
                        Some(("保存中", ansi.yellow))
                    } else if self.save_conflict.is_some() {
                        Some(("保存冲突", ansi.red))
                    } else if self.save_error.is_some() {
                        Some(("保存失败", ansi.red))
                    } else if self.editing {
                        self.dirty.then_some(("● 未保存", ansi.yellow))
                    } else {
                        Some(("已改动", ansi.yellow))
                    };
                    d.when_some(badge, |d, (label, color)| {
                        d.child(
                            div()
                                .px(px(8.))
                                .py(px(2.))
                                .rounded(px(crate::style::R_CHIP))
                                .border_1()
                                .border_color(cola(color, 0.30))
                                .bg(cola(color, 0.12))
                                .font_family(SharedString::from(self.config.font().family.clone()))
                                .text_size(px(crate::style::FS_MICRO))
                                .text_color(col(color))
                                .child(label),
                        )
                    })
                },
            )
            .child(tabset)
            // 显式关闭入口(SHEET 03):header 右端 ✕,不让关闭只依赖 footer Esc。
            .child(
                div()
                    .ml(px(2.))
                    .w(px(22.))
                    .h(px(22.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(crate::style::R_CHIP))
                    .text_color(gpui::rgb(crate::style::T2))
                    .hover(|s| s.bg(rgba(crate::style::ERR_SOFT)).text_color(col(ansi.red)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|_this, _e, _w, cx| {
                            cx.stop_propagation();
                            cx.emit(QuickLookEvent::Close);
                        }),
                    )
                    .child(icon("close", 13., ui.muted)),
            );

        // ── .code body:**虚拟化**列表(uniform_list 只渲染可见行 → 大文件不卡)。
        //    编辑态从 buf 渲染(高亮 + 选区 + 光标);预览态从 file_lines / diff 渲染。──
        let (lines, truncated) = match &self.file_data {
            QuickLookData::Text { lines, truncated } => (lines.clone(), *truncated),
            _ => (Arc::new(Vec::new()), false),
        };
        let line_count = lines.len();
        let buf = self.edit.lines();
        let config = self.config.clone();
        let _ui = &config.theme.ui;
        let diff = self.diff.clone();
        let editing = self.editing;
        let cursor = self.cursor;
        let sel = self.sel_range();
        let tab = self.tab;
        // Remote Diff tab → per-hunk accept/reject buttons on each `@@` row.
        let is_remote_diff = self.remote_diff_file.is_some();
        let hunk_busy = self.hunk_busy;
        let file_jump_highlight = (!editing && tab == Tab::File)
            .then_some(self.file_jump_highlight)
            .flatten();
        // Per-row click → place cursor (mouse). The row index `i` is known here, so
        // we only map x → column (gutter + measured char width); no scroll-offset
        // math needed. Capture a weak handle (the 'static closure can't borrow self).
        let entity = cx.entity().downgrade();
        let char_w = self.char_w;
        let canvas_bounds = self.code_bounds.clone(); // for the capturing canvas
        let row_bounds = self.code_bounds.clone(); // for the per-row click handler
        const GUTTER: f32 = CODE_GUTTER; // ln(38) + mr(14) + mk(14)
                                         // IME/text input handler captures (registered in the canvas paint below) —
                                         // active whenever editing. The handler routes composed/typed text to the
                                         // buffer, or to the find query when the find bar is open (中文 search),
                                         // so 中文 composition works in both (see `replace_text_in_range`).
        let ime_focus = self.focus_handle.clone();
        let ime_entity = cx.entity();
        let ime_active = editing;
        let count = if editing {
            buf.borrow().len()
        } else if tab == Tab::Diff {
            self.diff.len()
        } else {
            line_count
        };
        // ── body condition chain ────────────────────────────────────────────
        let mut body = div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .flex()
            .flex_col()
            .overflow_hidden()
            .pt(px(8.));

        if self.loading_state == LoadingState::Loading {
            // 不渲染占位符
        } else if let QuickLookData::Binary { size } = &self.file_data {
            let size_str = human_size(*size);
            body = body.child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .text_color(col(ui.muted))
                    .child(icon("file", 48., ui.muted))
                    .child(div().mt_4().text_size(px(crate::style::FS_LABEL)).child("无法预览此文件"))
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(crate::style::FS_CAPTION))
                            .child(format!("二进制文件或超过大小限制 ({size_str})")),
                    ),
            );
        } else if let QuickLookData::Pdf { pages, page_count } = &self.file_data {
            let pages = pages.clone();
            let page_count = *page_count;
            body = body.child(
                div()
                    .flex_1()
                    .overflow_hidden()
                    .bg(gpui::rgb(CODE_BG))
                    .child(
                        uniform_list(
                            "pdf_scroll_container",
                            page_count,
                            move |range, _window, _cx| {
                                let pages_lock = pages.lock().ok();
                                range
                                    .map(|i| {
                                        // 暗 gutter(同外层 viewer 色),页面图居中铺满高度 → 竖向不留白
                                        // (修「开头/页间大段白空」),横向余量是暗 gutter(非刺眼白边);
                                        // 去掉 .p_4() 白边框。未解码占位也用暗色,无白闪。
                                        if let Some(lock) = &pages_lock {
                                            if let Some(img) = &lock[i] {
                                                let img_source =
                                                    gpui::ImageSource::Render(img.clone());
                                                return div()
                                                    .w_full()
                                                    .h(px(1400.)) // 固定行高让 uniform_list 计算(只 measure row 0)
                                                    .bg(gpui::rgb(CODE_BG))
                                                    .flex()
                                                    .justify_center()
                                                    .items_center()
                                                    .child(
                                                        gpui::img(img_source)
                                                            .max_w_full()
                                                            .max_h_full()
                                                            .w_auto()
                                                            .h_auto()
                                                            .object_fit(gpui::ObjectFit::ScaleDown),
                                                    );
                                            }
                                        }
                                        div().w_full().h(px(1400.)).bg(gpui::rgb(CODE_BG))
                                    })
                                    .collect::<Vec<_>>()
                            },
                        )
                        .track_scroll(self.scroll.clone())
                        .w_full()
                        .h_full(),
                    ),
            );
        } else if let QuickLookData::Image { img } = &self.file_data {
            let img_source = gpui::ImageSource::Render(img.clone());
            body = body.child(
                div()
                    .w_full()
                    .h_full()
                    .flex()
                    .justify_center()
                    .items_center()
                    .bg(gpui::rgb(CODE_BG)) // 暗色背景
                    // Contain + 适度内边距:图片按比例**铺满**预览区(只在一轴留暗边),不再
                    // 自然小尺寸居中留大片空白(修「四周留白很多」)。
                    .p(px(10.))
                    .child(
                        gpui::img(img_source)
                            .size_full()
                            .object_fit(gpui::ObjectFit::Contain),
                    ),
            );
        } else if self.tab == Tab::Diff && self.diff_loading {
            // 不渲染占位符
        } else if !editing && tab == Tab::Diff && self.diff.is_empty() {
            body = body.child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .px(px(14.))
                    .py(px(8.))
                    .text_color(col(ui.muted))
                    .child("无改动 · git working tree clean"),
            );
        } else if !editing
            && tab == Tab::File
            && matches!(self.file_data, QuickLookData::Text { .. })
            && is_markdown_path(self.path.as_deref())
        {
            // Markdown 预览态:直接渲染排版(Enter 进编辑切回自绘编辑器,Esc 回预览)。
            body = body.child(markdown_view(&self.config, &lines));
        } else if should_self_paint_diff(self.el_render, editing, tab, &self.file_data) {
            // TnE-13: Diff tab now uses the same self-painted canvas/prepaint path
            // as File preview. `TN_QL_LEGACY=1` still falls through to the old list.
            body = body.child(self.diff_element(cx));
        } else if self.el_render
            && tab == Tab::File
            && matches!(self.file_data, QuickLookData::Text { .. })
        {
            // TnE-09/10/11: self-painted File preview + editor (default on; set
            // TN_QL_LEGACY=1 to force the old `uniform_list` path). Read-only preview
            // and the live editor both render here; the old list branch below is the
            // emergency fallback for File/Diff. caret-follow ran at the top of
            // render (before the immutable borrows here).
            body = body.child(self.file_element(cx));
        } else {
            let _sel_anchor = sel.as_ref().map(|s| s.0);
            // Longest line's display width (cols), computed BEFORE the list closure
            // moves `lines`/`diff`. Drives the horizontal-scroll content width below.
            // `disp_width` counts CJK as 2 cols so wide-char lines aren't under-sized.
            let max_cols = if editing {
                buf.borrow()
                    .iter()
                    .map(|l| disp_width(l))
                    .max()
                    .unwrap_or(0)
            } else if tab == Tab::Diff {
                diff.iter().map(|d| disp_width(&d.text)).max().unwrap_or(0)
            } else {
                lines.iter().map(|l| disp_width(l)).max().unwrap_or(0)
            };
            // Caret x within content space (editing only), computed here while `buf` is
            // still available (the list closure below moves it). 固定单元格下精确 =
            // GUTTER + disp_width(前缀)×char_w;驱动横向 caret-follow(打字到右缘自动滚)。
            let caret_content_x = if editing {
                let pre = buf
                    .borrow()
                    .get(cursor.0)
                    .map(|l| disp_width(&l.chars().take(cursor.1).collect::<String>()))
                    .unwrap_or(0);
                GUTTER + pre as f32 * char_w
            } else {
                0.0
            };
            let list = uniform_list("ql-code", count, move |range, _window, _cx| {
                let mut f_cache = file_cache.borrow_mut();
                range
                    .map(|i| {
                        if editing {
                            // 编辑态不缓存高亮:可见行仅 ~30,每帧直接算够快;按行号缓存
                            // 会在删除/撤销后显示陈旧内容(审查⑫)。直接从 buf[i] 算最稳。
                            let bref = buf.borrow();
                            let line = &bref[i];
                            let chars: Vec<char> = line.chars().collect();
                            let tints = tints_per_char(line);
                            let row =
                                edit_row_cached(&config, &chars, &tints, i, cursor, sel, char_w);
                            let entity = entity.clone();
                            let entity_mv = entity.clone();
                            let bounds = row_bounds.clone();
                            let bounds_mv = row_bounds.clone();
                            row.on_mouse_down(
                                MouseButton::Left,
                                move |ev: &MouseDownEvent, _w, app| {
                                    let left = f32::from(bounds.borrow().origin.x);
                                    let rel = f32::from(ev.position.x) - left - GUTTER;
                                    let shift = ev.modifiers.shift;
                                    let _ = entity.update(app, |this, cx| {
                                        // CJK 双宽:列由行内容步进算(见 caret_col_at_x),
                                        // 不能 rel/char_w 当单宽(汉字行会跑 ~2× 偏)。
                                        let col = this
                                            .row_text(i)
                                            .map(|l| {
                                                caret_col_at_x(&l, rel + this.hscroll_px, char_w)
                                            })
                                            .unwrap_or(0);
                                        this.place_cursor(i, col, shift);
                                        this.edit_drag = true; // 进入拖选
                                        cx.notify();
                                    });
                                    app.stop_propagation();
                                },
                            )
                            .on_mouse_move(
                                move |ev: &MouseMoveEvent, _w, app| {
                                    // 左键拖动 → 扩选。每行各自的 move(行号 i 已知)绕开
                                    // uniform_list 不可读的纵向 scroll offset。
                                    if ev.pressed_button != Some(MouseButton::Left) {
                                        return;
                                    }
                                    let left = f32::from(bounds_mv.borrow().origin.x);
                                    let rel = f32::from(ev.position.x) - left - GUTTER;
                                    let _ = entity_mv.update(app, |this, cx| {
                                        if !this.edit_drag {
                                            return;
                                        }
                                        // 鼠标悬停的字符索引(floor,CJK 双宽感知)。拖选要**包含**
                                        // 它,使「实心块拖到哪、选区就到哪(含该字符)」——相对锚点
                                        // 向右拖让 caret 落该字符右侧(+1)、向左落其左侧。(选区半开
                                        // [a,c),向右不 +1 会漏掉光标处那个字符 = 你见的「选到块之前」。)
                                        let hover = this
                                            .row_text(i)
                                            .map(|l| {
                                                hover_char_at_x(&l, rel + this.hscroll_px, char_w)
                                            })
                                            .unwrap_or(0);
                                        let anchor = this.sel_anchor.unwrap_or(this.cursor);
                                        let col = if (i, hover) >= anchor {
                                            hover + 1
                                        } else {
                                            hover
                                        };
                                        this.place_cursor(i, col, true);
                                        cx.notify();
                                    });
                                },
                            )
                        } else if tab == Tab::File {
                            let line = &lines[i];
                            // 选区触及本行 → 按 char 渲染(复用 edit_row_cached,caret=(MAX,MAX)
                            // 永不命中任何行 = 预览态不画光标)以显选区底色;否则用缓存的 tint
                            // spans(快)。预览态拖选 + Ctrl+C 复制,只读不改。
                            let mut row = if sel.map_or(false, |(s, e)| i >= s.0 && i <= e.0) {
                                let chars: Vec<char> = line.chars().collect();
                                let tints = tints_per_char(line);
                                edit_row_cached(
                                    &config,
                                    &chars,
                                    &tints,
                                    i,
                                    (usize::MAX, usize::MAX),
                                    sel,
                                    char_w,
                                )
                            } else {
                                let spans =
                                    f_cache.entry(i).or_insert_with(|| coalesce_spans(line));
                                file_row_cached(&config, spans, i, char_w)
                            };
                            if file_jump_highlight == Some(i) {
                                row = row
                                    .bg(cola(config.theme.ui.accent_alt, 0.16))
                                    .border_l(px(2.))
                                    .border_color(cola(config.theme.ui.accent_alt, 0.90));
                            }
                            let entity = entity.clone();
                            let entity_mv = entity.clone();
                            let bounds = row_bounds.clone();
                            let bounds_mv = row_bounds.clone();
                            row.on_mouse_down(
                                MouseButton::Left,
                                move |ev: &MouseDownEvent, _w, app| {
                                    let left = f32::from(bounds.borrow().origin.x);
                                    let rel = f32::from(ev.position.x) - left - GUTTER;
                                    let shift = ev.modifiers.shift;
                                    let _ = entity.update(app, |this, cx| {
                                        let col = this
                                            .row_text(i)
                                            .map(|l| {
                                                caret_col_at_x(&l, rel + this.hscroll_px, char_w)
                                            })
                                            .unwrap_or(0);
                                        this.place_cursor(i, col, shift);
                                        this.edit_drag = true;
                                        cx.notify();
                                    });
                                    app.stop_propagation();
                                },
                            )
                            .on_mouse_move(
                                move |ev: &MouseMoveEvent, _w, app| {
                                    if ev.pressed_button != Some(MouseButton::Left) {
                                        return;
                                    }
                                    let left = f32::from(bounds_mv.borrow().origin.x);
                                    let rel = f32::from(ev.position.x) - left - GUTTER;
                                    let _ = entity_mv.update(app, |this, cx| {
                                        if !this.edit_drag {
                                            return;
                                        }
                                        let hover = this
                                            .row_text(i)
                                            .map(|l| {
                                                hover_char_at_x(&l, rel + this.hscroll_px, char_w)
                                            })
                                            .unwrap_or(0);
                                        let anchor = this.sel_anchor.unwrap_or(this.cursor);
                                        let col = if (i, hover) >= anchor {
                                            hover + 1
                                        } else {
                                            hover
                                        };
                                        this.place_cursor(i, col, true);
                                        cx.notify();
                                    });
                                },
                            )
                        } else {
                            let base = diff_row(&config, &diff, i);
                            // Remote Diff tab: each `@@` hunk header gets accept /
                            // reject buttons (`git apply --cached` / `--reverse`).
                            let hunk_idx =
                                if is_remote_diff && matches!(diff[i].kind, DiffKind::Hunk) {
                                    diff[i].hunk_index
                                } else {
                                    None
                                };
                            if let Some(hunk_index) = hunk_idx {
                                let th = &config.theme;
                                let hbtn = |label: &'static str, c: tn_config::Color| {
                                    div()
                                        .px(px(7.))
                                        .py(px(1.))
                                        .rounded(px(6.))
                                        .flex_none()
                                        .text_size(px(crate::style::FS_MICRO))
                                        .font_weight(gpui::FontWeight(640.))
                                        .text_color(if hunk_busy {
                                            col(th.ui.muted)
                                        } else {
                                            col(c)
                                        })
                                        .bg(if hunk_busy {
                                            gpui::rgb(crate::style::L2)
                                        } else {
                                            cola(c, 0.12)
                                        })
                                        .border_1()
                                        .border_color(cola(c, 0.30))
                                        .child(label)
                                };
                                let entity_a = entity.clone();
                                let entity_r = entity.clone();
                                base.child(div().flex_1().min_w(px(0.)))
                                    .child(hbtn("接受", th.ansi.green).on_mouse_down(
                                        MouseButton::Left,
                                        move |_e: &MouseDownEvent, _w, app| {
                                            if hunk_busy {
                                                return;
                                            }
                                            let _ = entity_a.update(app, |this, cx| {
                                                this.apply_hunk(
                                                    hunk_index,
                                                    crate::remote_git::HunkAction::Apply,
                                                    cx,
                                                );
                                            });
                                            app.stop_propagation();
                                        },
                                    ))
                                    .child(hbtn("拒绝", th.ansi.red).on_mouse_down(
                                        MouseButton::Left,
                                        move |_e: &MouseDownEvent, _w, app| {
                                            if hunk_busy {
                                                return;
                                            }
                                            let _ = entity_r.update(app, |this, cx| {
                                                this.apply_hunk(
                                                    hunk_index,
                                                    crate::remote_git::HunkAction::Reject,
                                                    cx,
                                                );
                                            });
                                            app.stop_propagation();
                                        },
                                    ))
                                    .gap(px(6.))
                            } else {
                                base
                            }
                        }
                    })
                    .collect::<Vec<_>>()
            })
            .track_scroll(self.scroll.clone());
            // 横向滚动(修复:预览/编辑长行被截断、看不全)。把列表撑到「最长行宽」
            // (gutter + 列数×char_w + 余量)放裁剪窗、`.absolute().left(-h_off)` 平移,
            // 底部一条可拖 thumb 改 `hscroll_px`。**不用** overflow.x scroll(会让滚轮横纵同滚
            // =斜移) → 滚轮于是只到 uniform_list 纵向,横向只靠拖 thumb。编辑/预览同走此路:
            // hit-test 已 `rel + hscroll_px` 补偏移(CJK 双宽感知),编辑态再叠 caret-follow。
            let code_area = {
                let thumb_bg = cola(self.config.theme.ui.muted, 0.45);
                let (viewport_w, track_left) = {
                    let b = self.code_bounds.borrow();
                    (f32::from(b.size.width), f32::from(b.origin.x))
                };
                // 内容宽 = 最长行宽(gutter + 列×char_w + 1 列留给行尾光标),但**至少撑满视口**:
                // 短内容时正好填满、右侧不留白;只有确有超视口长行才 > 视口 → 才可横向滚(否则
                // max_off=0、无滚动条)。修「太宽留白 + 短内容也出横条」。
                let content_w = (GUTTER + (max_cols as f32 + 1.0) * char_w).max(viewport_w);
                self.hscroll_content_w = content_w; // for the drag handler (no lines there)
                let max_off = (content_w - viewport_w).max(0.0);
                // 编辑态 caret-follow(横向滚 + 纵向 scroll_to_item)——**只在光标变化时
                // 各跟随一次**(去抖 `last_follow_cursor`)。否则每帧都跑会把用户的手动滚动
                // 立刻拉回:横向 = 横滚条「拖不动」、纵向 = **鼠标滚轮失效**(滚出视口就被
                // scroll_to_item 拽回光标行)。手动滚动时 cursor 不变 → 不 follow → 保留。
                // 打字/移动光标时 cursor 变 → 横纵各跟随一次,保证光标可见。
                if editing && viewport_w > 0.0 && self.last_follow_cursor != Some(cursor) {
                    self.last_follow_cursor = Some(cursor);
                    // 横向
                    let margin = char_w * 4.0;
                    let mut off = self.hscroll_px;
                    if caret_content_x < off + margin {
                        off = (caret_content_x - margin).max(0.0);
                    } else if caret_content_x > off + viewport_w - margin {
                        off = caret_content_x - viewport_w + margin;
                    }
                    self.hscroll_px = off.clamp(0.0, max_off);
                    // 纵向:光标行滚出视口才拉回(此刻是因光标移动,非用户滚轮)
                    let viewport_h = f32::from(self.code_bounds.borrow().size.height);
                    let offset_y = f32::from(self.scroll.0.borrow().base_handle.offset().y);
                    if viewport_h > 0.0 {
                        let first = (-offset_y / ROW_H).floor().max(0.0) as usize;
                        let rows = (viewport_h / ROW_H).floor() as usize;
                        let last = first + rows.saturating_sub(1);
                        if cursor.0 < first || cursor.0 > last {
                            self.scroll
                                .scroll_to_item(cursor.0, gpui::ScrollStrategy::Center);
                        }
                    }
                }
                let h_off = self.hscroll_px.clamp(0.0, max_off);

                let mut area = div()
                    .flex_1()
                    .min_h(px(0.))
                    .relative()
                    .overflow_hidden()
                    .child(
                        list.w(px(content_w))
                            .h_full()
                            .absolute()
                            .top_0()
                            .left(px(-h_off)),
                    );
                // 横向滚动条:仅当**确有**超视口内容(>8px)才显;可见条细(3px)、暗(muted .45)、
                // 左右内缘各留 6px,不贴边、不抢视线。命中区做**高 14px**(透明、承接拖拽),里头细
                // bar 靠底显示 → 视觉仍纤细、但好抓(修「可交互区域太小」)。
                if max_off > 8.0 && viewport_w > 0.0 {
                    let inset = 6.0_f32;
                    let track_w = (viewport_w - inset * 2.0).max(1.0);
                    let thumb_w = (track_w / content_w * track_w).clamp(36.0, track_w);
                    let thumb_x = inset + h_off / max_off * (track_w - thumb_w);
                    let ent = cx.entity().downgrade();
                    area = area.child(
                        div()
                            .absolute()
                            .bottom_0()
                            .left(px(thumb_x))
                            .w(px(thumb_w))
                            .h(px(14.)) // 加高的透明命中区
                            .flex()
                            .items_end()
                            .on_mouse_down(
                                MouseButton::Left,
                                move |ev: &MouseDownEvent, _w, app| {
                                    let grab = f32::from(ev.position.x) - (track_left + thumb_x);
                                    let _ = ent.update(app, |this, cx| {
                                        this.hscroll_drag = Some(grab);
                                        cx.notify();
                                    });
                                    app.stop_propagation();
                                },
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(3.))
                                    .mb(px(2.))
                                    .rounded(px(2.))
                                    .bg(thumb_bg), // 细可见 bar,贴底
                            ),
                    );
                }
                area.into_any_element()
            };
            body = body
                .child(
                    canvas(
                        move |bounds, _w, _cx| *canvas_bounds.borrow_mut() = bounds,
                        move |bounds, _s, window, cx| {
                            if ime_active {
                                window.handle_input(
                                    &ime_focus,
                                    ElementInputHandler::new(bounds, ime_entity.clone()),
                                    cx,
                                );
                            }
                        },
                    )
                    .absolute()
                    .size_full(),
                )
                .child(code_area)
                .when(!editing && truncated && tab == Tab::File, |d| {
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
            // `.kbd`:mono 10 t1 · L2 + 1px h1(底 2px)· r3
            crate::style::kbd(label, SharedString::from(self.config.font().family.clone()))
        };
        // float-foot:高 30 · 顶 1px h1 · mono 10 t2(SHEET 03/06)
        let footer_base = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(7.))
            .h(px(30.))
            .px(px(14.))
            .flex_none()
            .font_family(SharedString::from(self.config.font().family.clone()))
            .text_size(px(crate::style::FS_MICRO))
            .text_color(gpui::rgb(crate::style::T2))
            .border_t_1()
            .border_color(rgba(crate::style::H1));
        // RAIL 读数(`.tag` mono 10 600 t2):从活动栏打开时 footer 右侧显示
        // 「RAIL · n/N」,标明 ↑↓ 在本次改动文件间导航(SHEET 03 footer)。
        let rail_tag = self.rail_pos.map(|(i, n)| {
            div()
                .font_family(SharedString::from(self.config.font().family.clone()))
                .text_size(px(crate::style::FS_MICRO))
                .font_weight(gpui::FontWeight(600.))
                .text_color(gpui::rgb(crate::style::T2))
                .child(SharedString::from(format!("RAIL · {}/{}", i + 1, n)))
        });
        let footer = if self.editing {
            // 编辑态:Ctrl+S 保存 · Ctrl+F 查找 · Esc 退出编辑 [sp] 选择/复制/撤销 ·
            // 右端 LN·COL 磷光读数(SHEET 03 板 B `.tag ph`;差异总结 3-18)。
            footer_base
                .child(kcap("Ctrl+S"))
                .child("保存 ·")
                .child(kcap("Ctrl+F"))
                .child("查找 ·")
                .child(kcap("Esc"))
                .child("退出编辑")
                .child(div().flex_1())
                .child(kcap("⇧方向"))
                .child("选择 ·")
                .child(kcap("Ctrl+C/V"))
                .child("复制粘贴 ·")
                .child(kcap("Ctrl+Z"))
                .child("撤销")
                .child(div().w(px(6.)))
                .child(
                    div()
                        .font_family(SharedString::from(self.config.font().family.clone()))
                        .text_size(px(crate::style::FS_MICRO))
                        .font_weight(gpui::FontWeight(600.))
                        .text_color(gpui::rgb(crate::style::PH))
                        .child(SharedString::from(format!(
                            "LN {} · COL {}",
                            self.cursor.0 + 1,
                            self.cursor.1 + 1
                        ))),
                )
        } else if self.tab == Tab::Diff {
            footer_base
                .child(kcap("↑↓"))
                .child("改动文件 ·")
                .child(kcap("PgUp/Dn"))
                .child("跳 hunk ·")
                .child(kcap("Enter"))
                .child("到 File 行")
                .child(div().flex_1())
                .child(kcap("Ctrl+C/A"))
                .child("复制/全选 ·")
                .child(kcap("Esc"))
                .child("关闭")
                .when_some(rail_tag, |d, t| d.child(div().w(px(10.))).child(t))
        } else if self.is_editable() {
            // 预览态(可编辑文本文件,SHEET 03 footer):↑↓ 改动文件 · ⇥ Diff · Enter 编辑 ·
            // Esc 关闭 · [flex] · RAIL · n/N
            footer_base
                .child(kcap("↑↓"))
                .child("改动文件 ·")
                .child(kcap("⇥"))
                .child("Diff ·")
                .child(kcap("Enter"))
                .child("编辑 ·")
                .child(kcap("Esc"))
                .child("关闭")
                .child(div().flex_1())
                .when_some(rail_tag, |d, t| d.child(t))
        } else {
            // 预览态(PDF / 图片 / Office / 二进制 — 只读):↑↓ 改动文件 · ⇥ 切 File · Esc 关闭
            footer_base
                .child(kcap("↑↓"))
                .child("改动文件 ·")
                .child(kcap("⇥"))
                .child("切 File ·")
                .child(div().flex_1())
                .child("只读预览 ·")
                .child(kcap("Esc"))
                .child("关闭")
                .when_some(rail_tag, |d, t| d.child(div().w(px(10.))).child(t))
        };

        // ── 查找/替换条(编辑态 Ctrl+F / Ctrl+H 唤出;输入由 on_key 的 find_key 捕获)──
        let mono = SharedString::from(self.config.font().family.clone());
        let find_bar = (self.editing && self.find_open).then(|| {
            let field_bounds = self.find_field_bounds.clone();
            let field = |label: &'static str, text: &str, active: bool| {
                // 激活字段的输入框挂占位 canvas,把窗口坐标写进 `find_field_bounds`,
                // 供 `bounds_for_range` 把 IME 候选框定位到查找框旁(TnE-12)。
                let bounds_sink = active.then(|| field_bounds.clone());
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(crate::style::FS_MICRO))
                            .text_color(col(ui.muted))
                            .child(label),
                    )
                    .child(
                        div()
                            .relative()
                            .min_w(px(140.))
                            .px(px(7.))
                            .py(px(2.))
                            .rounded(px(crate::style::R_CHIP))
                            .bg(gpui::rgb(crate::style::L0)) // 输入凹井
                            .border_1()
                            .border_color(if active {
                                rgba(crate::style::PH_DIM)
                            } else {
                                rgba(crate::style::H1)
                            })
                            .font_family(mono.clone())
                            .text_size(px(crate::style::FS_MICRO))
                            .text_color(col(ui.foreground))
                            // show a thin caret stand-in when the active field is empty
                            .child(SharedString::from(if text.is_empty() {
                                if active {
                                    "▏".to_string()
                                } else {
                                    String::new()
                                }
                            } else {
                                text.to_string()
                            }))
                            .when_some(bounds_sink, |d, sink| {
                                d.child(
                                    canvas(
                                        move |bounds, _w, _cx| sink.set(Some(bounds)),
                                        |_b, _s, _w, _cx| {},
                                    )
                                    .absolute()
                                    .size_full(),
                                )
                            }),
                    )
            };
            let edit_lines = self.edit.lines();
            let n = all_matches(&edit_lines.borrow(), &self.find_query).len();
            // Echo the live IME preedit in whichever field is active (中文 search).
            let preedit = self.ime_marked.as_deref().unwrap_or("");
            let find_disp = if self.find_field_replace {
                self.find_query.clone()
            } else {
                format!("{}{}", self.find_query, preedit)
            };
            let repl_disp = if self.find_field_replace {
                format!("{}{}", self.replace_query, preedit)
            } else {
                self.replace_query.clone()
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .h(px(32.))
                .px(px(13.))
                .flex_none()
                .font_family(UI_SANS)
                .bg(cola(ui.accent, 0.05))
                .border_b_1()
                .border_color(rgba(crate::style::H0))
                .child(field("查找", &find_disp, !self.find_field_replace))
                .when(self.replacing, |d| {
                    d.child(field("替换", &repl_disp, self.find_field_replace))
                })
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(col(ui.muted))
                        .child(SharedString::from(format!("{n} 项"))),
                )
                .child(kcap("Enter"))
                .child(
                    div()
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(col(ui.muted))
                        .child("下一个"),
                )
                .when(self.replacing, |d| {
                    d.child(kcap("Ctrl+↵")).child(
                        div()
                            .text_size(px(crate::style::FS_MICRO))
                            .text_color(col(ui.muted))
                            .child("全部替换"),
                    )
                })
                .child(kcap("Esc"))
                .child(
                    div()
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(col(ui.muted))
                        .child("关闭"),
                )
        });

        let save_notice = self
            .save_conflict
            .map(|conflict| {
                // `.btn` 小号:L2 + h1;danger = err-soft + err 边(SHEET 06)。
                let action = |label: &'static str, danger: bool| {
                    div()
                        .px(px(9.))
                        .py(px(2.))
                        .rounded(px(crate::style::R_CHIP))
                        .text_size(px(crate::style::FS_MICRO))
                        .font_weight(gpui::FontWeight(620.))
                        .text_color(col(if danger { ansi.red } else { ui.foreground }))
                        .bg(if danger {
                            cola(ansi.red, 0.10)
                        } else {
                            gpui::rgb(crate::style::L2)
                        })
                        .border_1()
                        .border_color(if danger {
                            cola(ansi.red, 0.32)
                        } else {
                            rgba(crate::style::H1)
                        })
                        .child(label)
                };
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(8.))
                    .px(px(13.))
                    .py(px(7.))
                    .flex_none()
                    .font_family(UI_SANS)
                    .text_size(px(crate::style::FS_MICRO))
                    .text_color(col(ui.muted))
                    .bg(cola(ansi.red, 0.06))
                    .border_t_1()
                    .border_color(rgba(crate::style::H0))
                    .child(icon("alert", 13., ansi.red))
                    .child(
                        div()
                            .text_color(col(ansi.red))
                            .child(SharedString::from(conflict.label())),
                    )
                    .child(div().flex_1())
                    .child(action("重新载入", false).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.reload_current_source(cx)),
                    ))
                    .child(action("取消", false).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.cancel_save_conflict(cx)),
                    ))
                    .child(action("覆盖保存", true).on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.force_save_current_source(cx)),
                    ))
            })
            .or_else(|| {
                self.save_error.as_ref().map(|error| {
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .px(px(13.))
                        .py(px(7.))
                        .flex_none()
                        .font_family(UI_SANS)
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(col(ui.muted))
                        .bg(cola(ansi.red, 0.06))
                        .border_t_1()
                        .border_color(rgba(crate::style::H0))
                        .child(icon("alert", 13., ansi.red))
                        .child(
                            div()
                                .text_color(col(ansi.red))
                                .child(SharedString::from(error.clone())),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .px(px(9.))
                                .py(px(2.))
                                .rounded(px(crate::style::R_CHIP))
                                .text_size(px(crate::style::FS_MICRO))
                                .font_weight(gpui::FontWeight(620.))
                                .text_color(col(ui.foreground))
                                .bg(gpui::rgb(crate::style::L2))
                                .border_1()
                                .border_color(rgba(crate::style::H1))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _e, _w, cx| this.cancel_save_conflict(cx)),
                                )
                                .child("关闭"),
                        )
                })
            });

        // 未保存确认 = 06-C 独立确认浮层(差异总结 3-21:不再用内嵌横条):
        // 迷你 scrim 压暗速览内容 + 居中 460 浮层(warn 头 + 文件 chip + 正文 +
        // 46 高按钮脚:「保存并关闭」btn primary 磷光底墨字 / 「丢弃」danger / 取消)。
        let leave_notice = self.pending_leave.clone().map(|pending| {
            let mono = SharedString::from(self.config.font().family.clone());
            let confirm_label = match &pending {
                PendingLeave::Close => "保存并关闭",
                PendingLeave::Quit => "保存并退出",
                _ => "保存并继续",
            };
            // `.btn` 家族(SHEET 06-C):primary = ph 底 + ph-ink 墨字 600;
            // danger = err 字 + err-soft 底 + err·35 边;普通 = L2 + h1。
            let btn = |label: &'static str| {
                div()
                    .px(px(14.))
                    .py(px(5.))
                    .rounded(px(crate::style::R_CARD))
                    .font_family(UI_SANS)
                    .text_size(px(crate::style::FS_CAPTION))
                    .text_color(gpui::rgb(crate::style::T1))
                    .bg(gpui::rgb(crate::style::L2))
                    .border_1()
                    .border_color(rgba(crate::style::H1))
                    .hover(|s| {
                        s.bg(gpui::rgb(crate::style::L4))
                            .text_color(gpui::rgb(crate::style::T0))
                    })
                    .child(label)
            };
            let card = div()
                .w(px(460.))
                .rounded(px(R_PANEL))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(crate::style::H2))
                .bg(gpui::rgb(crate::style::L3))
                .shadow(crate::style::shadow_float())
                .child(
                    // float-head:38 高 · L4 · ⚠ warn + 标题 + 文件名 chip
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(10.))
                        .h(px(38.))
                        .px(px(14.))
                        .flex_none()
                        .bg(gpui::rgb(crate::style::L4))
                        .border_b(px(1.))
                        .border_color(rgba(crate::style::H1))
                        .font_family(mono.clone())
                        .text_size(px(crate::style::FS_CAPTION))
                        .child(div().text_color(col(ansi.yellow)).child("⚠"))
                        .child(
                            div()
                                .text_color(gpui::rgb(crate::style::T1))
                                .child("未保存的改动"),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .px(px(8.))
                                .py(px(2.))
                                .rounded(px(crate::style::R_CHIP))
                                .border_1()
                                .border_color(rgba(crate::style::H1))
                                .text_size(px(crate::style::FS_MICRO))
                                .text_color(gpui::rgb(crate::style::T1))
                                .max_w(px(180.))
                                .overflow_hidden()
                                .child(SharedString::from(name.clone())),
                        ),
                )
                .child(
                    div()
                        .px(px(16.))
                        .py(px(14.))
                        .font_family(UI_SANS)
                        .text_size(px(crate::style::FS_CAPTION))
                        .text_color(gpui::rgb(crate::style::T1))
                        .child(SharedString::from(pending.prompt())),
                )
                .child(
                    // float-foot:46 高 · gap 8 · 右对齐按钮组
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_end()
                        .gap(px(8.))
                        .h(px(46.))
                        .px(px(14.))
                        .flex_none()
                        .border_t(px(1.))
                        .border_color(rgba(crate::style::H1))
                        .child(
                            btn(confirm_label)
                                .bg(gpui::rgb(crate::style::PH))
                                .border_color(gpui::rgb(crate::style::PH))
                                .text_color(gpui::rgb(crate::style::PH_INK))
                                .font_weight(gpui::FontWeight(600.))
                                .hover(|s| s.bg(gpui::rgb(crate::style::PH)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _e, _w, cx| this.save_pending_leave(cx)),
                                ),
                        )
                        .child(
                            btn("丢弃")
                                .text_color(col(ansi.red))
                                .bg(cola(ansi.red, 0.14))
                                .border_color(cola(ansi.red, 0.35))
                                .hover(|s| s.bg(cola(ansi.red, 0.22)))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _e, _w, cx| this.discard_pending_leave(cx)),
                                ),
                        )
                        .child(btn("取消").on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.cancel_pending_leave(cx)),
                        )),
                );
            // 覆盖整个速览面:纯色压暗(无模糊,契约 7)+ 居中卡;点压暗区 = 取消。
            div()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .right(px(0.))
                .bottom(px(0.))
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(crate::style::SCRIM))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.cancel_pending_leave(cx);
                    }),
                )
                .child(card.on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|_this, _e, _w, cx| cx.stop_propagation()),
                ))
        });

        // Remote hunk apply/reject failure → dismissible red banner (independent of
        // the save banner; either can show).
        let hunk_notice = self.hunk_error.as_ref().map(|error| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .px(px(13.))
                .py(px(7.))
                .flex_none()
                .font_family(UI_SANS)
                .text_size(px(crate::style::FS_MICRO))
                .text_color(col(ui.muted))
                .bg(cola(ansi.red, 0.06))
                .border_t_1()
                .border_color(rgba(crate::style::H0))
                .child(icon("alert", 13., ansi.red))
                .child(
                    div()
                        .text_color(col(ansi.red))
                        .child(SharedString::from(format!("应用失败:{error}"))),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .px(px(9.))
                        .py(px(2.))
                        .rounded(px(crate::style::R_CHIP))
                        .text_size(px(crate::style::FS_MICRO))
                        .font_weight(gpui::FontWeight(620.))
                        .text_color(col(ui.foreground))
                        .bg(gpui::rgb(crate::style::L2))
                        .border_1()
                        .border_color(rgba(crate::style::H1))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.dismiss_hunk_error(cx)),
                        )
                        .child("关闭"),
                )
        });

        // ── 左缘磷光脊(`.seam`):指向树中选中文件的「连接感」— 与选中态同语法 ──
        // SHEET 03:QuickLook 浮层左侧**不**画磷光竖脊 —— 海拔由 float 投影 + h2 边
        // 表达,磷光只留 header 内的小点。左脊是 tile/row/命令块的「选中」语义,浮层
        // 借用会让磷光从状态信号退化为装饰边框(原型与真机差异总结 P0)。
        let inner = div()
            .track_focus(&self.focus_handle)
            .on_key_down(
                cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)),
            )
            // Swallow any click landing on the panel (not already handled by a child
            // like a code row) so it neither bubbles to the workspace click-away scrim
            // (which would close the overlay) nor passes through to a terminal pane
            // (which would steal focus to the shell). Clicking the panel keeps focus
            // here (track_focus). 修「面板穿透事件 / 焦点漏到底层 shell」。
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|_, _ev, _w, cx| cx.stop_propagation()),
            )
            // Drag the preview's bottom horizontal scrollbar thumb (set in `body`).
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, _w, cx| {
                if this.hscroll_drag.is_some() {
                    // 只有左键仍按着才跟随;一旦松开就立即结束拖动。`on_mouse_up` 只在浮层
                    // bounds 内释放才触发 —— 鼠标拖出浮层外松开时收不到 up,若不在这里兜底
                    // 清掉,thumb 会"粘"在鼠标上随移动(用户实测的 bug)。
                    if ev.pressed_button == Some(MouseButton::Left) {
                        this.on_hscroll_move(f32::from(ev.position.x), cx);
                    } else {
                        this.hscroll_drag = None;
                        cx.notify();
                    }
                } else if this.edit_drag && ev.pressed_button != Some(MouseButton::Left) {
                    // 文本拖选时鼠标移出行/浮层后松开,行 move 收不到 → 这里兜底结束拖选。
                    this.edit_drag = false;
                    cx.notify();
                }
            }))
            // 滚轮兜底吞噬:正文滚动由子区处理后,事件不得再穿透到底层终端
            // (BUG发现 #5:QuickLook 内滚轮曾驱动底下 shell 的 scrollback)。
            .on_scroll_wheel(cx.listener(|_this, _ev: &ScrollWheelEvent, _w, cx| {
                cx.stop_propagation();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _ev: &MouseUpEvent, _w, cx| {
                    let mut changed = this.hscroll_drag.take().is_some();
                    if this.edit_drag {
                        this.edit_drag = false; // end text drag-selection
                        changed = true;
                    }
                    if changed {
                        cx.notify();
                    }
                }),
            )
            .size_full()
            .relative() // anchor absolute children (hscroll thumb / notices)
            .flex()
            .flex_col()
            .min_h(px(0.))
            .overflow_hidden()
            .rounded(px(R_PANEL - 1.)) // 1px tighter so the float hairline shows
            // 磷光浮板:不透明 L3(浮终端上须压住后字,契约 1)
            .bg(col(ui.palette_bg))
            .font_family(SharedString::from(self.config.font().family.clone()))
            .text_size(px(12.5))
            .child(header)
            .when_some(find_bar, |d, fb| d.child(fb))
            .child(body)
            .when_some(save_notice, |d, n| d.child(n))
            .when_some(hunk_notice, |d, n| d.child(n))
            .child(footer)
            // 确认浮层(06-C)绝对定位盖全面,挂在 footer 之后 = 画在最上层。
            .when_some(leave_notice, |d, n| d.child(n));

        // 浮层家族:1px h2 边 + float 投影(全系统唯一投影,契约 4)
        float_panel(inner)
    }
}

// ════════════════════════════════════════════════════════════════════════════
// Markdown 预览渲染(规则:.md 默认渲染态,Enter 进编辑、Esc 回预览)
//
// pulldown-cmark 解析事件流 → GPUI 原生元素(div + StyledText/TextRun)。无 WebView:
// 整进程是 GPUI 终端 app,且契约「原型必须 GPUI 可还原」。行内排版用 StyledText 带
// per-run 字重/斜体/删除线/底色以获得正确的换行;代码围栏复用文件预览的 `highlight()`
// 着色器。磷光是唯一生命色(契约),所以正文走文字阶梯 T0/T2、链接走 INFO 蓝,不滥用 PH。
// ════════════════════════════════════════════════════════════════════════════
use gpui::{FontStyle, FontWeight, StrikethroughStyle, StyledText, UnderlineStyle};
use pulldown_cmark::{Event as MdEvent, HeadingLevel, Options, Parser, Tag, TagEnd};

/// 该文件是否按 Markdown 渲染(预览态)。
fn is_markdown_path(p: Option<&std::path::Path>) -> bool {
    p.and_then(|p| p.extension())
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .is_some_and(|e| {
            matches!(
                e.as_str(),
                "md" | "markdown" | "mdown" | "mkd" | "mkdn" | "mdwn" | "mdx"
            )
        })
}

#[derive(Clone)]
struct MdFonts {
    sans: gpui::Font,
    mono: gpui::Font,
}

/// 行内一段文本的样式(随强调/链接/行内码层叠)。
#[derive(Clone, Copy)]
struct MdStyle {
    color: Hsla,
    weight: FontWeight,
    italic: bool,
    strike: bool,
    underline: Option<Hsla>,
    mono: bool,
    bg: Option<Hsla>,
}

/// 渲染上下文:字体 + 磷光体系派生色板 + 主题(供代码着色)。
struct MdCtx<'a> {
    config: &'a Loaded,
    fonts: MdFonts,
    body: Hsla,     // 正文 T0
    muted: Hsla,    // 弱文 T2(列表标记 / 图片占位)
    link: Hsla,     // 链接 INFO 蓝(不动用磷光)
    code_fg: Hsla,  // 行内码字
    code_bg: Hsla,  // 行内码底 L2
    block_bg: Hsla, // 代码块 / 表头底 L1
    border: Hsla,   // 发丝边 H1
    quote: Hsla,    // 引用左条 H2
}

impl MdCtx<'_> {
    fn base(&self) -> MdStyle {
        MdStyle {
            color: self.body,
            weight: FontWeight::NORMAL,
            italic: false,
            strike: false,
            underline: None,
            mono: false,
            bg: None,
        }
    }
}

/// 累积一段行内内容为 `(String, Vec<TextRun>)`,交给 `StyledText` 做正确换行。
struct MdInline {
    text: String,
    runs: Vec<TextRun>,
}

impl MdInline {
    fn new() -> Self {
        Self {
            text: String::new(),
            runs: Vec::new(),
        }
    }

    fn push(&mut self, s: &str, st: MdStyle, fonts: &MdFonts) {
        if s.is_empty() {
            return;
        }
        let mut font = if st.mono {
            fonts.mono.clone()
        } else {
            fonts.sans.clone()
        };
        font.weight = st.weight;
        font.style = if st.italic {
            FontStyle::Italic
        } else {
            FontStyle::Normal
        };
        self.runs.push(TextRun {
            len: s.len(),
            font,
            color: st.color,
            background_color: st.bg,
            underline: st.underline.map(|c| UnderlineStyle {
                thickness: px(1.),
                color: Some(c),
                wavy: false,
            }),
            strikethrough: st.strike.then(|| StrikethroughStyle {
                thickness: px(1.),
                color: Some(st.color),
            }),
        });
        self.text.push_str(s);
    }

    fn into_text(self) -> Option<StyledText> {
        if self.text.is_empty() {
            None
        } else {
            Some(StyledText::new(SharedString::from(self.text)).with_runs(self.runs))
        }
    }
}

/// 消费行内事件直到当前块(段落/标题/单元格)的 End。强调/链接用样式栈层叠;
/// 由于事件流良构,每进入一个行内 Start 压栈、其 End 弹栈,落到栈底的那个 End
/// 即关闭本块 → 退出。
fn md_inline<'e>(
    events: &mut impl Iterator<Item = MdEvent<'e>>,
    ctx: &MdCtx,
    base: MdStyle,
) -> MdInline {
    let mut inl = MdInline::new();
    let mut stack = vec![base];
    loop {
        match events.next() {
            None => break,
            Some(MdEvent::End(_)) => {
                if stack.len() <= 1 {
                    break;
                }
                stack.pop();
            }
            Some(MdEvent::Start(tag)) => {
                let mut s = *stack.last().unwrap();
                match tag {
                    Tag::Strong => s.weight = FontWeight::BOLD,
                    Tag::Emphasis => s.italic = true,
                    Tag::Strikethrough => s.strike = true,
                    Tag::Link { .. } => {
                        s.color = ctx.link;
                        s.underline = Some(ctx.link);
                    }
                    Tag::Image { .. } => {
                        inl.push("🖼 ", *stack.last().unwrap(), &ctx.fonts);
                        s.color = ctx.muted;
                    }
                    _ => {}
                }
                stack.push(s);
            }
            Some(MdEvent::Text(t)) => inl.push(&t, *stack.last().unwrap(), &ctx.fonts),
            Some(MdEvent::Code(t)) => {
                let mut s = *stack.last().unwrap();
                s.mono = true;
                s.bg = Some(ctx.code_bg);
                s.color = ctx.code_fg;
                inl.push(&t, s, &ctx.fonts);
            }
            Some(MdEvent::SoftBreak) => inl.push(" ", *stack.last().unwrap(), &ctx.fonts),
            Some(MdEvent::HardBreak) => inl.push("\n", *stack.last().unwrap(), &ctx.fonts),
            Some(MdEvent::TaskListMarker(done)) => {
                inl.push(
                    if done { "☑ " } else { "☐ " },
                    *stack.last().unwrap(),
                    &ctx.fonts,
                );
            }
            _ => {}
        }
    }
    inl
}

/// 把累积的行内缓冲冲刷成一个段落 div(空则不产出)。
fn md_flush_para(out: &mut Vec<gpui::Div>, inl: &mut MdInline, ctx: &MdCtx) {
    let taken = std::mem::replace(inl, MdInline::new());
    if let Some(t) = taken.into_text() {
        out.push(
            div()
                .pb(px(8.))
                .text_size(px(13.5))
                .line_height(px(21.))
                .text_color(ctx.body)
                .child(t),
        );
    }
}

/// 消费块级事件直到当前容器的 End(顶层则直到流结束)。
///
/// 关键:**块级层也要累积行内内容**。紧凑列表(tight list)的项内容不被包进
/// `Paragraph`,而是直接吐出 `Text`/`Code`/`Start(Link)` 等行内事件;若只认 `Text`
/// 就会丢掉行内码、把链接当容器吃掉、并让每个文本碎片各成一段(换行)。所以这里
/// 维护一个行内样式栈累积 `inl`,遇到真正的块边界(段落 End / 块容器 / 分隔线)
/// 才 flush。块容器(列表/引用/表/代码块/标题)各自在递归里吃掉自己的 End;落到
/// 本层 match 到的「非行内 End」即本容器收尾 → flush 并退出。
fn md_blocks<'e>(events: &mut impl Iterator<Item = MdEvent<'e>>, ctx: &MdCtx) -> Vec<gpui::Div> {
    let mut out = Vec::new();
    let mut inl = MdInline::new();
    let mut stack = vec![ctx.base()]; // 行内强调/链接样式栈
    loop {
        match events.next() {
            None => {
                md_flush_para(&mut out, &mut inl, ctx);
                break;
            }
            Some(MdEvent::End(end)) => match end {
                TagEnd::Emphasis
                | TagEnd::Strong
                | TagEnd::Strikethrough
                | TagEnd::Link
                | TagEnd::Image => {
                    if stack.len() > 1 {
                        stack.pop();
                    }
                }
                TagEnd::Paragraph => md_flush_para(&mut out, &mut inl, ctx),
                // 其余 End = 关闭本容器(Item / BlockQuote / 顶层不会到这);收尾退出。
                _ => {
                    md_flush_para(&mut out, &mut inl, ctx);
                    break;
                }
            },
            Some(MdEvent::Start(tag)) => match tag {
                // ── 行内开标签:压样式栈,内容继续累积到 inl ──
                Tag::Strong => {
                    let mut s = *stack.last().unwrap();
                    s.weight = FontWeight::BOLD;
                    stack.push(s);
                }
                Tag::Emphasis => {
                    let mut s = *stack.last().unwrap();
                    s.italic = true;
                    stack.push(s);
                }
                Tag::Strikethrough => {
                    let mut s = *stack.last().unwrap();
                    s.strike = true;
                    stack.push(s);
                }
                Tag::Link { .. } => {
                    let mut s = *stack.last().unwrap();
                    s.color = ctx.link;
                    s.underline = Some(ctx.link);
                    stack.push(s);
                }
                Tag::Image { .. } => {
                    inl.push("🖼 ", *stack.last().unwrap(), &ctx.fonts);
                    let mut s = *stack.last().unwrap();
                    s.color = ctx.muted;
                    stack.push(s);
                }
                // ── 块级开标签:先 flush 当前行内,再产出块 ──
                Tag::Paragraph => md_flush_para(&mut out, &mut inl, ctx),
                Tag::Heading { level, .. } => {
                    md_flush_para(&mut out, &mut inl, ctx);
                    let mut base = ctx.base();
                    base.weight = FontWeight::BOLD;
                    let h = md_inline(events, ctx, base);
                    out.push(md_heading(level, h, ctx));
                }
                Tag::CodeBlock(_) => {
                    md_flush_para(&mut out, &mut inl, ctx);
                    let lines = md_collect_code(events);
                    out.push(md_code_block(&lines, ctx));
                }
                Tag::List(start) => {
                    md_flush_para(&mut out, &mut inl, ctx);
                    out.push(md_list(events, start, ctx));
                }
                Tag::BlockQuote(_) => {
                    md_flush_para(&mut out, &mut inl, ctx);
                    let inner = md_blocks(events, ctx);
                    out.push(md_quote(inner, ctx));
                }
                Tag::Table(_) => {
                    md_flush_para(&mut out, &mut inl, ctx);
                    out.push(md_table(events, ctx));
                }
                _ => {
                    // 未建模的块容器(脚注定义等):flush 后递归消费其内容与 End。
                    md_flush_para(&mut out, &mut inl, ctx);
                    let _ = md_blocks(events, ctx);
                }
            },
            Some(MdEvent::Text(t)) => inl.push(&t, *stack.last().unwrap(), &ctx.fonts),
            Some(MdEvent::Code(t)) => {
                let mut s = *stack.last().unwrap();
                s.mono = true;
                s.bg = Some(ctx.code_bg);
                s.color = ctx.code_fg;
                inl.push(&t, s, &ctx.fonts);
            }
            Some(MdEvent::SoftBreak) => inl.push(" ", *stack.last().unwrap(), &ctx.fonts),
            Some(MdEvent::HardBreak) => inl.push("\n", *stack.last().unwrap(), &ctx.fonts),
            Some(MdEvent::TaskListMarker(done)) => {
                inl.push(
                    if done { "☑ " } else { "☐ " },
                    *stack.last().unwrap(),
                    &ctx.fonts,
                );
            }
            Some(MdEvent::Rule) => {
                md_flush_para(&mut out, &mut inl, ctx);
                out.push(div().my(px(10.)).h(px(1.)).bg(ctx.border));
            }
            _ => {}
        }
    }
    out
}

/// 标题:字号阶梯 + 粗体;h1/h2 加底部发丝边表达层级结构(契约:深度=线,不靠色块)。
fn md_heading(level: HeadingLevel, inl: MdInline, ctx: &MdCtx) -> gpui::Div {
    let (size, top, bottom, border) = match level {
        HeadingLevel::H1 => (22.0, 14.0, 8.0, true),
        HeadingLevel::H2 => (18.0, 12.0, 6.0, true),
        HeadingLevel::H3 => (15.5, 10.0, 4.0, false),
        _ => (13.5, 8.0, 4.0, false),
    };
    let mut d = div()
        .pt(px(top))
        .pb(px(bottom))
        .text_size(px(size))
        .line_height(px(size * 1.35))
        .text_color(ctx.body);
    if let Some(t) = inl.into_text() {
        d = d.child(t);
    }
    if border {
        d = d.border_b_1().border_color(ctx.border);
    }
    d
}

/// 收集围栏/缩进代码块的纯文本,按行切分(去掉收尾换行产生的空尾行)。
fn md_collect_code<'e>(events: &mut impl Iterator<Item = MdEvent<'e>>) -> Vec<String> {
    let mut text = String::new();
    loop {
        match events.next() {
            None | Some(MdEvent::End(_)) => break,
            Some(MdEvent::Text(t)) | Some(MdEvent::Code(t)) => text.push_str(&t),
            _ => {}
        }
    }
    let mut lines: Vec<String> = text.split('\n').map(|s| s.to_string()).collect();
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// 代码块:L1 底 + 发丝边 + 圆角,逐行复用文件预览的 `highlight()` 着色器(mono)。
fn md_code_block(lines: &[String], ctx: &MdCtx) -> gpui::Div {
    let mut col_div = div().flex().flex_col();
    let mut mono = ctx.base();
    mono.mono = true;
    for line in lines {
        let spans = highlight(line);
        let mut inl = MdInline::new();
        if spans.is_empty() {
            inl.push(" ", mono, &ctx.fonts); // 空行也占一行高
        }
        for (txt, tint) in spans {
            let mut s = mono;
            s.color = tint_color(ctx.config, tint).into();
            inl.push(&txt, s, &ctx.fonts);
        }
        let mut row = div().text_size(px(crate::style::FS_CAPTION)).line_height(px(18.0));
        if let Some(t) = inl.into_text() {
            row = row.child(t);
        }
        col_div = col_div.child(row);
    }
    div()
        .my(px(8.))
        .px(px(12.))
        .py(px(9.))
        .rounded(px(crate::style::R_CARD))
        .bg(ctx.block_bg)
        .border_1()
        .border_color(ctx.border)
        .child(col_div)
}

/// 引用块:左侧 2px 竖条 + 缩进 + 次文色,内部是任意块。
fn md_quote(inner: Vec<gpui::Div>, ctx: &MdCtx) -> gpui::Div {
    div()
        .my(px(6.))
        .pl(px(12.))
        .border_l(px(2.))
        .border_color(ctx.quote)
        .text_color(ctx.muted)
        .flex()
        .flex_col()
        .children(inner)
}

/// 列表:逐 Item 渲染「标记 + 内容块」。有序用 `n.`,无序用 `•`。
fn md_list<'e>(
    events: &mut impl Iterator<Item = MdEvent<'e>>,
    start: Option<u64>,
    ctx: &MdCtx,
) -> gpui::Div {
    let mut idx = start;
    let mut rows: Vec<gpui::Div> = Vec::new();
    loop {
        match events.next() {
            None | Some(MdEvent::End(_)) => break,
            Some(MdEvent::Start(Tag::Item)) => {
                let blocks = md_blocks(events, ctx);
                let marker = match idx {
                    Some(n) => format!("{n}."),
                    None => "•".to_string(),
                };
                if let Some(n) = idx.as_mut() {
                    *n += 1;
                }
                rows.push(
                    div()
                        .flex()
                        .flex_row()
                        .items_start()
                        .gap(px(8.))
                        .child(
                            div()
                                .flex_none()
                                .min_w(px(18.))
                                .text_size(px(13.5))
                                .line_height(px(21.))
                                .text_color(ctx.muted)
                                .child(SharedString::from(marker)),
                        )
                        .child(div().flex_1().min_w(px(0.)).flex().flex_col().children(blocks)),
                );
            }
            _ => {}
        }
    }
    div().flex().flex_col().pb(px(6.)).children(rows)
}

/// 表格:表头 L1 底,逐行发丝边分隔,单元格等分宽。
fn md_table<'e>(events: &mut impl Iterator<Item = MdEvent<'e>>, ctx: &MdCtx) -> gpui::Div {
    let mut header: Vec<MdInline> = Vec::new();
    let mut rows: Vec<Vec<MdInline>> = Vec::new();
    loop {
        match events.next() {
            None | Some(MdEvent::End(_)) => break,
            Some(MdEvent::Start(Tag::TableHead)) => header = md_row_cells(events, ctx),
            Some(MdEvent::Start(Tag::TableRow)) => rows.push(md_row_cells(events, ctx)),
            _ => {}
        }
    }
    let mk_row = |cells: Vec<MdInline>, is_head: bool, ctx: &MdCtx| {
        let mut r = div()
            .flex()
            .flex_row()
            .border_b_1()
            .border_color(ctx.border);
        for c in cells {
            let mut cell = div()
                .flex_1()
                .min_w(px(0.))
                .px(px(9.))
                .py(px(5.))
                .text_size(px(crate::style::FS_BODY))
                .line_height(px(18.))
                .text_color(ctx.body);
            if is_head {
                cell = cell.bg(ctx.block_bg).font_weight(FontWeight::BOLD);
            }
            if let Some(t) = c.into_text() {
                cell = cell.child(t);
            }
            r = r.child(cell);
        }
        r
    };
    let mut tbl = div()
        .my(px(8.))
        .rounded(px(crate::style::R_CARD))
        .border_1()
        .border_color(ctx.border)
        .overflow_hidden()
        .flex()
        .flex_col();
    if !header.is_empty() {
        tbl = tbl.child(mk_row(header, true, ctx));
    }
    for row in rows {
        tbl = tbl.child(mk_row(row, false, ctx));
    }
    tbl
}

/// 收集一行(表头/表体)的所有单元格行内内容。
fn md_row_cells<'e>(events: &mut impl Iterator<Item = MdEvent<'e>>, ctx: &MdCtx) -> Vec<MdInline> {
    let mut cells = Vec::new();
    loop {
        match events.next() {
            None | Some(MdEvent::End(_)) => break,
            Some(MdEvent::Start(Tag::TableCell)) => cells.push(md_inline(events, ctx, ctx.base())),
            _ => {}
        }
    }
    cells
}

/// Markdown 预览视图:解析整篇 → 块元素,装进可纵向滚动的浮板正文区。
fn markdown_view(config: &Loaded, lines: &[String]) -> impl IntoElement {
    let source = lines.join("\n");
    let ctx = MdCtx {
        config,
        fonts: MdFonts {
            sans: crate::style::with_cjk(UI_SANS),
            mono: crate::style::with_cjk(&config.font().family),
        },
        body: gpui::rgb(crate::style::T0).into(),
        muted: gpui::rgb(crate::style::T2).into(),
        link: gpui::rgb(crate::style::INFO).into(),
        code_fg: gpui::rgb(crate::style::T0).into(),
        code_bg: gpui::rgb(crate::style::L2).into(),
        block_bg: gpui::rgb(crate::style::L1).into(),
        border: rgba(crate::style::H1).into(),
        quote: rgba(crate::style::H2).into(),
    };
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_TABLES);
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TASKLISTS);
    opts.insert(Options::ENABLE_FOOTNOTES);
    let mut events = Parser::new_ext(&source, opts);
    let blocks = md_blocks(&mut events, &ctx);
    div()
        .id("ql-md")
        .size_full()
        .overflow_y_scroll()
        .bg(gpui::rgb(CODE_BG))
        .px(px(18.))
        .py(px(12.))
        .font_family(SharedString::from(UI_SANS))
        .children(blocks)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn markdown_path_detection() {
        use std::path::Path;
        assert!(is_markdown_path(Some(Path::new("README.md"))));
        assert!(is_markdown_path(Some(Path::new("a/b/notes.MARKDOWN"))));
        assert!(is_markdown_path(Some(Path::new("x.Mdx"))));
        assert!(!is_markdown_path(Some(Path::new("main.rs"))));
        assert!(!is_markdown_path(Some(Path::new("LICENSE"))));
        assert!(!is_markdown_path(None));
    }

    #[test]
    fn markdown_code_fence_collects_lines() {
        // 围栏代码块按行切分,且收尾换行不产生空尾行。
        let src = "```rust\nfn main() {}\nlet x = 1;\n```\n";
        let mut events = Parser::new(src);
        // 推进到 CodeBlock 的 Start,再交给 md_collect_code 吃掉正文 + End。
        let mut got = None;
        while let Some(ev) = events.next() {
            if matches!(ev, MdEvent::Start(Tag::CodeBlock(_))) {
                got = Some(md_collect_code(&mut events));
                break;
            }
        }
        assert_eq!(
            got.as_deref(),
            Some(&["fn main() {}".to_string(), "let x = 1;".to_string()][..]),
        );
    }

    #[test]
    fn tight_list_item_emits_inline_events_without_paragraph_wrapper() {
        // 根因守卫:紧凑列表的项内容**不被包进 Paragraph**,行内码 / 链接直接以
        // 行内事件出现在「块级层」。md_blocks 必须在块级层也累积行内,否则会丢码、
        // 把链接当容器吃掉、并让文本碎片各自换行(见 2026-06-15 修复)。
        let src = "- 纯色(海拔)→ `Div::bg`;杜绝 [链接](u) 收尾。\n";
        let evs: Vec<_> = Parser::new(src).collect();
        let mut saw_item = false;
        let mut paragraph_in_item = false;
        let mut code_in_item = false;
        let mut link_in_item = false;
        let mut depth = 0i32; // Item 内嵌套深度(此例无嵌套块)
        for ev in &evs {
            match ev {
                MdEvent::Start(Tag::Item) => {
                    saw_item = true;
                    depth = 0;
                }
                MdEvent::End(TagEnd::Item) => saw_item = false,
                MdEvent::Start(Tag::Paragraph) if saw_item && depth == 0 => paragraph_in_item = true,
                MdEvent::Code(_) if saw_item && depth == 0 => code_in_item = true,
                MdEvent::Start(Tag::Link { .. }) if saw_item && depth == 0 => link_in_item = true,
                _ => {}
            }
        }
        assert!(
            !paragraph_in_item,
            "紧凑列表项不应有 Paragraph 包裹(若 pulldown 改了语义,需重审 md_blocks)"
        );
        assert!(code_in_item, "行内码应作为块级层事件出现");
        assert!(link_in_item, "链接应作为块级层事件出现");
    }

    #[test]
    fn hit_test_accounts_for_cjk_double_width() {
        // char_w = 10px. ASCII line: each glyph 1 col (10px).
        assert_eq!(hover_char_at_x("abcd", 25.0, 10.0), 2, "25px → 3rd char");
        assert_eq!(
            caret_col_at_x("abcd", 25.0, 10.0),
            3,
            "past 中点 → boundary 右"
        );
        assert_eq!(
            caret_col_at_x("abcd", 21.0, 10.0),
            2,
            "刚过边界 → 左侧 boundary"
        );
        // CJK line "中文字": each 汉字 2 cols (20px). Naive rel/char_w would报 ~2× 偏:
        // 25px naively → idx 2, but visually it's still inside the 2nd 汉字 (20–40px).
        assert_eq!(
            hover_char_at_x("中文字", 25.0, 10.0),
            1,
            "25px 落第 2 个汉字"
        );
        assert_eq!(
            hover_char_at_x("中文字", 45.0, 10.0),
            2,
            "45px 落第 3 个汉字"
        );
        assert_eq!(
            caret_col_at_x("中文字", 35.0, 10.0),
            2,
            "过第2汉字中点 → 其右 boundary"
        );
        assert_eq!(
            caret_col_at_x("中文字", 25.0, 10.0),
            1,
            "未过中点 → 其左 boundary"
        );
        // mixed "a中b": cols a[0–10) 中[10–30) b[30–40)
        assert_eq!(hover_char_at_x("a中b", 5.0, 10.0), 0);
        assert_eq!(hover_char_at_x("a中b", 15.0, 10.0), 1, "15px 在汉字内");
        assert_eq!(hover_char_at_x("a中b", 35.0, 10.0), 2, "35px 在 b 上");
        // 边界:负/零偏移 → 0;远超行尾 → char count
        assert_eq!(hover_char_at_x("a中b", -3.0, 10.0), 0);
        assert_eq!(hover_char_at_x("a中b", 999.0, 10.0), 3);
        assert_eq!(caret_col_at_x("a中b", 999.0, 10.0), 3);
    }

    #[test]
    fn char_byte_handles_multibyte() {
        // "a中b": chars at byte 0 / 1 / 4; col past the end clamps to len.
        assert_eq!(char_to_byte("a中b", 0), 0);
        assert_eq!(char_to_byte("a中b", 1), 1);
        assert_eq!(char_to_byte("a中b", 2), 4);
        assert_eq!(char_to_byte("a中b", 3), 5);
        assert_eq!(char_to_byte("a中b", 99), 5, "past end → byte len");
    }

    #[test]
    fn insert_is_multibyte_safe() {
        let mut b = buf(&["a中b"]);
        let mut cur = (0, 2); // between 中 and b
        op_insert(&mut b, &mut cur, "X");
        assert_eq!(b[0], "a中Xb");
        assert_eq!(cur, (0, 3));
        // inserting a multibyte char advances the col by its char count (1), not bytes
        op_insert(&mut b, &mut cur, "你");
        assert_eq!(b[0], "a中X你b");
        assert_eq!(cur, (0, 4));
    }

    #[test]
    fn newline_splits_and_backspace_merges() {
        let mut b = buf(&["hello"]);
        let mut cur = (0, 2);
        op_newline(&mut b, &mut cur);
        assert_eq!(b, buf(&["he", "llo"]));
        assert_eq!(cur, (1, 0));
        // backspace at col 0 merges into the previous line, cursor at the seam
        assert!(op_backspace(&mut b, &mut cur));
        assert_eq!(b, buf(&["hello"]));
        assert_eq!(cur, (0, 2));
        // backspace at (0,0) is a no-op
        cur = (0, 0);
        assert!(!op_backspace(&mut b, &mut cur));
        assert_eq!(b, buf(&["hello"]));
    }

    #[test]
    fn delete_forward_joins_next_line() {
        let mut b = buf(&["ab", "cd"]);
        let mut cur = (0, 2); // end of line 0
        assert!(op_delete(&mut b, &mut cur));
        assert_eq!(b, buf(&["abcd"]));
        assert_eq!(cur, (0, 2));
        // delete at the very end is a no-op
        cur = (0, 4);
        assert!(!op_delete(&mut b, &mut cur));
    }

    #[test]
    fn move_wraps_lines_and_clamps_columns() {
        let b = buf(&["abc", "de"]);
        let mut cur = (0, 3); // end of "abc"
        op_move(&b, &mut cur, "right"); // → start of next line
        assert_eq!(cur, (1, 0));
        op_move(&b, &mut cur, "left"); // → end of prev line
        assert_eq!(cur, (0, 3));
        cur = (0, 3);
        op_move(&b, &mut cur, "down"); // col clamped to shorter line len (2)
        assert_eq!(cur, (1, 2));
        cur = (0, 1);
        op_move(&b, &mut cur, "end");
        assert_eq!(cur, (0, 3));
        op_move(&b, &mut cur, "home");
        assert_eq!(cur, (0, 0));
    }

    #[test]
    fn page_clamps_to_buffer() {
        let b: Vec<String> = (0..30).map(|i| i.to_string()).collect();
        let mut cur = (0, 0);
        op_page(&b, &mut cur, 1);
        assert_eq!(cur.0, 12);
        op_page(&b, &mut cur, 1);
        assert_eq!(cur.0, 24);
        op_page(&b, &mut cur, 1);
        assert_eq!(cur.0, 29, "clamp to last row");
        op_page(&b, &mut cur, -1);
        assert_eq!(cur.0, 17);
    }

    #[test]
    fn delete_range_same_and_multi_line() {
        let mut b = buf(&["hello"]);
        op_delete_range(&mut b, (0, 1), (0, 4)); // remove "ell"
        assert_eq!(b, buf(&["ho"]));

        let mut b = buf(&["abc", "def", "ghi"]);
        op_delete_range(&mut b, (0, 1), (2, 2)); // "a" + "i"
        assert_eq!(b, buf(&["ai"]));
    }

    #[test]
    fn selected_text_spans_lines() {
        let b = buf(&["abc", "def", "ghi"]);
        assert_eq!(selected_text(&b, (0, 1), (0, 3)), "bc");
        assert_eq!(selected_text(&b, (0, 1), (2, 2)), "bc\ndef\ngh");
    }

    #[test]
    fn insert_multiline_splits() {
        let mut b = buf(&["axz"]);
        let mut cur = (0, 1); // between a and x
        op_insert_multiline(&mut b, &mut cur, "B\nC\nD");
        assert_eq!(b, buf(&["aB", "C", "Dxz"]));
        assert_eq!(cur, (2, 1)); // after "D"
    }

    #[test]
    fn matches_and_replace_all() {
        let b = buf(&["foo bar foo", "baz foo"]);
        let m = all_matches(&b, "foo");
        assert_eq!(
            m,
            vec![((0, 0), (0, 3)), ((0, 8), (0, 11)), ((1, 4), (1, 7))]
        );
        let mut b2 = b.clone();
        let n = replace_all_in(&mut b2, "foo", "X");
        assert_eq!(n, 3);
        assert_eq!(b2, buf(&["X bar X", "baz X"]));
        // empty query → no matches, no replacements
        assert!(all_matches(&b, "").is_empty());
        assert_eq!(replace_all_in(&mut b.clone(), "", "X"), 0);
    }

    #[test]
    fn edit_state_uses_document_as_source_of_truth() {
        let edit = QuickLookEditState::from_lines(buf(&["abc"]));

        edit.place_cursor(0, 1, false);
        edit.type_char("X");

        assert_eq!(edit.document_lines(), buf(&["aXbc"]));
        assert_eq!(*edit.lines().borrow(), buf(&["aXbc"]));
        assert_eq!(edit.cursor(), (0, 2));
        assert_eq!(edit.sel_range(), None);

        edit.select_range((0, 1), (0, 3));
        assert_eq!(edit.selected_text().as_deref(), Some("Xb"));

        edit.undo();
        assert_eq!(edit.document_lines(), buf(&["abc"]));
        assert_eq!(*edit.lines().borrow(), buf(&["abc"]));
        assert_eq!(edit.cursor(), (0, 1));
    }

    #[test]
    fn edit_state_updates_line_mirror_without_replacing_whole_buffer() {
        let lines: Vec<String> = (0..MAX_LINES).map(|i| format!("line {i}")).collect();
        let edit = QuickLookEditState::from_lines(lines);
        let mirror = edit.lines();

        edit.place_cursor(2000, 4, false);
        edit.type_char("X");

        assert!(Rc::ptr_eq(&mirror, &edit.lines()));
        assert_eq!(edit.row_text(2000).as_deref(), Some("lineX 2000"));
    }

    #[test]
    fn highlight_terminates_on_alphanumeric_nonword_chars() {
        // Regression (踩过的坑): `①` (U+2460) is is_alphanumeric() but NOT
        // is_alphabetic()/is_ascii_digit(), so it fell through to the punct branch
        // which broke at j==i → infinite loop → OOM (froze opening an HTML with `①`).
        // These must all return promptly with token count bounded by char count.
        for s in [
            "①",
            "① 窗口外壳",
            "②③ x",
            "½ cup",
            "a①b",
            "<h1>① 标题</h1>",
            "Ⅷ ⑩ ㊀",
        ] {
            let toks = highlight(s);
            assert!(
                toks.len() <= s.chars().count() + 1,
                "highlight({s:?}) produced {} tokens for {} chars — runaway?",
                toks.len(),
                s.chars().count()
            );
            // reconstruct → original (no chars dropped/duplicated)
            let joined: String = toks.iter().map(|(t, _)| t.as_str()).collect();
            assert_eq!(joined, s, "highlight must preserve text for {s:?}");
        }
    }

    #[test]
    fn coalesce_merges_and_bounds_spans() {
        // A markup-ish line: many tokens, but most are Plain → coalesced to few runs.
        let line = r#"<symbol id="i-spark" viewBox="0 0 24 24"><path d="M12 3.4z"/></symbol>"#;
        let raw = highlight(line).len();
        let merged = coalesce_spans(line);
        assert!(
            merged.len() < raw,
            "coalesced ({}) must be fewer than raw tokens ({raw})",
            merged.len()
        );
        // reconstructing the merged runs yields the original text (nothing dropped)
        let joined: String = merged.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, line);
        // no two consecutive runs share a tint
        for w in merged.windows(2) {
            assert!(w[0].1 != w[1].1, "adjacent runs must differ in tint");
        }

        // A very long line → a single plain span (skips tokenization).
        let long = "x".repeat(LONG_LINE_BYTES + 10);
        let s = coalesce_spans(&long);
        assert_eq!(s.len(), 1);
        assert_eq!(
            s[0].0.len(),
            long.len(),
            "long line kept whole, just untinted"
        );

        // Span count is hard-capped, with the tail preserved.
        let many = "a.".repeat(200); // ~400 alternating tokens
        let capped = coalesce_spans(&many);
        assert!(capped.len() <= MAX_SPANS, "got {}", capped.len());
        let joined: String = capped.iter().map(|(s, _)| s.as_str()).collect();
        assert_eq!(joined, many, "cap collapses the tail, never drops content");
    }

    #[test]
    fn parse_diff_tracks_hunk_line_numbers() {
        let raw = concat!(
            "diff --git a/x.rs b/x.rs\n",
            "index 111..222 100644\n",
            "--- a/x.rs\n",
            "+++ b/x.rs\n",
            "@@ -10,3 +10,4 @@ fn main() {\n",
            " ctx line\n",
            "-removed\n",
            "+added one\n",
            "+added two\n",
        );
        let d = parse_diff(raw);
        // header lines (diff/index/---/+++) are dropped; hunk + 4 body lines kept
        assert_eq!(d.len(), 5);
        assert_eq!(d[0].kind, DiffKind::Hunk);
        // context line gets new-file number 10, then deletions carry None, adds count up
        assert_eq!(d[1].kind, DiffKind::Ctx);
        assert_eq!(d[1].new_no, Some(10));
        assert_eq!(d[2].kind, DiffKind::Del);
        assert_eq!(d[2].new_no, None, "deletions have no new-file line number");
        assert_eq!(d[3].kind, DiffKind::Add);
        assert_eq!(d[3].new_no, Some(11));
        assert_eq!(d[4].new_no, Some(12));
        // empty input → no lines
        assert!(parse_diff("").is_empty());
    }

    #[test]
    fn parse_diff_skips_new_file_metadata_before_hunk() {
        let raw = concat!(
            "diff --git a/tne13_diff_smoke.txt b/tne13_diff_smoke.txt\n",
            "new file mode 100644\n",
            "index 0000000..1111111\n",
            "--- /dev/null\n",
            "+++ b/tne13_diff_smoke.txt\n",
            "@@ -0,0 +1,3 @@\n",
            "+one\n",
            "+中文 line\n",
            "+very very long line\n",
        );

        let d = parse_diff(raw);

        assert_eq!(d.len(), 4);
        assert_eq!(d[0].kind, DiffKind::Hunk);
        assert_eq!(d[0].text, "@@ -0,0 +1,3 @@");
        assert_eq!(d[1].new_no, Some(1));
        assert_eq!(d[1].text, "one");
        assert_eq!(d[2].new_no, Some(2));
        assert_eq!(d[3].new_no, Some(3));
    }

    #[test]
    fn diff_renderer_rows_use_editor_decoration_model() {
        use crate::editor::DiffRowKind;

        let raw = concat!(
            "diff --git a/x.rs b/x.rs\n",
            "--- a/x.rs\n",
            "+++ b/x.rs\n",
            "@@ -10,2 +10,3 @@ fn main() {\n",
            " ctx line\n",
            "-removed\n",
            "+added\n",
        );
        let rows = diff_render_rows(&parse_diff(raw));

        assert_eq!(
            rows.iter().map(|r| r.kind).collect::<Vec<_>>(),
            vec![
                DiffRowKind::HunkHeader,
                DiffRowKind::Context,
                DiffRowKind::Deletion,
                DiffRowKind::Addition,
            ]
        );
        assert_eq!(rows[0].gutter(), '@');
        assert_eq!(rows[0].hunk_index, Some(0));
        assert_eq!(rows[1].new_no, Some(10));
        assert_eq!(rows[2].new_no, None);
        assert_eq!(rows[3].new_no, Some(11));
        assert_eq!(rows[3].text, "added");
    }

    #[test]
    fn diff_tab_self_paint_is_default_text_render_path() {
        let text = QuickLookData::Text {
            lines: Arc::new(buf(&["x"])),
            truncated: false,
        };
        let binary = QuickLookData::Binary { size: 4 };

        assert!(should_self_paint_diff(true, false, Tab::Diff, &text));
        assert!(!should_self_paint_diff(false, false, Tab::Diff, &text));
        assert!(!should_self_paint_diff(true, true, Tab::Diff, &text));
        assert!(!should_self_paint_diff(true, false, Tab::File, &text));
        assert!(!should_self_paint_diff(true, false, Tab::Diff, &binary));
    }

    #[test]
    fn markdown_file_uses_visual_soft_wrap_while_code_keeps_horizontal_scroll() {
        use tn_editor::WrapMode;

        let lines = buf(&["alpha beta gamma delta epsilon"]);
        let markdown = file_wrap_mode_for_path(std::path::Path::new("notes.md"), 16);
        let code = file_wrap_mode_for_path(std::path::Path::new("main.rs"), 16);

        assert_eq!(markdown, WrapMode::Word { width_cols: 16 });
        assert_eq!(code, WrapMode::None);

        let prose = quicklook_file_layout(
            &lines,
            std::path::Path::new("notes.md"),
            180.0,
            80.0,
            0.0,
            999.0,
            10.0,
        );
        assert!(
            prose.layout.visual_count() > lines.len(),
            "long markdown prose should paint more visual rows than logical rows"
        );
        assert_eq!(prose.pre.max_off, 0.0);
        assert_eq!(prose.pre.h_offset, 0.0);
        assert!(prose.pre.thumb.is_none());
        assert_eq!(
            prose.layout.hit_test(&lines, 1, 20.0, 10.0),
            (0, 13),
            "the second visual row must map back into the original logical line"
        );

        let code_layout = quicklook_file_layout(
            &lines,
            std::path::Path::new("main.rs"),
            180.0,
            80.0,
            0.0,
            999.0,
            10.0,
        );
        assert_eq!(code_layout.layout.visual_count(), lines.len());
        assert!(
            code_layout.pre.max_off > 0.0,
            "code keeps the existing horizontal overflow model"
        );
        assert_eq!(code_layout.pre.h_offset, code_layout.pre.max_off);
        assert!(code_layout.pre.thumb.is_some());
    }

    #[test]
    fn ime_caret_rect_uses_soft_wrap_cjk_and_scroll_offsets() {
        let lines = buf(&["abc def 中文 ghi"]);
        let rect = quicklook_caret_paint_rect(
            &lines,
            std::path::Path::new("notes.md"),
            (0, 10),
            160.0,
            80.0,
            -20.0,
            999.0,
            10.0,
        )
        .expect("caret rect");

        assert_eq!(
            rect,
            CaretPaintRect {
                x: CODE_GUTTER + 4.0 * 10.0,
                y: 0.0,
                width: 10.0,
                height: ROW_H,
            },
            "IME candidate anchor must match the same soft-wrapped/CJK-aware paint coordinates as the caret"
        );
    }

    #[test]
    fn self_painted_caret_visual_matches_terminal_radius_and_text_scale() {
        let visual = caret_visual_rect(100.0, 40.0, 9.0, ROW_H, CODE_FS, 1.0, 1.0, 0.0, 0.0);
        assert_eq!(
            visual,
            CaretVisualRect {
                x: 100.0,
                y: 41.75,
                width: 9.0,
                height: 16.5,
                radius: 1.0,
            },
            "Quick Look's block cursor should keep terminal-like radius while visually matching the code glyph scale"
        );

        let animated = caret_visual_rect(100.0, 40.0, 9.0, ROW_H, CODE_FS, 1.2, 0.8, -3.0, 2.0);
        assert_eq!(
            animated,
            CaretVisualRect {
                x: 96.1,
                y: 45.4,
                width: 10.8,
                height: 13.2,
                radius: 1.0,
            },
            "animation scales around the visual cursor block, not the full selection row box"
        );
    }

    #[test]
    fn soft_wrapped_vertical_motion_moves_between_visual_rows() {
        let lines = buf(&["abcdefghijklmnopqrstuvwxy"]);

        assert_eq!(
            quicklook_visual_vertical_cursor(
                &lines,
                std::path::Path::new("notes.md"),
                (0, 3),
                1,
                166.0,
                80.0,
                0.0,
                0.0,
                10.0,
            ),
            Some((0, 13)),
            "Down should move to the next visual row inside the same logical line"
        );
        assert_eq!(
            quicklook_visual_vertical_cursor(
                &lines,
                std::path::Path::new("notes.md"),
                (0, 13),
                -1,
                166.0,
                80.0,
                0.0,
                0.0,
                10.0,
            ),
            Some((0, 3)),
            "Up should return to the previous visual row at the same local column"
        );
        assert_eq!(
            quicklook_visual_vertical_cursor(
                &lines,
                std::path::Path::new("main.rs"),
                (0, 3),
                1,
                166.0,
                80.0,
                0.0,
                0.0,
                10.0,
            ),
            None,
            "code files keep the logical-line movement model and horizontal scroll"
        );
    }

    #[test]
    fn text_commit_only_centers_legacy_uniform_list_renderer() {
        assert!(!should_center_after_text_commit(true));
        assert!(should_center_after_text_commit(false));
    }

    #[test]
    fn motion_triggers_only_for_text_insert_and_delete() {
        assert_eq!(
            text_motion_trigger("x", (0, 1), (0, 2)),
            Some(MotionTrigger::Insert {
                from: (0, 1),
                to: (0, 2),
                inserted: Some('x'),
            })
        );
        assert_eq!(text_motion_trigger("xy", (0, 1), (0, 3)), None);
        assert_eq!(
            delete_motion_trigger((0, 3), (0, 2)),
            Some(MotionTrigger::Delete {
                from: (0, 3),
                to: (0, 2),
            })
        );
        assert_eq!(delete_motion_trigger((0, 3), (0, 3)), None);
    }

    #[test]
    fn soft_wrapped_selection_projects_to_visual_rows_without_changing_copy_text() {
        let lines = buf(&["alpha beta gamma delta epsilon"]);
        let prose = quicklook_file_layout(
            &lines,
            std::path::Path::new("notes.md"),
            180.0,
            80.0,
            0.0,
            0.0,
            10.0,
        );

        let range = TextRange::new((0, 6), (0, 22));
        assert_eq!(
            prose.layout.range_segments(range),
            vec![(0, 6, 11), (1, 0, 6), (2, 0, 5)],
            "selection paint must split at visual wrap boundaries"
        );
        assert_eq!(
            selected_text(&lines, range.start, range.end),
            "beta gamma delta",
            "copy remains based on logical document coordinates"
        );
    }

    #[test]
    fn parse_diff_numbers_hunks_in_lockstep_with_remote_file_diff() {
        // The Diff-tab `@@` rows must carry the same 0-based hunk index that
        // `remote_git::parse_file_diff` assigns, so an "接受/拒绝" click rebuilds
        // the patch for exactly the clicked hunk. Two hunks → 0 and 1.
        let raw = concat!(
            "diff --git a/x.rs b/x.rs\n",
            "--- a/x.rs\n",
            "+++ b/x.rs\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "@@ -8 +9 @@\n",
            " ctx\n",
            "+tail\n",
        );
        let d = parse_diff(raw);
        let hunk_indices: Vec<Option<usize>> = d
            .iter()
            .filter(|l| l.kind == DiffKind::Hunk)
            .map(|l| l.hunk_index)
            .collect();
        assert_eq!(hunk_indices, vec![Some(0), Some(1)]);
        // Non-hunk rows never carry a hunk index.
        assert!(d
            .iter()
            .filter(|l| l.kind != DiffKind::Hunk)
            .all(|l| l.hunk_index.is_none()));
        // Cross-check against the remote parser used to build the patch.
        let parsed = crate::remote_git::parse_file_diff("x.rs", raw);
        assert_eq!(parsed.hunks.len(), 2);
        assert_eq!(parsed.hunks[0].index, 0);
        assert_eq!(parsed.hunks[1].index, 1);
    }

    #[test]
    fn diff_selection_copies_content_text_across_rows() {
        let raw = concat!(
            "@@ -1,2 +1,3 @@\n",
            " ctx\n",
            "-old line\n",
            "+new line\n",
            "+中文 line\n",
        );
        let rows = diff_render_rows(&parse_diff(raw));

        assert_eq!(
            diff_selected_text(&rows, (1, 1), (4, 2)),
            "tx\nold line\nnew line\n中文"
        );
    }

    #[test]
    fn diff_drag_selection_uses_cjk_and_horizontal_scroll_hit_test() {
        let rows = vec![
            DiffRenderRow {
                kind: crate::editor::DiffRowKind::Context,
                new_no: Some(1),
                text: "a中b".to_string(),
                hunk_index: None,
            },
            DiffRenderRow {
                kind: crate::editor::DiffRowKind::Addition,
                new_no: Some(2),
                text: "tail".to_string(),
                hunk_index: None,
            },
        ];

        let down = diff_cursor_from_point(&rows, 0, CODE_GUTTER + 16.0, 10.0, 0.0);
        let drag = diff_drag_cursor_from_point(&rows, down, 0, CODE_GUTTER + 44.0, 10.0, 0.0);
        assert_eq!(
            down,
            (0, 1),
            "click just past 'a' lands before the CJK char"
        );
        assert_eq!(
            drag,
            (0, 3),
            "dragging right over 'b' includes the hovered character"
        );

        let scrolled = diff_cursor_from_point(&rows, 0, CODE_GUTTER + 6.0, 10.0, 20.0);
        assert_eq!(
            scrolled,
            (0, 2),
            "horizontal scroll is included before CJK-aware hit testing"
        );
    }

    #[test]
    fn diff_selection_spans_match_rendered_display_columns() {
        let rows = vec![
            DiffRenderRow {
                kind: crate::editor::DiffRowKind::Context,
                new_no: Some(1),
                text: "a中b".to_string(),
                hunk_index: None,
            },
            DiffRenderRow {
                kind: crate::editor::DiffRowKind::Addition,
                new_no: Some(2),
                text: "cd".to_string(),
                hunk_index: None,
            },
        ];

        assert_eq!(
            diff_selection_span_cols(&rows, 0, ((0, 1), (1, 1))),
            Some((1, 4)),
            "first row selection spans the CJK glyph as two display columns"
        );
        assert_eq!(
            diff_selection_span_cols(&rows, 1, ((0, 1), (1, 1))),
            Some((0, 1))
        );
        assert_eq!(diff_selection_span_cols(&rows, 1, ((0, 0), (0, 1))), None);
    }

    #[test]
    fn diff_enter_target_maps_to_zero_based_file_row() {
        let raw = concat!(
            "@@ -10,3 +20,4 @@ fn main() {\n",
            "-removed before first new line\n",
            "+added\n",
            " ctx\n",
        );
        let rows = diff_render_rows(&parse_diff(raw));

        assert_eq!(diff_target_file_row(&rows, 0), Some(19));
        assert_eq!(diff_target_file_row(&rows, 1), Some(19));
        assert_eq!(diff_target_file_row(&rows, 3), Some(20));
    }

    #[test]
    fn diff_jump_target_carries_file_highlight_row() {
        let raw = concat!(
            "@@ -10,3 +20,4 @@ fn main() {\n",
            " ctx before\n",
            "-removed\n",
            " ctx after\n",
        );
        let rows = diff_render_rows(&parse_diff(raw));

        assert_eq!(
            diff_file_jump_target(&rows, 2),
            Some(DiffFileJump {
                row: 19,
                highlight_row: 19,
            })
        );
    }

    #[test]
    fn parse_diff_keeps_hunk_body_lines_that_look_like_file_headers() {
        let raw = concat!(
            "diff --git a/x b/x\n",
            "index 111..222 100644\n",
            "--- a/x\n",
            "+++ b/x\n",
            "@@ -1,4 +1,4 @@\n",
            " keep 1\n",
            "--- removed markdown rule\n",
            "+++ added markdown rule\n",
            " keep 2\n",
        );
        let rows = diff_render_rows(&parse_diff(raw));

        assert_eq!(
            rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            vec![
                "@@ -1,4 +1,4 @@",
                "keep 1",
                "-- removed markdown rule",
                "++ added markdown rule",
                "keep 2",
            ],
            "only file-level headers are metadata; hunk body lines may start with ---/+++"
        );
        assert_eq!(
            rows.iter().map(|r| r.new_no).collect::<Vec<_>>(),
            vec![None, Some(1), None, Some(2), Some(3)],
            "skipping body-like headers would shift later new-file line numbers"
        );
        assert_eq!(
            diff_file_jump_target(&rows, 4),
            Some(DiffFileJump {
                row: 2,
                highlight_row: 2,
            })
        );
    }

    #[test]
    fn diff_jump_target_clamps_highlight_to_actual_file_rows() {
        let raw = concat!(
            "@@ -20,3 +20,0 @@\n",
            "-removed a\n",
            "-removed b\n",
            "-removed c\n",
        );
        let rows = diff_render_rows(&parse_diff(raw));

        assert_eq!(
            diff_file_jump_target(&rows, 2),
            Some(DiffFileJump {
                row: 19,
                highlight_row: 19,
            })
        );
        assert_eq!(
            diff_file_jump_target_for_file_len(&rows, 2, 12),
            Some(DiffFileJump {
                row: 11,
                highlight_row: 11,
            }),
            "the visible highlight must follow the row that place_cursor can actually reach"
        );
        assert_eq!(
            diff_file_jump_target_for_file_len(&rows, 2, 0),
            None,
            "an empty File tab has no row to jump or highlight"
        );
    }

    #[test]
    fn diff_hunk_navigation_uses_render_rows() {
        let raw = concat!(
            "@@ -1 +1 @@\n",
            "+one\n",
            "@@ -8 +9 @@\n",
            " ctx\n",
            "@@ -20 +21 @@\n",
            "-old\n",
        );
        let rows = diff_render_rows(&parse_diff(raw));

        assert_eq!(diff_hunk_jump_row(&rows, 0, true), Some(2));
        assert_eq!(diff_hunk_jump_row(&rows, 3, false), Some(2));
        assert_eq!(diff_hunk_jump_row(&rows, 4, true), None);
        assert_eq!(diff_hunk_jump_row(&rows, 0, false), None);
    }

    #[test]
    fn diff_rows_map_to_file_line_targets() {
        let raw = concat!(
            "@@ -10,3 +20,4 @@ fn main() {\n",
            "-removed before first new line\n",
            "+added\n",
            " ctx\n",
            "-removed after context\n",
        );
        let rows = diff_render_rows(&parse_diff(raw));

        assert_eq!(diff_target_new_line(&rows, 0), Some(20));
        assert_eq!(diff_target_new_line(&rows, 1), Some(20));
        assert_eq!(diff_target_new_line(&rows, 2), Some(20));
        assert_eq!(diff_target_new_line(&rows, 3), Some(21));
        assert_eq!(diff_target_new_line(&rows, 4), Some(21));
    }

    #[test]
    fn diff_deleted_rows_prefer_nearest_surviving_file_line() {
        let raw = concat!(
            "@@ -10,5 +20,3 @@ fn main() {\n",
            "-removed before first new line\n",
            " ctx before\n",
            "-removed between context\n",
            " ctx after\n",
            "-removed at hunk end\n",
        );
        let rows = diff_render_rows(&parse_diff(raw));

        assert_eq!(
            diff_target_new_line(&rows, 1),
            Some(20),
            "a deletion before any new-file row lands at the hunk start"
        );
        assert_eq!(
            diff_target_new_line(&rows, 3),
            Some(20),
            "a deletion between two surviving rows lands on the nearest row; ties prefer the previous row"
        );
        assert_eq!(
            diff_target_new_line(&rows, 5),
            Some(21),
            "a deletion at hunk end falls back to the previous surviving row"
        );
    }

    #[test]
    fn align_table_pads_columns_to_widest_cell() {
        // Ragged input: row 2 has the widest first column ("ccc").
        let rows = vec![
            vec!["a".to_string(), "11".to_string()],
            vec!["ccc".to_string(), "2".to_string()],
        ];
        let out = align_table(&rows);
        assert_eq!(out, vec!["a   | 11".to_string(), "ccc | 2".to_string()]);
        // The last cell is never padded → no trailing whitespace.
        assert!(out.iter().all(|l| !l.ends_with(' ')));
        // Empty input → empty output.
        assert!(align_table(&[]).is_empty());
    }

    #[test]
    fn align_table_handles_cjk_and_uneven_rows() {
        // CJK counts as display-width 2; a short row must not panic on a missing column.
        let rows = vec![
            vec!["名字".to_string(), "x".to_string()],
            vec!["ab".to_string()],
        ];
        let out = align_table(&rows);
        // col0 width = max(disp("名字")=4, disp("ab")=2) = 4. row0 col0 is widest → no pad.
        assert_eq!(out[0], "名字 | x");
        // row1's only cell is its last → not padded.
        assert_eq!(out[1], "ab");
    }

    #[test]
    fn file_guard_detects_disk_conflict_only_when_snapshot_changes() {
        let guard = FileGuard::from_parts(
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10),
            5,
            file_sample_hash(b"hello"),
        );
        let same = FileGuard::from_parts(
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10),
            5,
            file_sample_hash(b"hello"),
        );
        assert_eq!(detect_conflict(Some(&guard), Some(&same)), Conflict::Clean);

        let changed = FileGuard::from_parts(
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(11),
            5,
            file_sample_hash(b"hullo"),
        );
        assert_eq!(
            detect_conflict(Some(&guard), Some(&changed)),
            Conflict::ModifiedOnDisk
        );
        assert_eq!(detect_conflict(Some(&guard), None), Conflict::MissingOnDisk);
        assert_eq!(detect_conflict(None, Some(&changed)), Conflict::Unknown);
    }

    #[test]
    fn external_reload_decision_refreshes_clean_file_but_preserves_dirty_edit() {
        let opened = FileGuard::from_parts(
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(10),
            5,
            file_sample_hash(b"hello"),
        );
        let changed = FileGuard::from_parts(
            std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(11),
            5,
            file_sample_hash(b"hullo"),
        );

        assert_eq!(
            external_reload_decision(false, false, Some(&opened), Some(&opened)),
            ExternalReloadDecision::Unchanged
        );
        assert_eq!(
            external_reload_decision(false, false, Some(&opened), Some(&changed)),
            ExternalReloadDecision::Reload,
            "preview Quick Look content should follow an external save"
        );
        assert_eq!(
            external_reload_decision(true, false, Some(&opened), Some(&changed)),
            ExternalReloadDecision::Unchanged,
            "edit mode must not be reloaded or interrupted by an external save"
        );
        assert_eq!(
            external_reload_decision(true, true, Some(&opened), Some(&changed)),
            ExternalReloadDecision::Unchanged,
            "dirty edit mode keeps the buffer; save-time guards report the conflict"
        );
        assert_eq!(
            external_reload_decision(false, true, Some(&opened), Some(&changed)),
            ExternalReloadDecision::Conflict(Conflict::ModifiedOnDisk),
            "dirty preview state still shows conflict instead of auto-refreshing"
        );
        assert_eq!(
            external_reload_decision(false, true, Some(&opened), None),
            ExternalReloadDecision::Conflict(Conflict::MissingOnDisk)
        );
    }

    #[test]
    fn local_file_guard_hashes_current_file_bytes() {
        let path = std::env::temp_dir().join(format!(
            "tn-local-file-guard-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::write(&path, b"hello").unwrap();

        let guard = local_file_guard(&path).expect("guard for existing file");
        assert_eq!(guard.size, 5);
        assert_eq!(guard.hash, file_sample_hash(b"hello"));

        std::fs::remove_file(&path).unwrap();
        assert!(local_file_guard(&path).is_none());
    }

    #[test]
    fn local_save_preserves_format_and_refuses_external_changes() {
        let path = std::env::temp_dir().join(format!(
            "tn-local-save-{}-{}.txt",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::SystemTime::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let original = [0xFF, 0xFE, b'a', 0, b'\r', 0, b'\n', 0, b'b', 0];
        std::fs::write(&path, original).unwrap();
        let opened_guard = local_file_guard(&path).expect("opened guard");
        let decoded = decode_text_bytes(&std::fs::read(&path).unwrap(), "txt").unwrap();

        std::fs::write(&path, b"changed on disk").unwrap();
        let lines = buf(&["alpha", "beta"]);
        let result = save_local_text(
            &path,
            &lines,
            decoded.format,
            Some(&opened_guard),
            SaveGuardMode::Check,
        );
        assert!(matches!(
            result,
            LocalSaveResult::Conflict(Conflict::ModifiedOnDisk)
        ));
        assert_eq!(std::fs::read(&path).unwrap(), b"changed on disk");

        let result = save_local_text(
            &path,
            &lines,
            decoded.format,
            Some(&opened_guard),
            SaveGuardMode::Force,
        );
        assert!(matches!(result, LocalSaveResult::Saved { .. }));
        assert_eq!(
            std::fs::read(&path).unwrap(),
            encode_text_lines(&lines, decoded.format)
        );

        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn decode_and_encode_preserves_newline_style_final_newline_and_encoding() {
        let lf = decode_text_bytes(b"one\ntwo", "txt").expect("utf8 text");
        assert_eq!(lf.lines, buf(&["one", "two"]));
        assert_eq!(lf.format.newline, NewlineStyle::Lf);
        assert!(!lf.format.final_newline);
        assert_eq!(encode_text_lines(&lf.lines, lf.format), b"one\ntwo");

        let crlf = decode_text_bytes(b"one\r\ntwo\r\n", "txt").expect("crlf text");
        assert_eq!(crlf.lines, buf(&["one", "two"]));
        assert_eq!(crlf.format.newline, NewlineStyle::Crlf);
        assert!(crlf.format.final_newline);
        assert_eq!(
            encode_text_lines(&crlf.lines, crlf.format),
            b"one\r\ntwo\r\n"
        );

        let utf16 = [0xFF, 0xFE, b'a', 0, b'\r', 0, b'\n', 0, b'b', 0];
        let decoded = decode_text_bytes(&utf16, "txt").expect("utf16 text");
        assert_eq!(decoded.lines, buf(&["a", "b"]));
        assert_eq!(decoded.format.encoding, TextEncoding::Utf16Le);
        assert_eq!(decoded.format.newline, NewlineStyle::Crlf);
        assert_eq!(
            encode_text_lines(&decoded.lines, decoded.format),
            utf16.to_vec()
        );
    }

    #[test]
    fn remote_preview_bytes_are_bounded_text_or_binary_and_editable_when_text() {
        let text =
            preview_payload_from_bytes(b"fn main() {}\n".to_vec(), "rs", Some(13), None).data;
        let QuickLookData::Text { lines, truncated } = &text else {
            panic!("expected text preview");
        };
        assert_eq!(lines.as_ref(), &buf(&["fn main() {}"]));
        assert!(!truncated);
        assert!(preview_is_editable(
            std::path::Path::new("main.rs"),
            &text,
            false
        ));
        assert!(preview_is_editable(
            std::path::Path::new("ssh://alice@example.com:22/home/alice/main.rs"),
            &text,
            true
        ));

        let binary = preview_payload_from_bytes(vec![0, 1, 2, 3], "bin", Some(4), None).data;
        assert!(matches!(binary, QuickLookData::Binary { size: 4 }));

        let too_large =
            preview_payload_from_bytes(Vec::new(), "log", Some(MAX_FILE_SIZE + 1), None).data;
        assert!(matches!(
            too_large,
            QuickLookData::Binary {
                size
            } if size == MAX_FILE_SIZE + 1
        ));
    }

    #[test]
    fn remote_file_guard_uses_stat_and_content_hash_for_conflicts() {
        let stat = RemoteFileStat {
            is_dir: false,
            size: Some(5),
            permissions: Some(0o100644),
            mtime: Some(22),
        };
        let guard = remote_file_guard(&stat, b"hello");
        assert_eq!(
            guard,
            FileGuard::from_parts(
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(22),
                5,
                file_sample_hash(b"hello"),
            )
        );

        let changed = RemoteFileStat {
            size: Some(5),
            mtime: Some(23),
            ..stat
        };
        let changed_guard = remote_file_guard(&changed, b"hullo");
        assert_eq!(
            detect_conflict(Some(&guard), Some(&changed_guard)),
            Conflict::ModifiedOnDisk
        );
    }

    #[test]
    fn preview_payload_keeps_text_format_and_remote_guard() {
        let stat = RemoteFileStat {
            is_dir: false,
            size: Some(14),
            permissions: Some(0o100644),
            mtime: Some(30),
        };
        let loaded =
            preview_payload_from_bytes(b"one\r\ntwo\r\n".to_vec(), "rs", Some(14), Some(&stat));
        let QuickLookData::Text { lines, truncated } = &loaded.data else {
            panic!("expected text payload");
        };
        assert_eq!(lines.as_ref(), &buf(&["one", "two"]));
        assert!(!truncated);
        assert_eq!(
            loaded.format,
            Some(TextFormat {
                encoding: TextEncoding::Utf8,
                newline: NewlineStyle::Crlf,
                final_newline: true,
            })
        );
        assert_eq!(
            loaded.guard,
            Some(FileGuard::from_parts(
                std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(30),
                14,
                file_sample_hash(b"one\r\ntwo\r\n"),
            ))
        );
    }

    #[test]
    fn save_success_marks_remote_diff_dirty_when_opened_from_remote_git_card() {
        assert_eq!(
            save_state_after_success(false, false),
            SaveStateUpdate {
                dirty: false,
                diff_dirty: true,
            },
            "local saves keep local git diff stale until recomputed"
        );
        assert_eq!(
            save_state_after_success(true, false),
            SaveStateUpdate {
                dirty: false,
                diff_dirty: false,
            },
            "plain remote file previews have no diff source"
        );
        assert_eq!(
            save_state_after_success(true, true),
            SaveStateUpdate {
                dirty: false,
                diff_dirty: true,
            },
            "remote git-card previews must refresh the remote diff after save"
        );
    }

    #[test]
    fn dirty_leave_decision_requires_confirmation_and_remembers_action() {
        let mut pending = None;

        assert_eq!(
            dirty_leave_decision(true, &mut pending, PendingLeave::Nav(1)),
            LeaveDecision::Confirm
        );
        assert_eq!(pending, Some(PendingLeave::Nav(1)));

        assert_eq!(
            dirty_leave_decision(true, &mut pending, PendingLeave::Tab(Tab::Diff)),
            LeaveDecision::Confirm
        );
        assert_eq!(pending, Some(PendingLeave::Tab(Tab::Diff)));

        assert_eq!(
            dirty_leave_decision(true, &mut pending, PendingLeave::Close),
            LeaveDecision::Confirm
        );
        assert_eq!(pending, Some(PendingLeave::Close));

        assert_eq!(
            dirty_leave_decision(true, &mut pending, PendingLeave::Quit),
            LeaveDecision::Confirm
        );
        assert_eq!(pending, Some(PendingLeave::Quit));

        assert_eq!(
            dirty_leave_decision(false, &mut pending, PendingLeave::Close),
            LeaveDecision::Continue
        );
        assert_eq!(pending, None);
    }
}
