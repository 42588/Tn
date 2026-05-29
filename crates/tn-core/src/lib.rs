//! Tn terminal engine: a headless wrapper around `alacritty_terminal`.
//!
//! [`Terminal`] owns an alacritty [`Term`] plus a VTE [`Processor`]. PTY output
//! bytes are fed in via [`Terminal::advance`]; the renderer pulls an immutable
//! [`TerminalSnapshot`] via [`Terminal::snapshot`]. No GPUI, no IO — this crate
//! is fully headless and unit-testable.

use std::sync::mpsc::{Receiver, Sender};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{viewport_to_point, Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, Processor};

/// Re-export so consumers can match on terminal events without depending on
/// alacritty_terminal directly.
pub use alacritty_terminal::event::Event as TermEvent;

/// Granularity for a click-drag selection: a single cell (single click), the
/// word under the cursor (double click), or the whole line (triple click).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectKind {
    Cell,
    Word,
    Line,
}

/// One hit from [`Terminal::search`]: the absolute line (0 = oldest retained
/// scrollback line — the same numbering as [`Terminal::cursor_abs_line`], so the
/// UI can scroll a match into view) and the half-open column range `[start,end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    pub line: usize,
    pub col_start: usize,
    pub col_end: usize,
}

/// A clickable URL found in the visible grid (see [`TerminalSnapshot::urls`]).
/// Coordinates are viewport cells (matching what the renderer draws), so the UI
/// can underline `[col_start, col_end)` on `row` and open `url` on Ctrl+Click.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UrlSpan {
    pub row: usize,
    pub col_start: usize,
    pub col_end: usize,
    pub url: String,
}

/// Viewport size in character cells. Implements alacritty's [`Dimensions`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GridSize {
    pub rows: usize,
    pub cols: usize,
}

impl GridSize {
    pub fn new(rows: usize, cols: usize) -> Self {
        Self {
            rows: rows.max(1),
            cols: cols.max(1),
        }
    }
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

/// 24-bit RGB color.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub const fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }
}

/// `0xRRGGBB` -> [`Rgb`].
const fn hex(v: u32) -> Rgb {
    Rgb::new((v >> 16) as u8, (v >> 8) as u8, v as u8)
}

/// Palette used to resolve terminal colors (ANSI 0..16 + fg/bg/cursor) to RGB.
/// Defaults to the built-in **Tn Dark** theme; the UI can replace it from config.
#[derive(Clone, Copy, Debug)]
pub struct Palette {
    pub ansi: [Rgb; 16],
    pub fg: Rgb,
    pub bg: Rgb,
    pub cursor: Rgb,
    pub selection_fg: Rgb,
    pub selection_bg: Rgb,
}

impl Default for Palette {
    fn default() -> Self {
        // Tn Dark (Tokyo Night tuned) — mirrors config/themes/tn-dark.toml.
        Self {
            ansi: [
                hex(0x15161E), hex(0xF7768E), hex(0x9ECE6A), hex(0xE0AF68),
                hex(0x7AA2F7), hex(0xBB9AF7), hex(0x7DCFFF), hex(0xA9B1D6),
                hex(0x414868), hex(0xF7768E), hex(0x9ECE6A), hex(0xE0AF68),
                hex(0x7AA2F7), hex(0xBB9AF7), hex(0x7DCFFF), hex(0xC0CAF5),
            ],
            fg: hex(0xC0CAF5),
            bg: hex(0x1A1B26),
            cursor: hex(0xC0CAF5),
            selection_fg: hex(0xC0CAF5),
            selection_bg: hex(0x283457),
        }
    }
}

impl Palette {
    /// Resolve an alacritty color to RGB, honoring live OSC overrides in `colors`.
    fn resolve(&self, color: Color, colors: &Colors) -> Rgb {
        match color {
            Color::Spec(c) => Rgb::new(c.r, c.g, c.b),
            Color::Named(n) => self.resolve_index(n as usize, colors),
            Color::Indexed(i) => self.resolve_index(i as usize, colors),
        }
    }

    fn resolve_index(&self, idx: usize, colors: &Colors) -> Rgb {
        // Live override (OSC 4 / 10 / 11 ...) wins when present.
        if idx < alacritty_terminal::term::color::COUNT {
            if let Some(c) = colors[idx] {
                return Rgb::new(c.r, c.g, c.b);
            }
        }
        match idx {
            0..=15 => self.ansi[idx],
            16..=231 => {
                // 6x6x6 color cube.
                let j = idx - 16;
                let chan = |v: usize| -> u8 {
                    if v == 0 {
                        0
                    } else {
                        (v * 40 + 55) as u8
                    }
                };
                Rgb::new(chan(j / 36), chan((j / 6) % 6), chan(j % 6))
            }
            232..=255 => {
                let v = ((idx - 232) * 10 + 8) as u8;
                Rgb::new(v, v, v)
            }
            256 => self.fg,
            257 => self.bg,
            258 => self.cursor,
            259..=266 => self.ansi[idx - 259], // dim X -> ANSI X
            267 => self.fg,                    // bright foreground
            268 => self.bg,                    // dim background
            _ => self.fg,
        }
    }
}

/// Event sink that forwards alacritty terminal events into an mpsc channel so
/// the owning thread can drain them (title changes, bells, PTY write-backs...).
struct ChannelListener(Sender<Event>);

impl EventListener for ChannelListener {
    fn send_event(&self, event: Event) {
        // Receiver gone simply means the Terminal was dropped; ignore.
        let _ = self.0.send(event);
    }
}

/// A single rendered cell: character, resolved RGB colors, and style flags.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotCell {
    /// Row within the viewport, 0 = top.
    pub row: usize,
    /// Column within the viewport, 0 = left.
    pub col: usize,
    pub c: char,
    /// Foreground / background already resolved to RGB (INVERSE applied).
    pub fg: Rgb,
    pub bg: Rgb,
    pub flags: Flags,
}

/// A contiguous run of cells sharing fg/bg/style — the unit the renderer draws
/// (one styled box of text), so it doesn't emit one element per cell. Style is
/// exposed as plain bools so the UI needn't depend on alacritty's `Flags`.
#[derive(Clone, Debug)]
pub struct CellRun {
    pub text: String,
    pub fg: Rgb,
    pub bg: Rgb,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    /// Grid columns this run spans. A wide (CJK / double-width) char counts as 2,
    /// every other char as 1 — so the renderer can force the run's box to exactly
    /// `cols × cell_width` and keep the cell grid aligned even when a glyph's font
    /// advance differs (CJK in a fallback font). Usually `== text.chars().count()`,
    /// but larger when the run contains wide chars.
    pub cols: usize,
}

/// An immutable view of the visible terminal grid, produced for rendering.
#[derive(Clone, Debug, Default)]
pub struct TerminalSnapshot {
    pub rows: usize,
    pub cols: usize,
    /// Cursor position within the viewport as (row, col).
    pub cursor: (usize, usize),
    /// Whether the cursor should be drawn (false when an app hides it, e.g. vim).
    pub cursor_visible: bool,
    /// Rows scrolled up from the live bottom (0 = at the bottom). For a scrollbar.
    pub scroll_offset: usize,
    /// Total scrollback rows retained above the viewport. For a scrollbar; a
    /// scrollbar is meaningful only when this exceeds 0.
    pub scroll_history: usize,
    /// Default foreground / background (resolved theme colors).
    pub fg: Rgb,
    pub bg: Rgb,
    /// Visible cells, row-major. Default blank cells are included so renderers
    /// and [`TerminalSnapshot::to_text`] can address the grid directly.
    pub cells: Vec<SnapshotCell>,
}

/// Scan one row of chars for `http(s)://` URLs → half-open `[start, end)` char
/// ranges. The scheme must sit on a word boundary (so `xhttps://…` mid-token
/// isn't matched), and trailing sentence punctuation / a closing bracket is
/// trimmed off the tail.
fn find_urls(chars: &[char]) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let at_boundary = i == 0 || !chars[i - 1].is_ascii_alphanumeric();
        if at_boundary {
            if let Some(scheme_len) = url_scheme_len(&chars[i..]) {
                let body_start = i + scheme_len;
                let mut end = body_start;
                while end < chars.len() && is_url_char(chars[end]) {
                    end += 1;
                }
                while end > body_start && is_trailing_punct(chars[end - 1]) {
                    end -= 1;
                }
                if end > body_start {
                    spans.push((i, end));
                    i = end;
                    continue;
                }
            }
        }
        i += 1;
    }
    spans
}

/// Length of an `http://` (7) / `https://` (8) scheme at the start of `s`, else `None`.
fn url_scheme_len(s: &[char]) -> Option<usize> {
    const HTTP: [char; 7] = ['h', 't', 't', 'p', ':', '/', '/'];
    const HTTPS: [char; 8] = ['h', 't', 't', 'p', 's', ':', '/', '/'];
    if s.len() >= HTTPS.len() && s[..HTTPS.len()] == HTTPS {
        Some(HTTPS.len())
    } else if s.len() >= HTTP.len() && s[..HTTP.len()] == HTTP {
        Some(HTTP.len())
    } else {
        None
    }
}

/// RFC 3986 unreserved + reserved + `%` — the chars allowed inside a URL.
fn is_url_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-._~:/?#[]@!$&'()*+,;=%".contains(c)
}

/// Punctuation that, at a URL's tail, is almost always sentence/markup, not link.
fn is_trailing_punct(c: char) -> bool {
    matches!(c, '.' | ',' | ';' | ':' | '!' | '?' | ')' | ']' | '}' | '\'' | '"' | '>')
}

impl TerminalSnapshot {
    /// Each visible row as a string, trailing blanks trimmed. Row count equals
    /// `self.rows`. Intended for line-by-line rendering and debugging.
    pub fn rows_text(&self) -> Vec<String> {
        let mut grid = vec![vec![' '; self.cols]; self.rows];
        for cell in &self.cells {
            if cell.row < self.rows && cell.col < self.cols && cell.c != '\0' {
                grid[cell.row][cell.col] = cell.c;
            }
        }
        grid.iter()
            .map(|row| row.iter().collect::<String>().trim_end().to_string())
            .collect()
    }

    /// Render the visible grid to plain text, one line per row. Intended for
    /// headless tests and debugging.
    pub fn to_text(&self) -> String {
        self.rows_text().join("\n")
    }

    /// Detect `http(s)://` URLs in the visible grid for hover/click. Each
    /// [`UrlSpan`] is in viewport cell coordinates (matching the rendered grid),
    /// so the UI can underline the run and open it on Ctrl+Click. A URL is not
    /// matched across a row boundary (terminals hard-wrap), and trailing
    /// sentence punctuation / a closing bracket is trimmed off the end.
    pub fn urls(&self) -> Vec<UrlSpan> {
        let mut out = Vec::new();
        for (row, line) in self.rows_text().iter().enumerate() {
            let chars: Vec<char> = line.chars().collect();
            for (col_start, col_end) in find_urls(&chars) {
                out.push(UrlSpan {
                    row,
                    col_start,
                    col_end,
                    url: chars[col_start..col_end].iter().collect(),
                });
            }
        }
        out
    }

    /// Per-row runs of same-style cells, for colored rendering. Each inner
    /// `Vec<CellRun>` is one visible row, left to right.
    pub fn row_runs(&self) -> Vec<Vec<CellRun>> {
        // Reconstruct a rows x cols grid (display_iter already yields every
        // visible cell, but missing slots fall back to default blanks).
        let blank = SnapshotCell {
            row: 0,
            col: 0,
            c: ' ',
            fg: self.fg,
            bg: self.bg,
            flags: Flags::empty(),
        };
        let mut grid = vec![vec![blank; self.cols]; self.rows];
        for cell in &self.cells {
            if cell.row < self.rows && cell.col < self.cols {
                grid[cell.row][cell.col] = *cell;
            }
        }
        grid.into_iter()
            .map(|row| {
                let mut runs: Vec<CellRun> = Vec::new();
                // A wide char is emitted as its **own** run so the renderer can box it
                // to exactly 2 cols. Batching it with neighbours into one wide run made
                // the run's forced width (cols×cell_width) exceed the glyphs' real
                // advance (CJK fallback font ≠ 2×cell_width), pushing all the slack to
                // the run's end → the cursor sat far past the text ("光标距离很长").
                // Per-char boxes distribute that tiny slack and keep every cell on the
                // grid, so the cursor lands right after the last char.
                let mut last_wide = false;
                for cell in row {
                    // A wide char (CJK) occupies two grid columns: the char itself
                    // (WIDE_CHAR) + a phantom spacer in the next column. **Skip the
                    // spacer** — rendering it as a blank put a half-width gap after
                    // every CJK char ("间距那么大" bug).
                    if cell.flags.contains(Flags::WIDE_CHAR_SPACER)
                        || cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
                    {
                        continue;
                    }
                    let ch = if cell.c == '\0' { ' ' } else { cell.c };
                    let wide = cell.flags.contains(Flags::WIDE_CHAR);
                    let span = if wide { 2 } else { 1 };
                    let bold = cell.flags.contains(Flags::BOLD);
                    let italic = cell.flags.contains(Flags::ITALIC);
                    let underline = cell.flags.contains(Flags::UNDERLINE);
                    // Merge into the previous run only for narrow chars whose neighbour
                    // is also narrow + same style; a wide char always starts (and ends)
                    // its own run.
                    let merge = !wide
                        && !last_wide
                        && matches!(runs.last(), Some(r)
                            if r.fg == cell.fg
                                && r.bg == cell.bg
                                && r.bold == bold
                                && r.italic == italic
                                && r.underline == underline);
                    if merge {
                        let r = runs.last_mut().unwrap();
                        r.text.push(ch);
                        r.cols += span;
                    } else {
                        runs.push(CellRun {
                            text: ch.to_string(),
                            fg: cell.fg,
                            bg: cell.bg,
                            bold,
                            italic,
                            underline,
                            cols: span,
                        });
                    }
                    last_wide = wide;
                }
                runs
            })
            .collect()
    }
}

/// Terminal mode bits that affect how keystrokes are encoded into bytes.
/// Mirrors the relevant `alacritty_terminal::term::TermMode` flags so the input
/// layer needn't depend on alacritty directly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputMode {
    /// DECCKM — application cursor keys (arrows/Home/End emit SS3, not CSI).
    pub app_cursor: bool,
    /// DECKPAM — application keypad.
    pub app_keypad: bool,
    /// LNM — Enter sends CR+LF instead of CR.
    pub line_feed_newline: bool,
    /// Bracketed paste (DEC 2004) — pastes wrapped in `ESC[200~`/`ESC[201~`.
    pub bracketed_paste: bool,
    /// Alternate screen (DECSET 1049) active — a full-screen app (vim, less) is
    /// running, so the mouse wheel should drive the app, not scrollback.
    pub alt_screen: bool,
}

/// A headless terminal: VTE parser + alacritty grid + event channel.
pub struct Terminal {
    term: Term<ChannelListener>,
    parser: Processor,
    size: GridSize,
    palette: Palette,
    events: Receiver<Event>,
    /// Monotonic counter bumped on every grid-affecting mutation (output,
    /// scroll, resize, selection, palette). A renderer can compare it to skip
    /// rebuilding its [`snapshot`](Self::snapshot)/[`row_runs`] when nothing
    /// changed (e.g. a cursor-blink-only repaint) — see 待优化清单 §2.1.
    /// **Any new `&mut self` method that changes what the grid renders MUST call
    /// [`bump`](Self::bump)**, or the cache will show stale content.
    generation: u64,
}

impl Terminal {
    /// Create a terminal of the given viewport size with the default 10 000
    /// lines of scrollback.
    pub fn new(size: GridSize) -> Self {
        Self::with_scrollback(size, 10_000)
    }

    /// Create a terminal with an explicit scrollback history size (lines).
    pub fn with_scrollback(size: GridSize, scrollback: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let term = Term::new(config, &size, ChannelListener(tx));
        Self {
            term,
            parser: Processor::new(),
            size,
            palette: Palette::default(),
            events: rx,
            generation: 0,
        }
    }

    /// The render-cache generation (see the [`generation`](Self::generation)
    /// field). Bumped by every mutation that changes what the grid renders.
    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Mark the grid as changed for cache-invalidation purposes.
    #[inline]
    fn bump(&mut self) {
        self.generation = self.generation.wrapping_add(1);
    }

    /// Replace the color palette (e.g. from the user's theme).
    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
        self.bump();
    }

    /// Feed raw PTY output bytes into the parser and grid.
    pub fn advance(&mut self, bytes: &[u8]) {
        // Disjoint field borrows: `parser` (receiver) and `term` (argument).
        self.parser.advance(&mut self.term, bytes);
        self.bump();
    }

    /// Resize the grid to a new viewport size.
    ///
    /// Standard terminal semantics (bottom-anchored): on a row *grow* alacritty
    /// pulls `delta` rows out of scrollback into the top of the viewport. For a
    /// ConPTY-backed pane prefer [`resize_conpty`], which avoids losing those
    /// rows to ConPTY's resize-repaint.
    pub fn resize(&mut self, size: GridSize) {
        self.size = size;
        self.term.resize(size);
        self.bump();
    }

    /// Resize a **ConPTY-backed** pane: like [`resize`](Self::resize) but on a
    /// row *grow* it keeps content **top-anchored** — the new rows are blanks at
    /// the bottom, and scrollback stays in scrollback (it is not pulled up into
    /// the viewport).
    ///
    /// Why this exists (see CLAUDE.md 踩过的坑 / `TN_RESIZE_EXP`): alacritty's
    /// standard grow is bottom-anchored — it pulls `delta` lines from history
    /// into the viewport top. But ConPTY repaints *its* (top-anchored) viewport
    /// right after a resize and overwrites exactly those pulled-up cells; since
    /// alacritty already promoted them out of the history ring, they're lost for
    /// good (probe: 12→24 drops 12 lines of scrollback). By pushing the pulled
    /// rows back into scrollback *before* ConPTY's repaint lands, the history
    /// stays in the ring (untouched by the repaint) and the grid already matches
    /// ConPTY's top-anchored repaint. Net: dragging a split bigger no longer eats
    /// scrollback. (This also matches how native Windows consoles grow — content
    /// stays put, blank space opens below — rather than Unix's reveal-history.)
    pub fn resize_conpty(&mut self, size: GridSize) {
        let old_rows = self.size.rows;
        // History present *before* the grow == the pool grow_lines pulls from.
        let history_before = self.term.grid().history_size();
        self.size = size;
        self.term.resize(size);
        if size.rows > old_rows {
            let delta = size.rows - old_rows;
            // grow_lines pulled `min(history_before, delta)` rows up; push exactly
            // those back down into scrollback (scroll_up over the full viewport
            // rotates the top rows into history and clears blanks at the bottom).
            let pulled = history_before.min(delta);
            if pulled > 0 {
                let rows = size.rows as i32;
                self.term.grid_mut().scroll_up(&(Line(0)..Line(rows)), pulled);
            }
        }
        self.bump();
    }

    pub fn size(&self) -> GridSize {
        self.size
    }

    /// Current input-relevant terminal modes (DECCKM, keypad, LNM, ...), read
    /// from the live alacritty `Term`. The input layer uses these to encode keys.
    pub fn input_mode(&self) -> InputMode {
        let m = self.term.mode();
        InputMode {
            app_cursor: m.contains(TermMode::APP_CURSOR),
            app_keypad: m.contains(TermMode::APP_KEYPAD),
            line_feed_newline: m.contains(TermMode::LINE_FEED_NEW_LINE),
            bracketed_paste: m.contains(TermMode::BRACKETED_PASTE),
            alt_screen: m.contains(TermMode::ALT_SCREEN),
        }
    }

    /// Absolute line of the cursor, counted from the top of the retained
    /// scrollback (history start). Monotonic as output scrolls — until the
    /// scrollback cap drops the oldest lines, after which all anchors shift
    /// together. `tn-blocks` uses this to anchor command blocks into history.
    pub fn cursor_abs_line(&self) -> u64 {
        let grid = self.term.grid();
        let history = grid.history_size() as u64;
        let line = grid.cursor.point.line.0.max(0) as u64; // 0..screen_lines
        history + line
    }

    /// Scroll the viewport through scrollback by `lines` (positive = back toward
    /// older output). Clamped to the history bounds by the engine.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
        self.bump();
    }

    /// Jump the viewport back to the live bottom (latest output).
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
        self.bump();
    }

    /// Current `(display_offset, history_size)` — for scrollbar drag math.
    pub fn scroll_position(&self) -> (usize, usize) {
        let g = self.term.grid();
        (g.display_offset(), g.history_size())
    }

    /// Scroll so the display offset becomes `offset` (0 = bottom; clamped to the
    /// retained history). Used by scrollbar drag.
    pub fn scroll_to_offset(&mut self, offset: usize) {
        let cur = self.term.grid().display_offset() as i32;
        let target = offset.min(self.term.grid().history_size()) as i32;
        if target != cur {
            self.term.scroll_display(Scroll::Delta(target - cur));
            self.bump();
        }
    }

    /// Find every occurrence of `query` (plain, case-sensitive substring) across
    /// the whole buffer — retained scrollback **and** the visible grid — top to
    /// bottom. Each [`SearchMatch`] carries an absolute line (0 = oldest history,
    /// aligned with [`cursor_abs_line`](Self::cursor_abs_line)) so the UI can
    /// scroll a hit into view. An empty query yields nothing.
    ///
    /// Read-only (doesn't move the viewport). Matching is per-grid-row: a match
    /// can't span a wrapped line, and a wide-char's trailing spacer cell is
    /// included verbatim (fine for the ASCII text that searches usually target).
    pub fn search(&self, query: &str) -> Vec<SearchMatch> {
        let needle: Vec<char> = query.chars().collect();
        if needle.is_empty() {
            return Vec::new();
        }
        let grid = self.term.grid();
        let mut out = Vec::new();
        // `topmost_line()` is the oldest history line (negative); enumerate gives
        // the absolute index from the top of history.
        for (abs, li) in (grid.topmost_line().0..=grid.bottommost_line().0).enumerate() {
            let row = &grid[Line(li)];
            let line: Vec<char> = (0..row.len()).map(|c| row[Column(c)].c).collect();
            if line.len() < needle.len() {
                continue;
            }
            for start in 0..=(line.len() - needle.len()) {
                if line[start..start + needle.len()] == needle[..] {
                    out.push(SearchMatch { line: abs, col_start: start, col_end: start + needle.len() });
                }
            }
        }
        out
    }

    /// Begin a simple (cell-granularity) text selection at viewport cell
    /// `(row, col)` (0 = top/left).
    pub fn selection_start(&mut self, row: usize, col: usize) {
        self.selection_start_kind(row, col, SelectKind::Cell);
    }

    /// Begin a selection of the given granularity — used for double-click (word)
    /// and triple-click (line). The selection auto-expands to the word/line at
    /// the click point; a subsequent [`selection_update`] drag extends it by the
    /// same granularity.
    pub fn selection_start_kind(&mut self, row: usize, col: usize, kind: SelectKind) {
        let offset = self.term.grid().display_offset();
        let point = viewport_to_point(offset, Point::new(row, Column(col)));
        let ty = match kind {
            SelectKind::Cell => SelectionType::Simple,
            SelectKind::Word => SelectionType::Semantic,
            SelectKind::Line => SelectionType::Lines,
        };
        self.term.selection = Some(Selection::new(ty, point, Side::Left));
        self.bump();
    }

    /// Extend the active selection to viewport cell `(row, col)`.
    pub fn selection_update(&mut self, row: usize, col: usize) {
        let offset = self.term.grid().display_offset();
        let point = viewport_to_point(offset, Point::new(row, Column(col)));
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(point, Side::Right);
        }
        self.bump();
    }

    /// Clear any active selection.
    pub fn clear_selection(&mut self) {
        self.term.selection = None;
        self.bump();
    }

    /// Whether a text selection is currently active.
    pub fn has_selection(&self) -> bool {
        self.term.selection.is_some()
    }

    /// The currently selected text, if any (handles line wrapping).
    pub fn selection_text(&self) -> Option<String> {
        self.term.selection_to_string()
    }

    /// Drain pending terminal events (Title, Bell, PtyWrite, ChildExit, ...).
    pub fn drain_events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Build an immutable snapshot of the visible grid.
    pub fn snapshot(&self) -> TerminalSnapshot {
        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;
        let colors = content.colors;
        let selection = content.selection;

        let mut cells = Vec::with_capacity(self.size.rows * self.size.cols);
        for indexed in content.display_iter {
            let point = indexed.point;
            let row = point.line.0 + offset;
            if row < 0 {
                continue;
            }
            let cell = indexed.cell;
            let mut fg = self.palette.resolve(cell.fg, colors);
            let mut bg = self.palette.resolve(cell.bg, colors);
            if cell.flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            // Selected cells take the theme's selection colors (baked in, so the
            // run batcher groups them and the renderer needs no selection logic).
            if selection.as_ref().is_some_and(|s| s.contains(point)) {
                fg = self.palette.selection_fg;
                bg = self.palette.selection_bg;
            }
            cells.push(SnapshotCell {
                row: row as usize,
                col: indexed.point.column.0,
                c: cell.c,
                fg,
                bg,
                flags: cell.flags,
            });
        }

        let cur = content.cursor.point;
        let cursor_row = (cur.line.0 + offset).max(0) as usize;
        TerminalSnapshot {
            rows: self.size.rows,
            cols: self.size.cols,
            cursor: (cursor_row, cur.column.0),
            cursor_visible: content.cursor.shape != CursorShape::Hidden,
            scroll_offset: content.display_offset,
            scroll_history: self.term.grid().history_size(),
            fg: self.palette.fg,
            bg: self.palette.bg,
            cells,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_plain_text_into_grid() {
        let mut t = Terminal::new(GridSize::new(5, 20));
        t.advance(b"hello world");
        let snap = t.snapshot();
        assert_eq!(snap.rows, 5);
        assert_eq!(snap.cols, 20);
        assert!(snap.to_text().contains("hello world"));
    }

    #[test]
    fn wide_char_runs_skip_spacer_and_span_two_cols() {
        // A CJK char is double-width: alacritty stores it (WIDE_CHAR) + a phantom
        // spacer in the next column. row_runs must drop the spacer (no half-width gap
        // after the char — the "间距那么大" bug) and report 2 columns for the char so
        // the renderer can size its box to the real grid.
        let mut t = Terminal::new(GridSize::new(2, 20));
        t.advance("中a".as_bytes()); // 中 = wide (2 cols), a = narrow (1 col)
        let rows = t.snapshot().row_runs();
        // Flatten row 0's runs into the visible chars + a column total.
        let text: String = rows[0].iter().flat_map(|r| r.text.chars()).collect();
        let cols: usize = rows[0].iter().map(|r| r.cols).sum();
        // No phantom space inserted between 中 and a.
        assert!(text.starts_with("中a"), "got {text:?}");
        // 中(2) + a(1) + 17 blank cols = 20 (the spacer did not add a column).
        assert_eq!(cols, 20, "run columns must equal the grid width");
        // The 中-bearing run reports 2 columns for that one char.
        let wide = rows[0].iter().find(|r| r.text.starts_with('中')).unwrap();
        assert!(wide.cols >= 2, "wide char spans >= 2 cols, got {}", wide.cols);
    }

    #[test]
    fn bel_byte_emits_bell_event() {
        // The BEL control byte (0x07) must surface as a `TermEvent::Bell` so the
        // UI can flash/beep (待优化清单 §3.8). alacritty raises it via the event
        // proxy; we just confirm it reaches `drain_events`.
        let mut t = Terminal::new(GridSize::new(2, 10));
        t.advance(b"a\x07b");
        let bells = t.drain_events().into_iter().filter(|e| matches!(e, Event::Bell)).count();
        assert_eq!(bells, 1, "one BEL byte yields exactly one Bell event");
        // The visible text is unaffected by the bell.
        assert!(t.snapshot().to_text().contains("ab"));
    }

    #[test]
    fn handles_newline_and_cr() {
        let mut t = Terminal::new(GridSize::new(5, 20));
        t.advance(b"line1\r\nline2");
        let text = t.snapshot().to_text();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "line1");
        assert_eq!(lines[1], "line2");
    }

    #[test]
    fn resolves_ansi_and_default_colors() {
        let mut t = Terminal::new(GridSize::new(2, 10));
        t.advance(b"\x1b[31mR"); // SGR 31 = red foreground
        let snap = t.snapshot();
        let r = snap.cells.iter().find(|c| c.c == 'R').unwrap();
        assert_eq!(r.fg, Rgb::new(0xF7, 0x76, 0x8E)); // Tn Dark ANSI red
        assert_eq!(r.bg, Rgb::new(0x1A, 0x1B, 0x26)); // default background
    }

    #[test]
    fn inverse_swaps_fg_bg() {
        let mut t = Terminal::new(GridSize::new(2, 10));
        t.advance(b"\x1b[7mX"); // SGR 7 = inverse
        let snap = t.snapshot();
        let x = snap.cells.iter().find(|c| c.c == 'X').unwrap();
        assert_eq!(x.fg, Rgb::new(0x1A, 0x1B, 0x26)); // was bg
        assert_eq!(x.bg, Rgb::new(0xC0, 0xCA, 0xF5)); // was fg
    }

    #[test]
    fn tracks_app_cursor_mode() {
        let mut t = Terminal::new(GridSize::new(2, 10));
        assert!(!t.input_mode().app_cursor);
        t.advance(b"\x1b[?1h"); // DECSET 1 = application cursor keys
        assert!(t.input_mode().app_cursor);
        t.advance(b"\x1b[?1l"); // DECRST 1
        assert!(!t.input_mode().app_cursor);
    }

    #[test]
    fn scrollback_changes_and_restores_view() {
        let mut t = Terminal::new(GridSize::new(3, 10));
        for i in 0..10 {
            t.advance(format!("line{i}\r\n").as_bytes());
        }
        let bottom = t.snapshot().to_text();
        // At the bottom: history accrued, offset 0 (scrollbar would sit at bottom).
        let snap = t.snapshot();
        assert!(snap.scroll_history > 0, "scrollback retained");
        assert_eq!(snap.scroll_offset, 0);
        t.scroll(2);
        assert_ne!(bottom, t.snapshot().to_text(), "scrolling up changes visible rows");
        assert_eq!(t.snapshot().scroll_offset, 2, "snapshot reflects scroll offset");
        t.scroll_to_bottom();
        assert_eq!(bottom, t.snapshot().to_text(), "scroll-to-bottom restores live view");
        assert_eq!(t.snapshot().scroll_offset, 0);
    }

    #[test]
    fn selection_extracts_text() {
        let mut t = Terminal::new(GridSize::new(2, 20));
        t.advance(b"hello world");
        assert!(!t.has_selection());
        t.selection_start(0, 0);
        t.selection_update(0, 4); // through the second 'l'/'o' of "hello"
        let s = t.selection_text().unwrap_or_default();
        assert!(s.starts_with("hello"), "selection text was {s:?}");
        t.clear_selection();
        assert!(!t.has_selection());
    }

    #[test]
    fn resize_preserves_content_via_scrollback() {
        // Resizing the viewport (e.g. dragging a split divider) must not lose
        // content: shrinking moves the top rows into scrollback (staying
        // bottom-anchored), and growing pulls them back into view.
        let mut t = Terminal::with_scrollback(GridSize::new(6, 20), 1000);
        for i in 0..20 {
            t.advance(format!("line{i}\r\n").as_bytes());
        }
        let before = t.snapshot().to_text();
        t.resize(GridSize::new(3, 20)); // shrink
        assert_eq!(t.scroll_position().0, 0, "stays bottom-anchored after shrink");
        assert!(t.scroll_position().1 >= 18, "shrunk rows go to scrollback");
        t.resize(GridSize::new(6, 20)); // grow back
        assert_eq!(t.snapshot().to_text(), before, "grow restores the prior view from history");
    }

    #[test]
    fn resize_conpty_grow_is_top_anchored_and_keeps_scrollback() {
        // `resize_conpty` (ConPTY-backed panes) top-anchors a grow: the prior
        // visible rows stay put, the new rows are blank at the bottom, and the
        // rows alacritty momentarily pulled from history are pushed back into
        // scrollback. This is what keeps ConPTY's top-anchored resize-repaint
        // from clobbering pulled-up history (probe: TN_RESIZE_EXP=topgrow → no
        // loss, vs `resize` losing 12 lines). Contrast with the bottom-anchored
        // `resize` above, which reveals older history at the top on a grow.
        let first_nonempty = |t: &mut Terminal| -> String {
            t.snapshot().rows_text().into_iter().find(|l| !l.trim().is_empty()).unwrap_or_default()
        };
        let mut t = Terminal::with_scrollback(GridSize::new(6, 20), 1000);
        for i in 0..20 {
            t.advance(format!("line{i}\r\n").as_bytes());
        }
        let top_before = first_nonempty(&mut t);
        let hist_before = t.scroll_position().1;
        assert!(hist_before > 0, "precondition: there is scrollback to (not) reveal");

        t.resize_conpty(GridSize::new(12, 20)); // grow taller

        assert_eq!(
            first_nonempty(&mut t).trim(),
            top_before.trim(),
            "top-anchored grow: the first visible line is unchanged (a bottom-anchored \
             `resize` would instead reveal an older line here)"
        );
        assert!(
            t.scroll_position().1 >= hist_before,
            "pulled rows were pushed back: scrollback didn't shrink (was {hist_before}, now {})",
            t.scroll_position().1
        );
    }

    #[test]
    fn word_selection_grabs_whole_word() {
        let mut t = Terminal::new(GridSize::new(2, 20));
        t.advance(b"hello world");
        // Double-click in the middle of "world" selects the whole word, no drag.
        t.selection_start_kind(0, 8, SelectKind::Word); // 'r' in "world"
        let s = t.selection_text().unwrap_or_default();
        assert_eq!(s.trim(), "world", "word selection was {s:?}");
    }

    #[test]
    fn line_selection_grabs_whole_line() {
        let mut t = Terminal::new(GridSize::new(2, 20));
        t.advance(b"hello world");
        // Triple-click anywhere on the row selects the whole line.
        t.selection_start_kind(0, 2, SelectKind::Line);
        let s = t.selection_text().unwrap_or_default();
        assert_eq!(s.trim(), "hello world", "line selection was {s:?}");
    }

    #[test]
    fn selection_highlights_cells() {
        let mut t = Terminal::new(GridSize::new(2, 20));
        t.advance(b"abc");
        t.selection_start(0, 0);
        t.selection_update(0, 1);
        let snap = t.snapshot();
        let a = snap.cells.iter().find(|c| c.c == 'a').unwrap();
        assert_eq!(a.bg, Rgb::new(0x28, 0x34, 0x57)); // selection_bg
        assert_eq!(a.fg, Rgb::new(0xC0, 0xCA, 0xF5)); // selection_fg
        // A cell outside the selection keeps the default background.
        let c = snap.cells.iter().find(|c| c.c == 'c').unwrap();
        assert_eq!(c.bg, Rgb::new(0x1A, 0x1B, 0x26));
    }

    #[test]
    fn resize_changes_dimensions() {
        let mut t = Terminal::new(GridSize::new(5, 20));
        t.resize(GridSize::new(10, 40));
        let snap = t.snapshot();
        assert_eq!(snap.rows, 10);
        assert_eq!(snap.cols, 40);
    }

    #[test]
    fn cursor_abs_line_grows_as_output_scrolls() {
        let mut t = Terminal::new(GridSize::new(3, 10));
        let start = t.cursor_abs_line();
        for i in 0..12 {
            t.advance(format!("line{i}\r\n").as_bytes());
        }
        // Output past the 3-row viewport pushes lines into history, so the
        // cursor's absolute line must advance monotonically (block anchors).
        assert!(
            t.cursor_abs_line() > start,
            "cursor absolute line advances as output scrolls into history"
        );
    }

    #[test]
    fn golden_snapshot_of_a_representative_ansi_program() {
        // Regression guard for the whole VT pipeline (待优化清单 §7.2): one fixed
        // input mixing SGR color, bold, a background fill, carriage-return
        // overwrite, and absolute cursor positioning must always yield this exact
        // grid + colors. Catches silent behavior drift when alacritty_terminal is
        // upgraded — a golden test the existing per-feature asserts didn't cover.
        let mut t = Terminal::new(GridSize::new(4, 20));
        t.advance(b"\x1b[31mred\x1b[0m\r\n"); // SGR fg red, then reset
        t.advance(b"\x1b[1;42mbold-green\x1b[0m\r\n"); // bold + green background
        t.advance(b"abcd\rxy\r\n"); // CR overwrite: "abcd" -> "xycd"
        t.advance(b"\x1b[4;5Htail"); // absolute cursor to row 4, col 5

        let snap = t.snapshot();
        assert_eq!(
            snap.to_text(),
            "red\nbold-green\nxycd\n    tail",
            "golden grid text drifted (VT parser behavior changed?)"
        );

        // Colors resolve through the default palette (Tokyo Night). `cells` is
        // row-major, so `find` hits the first matching cell (row 0 before row 1).
        let cell = |c: char| *snap.cells.iter().find(|x| x.c == c).unwrap();
        assert_eq!(cell('r').fg, Rgb::new(0xF7, 0x76, 0x8E), "ANSI red fg");
        let b = cell('b'); // first char of "bold-green"
        assert!(b.flags.contains(Flags::BOLD), "bold flag set");
        assert_eq!(b.bg, Rgb::new(0x9E, 0xCE, 0x6A), "ANSI green bg");
        // The CR-overwritten 'x' carries the default fg — no SGR leaked across the
        // reset from the prior line.
        assert_eq!(cell('x').fg, Rgb::new(0xC0, 0xCA, 0xF5), "default fg after reset");
    }

    #[test]
    fn search_finds_matches_in_visible_grid() {
        let mut t = Terminal::new(GridSize::new(4, 20));
        t.advance(b"hello world\r\nhello again\r\n");
        let m = t.search("hello");
        assert_eq!(m.len(), 2, "two rows begin with hello");
        assert_eq!((m[0].line, m[0].col_start, m[0].col_end), (0, 0, 5));
        assert_eq!(m[1].line, 1, "second hit on the next row");
        // A query inside a line is located at the right column.
        let w = t.search("world");
        assert_eq!((w[0].line, w[0].col_start, w[0].col_end), (0, 6, 11));
    }

    #[test]
    fn search_finds_multiple_hits_on_one_line() {
        let mut t = Terminal::new(GridSize::new(2, 20));
        t.advance(b"ab ab ab");
        let cols: Vec<usize> = t.search("ab").iter().map(|m| m.col_start).collect();
        assert_eq!(cols, vec![0, 3, 6]);
    }

    #[test]
    fn search_reaches_into_scrollback() {
        let mut t = Terminal::with_scrollback(GridSize::new(3, 20), 1000);
        for i in 0..20 {
            t.advance(format!("line{i}\r\n").as_bytes());
        }
        // "line0" scrolled into history long ago, but search still finds it — and
        // it's the oldest retained line, so its absolute line is 0.
        let m = t.search("line0");
        assert_eq!(m.len(), 1, "exactly one 'line0' (line10..19 don't contain it)");
        assert_eq!(m[0].line, 0);
        // A recent line sits at a higher absolute line than the oldest.
        let recent = t.search("line19");
        assert_eq!(recent.len(), 1);
        assert!(recent[0].line > m[0].line, "newer output is further down");
    }

    #[test]
    fn search_empty_query_and_no_match_yield_nothing() {
        let mut t = Terminal::new(GridSize::new(2, 10));
        t.advance(b"abc");
        assert!(t.search("").is_empty(), "empty query matches nothing");
        assert!(t.search("xyz").is_empty(), "absent text matches nothing");
    }

    #[test]
    fn urls_detected_with_position() {
        let mut t = Terminal::new(GridSize::new(4, 60));
        t.advance(b"visit https://example.com/path?q=1 now\r\n");
        let u = t.snapshot().urls();
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].url, "https://example.com/path?q=1");
        assert_eq!(u[0].row, 0);
        assert_eq!(u[0].col_start, 6); // after "visit "
        assert_eq!(u[0].col_end, 6 + "https://example.com/path?q=1".len());
    }

    #[test]
    fn urls_trim_trailing_punctuation_and_brackets() {
        let mut t = Terminal::new(GridSize::new(3, 60));
        t.advance(b"see (https://a.bc/x). end\r\n");
        let u = t.snapshot().urls();
        assert_eq!(u.len(), 1);
        assert_eq!(u[0].url, "https://a.bc/x", "trailing `).` trimmed");
    }

    #[test]
    fn urls_two_per_line_and_plain_http() {
        let mut t = Terminal::new(GridSize::new(2, 80));
        t.advance(b"http://a.com and https://b.org/y");
        let got: Vec<String> = t.snapshot().urls().into_iter().map(|x| x.url).collect();
        assert_eq!(got, vec!["http://a.com".to_string(), "https://b.org/y".to_string()]);
    }

    #[test]
    fn urls_ignore_bare_scheme_and_mid_word() {
        let mut t = Terminal::new(GridSize::new(2, 50));
        // a bare scheme (no host) and a scheme glued mid-token must not match.
        t.advance(b"text https:// and xhttps://nope\r\n");
        assert!(t.snapshot().urls().is_empty());
    }

    #[test]
    fn generation_bumps_on_mutation_only() {
        // The render cache (待优化清单 §2.1) keys on `generation`: it must advance
        // on every grid-affecting mutation and stay put for read-only calls (so a
        // cursor-blink repaint, which mutates nothing here, reuses the cache).
        let mut t = Terminal::new(GridSize::new(4, 20));
        let g0 = t.generation();
        let _ = t.snapshot();
        let _ = t.search("x");
        let _ = t.scroll_position();
        assert_eq!(t.generation(), g0, "read-only ops must not bump");

        let mut prev = g0;
        for step in [
            "advance", "scroll", "resize", "select", "select_update", "clear", "to_bottom",
        ] {
            match step {
                "advance" => t.advance(b"hi"),
                "scroll" => t.scroll(1),
                "resize" => t.resize(GridSize::new(6, 30)),
                "select" => t.selection_start(0, 0),
                "select_update" => t.selection_update(0, 2),
                "clear" => t.clear_selection(),
                "to_bottom" => t.scroll_to_bottom(),
                _ => unreachable!(),
            }
            assert!(t.generation() > prev, "{step} must bump the generation");
            prev = t.generation();
        }
    }
}
