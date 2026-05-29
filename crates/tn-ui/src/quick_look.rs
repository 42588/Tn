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

use gpui::{
    canvas, div, linear_color_stop, linear_gradient, point, prelude::*, px, rgba, size,
    uniform_list, Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler,
    FocusHandle, KeyDownEvent, MouseButton, MouseDownEvent, Pixels, Point, Rgba, ScrollStrategy,
    SharedString, UniformListScrollHandle, UTF16Selection, Window,
};
use tn_config::Loaded;

use crate::style::{
    col, cola, icon, quicklook_fill, quicklook_frame, specular_top, HOVER, INSET, R_PANEL, UI_SANS,
};

/// A (row, char-col) position in the edit buffer.
type Pos = (usize, usize);

/// Cap the lines read/stored on open (bounds one-time work; the list itself is
/// virtualized via `uniform_list`, so only visible rows ever lay out / highlight).
const MAX_LINES: usize = 4000;

/// Code font size (px) — mockup `.code` font-size (also the mouse char-width probe).
const CODE_FS: f32 = 12.5;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    File,
    Diff,
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
        // CRITICAL: never stall. A char that `is_alphanumeric()` but is neither
        // `is_alphabetic()` (word branch) nor `is_ascii_digit()` (number branch) —
        // e.g. `①`/`②`/`½` (Unicode "No"/numeric) — enters none of those branches,
        // falls here, and breaks the loop at `j == i` → `i` never advances →
        // INFINITE LOOP pushing empty tokens → OOM (froze on a `①` in the HTML; see
        // 踩过的坑). Consume the offending char so the scanner always progresses.
        if j == i {
            j = i + 1;
        }
        out.push((chars[i..j].iter().collect(), Tint::Plain));
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
    /// Virtualized code list scroll position (kept across frames per gpui).
    scroll: UniformListScrollHandle,
    /// Grab focus in the next render (focusing in an event/open callback doesn't
    /// land — the overlay isn't rendered yet; see 踩过的坑).
    needs_focus: bool,
    focus_handle: FocusHandle,
}

impl QuickLook {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let root = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        // Measure the monospace advance once (mouse click → column). Mirrors
        // terminal_view's cell-width probe; falls back to a 0.6 ratio.
        let font_id = cx.text_system().resolve_font(&gpui::font(&config.font().family));
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
            file_lines: Rc::new(Vec::new()),
            file_truncated: false,
            diff: Rc::new(Vec::new()),
            diff_dirty: true,
            editing: false,
            buf: Rc::new(Vec::new()),
            cursor: (0, 0),
            sel_anchor: None,
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
            scroll: UniformListScrollHandle::default(),
            needs_focus: false,
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

    /// Open `path`: read its text + compute its git diff, default to the File tab
    /// (preview). Always (re)grabs focus so keys route to the overlay.
    pub fn open(&mut self, path: PathBuf) {
        self.path = Some(path.clone());
        self.tab = Tab::File;
        self.editing = false;
        self.dirty = false;
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let all: Vec<String> = text.lines().map(str::to_string).collect();
        self.file_truncated = all.len() > MAX_LINES;
        self.file_lines = Rc::new(all.into_iter().take(MAX_LINES).collect());
        // Defer the diff: `git diff` is a blocking subprocess and running it on
        // every open/navigation froze the UI (踩过的坑). Mark stale; compute only
        // when the Diff tab is actually shown (`ensure_diff`).
        self.diff = Rc::new(Vec::new());
        self.diff_dirty = true;
        self.scroll = UniformListScrollHandle::default(); // new file → scroll to top
        self.needs_focus = true;
        self.find_open = false;
    }

    /// Open `path` straight into the editor (app menu「设置」opens config.toml here).
    pub fn open_for_edit(&mut self, path: PathBuf) {
        self.open(path);
        self.enter_edit();
    }

    /// Open `path` straight on the Diff tab — the agent activity-rail card click
    /// ("点卡片 = 速览全 diff") lands here.
    pub fn open_diff(&mut self, path: PathBuf) {
        self.open(path);
        self.select_tab(Tab::Diff);
    }

    /// Recompute `diff` if stale — called only when the Diff tab is shown (keeps the
    /// blocking `git diff` off the open/navigation hot path). Bounded + non-flashing
    /// inside `compute_diff`.
    fn ensure_diff(&mut self) {
        if !self.diff_dirty {
            return;
        }
        if let Some(path) = self.path.clone() {
            self.diff = Rc::new(self.compute_diff(&path));
        }
        self.diff_dirty = false;
    }

    /// Switch tabs; computing the diff lazily when entering the Diff tab.
    fn select_tab(&mut self, tab: Tab) {
        self.tab = tab;
        if tab == Tab::Diff {
            self.ensure_diff();
        }
    }

    /// Enter edit mode: copy the file into the editable buffer, cursor at (0,0).
    fn enter_edit(&mut self) {
        self.buf = self.file_lines.clone();
        if self.buf.is_empty() {
            Rc::make_mut(&mut self.buf).push(String::new());
        }
        self.cursor = (0, 0);
        self.sel_anchor = None;
        self.undo.clear();
        self.redo.clear();
        self.coalesce_insert = false;
        self.editing = true;
        self.dirty = false;
    }

    /// Write the edit buffer back to disk, then refresh the preview + diff.
    fn save(&mut self, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else { return };
        let joined = self.buf.join("\n");
        let content = if joined.is_empty() { joined } else { format!("{joined}\n") };
        match std::fs::write(&path, content) {
            Ok(()) => {
                self.dirty = false;
                self.file_lines = self.buf.clone();
                // The diff is now stale; recompute lazily (only if the Diff tab is
                // currently showing — otherwise just mark it dirty so Ctrl+S stays
                // fast and never blocks on `git diff`).
                self.diff_dirty = true;
                if self.tab == Tab::Diff {
                    self.ensure_diff();
                }
                // Tell the workspace so it refreshes any agent pane's「本次改动」rail
                // now — don't rely on the file watcher (debounce / cwd coverage gaps).
                cx.emit(QuickLookEvent::FileSaved(path.clone()));
                tracing::info!(target: "tn::quicklook", path = %path.display(), lines = self.buf.len(), "quick look saved");
            }
            Err(e) => tracing::error!(path = %path.display(), error = %e, "quick look save failed"),
        }
        cx.notify();
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
        if self.undo.len() > 500 {
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
        let last = self.buf.len().saturating_sub(1);
        self.sel_anchor = Some((0, 0));
        self.cursor = (last, line_chars(&self.buf, last));
    }

    /// Place the cursor at (row, col) on click; `extend` = Shift-click selects.
    fn place_cursor(&mut self, row: usize, col: usize, extend: bool) {
        self.coalesce_insert = false;
        let r = row.min(self.buf.len().saturating_sub(1));
        let c = col.min(line_chars(&self.buf, r));
        if extend {
            if self.sel_anchor.is_none() {
                self.sel_anchor = Some(self.cursor);
            }
        } else {
            self.sel_anchor = None;
        }
        self.cursor = (r, c);
    }

    // ── clipboard ──

    fn copy(&mut self, cx: &mut Context<Self>) {
        let text = match self.sel_range() {
            Some((s, e)) => selected_text(&self.buf, s, e),
            None => format!("{}\n", self.buf.get(self.cursor.0).cloned().unwrap_or_default()),
        };
        cx.write_to_clipboard(ClipboardItem::new_string(text));
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
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else { return };
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
            matches.iter().rposition(|(s, _)| *s < cur).unwrap_or(matches.len() - 1)
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
                op_insert_multiline(Rc::make_mut(&mut self.buf), &mut self.cursor, &self.replace_query);
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
        let n = replace_all_in(Rc::make_mut(&mut self.buf), &self.find_query, &self.replace_query);
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
                let q = if self.find_field_replace { &mut self.replace_query } else { &mut self.find_query };
                q.pop();
            }
            _ => {
                if let Some(ch) = key_char.filter(|s| !s.is_empty()) {
                    let q = if self.find_field_replace { &mut self.replace_query } else { &mut self.find_query };
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
                    self.scroll.scroll_to_item(self.cursor.0, ScrollStrategy::Center);
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
                self.scroll.scroll_to_item(self.cursor.0, ScrollStrategy::Center);
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
                }
                "delete" => self.delete_forward(),
                "enter" => self.newline(),
                "tab" => self.indent(),
                // NOTE: `space` and `backspace` are intentionally NOT handled here —
                // they fall to `_ => handled = false` and defer to the IME input
                // handler (so an IME with no composition string can still commit/edit
                // while composing). When NOT composing they return as WM_CHAR:
                // `replace_text_in_range` types the space and remaps 0x08 → `backspace()`.
                // Handling them here (+stop) would starve the IME of its commit/edit keys.
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
                    self.scroll.scroll_to_item(self.cursor.0, ScrollStrategy::Center);
                }
                cx.stop_propagation();
                cx.notify();
            }
        } else {
            if m.control || m.alt || m.platform {
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
                    self.select_tab(if self.tab == Tab::File { Tab::Diff } else { Tab::File });
                    cx.stop_propagation();
                    cx.notify();
                }
                "enter" => {
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

    /// `git diff` for `path`, parsed into renderable lines (tracking new-file
    /// line numbers from each hunk header). Empty when not a repo / no changes.
    fn compute_diff(&self, path: &PathBuf) -> Vec<DiffLine> {
        let rel = path
            .strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();
        // Bounded so a slow / .git-locked / AV-scanned git can never hang the UI
        // (worst case ~1.5s on an explicit Diff-tab view); `None` = timed out → no diff.
        // Shared bounded git capture lives in `crate::gitutil` (single source).
        let text = crate::gitutil::capture_bounded(
            &self.root,
            &["diff", "--no-color", "--", &rel],
            std::time::Duration::from_millis(1500),
        );
        if text.is_none() {
            tracing::warn!(target: "tn::quicklook", path = %path.display(), "git diff timed out (>1.5s); showing no diff");
        }
        parse_diff(text.as_deref().unwrap_or(""))
    }

}

/// Parse `git diff --no-color` output into renderable lines (tracking new-file line
/// numbers from each hunk header). Pure → unit-testable headless.
fn parse_diff(text: &str) -> Vec<DiffLine> {
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
        .child(div().w(px(14.)).flex_none().text_center().text_color(mark_col).child(mark))
        .child(div().flex().flex_row().children(spans))
}

/// Char index → byte offset within `line` (cursor cols are char-based).
fn char_to_byte(line: &str, col: usize) -> usize {
    line.char_indices().nth(col).map(|(b, _)| b).unwrap_or(line.len())
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
        return buf.get(s.0).map(|l| l.chars().skip(s.1).take(e.1 - s.1).collect()).unwrap_or_default();
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

/// First index ≥ `from` where `query`'s chars occur in `chars` (substring search).
fn find_in_chars(chars: &[char], query: &[char], from: usize) -> Option<usize> {
    if query.is_empty() || query.len() > chars.len() {
        return None;
    }
    let mut i = from;
    while i + query.len() <= chars.len() {
        if chars[i..i + query.len()] == *query {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// All matches of `query` (single-line) in the buffer, as `(start, end)` char
/// positions, in document order.
fn all_matches(buf: &[String], query: &str) -> Vec<((usize, usize), (usize, usize))> {
    let q: Vec<char> = query.chars().collect();
    let mut out = Vec::new();
    if q.is_empty() {
        return out;
    }
    for (r, line) in buf.iter().enumerate() {
        let chars: Vec<char> = line.chars().collect();
        let mut start = 0;
        while let Some(col) = find_in_chars(&chars, &q, start) {
            out.push(((r, col), (r, col + q.len())));
            start = col + 1;
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
fn cursor_block(config: &Loaded) -> gpui::Div {
    div().w(px(2.)).h(px(15.)).flex_none().bg(col(config.theme.ui.foreground))
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

/// Build one edit-mode row `i`: syntax-tinted text + selection background + the
/// caret, by walking chars and grouping equal (tint, selected) runs (caret splits
/// a run). `sel` is the normalized document selection (if any).
fn edit_row(
    config: &Loaded,
    buf: &[String],
    i: usize,
    cursor: (usize, usize),
    sel: Option<((usize, usize), (usize, usize))>,
) -> gpui::Div {
    let line = &buf[i];
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    // Long line: skip per-char tinting (bounds paint cost); render before/[caret]/
    // after as plain spans so editing still works.
    if line.len() > LONG_LINE_BYTES {
        let fg = col(config.theme.ui.foreground);
        let cc = if i == cursor.0 { cursor.1.min(n) } else { n + 1 };
        let before: String = chars.iter().take(cc.min(n)).collect();
        let after: String = chars.iter().skip(cc.min(n)).collect();
        let mut row = div().flex().flex_row().items_center()
            .child(div().text_color(fg).child(SharedString::from(before)));
        if i == cursor.0 {
            row = row.child(cursor_block(config));
        }
        row = row.child(div().text_color(fg).child(SharedString::from(after)));
        return code_row(format!("{}", i + 1), "", col(config.theme.ui.muted), vec![row]);
    }
    let tints = tints_per_char(line);
    let tint_at = |k: usize| *tints.get(k).unwrap_or(&Tint::Plain);

    // selection columns within this row ([sel_s, sel_e))
    let (sel_s, sel_e) = match sel {
        Some((s, e)) if i >= s.0 && i <= e.0 => {
            let ss = if i == s.0 { s.1 } else { 0 };
            let ee = if i == e.0 { e.1 } else { n }; // whole line for middle lines
            (ss, ee)
        }
        _ => (0, 0),
    };
    let selected = |k: usize| k >= sel_s && k < sel_e;
    let caret_col = (i == cursor.0).then(|| cursor.1.min(n));
    let sel_bg = cola(config.theme.ui.accent, 0.22);

    let mut spans: Vec<gpui::Div> = Vec::new();
    let mut k = 0;
    loop {
        if caret_col == Some(k) {
            spans.push(cursor_block(config));
        }
        if k >= n {
            break;
        }
        let t0 = tint_at(k);
        let s0 = selected(k);
        let mut j = k + 1;
        while j < n && tint_at(j) == t0 && selected(j) == s0 && caret_col != Some(j) {
            j += 1;
        }
        let text: String = chars[k..j].iter().collect();
        let mut span = div().text_color(tint_color(config, t0)).child(SharedString::from(text));
        if s0 {
            span = span.bg(sel_bg);
        }
        spans.push(span);
        k = j;
    }
    let content = div().flex().flex_row().items_center().children(spans);
    code_row(format!("{}", i + 1), "", col(config.theme.ui.muted), vec![content])
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
fn coalesce_spans(line: &str) -> Vec<(String, Tint)> {
    if line.len() > LONG_LINE_BYTES {
        return vec![(line.to_string(), Tint::Plain)];
    }
    let mut merged: Vec<(String, Tint)> = Vec::new();
    for (text, tint) in highlight(line) {
        match merged.last_mut() {
            Some((s, lt)) if *lt == tint => s.push_str(&text),
            _ => merged.push((text, tint)),
        }
    }
    if merged.len() > MAX_SPANS {
        let tail: String = merged.drain(MAX_SPANS - 1..).map(|(s, _)| s).collect();
        merged.push((tail, Tint::Plain));
    }
    merged
}

/// Build one File-tab row `i` (1-based line number + syntax-tinted source).
fn file_row(config: &Loaded, lines: &[String], i: usize) -> gpui::Div {
    let spans: Vec<gpui::Div> = coalesce_spans(&lines[i])
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
        let units: Vec<u16> = self.ime_marked.as_deref().unwrap_or("").encode_utf16().collect();
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
        let end = self.ime_marked.as_deref().map(|s| s.encode_utf16().count()).unwrap_or(0);
        Some(UTF16Selection { range: end..end, reversed: false })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.ime_marked.as_deref().map(|s| 0..s.encode_utf16().count())
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
        // IME commit (中文) → insert at the cursor like typed text (op handles
        // multi-char + selection + undo). A deferred Backspace not consumed by the IME
        // (not composing) arrives as WM_CHAR 0x08 → delete, don't insert it. Empty
        // `text` = composition cancel.
        if text == "\u{8}" {
            self.backspace();
            self.scroll.scroll_to_item(self.cursor.0, ScrollStrategy::Center);
        } else if !text.is_empty() {
            self.type_char(text);
            self.scroll.scroll_to_item(self.cursor.0, ScrollStrategy::Center);
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
        let x = f32::from(element_bounds.origin.x) + CODE_GUTTER + self.cursor.1 as f32 * self.char_w;
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
                        this.select_tab(to);
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
            // mockup .vh .by:编辑态 = 「编辑中(●)」,预览态有未提交改动 = 「已改动」(claude)
            .when(self.editing || !self.diff.is_empty(), |d| {
                let label = if self.editing {
                    if self.dirty { "编辑中 ●" } else { "编辑中" }
                } else {
                    "已改动"
                };
                d.child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(5.)) // §16 .vh .by gap 5
                        .text_size(px(11.))
                        .text_color(col(th.agents.claude))
                        .child(icon("pen", 13., th.agents.claude))
                        .child(label),
                )
            })
            .child(tabset);

        // ── .code body:**虚拟化**列表(uniform_list 只渲染可见行 → 大文件不卡)。
        //    编辑态从 buf 渲染(高亮 + 选区 + 光标);预览态从 file_lines / diff 渲染。──
        let config = self.config.clone(); // Arc clone for the 'static closure
        let lines = self.file_lines.clone(); // Rc clone (cheap)
        let diff = self.diff.clone();
        let buf = self.buf.clone();
        let editing = self.editing;
        let cursor = self.cursor;
        let sel = self.sel_range();
        let tab = self.tab;
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
            diff.len()
        } else {
            lines.len()
        };
        let body = if !editing && tab == Tab::Diff && diff.is_empty() {
            div()
                .flex_1()
                .min_h(px(0.))
                .px(px(14.))
                .py(px(8.))
                .text_color(col(ui.muted))
                .child("无改动 · git working tree clean")
        } else {
            div()
                .relative() // anchor the bounds-capturing canvas
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_col()
                .overflow_hidden()
                .pt(px(8.)) // mockup .code padding 8px 0(顶部留白;余下走列表内滚动)
                .child(
                    // canvas captures the code area's window-space bounds (left edge
                    // → column mapping for clicks).
                    canvas(
                        move |bounds, _w, _cx| *canvas_bounds.borrow_mut() = bounds,
                        // Register the IME/text input handler for this frame (edit mode
                        // only) so composed text (中文) reaches `replace_text_in_range`.
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
                .child(
                    uniform_list("ql-code", count, move |range, _window, _cx| {
                        range
                            .map(|i| {
                                if editing {
                                    let row = edit_row(&config, &buf, i, cursor, sel);
                                    // click-to-place (Shift extends): map x → column.
                                    let entity = entity.clone();
                                    let bounds = row_bounds.clone();
                                    row.on_mouse_down(
                                        MouseButton::Left,
                                        move |ev: &MouseDownEvent, _w, app| {
                                            let left = f32::from(bounds.borrow().origin.x);
                                            let rel = f32::from(ev.position.x) - left - GUTTER;
                                            let col = (rel / char_w).round().max(0.0) as usize;
                                            let shift = ev.modifiers.shift;
                                            let _ = entity.update(app, |this, cx| {
                                                this.place_cursor(i, col, shift);
                                                cx.notify();
                                            });
                                            // Don't let the click bubble to the workspace scrim /
                                            // a terminal pane (would steal focus to the shell — the
                                            // "焦点漏到底层 shell" bug).
                                            app.stop_propagation();
                                        },
                                    )
                                } else if tab == Tab::File {
                                    file_row(&config, &lines, i)
                                } else {
                                    diff_row(&config, &diff, i)
                                }
                            })
                            .collect::<Vec<_>>()
                    })
                    .flex_1()
                    .min_h(px(0.))
                    .track_scroll(self.scroll.clone()),
                )
                .when(!editing && self.file_truncated && tab == Tab::File, |d| {
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
        } else {
            // 预览态:↑↓ 换文件 · ⇥ 切 File · Enter 编辑 [sp] Diff 只读审阅 · Esc 关闭
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
                    .child(div().text_size(px(10.)).text_color(col(ui.muted)).child(label))
                    .child(
                        div()
                            .min_w(px(140.))
                            .px(px(7.))
                            .py(px(2.))
                            .rounded(px(6.))
                            .bg(rgba(INSET))
                            .border_1()
                            .border_color(if active { cola(ui.accent, 0.5) } else { rgba(0x00000000) })
                            .font_family(mono.clone())
                            .text_size(px(11.))
                            .text_color(col(ui.foreground))
                            // show a thin caret stand-in when the active field is empty
                            .child(SharedString::from(if text.is_empty() {
                                if active { "▏".to_string() } else { String::new() }
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
                .child(div().text_size(px(10.)).text_color(col(ui.muted)).child("下一个"))
                .when(self.replacing, |d| {
                    d.child(kcap("Ctrl+↵"))
                        .child(div().text_size(px(10.)).text_color(col(ui.muted)).child("全部替换"))
                })
                .child(kcap("Esc"))
                .child(div().text_size(px(10.)).text_color(col(ui.muted)).child("关闭"))
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
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)))
            // Swallow any click landing on the panel (not already handled by a child
            // like a code row) so it neither bubbles to the workspace click-away scrim
            // (which would close the overlay) nor passes through to a terminal pane
            // (which would steal focus to the shell). Clicking the panel keeps focus
            // here (track_focus). 修「面板穿透事件 / 焦点漏到底层 shell」。
            .on_mouse_down(MouseButton::Left, cx.listener(|_, _ev, _w, cx| cx.stop_propagation()))
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
            .when_some(find_bar, |d, fb| d.child(fb))
            .child(body)
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
        assert_eq!(m, vec![((0, 0), (0, 3)), ((0, 8), (0, 11)), ((1, 4), (1, 7))]);
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
        for s in ["①", "① 窗口外壳", "②③ x", "½ cup", "a①b", "<h1>① 标题</h1>", "Ⅷ ⑩ ㊀"] {
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
        assert!(merged.len() < raw, "coalesced ({}) must be fewer than raw tokens ({raw})", merged.len());
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
        assert_eq!(s[0].0.len(), long.len(), "long line kept whole, just untinted");

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
    fn find_in_chars_overlap_and_bounds() {
        let cs: Vec<char> = "aXbXc".chars().collect();
        let q: Vec<char> = "X".chars().collect();
        assert_eq!(find_in_chars(&cs, &q, 0), Some(1));
        assert_eq!(find_in_chars(&cs, &q, 2), Some(3));
        assert_eq!(find_in_chars(&cs, &q, 4), None);
        // query longer than haystack
        let long: Vec<char> = "abcd".chars().collect();
        assert_eq!(find_in_chars(&cs, &long, 0), None);
    }
}
