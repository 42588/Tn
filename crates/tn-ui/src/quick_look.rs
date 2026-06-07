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

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::SystemTime;

use gpui::{
    canvas, div, linear_color_stop, linear_gradient, point, prelude::*, px, rgba, size,
    uniform_list, AsyncApp, Bounds, ClipboardItem, Context, ElementInputHandler,
    EntityInputHandler, FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, Rgba, ScrollStrategy, SharedString, UTF16Selection,
    UniformListScrollHandle, WeakEntity, Window,
};
use tn_config::Loaded;
use tn_pty::remote_cmd::SshCommandService;
use tn_pty::remote_fs::{
    remote_path_to_virtual_path, RemoteFileService, RemoteFileStat, RemoteId, SftpFileService,
    REMOTE_READ_LIMIT,
};

use crate::style::{
    col, cola, icon, quicklook_fill, quicklook_frame, HOVER, INSET, R_PANEL, UI_SANS,
};

/// A (row, char-col) position in the edit buffer.
type Pos = (usize, usize);

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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    File,
    Diff,
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

/// Emitted to the workspace for the few cross-entity needs (keyboard focus lives
/// on the overlay while it's open; these are the things it can't do alone).
pub enum QuickLookEvent {
    /// `↑↓` preview nav: move to the prev(-1)/next(+1) **file** in the tree.
    Nav(i32),
    /// `Esc`/`Space` in preview: close the overlay (give space back to the terminal).
    Close,
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
        pages: Arc<std::sync::Mutex<Vec<Option<Arc<gpui::Image>>>>>,
        page_count: usize,
    },
    Image {
        img: Arc<gpui::Image>,
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
            Self::ModifiedOnDisk => "远端文件已被其他进程修改",
            Self::MissingOnDisk => "远端文件已不存在",
            Self::Unknown => "无法确认远端文件状态",
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
        std::time::SystemTime::UNIX_EPOCH
            + std::time::Duration::from_secs(stat.mtime.unwrap_or(0)),
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
    /// the next time the Diff tab is viewed. (See 踩过的坑 + 架构蓝图 §8 ①.)
    diff_dirty: bool,
    /// Edit state (our own small modeless editor — see §16 / 架构蓝图 §8 ①).
    editing: bool,
    /// Editable buffer (copied from `file_lines` on entering edit; `Rc` so the
    /// `'static` render closure captures it cheaply, `make_mut` on each edit).
    buf: Rc<Vec<String>>,
    /// Cursor as (row, char-col); also the selection head.
    cursor: Pos,
    /// Selection anchor (head = `cursor`); `None` = no selection.
    sel_anchor: Option<Pos>,
    /// True while a left-drag text selection is in progress in the editor (mouse down
    /// on a row starts it, per-row mouse move extends it). Cleared on mouse up / when
    /// the button is found released (drag can end outside the panel — same bounds
    /// caveat as the horizontal scrollbar).
    edit_drag: bool,
    /// Undo / redo stacks of (buffer, cursor) snapshots (`Rc` → cheap to keep).
    undo: Vec<(Rc<Vec<String>>, Pos)>,
    redo: Vec<(Rc<Vec<String>>, Pos)>,
    /// Coalesce a run of single-char inserts into one undo step.
    coalesce_insert: bool,
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
    /// Virtualized code list scroll position (kept across frames per gpui).
    scroll: UniformListScrollHandle,
    /// Grab focus in the next render (focusing in an event/open callback doesn't
    /// land — the overlay isn't rendered yet; see 踩过的坑).
    needs_focus: bool,
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
    /// 编辑态横向 caret-follow 的去抖:只在光标**变化**时跟随一次,否则手动拖横滚条会被
    /// 每帧的 follow 立刻拉回(=「光标固定后拖不动」)。`None` ⇒ 下一帧无条件跟随一次。
    last_follow_cursor: Option<(usize, usize)>,
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
        Self {
            config,
            root,
            path: None,
            tab: Tab::File,
            file_data: QuickLookData::None,
            diff: Rc::new(Vec::new()),
            diff_dirty: true,
            editing: false,
            buf: Rc::new(Vec::new()),
            cursor: (0, 0),
            sel_anchor: None,
            edit_drag: false,
            undo: Vec::new(),
            redo: Vec::new(),
            coalesce_insert: false,
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
            scroll: UniformListScrollHandle::default(),
            needs_focus: false,
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
            edit_on_ready: false,
            diff_loading: false,
            diff_generation: 0,
            hunk_busy: false,
            hunk_error: None,
            cancel_token: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            hscroll_px: 0.0,
            hscroll_drag: None,
            hscroll_content_w: 0.0,
            last_follow_cursor: None,
        }
    }

    /// Whether a file is currently loaded (the workspace shows the overlay only
    /// when there is one — an empty overlay would float over nothing).
    pub fn has_file(&self) -> bool {
        self.path.is_some()
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

    /// Explicitly close QuickLook, evicting any GPUI caches and freeing memory capacity
    /// for HashMaps and large vectors to prevent "ghost" memory leaks when hidden.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        // --- EXPLICIT GPUI CACHE EVICTION ---
        match &self.file_data {
            QuickLookData::Image { img } => {
                img.clone().remove_asset(cx);
            }
            QuickLookData::Pdf { pages, .. } => {
                if let Ok(lock) = pages.lock() {
                    for page in lock.iter().flatten() {
                        page.clone().remove_asset(cx);
                    }
                }
            }
            _ => {}
        }

        // --- MEMORY CAPACITY RELEASE ---
        self.path = None;
        self.file_data = QuickLookData::None;
        self.buf = Rc::new(Vec::new());
        self.diff = Rc::new(Vec::new());
        self.undo = Vec::new();
        self.redo = Vec::new();
        self.ime_marked = None;

        // Replace HashMaps entirely to return their capacity to the OS!
        self.file_highlight_cache =
            std::rc::Rc::new(std::cell::RefCell::new(std::collections::HashMap::new()));

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
        self.hunk_busy = false;
        self.hunk_error = None;

        cx.notify();
    }

    fn reset_for_open(&mut self, path: PathBuf, is_remote: bool, cx: &mut Context<Self>) {
        // --- EXPLICIT GPUI CACHE EVICTION ---
        // GPUI caches textures and images globally. If we don't manually remove the old
        // image asset, switching between many large images will cause memory to grow
        // unboundedly (e.g. hitting 1GB+).
        match &self.file_data {
            QuickLookData::Image { img } => {
                img.clone().remove_asset(cx);
            }
            QuickLookData::Pdf { pages, .. } => {
                if let Ok(lock) = pages.lock() {
                    for page in lock.iter().flatten() {
                        page.clone().remove_asset(cx);
                    }
                }
            }
            _ => {}
        }

        self.path = Some(path.clone());
        self.root = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        self.tab = Tab::File;
        self.editing = false;
        self.sel_anchor = None; // 清上个文件残留的(预览)选区
        self.cursor = (0, 0);
        self.edit_drag = false;
        self.hscroll_px = 0.0; // 新文件从最左开始
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
        self.is_remote_source = is_remote;
        self.remote_source = None;
        self.remote_diff_file = None;
        self.text_format = None;
        self.opened_guard = None;
        self.save_conflict = None;
        self.save_error = None;
        self.save_in_flight = false;
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
                exec.spawn(async move {
                    use pdfium_render::prelude::*;
                    static PDFIUM: std::sync::OnceLock<Option<Pdfium>> = std::sync::OnceLock::new();
                    let pdfium_lock = PDFIUM.get_or_init(|| {
                        let exe_dir = std::env::current_exe().unwrap();
                        let workspace_dir = exe_dir
                            .parent()
                            .unwrap()
                            .parent()
                            .unwrap()
                            .parent()
                            .unwrap();
                        let pdfium_dll = workspace_dir.join("pdfium.dll");
                        let bind_result = Pdfium::bind_to_system_library()
                            .or_else(|_| Pdfium::bind_to_library(&pdfium_dll));
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
                                        let mut cursor = std::io::Cursor::new(Vec::new());
                                        let rgb_img =
                                            image::DynamicImage::ImageRgb8(img.into_rgb8());
                                        if rgb_img
                                            .write_to(&mut cursor, image::ImageFormat::Jpeg)
                                            .is_ok()
                                        {
                                            let gpui_img = gpui::Image::from_bytes(
                                                gpui::ImageFormat::Jpeg,
                                                cursor.into_inner(),
                                            );
                                            let _ = tx.unbounded_send(Ok((
                                                limit,
                                                Some((i, Arc::new(gpui_img))),
                                            )));
                                        }
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
                let mut pages_arc: Option<Arc<std::sync::Mutex<Vec<Option<Arc<gpui::Image>>>>>> =
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
                                        img.clone().remove_asset(cx);
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
                let bytes_res = cx
                    .background_executor()
                    .spawn(async move {
                        if img_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::Interrupted,
                                "Cancelled",
                            ));
                        }
                        let bytes = std::fs::read(&path_for_bg)?;
                        if img_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                            return Err(std::io::Error::new(
                                std::io::ErrorKind::Interrupted,
                                "Cancelled",
                            ));
                        }
                        let fmt = match ext.as_str() {
                            "png" => gpui::ImageFormat::Png,
                            "jpg" | "jpeg" => gpui::ImageFormat::Jpeg,
                            "webp" => gpui::ImageFormat::Webp,
                            "gif" => gpui::ImageFormat::Gif,
                            "bmp" => gpui::ImageFormat::Bmp,
                            _ => gpui::ImageFormat::Png,
                        };
                        Ok(gpui::Image::from_bytes(fmt, bytes))
                    })
                    .await;

                if cancel_token.load(std::sync::atomic::Ordering::Relaxed) {
                    return;
                }

                if let Ok(img) = bytes_res {
                    let _ = this.update(cx, |v, cx| {
                        if v.generation != gen {
                            Arc::new(img).remove_asset(cx);
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
            let res = exec
                .spawn(async move {
                    if txt_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return QuickLookData::None;
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
                                        return QuickLookData::None;
                                    }
                                    lines.push(line.to_string());
                                }
                                let truncated = line_iter.next().is_some();
                                return QuickLookData::Text {
                                    lines: Arc::new(lines),
                                    truncated,
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
                                            return QuickLookData::None;
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
                                return QuickLookData::Text {
                                    lines: Arc::new(align_table(&cells)),
                                    truncated,
                                };
                            }
                        }
                    }

                    if size > MAX_FILE_SIZE || is_binary {
                        return QuickLookData::Binary { size };
                    }

                    if txt_cancel.load(std::sync::atomic::Ordering::Relaxed) {
                        return QuickLookData::None;
                    }

                    let mut text = String::new();
                    if let Ok(bytes) = std::fs::read(&path) {
                        if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
                            text = String::from_utf8_lossy(&bytes[3..]).into_owned();
                        } else if bytes.starts_with(&[0xFF, 0xFE]) {
                            let (cow, _, _) = encoding_rs::UTF_16LE.decode(&bytes[2..]);
                            text = cow.into_owned();
                        } else if bytes.starts_with(&[0xFE, 0xFF]) {
                            let (cow, _, _) = encoding_rs::UTF_16BE.decode(&bytes[2..]);
                            text = cow.into_owned();
                        } else {
                            if let Ok(utf8_str) = std::str::from_utf8(&bytes) {
                                text = utf8_str.to_string();
                            } else {
                                let (cow, _, _) = encoding_rs::GBK.decode(&bytes);
                                text = cow.into_owned();
                            }
                        }
                    }
                    let mut line_iter = text.lines();
                    let mut lines = Vec::with_capacity(MAX_LINES.min(1000));
                    for line in (&mut line_iter).take(MAX_LINES) {
                        lines.push(line.to_string());
                    }
                    let truncated = line_iter.next().is_some();
                    QuickLookData::Text {
                        lines: Arc::new(lines),
                        truncated,
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
                v.file_data = res;
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
        self.open(path, cx);
        self.select_tab(Tab::Diff, cx);
    }

    pub(crate) fn open_remote_diff(
        &mut self,
        file: crate::remote_git::RemoteGitFile,
        cx: &mut Context<Self>,
    ) {
        let id = RemoteId::new(&file.cfg, file.remote_path().as_str());
        self.open_remote(file.cfg.clone(), id, None, cx);
        self.remote_diff_file = Some(file);
        self.diff_dirty = true;
        self.select_tab(Tab::Diff, cx);
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
                    let text =
                        crate::remote_git::diff_for_remote_file(service.as_ref(), &file)
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
        self.tab = tab;
        self.hscroll_px = 0.0; // File↔Diff 内容宽不同,切换从最左开始,不残留横滚
        if tab == Tab::Diff {
            self.ensure_diff(cx);
        }
    }

    /// Enter edit mode: copy the file into the editable buffer, cursor at (0,0).
    fn enter_edit(&mut self) {
        if let QuickLookData::Text { lines, .. } = &self.file_data {
            self.buf = Rc::new(lines.as_ref().clone());
        } else {
            self.buf = Rc::new(Vec::new());
        }
        if self.buf.is_empty() {
            Rc::make_mut(&mut self.buf).push(String::new());
        }
        self.cursor = (0, 0);
        self.sel_anchor = None;
        self.undo.clear();
        self.redo.clear();
        self.coalesce_insert = false;
        self.hscroll_px = 0.0; // 进编辑从最左开始
        self.last_follow_cursor = None;
        self.editing = true;
        self.dirty = false;
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
        let joined = self.buf.join("\n");
        let content = if joined.is_empty() {
            joined
        } else {
            format!("{joined}\n")
        };
        match std::fs::write(&path, content) {
            Ok(()) => {
                let update = save_state_after_success(false, false);
                self.dirty = update.dirty;
                self.file_data = QuickLookData::Text {
                    lines: Arc::new(self.buf.as_ref().clone()),
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
            }
            Err(e) => tracing::error!(path = %path.display(), error = %e, "quick look save failed"),
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
        let bytes = encode_text_lines(self.buf.as_ref(), format);
        let lines = self.buf.as_ref().clone();
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
                        Err(e) => return RemoteSaveResult::Error(format!("Remote stat failed: {e}")),
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
                        let update =
                            save_state_after_success(true, v.remote_diff_file.is_some());
                        v.dirty = update.dirty;
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

    fn cancel_save_conflict(&mut self, cx: &mut Context<Self>) {
        self.save_conflict = None;
        self.save_error = None;
        cx.notify();
    }

    fn reload_remote_source(&mut self, cx: &mut Context<Self>) {
        let Some(source) = self.remote_source.clone() else {
            return;
        };
        self.loading_state = LoadingState::Loading;
        self.open_remote(source.cfg, source.id, None, cx);
    }

    // ── selection / undo helpers ──

    /// Active selection range (normalized `start ≤ end`), or `None` when collapsed.
    fn sel_range(&self) -> Option<(Pos, Pos)> {
        let a = self.sel_anchor?;
        if a == self.cursor {
            None
        } else if a <= self.cursor {
            Some((a, self.cursor))
        } else {
            Some((self.cursor, a))
        }
    }

    fn has_sel(&self) -> bool {
        self.sel_range().is_some()
    }

    /// Push the current (buffer, cursor) onto the undo stack (clearing redo).
    /// `coalesce` = part of a run of single-char inserts → only the first snapshots.
    fn snapshot(&mut self, coalesce: bool) {
        if coalesce && self.coalesce_insert {
            return; // same insert run — already captured at its start
        }
        self.undo.push((self.buf.clone(), self.cursor));
        if self.undo.len() > 100 {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.coalesce_insert = coalesce;
    }

    fn undo(&mut self) {
        if let Some((buf, cur)) = self.undo.pop() {
            self.redo.push((self.buf.clone(), self.cursor));
            self.buf = buf;
            self.cursor = cur;
            self.sel_anchor = None;
            self.coalesce_insert = false;
            self.dirty = true;
        }
    }

    fn redo(&mut self) {
        if let Some((buf, cur)) = self.redo.pop() {
            self.undo.push((self.buf.clone(), self.cursor));
            self.buf = buf;
            self.cursor = cur;
            self.sel_anchor = None;
            self.coalesce_insert = false;
            self.dirty = true;
        }
    }

    /// Delete the active selection (no snapshot — the caller already took one).
    fn delete_sel_inner(&mut self) {
        if let Some((s, e)) = self.sel_range() {
            op_delete_range(Rc::make_mut(&mut self.buf), s, e);
            self.cursor = s;
            self.sel_anchor = None;
            self.dirty = true;
        }
    }

    // ── editor ops (selection-aware; buffer math in pure `op_*` fns, unit-tested) ──

    fn type_char(&mut self, ch: &str) {
        if self.has_sel() {
            self.snapshot(false);
            self.delete_sel_inner();
        } else {
            self.snapshot(true); // coalesce a typing run
        }
        op_insert(Rc::make_mut(&mut self.buf), &mut self.cursor, ch);
        self.sel_anchor = None;
        self.dirty = true;
    }

    fn newline(&mut self) {
        self.snapshot(false);
        self.delete_sel_inner();
        op_newline(Rc::make_mut(&mut self.buf), &mut self.cursor);
        self.sel_anchor = None;
        self.dirty = true;
    }

    fn indent(&mut self) {
        self.snapshot(false);
        self.delete_sel_inner();
        op_insert(Rc::make_mut(&mut self.buf), &mut self.cursor, "    ");
        self.sel_anchor = None;
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.has_sel() {
            self.snapshot(false);
            self.delete_sel_inner();
            return;
        }
        if self.cursor == (0, 0) {
            return; // no-op, don't pollute undo
        }
        self.snapshot(false);
        self.dirty |= op_backspace(Rc::make_mut(&mut self.buf), &mut self.cursor);
    }

    fn delete_forward(&mut self) {
        if self.has_sel() {
            self.snapshot(false);
            self.delete_sel_inner();
            return;
        }
        let (r, c) = self.cursor;
        if r + 1 >= self.buf.len() && c >= line_chars(&self.buf, r) {
            return; // at end of buffer, no-op
        }
        self.snapshot(false);
        self.dirty |= op_delete(Rc::make_mut(&mut self.buf), &mut self.cursor);
    }

    /// Move the cursor; `extend` keeps/starts the selection (Shift held).
    fn move_cursor(&mut self, key: &str, extend: bool) {
        self.coalesce_insert = false;
        if !extend {
            // Collapsing a selection lands at its near/far edge per direction.
            if let Some((s, e)) = self.sel_range() {
                self.sel_anchor = None;
                match key {
                    "left" | "up" | "home" => {
                        self.cursor = s;
                        return;
                    }
                    "right" | "down" | "end" => {
                        self.cursor = e;
                        return;
                    }
                    _ => {}
                }
            }
            self.sel_anchor = None;
        } else if self.sel_anchor.is_none() {
            self.sel_anchor = Some(self.cursor);
        }
        op_move(&self.buf, &mut self.cursor, key);
    }

    fn page(&mut self, dir: i32, extend: bool) {
        self.coalesce_insert = false;
        if extend && self.sel_anchor.is_none() {
            self.sel_anchor = Some(self.cursor);
        } else if !extend {
            self.sel_anchor = None;
        }
        op_page(&self.buf, &mut self.cursor, dir);
    }

    fn select_all(&mut self) {
        self.coalesce_insert = false;
        let (last, last_len) = if self.editing {
            let last = self.buf.len().saturating_sub(1);
            (last, line_chars(&self.buf, last))
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
        self.coalesce_insert = false;
        // 行/列 clamp 的来源:编辑态是 `buf`,预览态(只读拖选)是 file_data 的 lines。
        let (r, c) = if self.editing {
            let r = row.min(self.buf.len().saturating_sub(1));
            (r, col.min(line_chars(&self.buf, r)))
        } else if let QuickLookData::Text { lines, .. } = &self.file_data {
            let r = row.min(lines.len().saturating_sub(1));
            (
                r,
                col.min(lines.get(r).map(|l| l.chars().count()).unwrap_or(0)),
            )
        } else {
            return; // 非文本预览(图片 / PDF)不可选
        };
        if extend {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
        } else {
            self.sel_anchor = None;
        }
        self.cursor = (r, c);
    }

    /// Text of display row `row` for mouse hit-testing: editing → live `buf`,
    /// read-only preview → the `Text` file lines. `None` for non-text previews.
    fn row_text(&self, row: usize) -> Option<&str> {
        if self.editing {
            self.buf.get(row).map(|s| s.as_str())
        } else if let QuickLookData::Text { lines, .. } = &self.file_data {
            lines.get(row).map(|s| s.as_str())
        } else {
            None
        }
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
        cx.notify();
    }

    // ── clipboard ──

    fn copy(&mut self, cx: &mut Context<Self>) {
        if self.editing {
            let text = match self.sel_range() {
                Some((s, e)) => selected_text(&self.buf, s, e),
                None => format!(
                    "{}\n",
                    self.buf.get(self.cursor.0).cloned().unwrap_or_default()
                ),
            };
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        } else if let QuickLookData::Text { lines, .. } = &self.file_data {
            // 预览态(只读):仅在有选区时复制选中文本(基于 lines,不碰 buf)。
            if let Some((s, e)) = self.sel_range() {
                cx.write_to_clipboard(ClipboardItem::new_string(selected_text(lines, s, e)));
            }
        }
    }

    fn cut(&mut self, cx: &mut Context<Self>) {
        if self.has_sel() {
            let (s, e) = self.sel_range().unwrap();
            cx.write_to_clipboard(ClipboardItem::new_string(selected_text(&self.buf, s, e)));
            self.snapshot(false);
            self.delete_sel_inner();
        } else {
            let line = self.buf.get(self.cursor.0).cloned().unwrap_or_default();
            cx.write_to_clipboard(ClipboardItem::new_string(format!("{line}\n")));
            self.snapshot(false);
            let r = self.cursor.0;
            let buf = Rc::make_mut(&mut self.buf);
            if buf.len() > 1 {
                buf.remove(r);
                self.cursor = (r.min(buf.len() - 1), 0);
            } else {
                buf[0].clear();
                self.cursor = (0, 0);
            }
            self.dirty = true;
        }
    }

    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        self.snapshot(false);
        self.delete_sel_inner();
        op_insert_multiline(Rc::make_mut(&mut self.buf), &mut self.cursor, &text);
        self.sel_anchor = None;
        self.dirty = true;
    }

    // ── find / replace ──

    fn open_find(&mut self, replacing: bool) {
        self.find_open = true;
        self.replacing = replacing;
        self.find_field_replace = false;
        // Prefill the query from a single-line selection.
        if let Some((s, e)) = self.sel_range() {
            if s.0 == e.0 {
                self.find_query = selected_text(&self.buf, s, e);
            }
        }
    }

    /// Move to the next(`forward`)/prev match of the query (wraps), selecting it.
    fn find_next(&mut self, forward: bool) {
        let matches = all_matches(&self.buf, &self.find_query);
        if matches.is_empty() {
            return;
        }
        let cur = self.cursor;
        let idx = if forward {
            matches.iter().position(|(s, _)| *s > cur).unwrap_or(0)
        } else {
            matches
                .iter()
                .rposition(|(s, _)| *s < cur)
                .unwrap_or(matches.len() - 1)
        };
        let (s, e) = matches[idx];
        self.sel_anchor = Some(s);
        self.cursor = e;
        self.scroll.scroll_to_item(s.0, ScrollStrategy::Center);
    }

    fn replace_current(&mut self) {
        if self.find_query.is_empty() {
            return;
        }
        if let Some((s, e)) = self.sel_range() {
            if selected_text(&self.buf, s, e) == self.find_query {
                self.snapshot(false);
                op_delete_range(Rc::make_mut(&mut self.buf), s, e);
                self.cursor = s;
                self.sel_anchor = None;
                op_insert_multiline(
                    Rc::make_mut(&mut self.buf),
                    &mut self.cursor,
                    &self.replace_query,
                );
                self.dirty = true;
            }
        }
        self.find_next(true);
    }

    fn replace_all(&mut self) {
        if self.find_query.is_empty() {
            return;
        }
        self.snapshot(false);
        let n = replace_all_in(
            Rc::make_mut(&mut self.buf),
            &self.find_query,
            &self.replace_query,
        );
        if n > 0 {
            self.dirty = true;
            self.cursor = (0, 0);
            self.sel_anchor = None;
        }
    }

    /// Find-bar keystrokes (the bar captures input while open).
    fn find_key(&mut self, key: &str, key_char: Option<&str>, shift: bool) {
        match key {
            "escape" => self.find_open = false,
            "enter" => {
                if self.replacing && self.find_field_replace {
                    self.replace_current();
                } else {
                    self.find_next(!shift); // Enter = next, Shift+Enter = prev
                }
            }
            "tab" => {
                if self.replacing {
                    self.find_field_replace = !self.find_field_replace;
                }
            }
            "backspace" => {
                let q = if self.find_field_replace {
                    &mut self.replace_query
                } else {
                    &mut self.find_query
                };
                q.pop();
            }
            _ => {
                if let Some(ch) = key_char.filter(|s| !s.is_empty()) {
                    let q = if self.find_field_replace {
                        &mut self.replace_query
                    } else {
                        &mut self.find_query
                    };
                    q.push_str(ch);
                }
            }
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
            // The find bar captures plain input while it's open.
            if self.find_open {
                self.find_key(key, ks.key_char.as_deref(), m.shift);
                self.scroll
                    .scroll_to_item(self.cursor.0, ScrollStrategy::Center);
                cx.stop_propagation();
                cx.notify();
                return;
            }
            let shift = m.shift;
            let mut handled = true;
            match key {
                "escape" => {
                    self.sel_anchor = None;
                    self.editing = false; // exit edit → preview (stay focused)
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
                "up" => {
                    cx.emit(QuickLookEvent::Nav(-1));
                    cx.stop_propagation();
                }
                "down" => {
                    cx.emit(QuickLookEvent::Nav(1));
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

/// Parse `git diff --no-color` output into renderable lines (tracking new-file line
/// numbers from each hunk header). Pure → unit-testable headless.
fn parse_diff(text: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut new_no = 0u32;
    // 0-based hunk counter — kept in lockstep with `remote_git::parse_file_diff`
    // (both skip the same header lines, count `@@` in order) so a clicked hunk
    // header maps back to the right `FileDiff` hunk for accept/reject.
    let mut hunk_no = 0usize;
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
                .text_size(px(11.))
                .text_color(gpui::rgb(0x474E72)) // faint(无主题 token,字面量)
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

/// Char index → byte offset within `line` (cursor cols are char-based).
fn char_to_byte(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map(|(b, _)| b)
        .unwrap_or(line.len())
}

/// Char length of buffer line `i` (0 if out of range).
fn line_chars(buf: &[String], i: usize) -> usize {
    buf.get(i).map(|l| l.chars().count()).unwrap_or(0)
}

// ── pure editor ops over (buffer, cursor) — unit-tested headless (no gpui) ──

/// Insert `s` at the cursor (no newlines), advancing the cursor past it.
fn op_insert(buf: &mut Vec<String>, cur: &mut (usize, usize), s: &str) {
    if buf.is_empty() {
        buf.push(String::new());
    }
    let (r, c) = *cur;
    let byte = char_to_byte(&buf[r], c);
    buf[r].insert_str(byte, s);
    cur.1 = c + s.chars().count();
}

/// Split the line at the cursor → new line; cursor to its start.
fn op_newline(buf: &mut Vec<String>, cur: &mut (usize, usize)) {
    if buf.is_empty() {
        buf.push(String::new());
    }
    let (r, c) = *cur;
    let byte = char_to_byte(&buf[r], c);
    let tail = buf[r].split_off(byte);
    buf.insert(r + 1, tail);
    *cur = (r + 1, 0);
}

/// Delete the char before the cursor (or merge with the previous line). Returns
/// whether the buffer changed.
fn op_backspace(buf: &mut Vec<String>, cur: &mut (usize, usize)) -> bool {
    let (r, c) = *cur;
    if c > 0 {
        let b0 = char_to_byte(&buf[r], c - 1);
        let b1 = char_to_byte(&buf[r], c);
        buf[r].replace_range(b0..b1, "");
        cur.1 = c - 1;
        true
    } else if r > 0 {
        let line = buf.remove(r);
        let prev_len = buf[r - 1].chars().count();
        buf[r - 1].push_str(&line);
        *cur = (r - 1, prev_len);
        true
    } else {
        false
    }
}

/// Delete the char at the cursor (or join the next line). Returns whether changed.
fn op_delete(buf: &mut Vec<String>, cur: &mut (usize, usize)) -> bool {
    let (r, c) = *cur;
    let len = buf.get(r).map(|l| l.chars().count()).unwrap_or(0);
    if c < len {
        let b0 = char_to_byte(&buf[r], c);
        let b1 = char_to_byte(&buf[r], c + 1);
        buf[r].replace_range(b0..b1, "");
        true
    } else if r + 1 < buf.len() {
        let next = buf.remove(r + 1);
        buf[r].push_str(&next);
        true
    } else {
        false
    }
}

/// Move the cursor for an arrow / home / end key (clamps to line/buffer bounds).
fn op_move(buf: &[String], cur: &mut (usize, usize), key: &str) {
    let (r, c) = *cur;
    match key {
        "left" => {
            if c > 0 {
                cur.1 = c - 1;
            } else if r > 0 {
                *cur = (r - 1, line_chars(buf, r - 1));
            }
        }
        "right" => {
            if c < line_chars(buf, r) {
                cur.1 = c + 1;
            } else if r + 1 < buf.len() {
                *cur = (r + 1, 0);
            }
        }
        "up" => {
            if r > 0 {
                *cur = (r - 1, c.min(line_chars(buf, r - 1)));
            }
        }
        "down" => {
            if r + 1 < buf.len() {
                *cur = (r + 1, c.min(line_chars(buf, r + 1)));
            }
        }
        "home" => cur.1 = 0,
        "end" => cur.1 = line_chars(buf, r),
        _ => {}
    }
}

/// Move the cursor a page (≈12 rows) up(-1)/down(+1), clamping the column.
fn op_page(buf: &[String], cur: &mut (usize, usize), dir: i32) {
    const PAGE: usize = 12;
    let (r, c) = *cur;
    let nr = if dir < 0 {
        r.saturating_sub(PAGE)
    } else {
        (r + PAGE).min(buf.len().saturating_sub(1))
    };
    *cur = (nr, c.min(line_chars(buf, nr)));
}

/// Delete the normalized range `[s, e)` from the buffer (`s ≤ e`).
fn op_delete_range(buf: &mut Vec<String>, s: (usize, usize), e: (usize, usize)) {
    if buf.is_empty() {
        return;
    }
    if s.0 == e.0 {
        let b0 = char_to_byte(&buf[s.0], s.1);
        let b1 = char_to_byte(&buf[s.0], e.1);
        buf[s.0].replace_range(b0..b1, "");
    } else {
        let head: String = buf[s.0].chars().take(s.1).collect();
        let tail: String = buf[e.0].chars().skip(e.1).collect();
        buf.drain(s.0 + 1..=e.0.min(buf.len() - 1));
        buf[s.0] = head + &tail;
    }
}

/// Insert `text` (may contain `\n`) at the cursor, leaving it after the insert.
fn op_insert_multiline(buf: &mut Vec<String>, cur: &mut (usize, usize), text: &str) {
    let parts: Vec<&str> = text.split('\n').collect();
    if parts.len() == 1 {
        op_insert(buf, cur, parts[0]);
        return;
    }
    if buf.is_empty() {
        buf.push(String::new());
    }
    let (r, c) = *cur;
    let byte = char_to_byte(&buf[r], c);
    let tail = buf[r].split_off(byte);
    buf[r].push_str(parts[0]);
    let mut at = r + 1;
    for mid in &parts[1..parts.len() - 1] {
        buf.insert(at, mid.to_string());
        at += 1;
    }
    let last = parts[parts.len() - 1];
    let last_col = last.chars().count();
    buf.insert(at, format!("{last}{tail}"));
    *cur = (at, last_col);
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
        .h(px(16.))
        .flex_none()
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
    // 反相块光标(终端式):光标处字符以光标色为底、面板底色为字 → 就地反色成实心块,
    // 瞬时、精确(固定单元格下块 = 该字符格)、随字符列宽(中文 2 列宽、英文 1 列细)。
    let caret_bg = col(config.theme.ui.foreground);
    let caret_fg = col(config.theme.ui.chrome_bg);
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
        // IME commit (中文) or a printable WM_CHAR → insert at the cursor like typed
        // text (op handles multi-char + selection + undo). Empty `text` = composition
        // cancel. (Backspace is encoded in `on_key`, never routed here.)
        if !text.is_empty() {
            self.type_char(text);
            self.scroll
                .scroll_to_item(self.cursor.0, ScrollStrategy::Center);
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
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // Candidate window at the cursor: column is exact (gutter + col×char_w); the
        // row is approximated to the code area's vertical center (edits scroll the
        // cursor to center, and `uniform_list`'s scroll offset isn't readable in
        // production — see坑), which is close enough for the IME popup.
        let x =
            f32::from(element_bounds.origin.x) + CODE_GUTTER + self.cursor.1 as f32 * self.char_w;
        let y = f32::from(element_bounds.origin.y) + f32::from(element_bounds.size.height) * 0.5;
        Some(Bounds {
            origin: point(px(x), px(y)),
            size: size(px(self.char_w.max(1.0)), px(ROW_H)),
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
                        this.select_tab(to, cx);
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
            // mockup .vh .by:编辑态 = 「编辑中(●)」,预览态有未提交改动 = 「已改动」(claude)
            .when(
                self.editing
                    || !self.diff.is_empty()
                    || self.save_in_flight
                    || self.save_conflict.is_some()
                    || self.save_error.is_some(),
                |d| {
                    let (label, color) = if self.save_in_flight {
                        ("保存中", th.ansi.yellow)
                    } else if self.save_conflict.is_some() {
                        ("保存冲突", th.ansi.red)
                    } else if self.save_error.is_some() {
                        ("保存失败", th.ansi.red)
                    } else if self.editing {
                        if self.dirty {
                            ("编辑中 ●", th.agents.claude)
                        } else {
                            ("编辑中", th.agents.claude)
                        }
                    } else {
                        ("已改动", th.agents.claude)
                    };
                    d.child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap(px(5.)) // §16 .vh .by gap 5
                            .text_size(px(11.))
                            .text_color(col(color))
                            .child(icon("pen", 13., color))
                            .child(label),
                    )
                },
            )
            .child(tabset);

        // ── .code body:**虚拟化**列表(uniform_list 只渲染可见行 → 大文件不卡)。
        //    编辑态从 buf 渲染(高亮 + 选区 + 光标);预览态从 file_lines / diff 渲染。──
        let (lines, truncated) = match &self.file_data {
            QuickLookData::Text { lines, truncated } => (lines.clone(), *truncated),
            _ => (Arc::new(Vec::new()), false),
        };
        let line_count = lines.len();
        let buf = self.buf.clone();
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
        // Per-row click → place cursor (mouse). The row index `i` is known here, so
        // we only map x → column (gutter + measured char width); no scroll-offset
        // math needed. Capture a weak handle (the 'static closure can't borrow self).
        let entity = cx.entity().downgrade();
        let char_w = self.char_w;
        let canvas_bounds = self.code_bounds.clone(); // for the capturing canvas
        let row_bounds = self.code_bounds.clone(); // for the per-row click handler
        const GUTTER: f32 = CODE_GUTTER; // ln(38) + mr(14) + mk(14)
                                         // IME/text input handler captures (registered in the canvas paint below) —
                                         // only while editing AND the find bar is closed (else composed/typed text
                                         // would wrongly insert into the buffer instead of the find query; find typing
                                         // stays on the `find_key`/key_char path).
        let ime_focus = self.focus_handle.clone();
        let ime_entity = cx.entity();
        let ime_active = editing && !self.find_open;
        let count = if editing {
            buf.len()
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
                    .child(div().mt_4().text_size(px(14.)).child("无法预览此文件"))
                    .child(
                        div()
                            .mt_2()
                            .text_size(px(12.))
                            .child(format!("二进制文件或超过大小限制 ({size_str})")),
                    ),
            );
        } else if let QuickLookData::Pdf { pages, page_count } = &self.file_data {
            let pages = pages.clone();
            let page_count = *page_count;
            body = body.child(
                div().flex_1().overflow_hidden().bg(rgba(0x1e1e1e)).child(
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
                                            let img_source = gpui::ImageSource::Image(img.clone());
                                            return div()
                                                .w_full()
                                                .h(px(1400.)) // 固定行高让 uniform_list 计算(只 measure row 0)
                                                .bg(rgba(0x1e1e1e))
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
                                    div().w_full().h(px(1400.)).bg(rgba(0x1e1e1e))
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
            let img_source = gpui::ImageSource::Image(img.clone());
            body = body.child(
                div()
                    .w_full()
                    .h_full()
                    .flex()
                    .justify_center()
                    .items_center()
                    .bg(rgba(0x1e1e1e)) // 暗色背景
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
        } else {
            let _sel_anchor = sel.as_ref().map(|s| s.0);
            // Longest line's display width (cols), computed BEFORE the list closure
            // moves `lines`/`diff`. Drives the horizontal-scroll content width below.
            // `disp_width` counts CJK as 2 cols so wide-char lines aren't under-sized.
            let max_cols = if editing {
                buf.iter().map(|l| disp_width(l)).max().unwrap_or(0)
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
                            let line = &buf[i];
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
                                                caret_col_at_x(l, rel + this.hscroll_px, char_w)
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
                                                hover_char_at_x(l, rel + this.hscroll_px, char_w)
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
                            let row = if sel.map_or(false, |(s, e)| i >= s.0 && i <= e.0) {
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
                                                caret_col_at_x(l, rel + this.hscroll_px, char_w)
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
                                                hover_char_at_x(l, rel + this.hscroll_px, char_w)
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
                            let hunk_idx = if is_remote_diff
                                && matches!(diff[i].kind, DiffKind::Hunk)
                            {
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
                                        .text_size(px(10.))
                                        .font_weight(gpui::FontWeight(640.))
                                        .text_color(if hunk_busy {
                                            col(th.ui.muted)
                                        } else {
                                            col(c)
                                        })
                                        .bg(if hunk_busy {
                                            rgba(INSET)
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
                                    .child(
                                        hbtn("接受", th.ansi.green).on_mouse_down(
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
                                        ),
                                    )
                                    .child(
                                        hbtn("拒绝", th.ansi.red).on_mouse_down(
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
                                        ),
                                    )
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
        let footer_base = div()
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
            .border_color(rgba(0xffffff0d)); // mockup .qlfoot border-top 白 .05 = round(.05×255)=13=0x0d
        let footer = if self.editing {
            // 编辑态:Ctrl+S 保存 · Ctrl+F 查找 · Esc 退出编辑 [sp] 选择/复制/撤销
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
        } else if self.is_editable() {
            // 预览态(可编辑文本文件):↑↓ 换文件 · ⇥ 切 File · Enter 编辑 · Esc 关闭
            footer_base
                .child(kcap("↑↓"))
                .child("换文件 ·")
                .child(kcap("⇥"))
                .child("切 File ·")
                .child(kcap("Enter"))
                .child("编辑")
                .child(div().flex_1())
                .child("Diff 只读审阅 ·")
                .child(kcap("Esc"))
                .child("关闭")
        } else {
            // 预览态(PDF / 图片 / Office / 二进制 — 只读):↑↓ 换文件 · ⇥ 切 File · Esc 关闭
            footer_base
                .child(kcap("↑↓"))
                .child("换文件 ·")
                .child(kcap("⇥"))
                .child("切 File ·")
                .child(div().flex_1())
                .child("只读预览 ·")
                .child(kcap("Esc"))
                .child("关闭")
        };

        // ── 查找/替换条(编辑态 Ctrl+F / Ctrl+H 唤出;输入由 on_key 的 find_key 捕获)──
        let mono = SharedString::from(self.config.font().family.clone());
        let find_bar = (self.editing && self.find_open).then(|| {
            let field = |label: &'static str, text: &str, active: bool| {
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(10.))
                            .text_color(col(ui.muted))
                            .child(label),
                    )
                    .child(
                        div()
                            .min_w(px(140.))
                            .px(px(7.))
                            .py(px(2.))
                            .rounded(px(6.))
                            .bg(rgba(INSET))
                            .border_1()
                            .border_color(if active {
                                cola(ui.accent, 0.5)
                            } else {
                                rgba(0x00000000)
                            })
                            .font_family(mono.clone())
                            .text_size(px(11.))
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
                            })),
                    )
            };
            let n = all_matches(&self.buf, &self.find_query).len();
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
                .border_color(rgba(0xffffff0d))
                .child(field("查找", &self.find_query, !self.find_field_replace))
                .when(self.replacing, |d| {
                    d.child(field("替换", &self.replace_query, self.find_field_replace))
                })
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(col(ui.muted))
                        .child(SharedString::from(format!("{n} 项"))),
                )
                .child(kcap("Enter"))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(col(ui.muted))
                        .child("下一个"),
                )
                .when(self.replacing, |d| {
                    d.child(kcap("Ctrl+↵")).child(
                        div()
                            .text_size(px(10.))
                            .text_color(col(ui.muted))
                            .child("全部替换"),
                    )
                })
                .child(kcap("Esc"))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(col(ui.muted))
                        .child("关闭"),
                )
        });

        let save_notice = self
            .save_conflict
            .map(|conflict| {
                let action = |label: &'static str, danger: bool| {
                    div()
                        .px(px(9.))
                        .py(px(2.))
                        .rounded(px(7.))
                        .text_size(px(10.5))
                        .font_weight(gpui::FontWeight(620.))
                        .text_color(col(if danger { th.ansi.red } else { ui.foreground }))
                        .bg(if danger { cola(th.ansi.red, 0.10) } else { rgba(INSET) })
                        .border_1()
                        .border_color(if danger {
                            cola(th.ansi.red, 0.32)
                        } else {
                            rgba(0xffffff14)
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
                    .text_size(px(10.5))
                    .text_color(col(ui.muted))
                    .bg(cola(th.ansi.red, 0.06))
                    .border_t_1()
                    .border_color(rgba(0xffffff0d))
                    .child(icon("alert", 13., th.ansi.red))
                    .child(
                        div()
                            .text_color(col(th.ansi.red))
                            .child(SharedString::from(conflict.label())),
                    )
                    .child(div().flex_1())
                    .child(
                        action("重新载入", false).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.reload_remote_source(cx)),
                        ),
                    )
                    .child(
                        action("取消", false).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.cancel_save_conflict(cx)),
                        ),
                    )
                    .child(
                        action("覆盖远端", true).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.force_remote_save(cx)),
                        ),
                    )
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
                        .text_size(px(10.5))
                        .text_color(col(ui.muted))
                        .bg(cola(th.ansi.red, 0.06))
                        .border_t_1()
                        .border_color(rgba(0xffffff0d))
                        .child(icon("alert", 13., th.ansi.red))
                        .child(
                            div()
                                .text_color(col(th.ansi.red))
                                .child(SharedString::from(error.clone())),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .px(px(9.))
                                .py(px(2.))
                                .rounded(px(7.))
                                .text_size(px(10.5))
                                .font_weight(gpui::FontWeight(620.))
                                .text_color(col(ui.foreground))
                                .bg(rgba(INSET))
                                .border_1()
                                .border_color(rgba(0xffffff14))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _e, _w, cx| this.cancel_save_conflict(cx)),
                                )
                                .child("关闭"),
                        )
                })
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
                .text_size(px(10.5))
                .text_color(col(ui.muted))
                .bg(cola(th.ansi.red, 0.06))
                .border_t_1()
                .border_color(rgba(0xffffff0d))
                .child(icon("alert", 13., th.ansi.red))
                .child(
                    div()
                        .text_color(col(th.ansi.red))
                        .child(SharedString::from(format!("应用失败:{error}"))),
                )
                .child(div().flex_1())
                .child(
                    div()
                        .px(px(9.))
                        .py(px(2.))
                        .rounded(px(7.))
                        .text_size(px(10.5))
                        .font_weight(gpui::FontWeight(620.))
                        .text_color(col(ui.foreground))
                        .bg(rgba(INSET))
                        .border_1()
                        .border_color(rgba(0xffffff14))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.dismiss_hunk_error(cx)),
                        )
                        .child("关闭"),
                )
        });

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
            .child(header)
            .when_some(find_bar, |d, fb| d.child(fb))
            .child(body)
            .when_some(save_notice, |d, n| d.child(n))
            .when_some(hunk_notice, |d, n| d.child(n))
            .child(footer)
            .child(seam);

        // mockup .quicklook::before 冷能量描边 + 更深的浮起投影
        quicklook_frame(inner, ui.accent)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
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
        let loaded = preview_payload_from_bytes(
            b"one\r\ntwo\r\n".to_vec(),
            "rs",
            Some(14),
            Some(&stat),
        );
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
}
