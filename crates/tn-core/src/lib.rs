//! Tn terminal engine: a headless wrapper around `alacritty_terminal`.
//!
//! [`Terminal`] owns an alacritty [`Term`] plus a VTE [`Processor`]. PTY output
//! bytes are fed in via [`Terminal::advance`]; the renderer pulls an immutable
//! [`TerminalSnapshot`] via [`Terminal::snapshot`]. No GPUI, no IO — this crate
//! is fully headless and unit-testable.

use std::sync::mpsc::{Receiver, Sender};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;

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

/// Event sink that forwards alacritty terminal events into an mpsc channel so
/// the owning thread can drain them (title changes, bells, PTY write-backs...).
struct ChannelListener(Sender<Event>);

impl EventListener for ChannelListener {
    fn send_event(&self, event: Event) {
        // Receiver gone simply means the Terminal was dropped; ignore.
        let _ = self.0.send(event);
    }
}

/// A single rendered cell. Minimal for M0 (char + style flags); fg/bg colors
/// are added alongside the theme system in M1.
#[derive(Clone, Copy, Debug)]
pub struct SnapshotCell {
    /// Row within the viewport, 0 = top.
    pub row: usize,
    /// Column within the viewport, 0 = left.
    pub col: usize,
    pub c: char,
    pub flags: Flags,
}

/// An immutable view of the visible terminal grid, produced for rendering.
#[derive(Clone, Debug, Default)]
pub struct TerminalSnapshot {
    pub rows: usize,
    pub cols: usize,
    /// Cursor position within the viewport as (row, col).
    pub cursor: (usize, usize),
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
}

/// A headless terminal: VTE parser + alacritty grid + event channel.
pub struct Terminal {
    term: Term<ChannelListener>,
    parser: Processor,
    size: GridSize,
    events: Receiver<Event>,
}

impl Terminal {
    /// Create a terminal of the given viewport size with default scrollback.
    pub fn new(size: GridSize) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let term = Term::new(Config::default(), &size, ChannelListener(tx));
        Self {
            term,
            parser: Processor::new(),
            size,
            events: rx,
        }
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

    /// Drain pending terminal events (Title, Bell, PtyWrite, ChildExit, ...).
    pub fn drain_events(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Build an immutable snapshot of the visible grid.
    pub fn snapshot(&self) -> TerminalSnapshot {
        let content = self.term.renderable_content();
        let offset = content.display_offset as i32;

        let mut cells = Vec::with_capacity(self.size.rows * self.size.cols);
        for indexed in content.display_iter {
            let row = indexed.point.line.0 + offset;
            if row < 0 {
                continue;
            }
            cells.push(SnapshotCell {
                row: row as usize,
                col: indexed.point.column.0,
                c: indexed.cell.c,
                flags: indexed.cell.flags,
            });
        }

        let cur = content.cursor.point;
        let cursor_row = (cur.line.0 + offset).max(0) as usize;
        TerminalSnapshot {
            rows: self.size.rows,
            cols: self.size.cols,
            cursor: (cursor_row, cur.column.0),
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
    fn resize_changes_dimensions() {
        let mut t = Terminal::new(GridSize::new(5, 20));
        t.resize(GridSize::new(10, 40));
        let snap = t.snapshot();
        assert_eq!(snap.rows, 10);
        assert_eq!(snap.cols, 40);
    }
}
