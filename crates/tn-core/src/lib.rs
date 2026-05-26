//! Tn terminal engine: a headless wrapper around `alacritty_terminal`.
//!
//! [`Terminal`] owns an alacritty [`Term`] plus a VTE [`Processor`]. PTY output
//! bytes are fed in via [`Terminal::advance`]; the renderer pulls an immutable
//! [`TerminalSnapshot`] via [`Terminal::snapshot`]. No GPUI, no IO — this crate
//! is fully headless and unit-testable.

use std::sync::mpsc::{Receiver, Sender};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Column, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::color::Colors;
use alacritty_terminal::term::{viewport_to_point, Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, Processor};

/// Re-export so consumers can match on terminal events without depending on
/// alacritty_terminal directly.
pub use alacritty_terminal::event::Event as TermEvent;

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
}

/// An immutable view of the visible terminal grid, produced for rendering.
#[derive(Clone, Debug, Default)]
pub struct TerminalSnapshot {
    pub rows: usize,
    pub cols: usize,
    /// Cursor position within the viewport as (row, col).
    pub cursor: (usize, usize),
    /// Default foreground / background (resolved theme colors).
    pub fg: Rgb,
    pub bg: Rgb,
    /// Visible cells, row-major. Default blank cells are included so renderers
    /// and [`TerminalSnapshot::to_text`] can address the grid directly.
    pub cells: Vec<SnapshotCell>,
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
                for cell in row {
                    let ch = if cell.c == '\0' { ' ' } else { cell.c };
                    let bold = cell.flags.contains(Flags::BOLD);
                    let italic = cell.flags.contains(Flags::ITALIC);
                    let underline = cell.flags.contains(Flags::UNDERLINE);
                    match runs.last_mut() {
                        Some(r)
                            if r.fg == cell.fg
                                && r.bg == cell.bg
                                && r.bold == bold
                                && r.italic == italic
                                && r.underline == underline =>
                        {
                            r.text.push(ch);
                        }
                        _ => runs.push(CellRun {
                            text: ch.to_string(),
                            fg: cell.fg,
                            bg: cell.bg,
                            bold,
                            italic,
                            underline,
                        }),
                    }
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
        }
    }

    /// Replace the color palette (e.g. from the user's theme).
    pub fn set_palette(&mut self, palette: Palette) {
        self.palette = palette;
    }

    /// Feed raw PTY output bytes into the parser and grid.
    pub fn advance(&mut self, bytes: &[u8]) {
        // Disjoint field borrows: `parser` (receiver) and `term` (argument).
        self.parser.advance(&mut self.term, bytes);
    }

    /// Resize the grid to a new viewport size.
    pub fn resize(&mut self, size: GridSize) {
        self.size = size;
        self.term.resize(size);
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

    /// Scroll the viewport through scrollback by `lines` (positive = back toward
    /// older output). Clamped to the history bounds by the engine.
    pub fn scroll(&mut self, lines: i32) {
        self.term.scroll_display(Scroll::Delta(lines));
    }

    /// Jump the viewport back to the live bottom (latest output).
    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    /// Begin a simple text selection at viewport cell `(row, col)` (0 = top/left).
    pub fn selection_start(&mut self, row: usize, col: usize) {
        let offset = self.term.grid().display_offset();
        let point = viewport_to_point(offset, Point::new(row, Column(col)));
        self.term.selection = Some(Selection::new(SelectionType::Simple, point, Side::Left));
    }

    /// Extend the active selection to viewport cell `(row, col)`.
    pub fn selection_update(&mut self, row: usize, col: usize) {
        let offset = self.term.grid().display_offset();
        let point = viewport_to_point(offset, Point::new(row, Column(col)));
        if let Some(sel) = self.term.selection.as_mut() {
            sel.update(point, Side::Right);
        }
    }

    /// Clear any active selection.
    pub fn clear_selection(&mut self) {
        self.term.selection = None;
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
        t.scroll(2);
        assert_ne!(bottom, t.snapshot().to_text(), "scrolling up changes visible rows");
        t.scroll_to_bottom();
        assert_eq!(bottom, t.snapshot().to_text(), "scroll-to-bottom restores live view");
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
}
