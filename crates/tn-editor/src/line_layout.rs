//! Logical→visual line layout for soft wrapping (TnE-15).
//!
//! A *logical* line is one entry in the document's `Vec<String>`. A *visual* line
//! is one painted row. With [`WrapMode::None`] they are 1:1 (code files keep
//! horizontal scrolling). With [`WrapMode::Word`] a long logical line is split
//! into several visual lines at a display-column budget, preferring to break after
//! a space (prose / Markdown / log).
//!
//! This is a **headless model only** — no GPUI, no painting. It provides the
//! logical↔visual mapping, visual hit-testing and selection/search range
//! projection a future renderer (TnE-16) needs. Columns are char indices; widths
//! are display columns counting non-ASCII as 2 (the same 1/2-col fixed-cell model
//! used everywhere in Tn). Tab-stop expansion is out of scope: a tab counts as one
//! column, matching the current renderer's `disp_width`.

use std::ops::Range;

use crate::{Cursor, TextRange};

/// Display columns occupied by one char (ASCII / control = 1, CJK / wide = 2).
fn char_cols(c: char) -> usize {
    if c.is_ascii() {
        1
    } else {
        2
    }
}

/// How logical lines are mapped onto visual lines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WrapMode {
    /// One visual line per logical line; long lines scroll horizontally (code).
    None,
    /// Wrap each logical line at `width_cols` display columns, preferring to break
    /// after a space (prose). `width_cols` is clamped to at least 1.
    Word { width_cols: usize },
}

/// One painted row: a `[char_start, char_end)` slice of a logical line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisualLine {
    /// Index of the logical line this visual row belongs to.
    pub logical_row: usize,
    /// First char index (inclusive) of this visual row within the logical line.
    pub char_start: usize,
    /// Char index (exclusive) one past this visual row's last char.
    pub char_end: usize,
}

impl VisualLine {
    /// Char count of this visual row.
    pub fn len(&self) -> usize {
        self.char_end - self.char_start
    }

    pub fn is_empty(&self) -> bool {
        self.char_end == self.char_start
    }
}

/// A precomputed logical→visual mapping for a document under a [`WrapMode`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineLayout {
    visual: Vec<VisualLine>,
    /// For each logical row, the half-open range of indices into `visual`.
    row_to_visual: Vec<Range<usize>>,
}

impl LineLayout {
    /// Build the layout for `lines` under `mode`.
    pub fn build(lines: &[String], mode: WrapMode) -> Self {
        let mut visual = Vec::new();
        let mut row_to_visual = Vec::with_capacity(lines.len());
        for (row, line) in lines.iter().enumerate() {
            let start_idx = visual.len();
            match mode {
                WrapMode::None => {
                    visual.push(VisualLine {
                        logical_row: row,
                        char_start: 0,
                        char_end: line.chars().count(),
                    });
                }
                WrapMode::Word { width_cols } => {
                    wrap_logical_line(row, line, width_cols.max(1), &mut visual);
                }
            }
            row_to_visual.push(start_idx..visual.len());
        }
        // An empty document still has one (empty) logical line in `Document`, but
        // guard the truly-empty slice so callers never index an empty layout.
        if visual.is_empty() {
            visual.push(VisualLine {
                logical_row: 0,
                char_start: 0,
                char_end: 0,
            });
            row_to_visual.push(0..1);
        }
        Self {
            visual,
            row_to_visual,
        }
    }

    /// Total number of visual (painted) rows.
    pub fn visual_count(&self) -> usize {
        self.visual.len()
    }

    /// The visual line at `idx`, if any.
    pub fn visual_line(&self, idx: usize) -> Option<VisualLine> {
        self.visual.get(idx).copied()
    }

    /// All visual rows (in paint order).
    pub fn visual_lines(&self) -> &[VisualLine] {
        &self.visual
    }

    /// The range of visual indices belonging to logical `row`.
    pub fn visual_range_of_row(&self, row: usize) -> Range<usize> {
        self.row_to_visual.get(row).cloned().unwrap_or(0..0)
    }

    /// Map a logical cursor `(row, col)` to `(visual_index, col_within_visual)`.
    /// A column at a wrap boundary maps to the **start of the next** visual line
    /// (so a caret at the wrap point sits at the next row's left edge), except the
    /// very end of the logical line, which stays on its last visual row.
    pub fn logical_to_visual(&self, cursor: Cursor) -> (usize, usize) {
        let (row, col) = cursor;
        let range = self.visual_range_of_row(row);
        if range.is_empty() {
            return (0, 0);
        }
        for idx in range.clone() {
            let vl = self.visual[idx];
            let is_last = idx + 1 == range.end;
            if col < vl.char_end || (is_last && col <= vl.char_end) {
                let local = col.saturating_sub(vl.char_start);
                return (idx, local.min(vl.len()));
            }
        }
        // col past the end → clamp to the last visual row's end.
        let last = range.end - 1;
        (last, self.visual[last].len())
    }

    /// Map `(visual_index, col_within_visual)` back to a logical cursor.
    pub fn visual_to_logical(&self, visual_idx: usize, col_within: usize) -> Cursor {
        match self.visual.get(visual_idx) {
            Some(vl) => (
                vl.logical_row,
                (vl.char_start + col_within).min(vl.char_end),
            ),
            None => (0, 0),
        }
    }

    /// Project a logical [`TextRange`] onto per-visual-line highlight segments,
    /// each `(visual_index, col_start_char, col_end_char)` with char columns
    /// **local to that visual line**. Used to paint a selection / search match that
    /// may span several visual (and logical) rows.
    pub fn range_segments(&self, range: TextRange) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::new();
        if range.is_collapsed() {
            return out;
        }
        let (sr, sc) = range.start;
        let (er, ec) = range.end;
        for (idx, vl) in self.visual.iter().enumerate() {
            let row = vl.logical_row;
            if row < sr || row > er {
                continue;
            }
            // Selection's char span on this logical row.
            let row_lo = if row == sr { sc } else { 0 };
            let row_hi = if row == er { ec } else { usize::MAX };
            // Intersect with this visual line's char window.
            let seg_start = row_lo.max(vl.char_start);
            let seg_end = row_hi.min(vl.char_end);
            if seg_start < seg_end {
                out.push((idx, seg_start - vl.char_start, seg_end - vl.char_start));
            }
        }
        out
    }

    /// Hit-test a pointer at `rel_x` (pixels from the glyph start) on visual line
    /// `visual_idx` to a logical cursor. `char_w` is the single-column advance.
    /// Uses the same caret-rounding (past a glyph's midpoint lands to its right) as
    /// the editor's flat hit-testing, but scoped to the visual line's char window.
    pub fn hit_test(&self, lines: &[String], visual_idx: usize, rel_x: f32, char_w: f32) -> Cursor {
        let Some(vl) = self.visual.get(visual_idx).copied() else {
            return (0, 0);
        };
        let Some(line) = lines.get(vl.logical_row) else {
            return (vl.logical_row, vl.char_start);
        };
        if rel_x <= 0.0 || char_w <= 0.0 {
            return (vl.logical_row, vl.char_start);
        }
        let target = rel_x / char_w;
        let mut acc = 0.0f32;
        let mut col = vl.char_start;
        for c in line.chars().skip(vl.char_start).take(vl.len()) {
            let w = char_cols(c) as f32;
            if target < acc + w {
                let here = if target < acc + w / 2.0 { col } else { col + 1 };
                return (vl.logical_row, here.min(vl.char_end));
            }
            acc += w;
            col += 1;
        }
        (vl.logical_row, vl.char_end)
    }
}

/// Greedy word wrap of one logical line into `out`. Breaks after a space when one
/// is available before the budget, else hard-breaks. A char wider than the whole
/// budget gets its own visual line (no infinite loop: `start` strictly advances).
fn wrap_logical_line(row: usize, line: &str, width: usize, out: &mut Vec<VisualLine>) {
    let chars: Vec<char> = line.chars().collect();
    let n = chars.len();
    if n == 0 {
        out.push(VisualLine {
            logical_row: row,
            char_start: 0,
            char_end: 0,
        });
        return;
    }
    let mut start = 0usize;
    let mut cols = 0usize;
    // Char index just after the most recent space within the current visual line.
    let mut last_break: Option<usize> = None;
    let mut i = 0usize;
    while i < n {
        let w = char_cols(chars[i]);
        if cols + w > width && i > start {
            let brk = match last_break {
                Some(b) if b > start => b,
                _ => i,
            };
            out.push(VisualLine {
                logical_row: row,
                char_start: start,
                char_end: brk,
            });
            start = brk;
            last_break = None;
            cols = chars[start..i].iter().map(|&c| char_cols(c)).sum();
            // Re-evaluate chars[i] on the new visual line (don't advance i).
            continue;
        }
        cols += w;
        i += 1;
        if i < n && chars[i - 1] == ' ' {
            last_break = Some(i);
        }
    }
    out.push(VisualLine {
        logical_row: row,
        char_start: start,
        char_end: n,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_wrap_is_one_visual_per_logical() {
        let lines = buf(&["alpha", "bravo charlie", ""]);
        let lay = LineLayout::build(&lines, WrapMode::None);
        assert_eq!(lay.visual_count(), 3);
        assert_eq!(
            lay.visual_line(1).unwrap(),
            VisualLine {
                logical_row: 1,
                char_start: 0,
                char_end: 13
            }
        );
        // round-trip identity for a column mid-line.
        let (vi, col) = lay.logical_to_visual((1, 6));
        assert_eq!((vi, col), (1, 6));
        assert_eq!(lay.visual_to_logical(vi, col), (1, 6));
    }

    #[test]
    fn word_wrap_breaks_after_spaces() {
        // width 10: "the quick " (10 cols incl trailing space) then "brown fox".
        let lines = buf(&["the quick brown fox"]);
        let lay = LineLayout::build(&lines, WrapMode::Word { width_cols: 10 });
        let segs: Vec<String> = lay
            .visual_lines()
            .iter()
            .map(|vl| {
                lines[vl.logical_row]
                    .chars()
                    .skip(vl.char_start)
                    .take(vl.len())
                    .collect::<String>()
            })
            .collect();
        assert_eq!(segs, buf(&["the quick ", "brown fox"]));
    }

    #[test]
    fn word_wrap_hard_breaks_a_long_unbroken_token() {
        // No spaces, width 4 → hard breaks every 4 chars.
        let lines = buf(&["abcdefghij"]);
        let lay = LineLayout::build(&lines, WrapMode::Word { width_cols: 4 });
        let lens: Vec<usize> = lay.visual_lines().iter().map(|v| v.len()).collect();
        assert_eq!(lens, vec![4, 4, 2]);
    }

    #[test]
    fn word_wrap_counts_cjk_as_two_columns() {
        // width 4 cols → two CJK chars (2+2) per visual line.
        let lines = buf(&["中文测试"]);
        let lay = LineLayout::build(&lines, WrapMode::Word { width_cols: 4 });
        let lens: Vec<usize> = lay.visual_lines().iter().map(|v| v.len()).collect();
        assert_eq!(lens, vec![2, 2]); // 2 chars each, 4 cols each
                                      // a char wider than the budget still makes progress (width 1, CJK=2).
        let lay = LineLayout::build(&lines, WrapMode::Word { width_cols: 1 });
        assert_eq!(lay.visual_count(), 4);
    }

    #[test]
    fn empty_and_tab_lines() {
        let lines = buf(&["", "\tindented"]);
        let lay = LineLayout::build(&lines, WrapMode::Word { width_cols: 8 });
        // empty logical line → exactly one empty visual line.
        assert_eq!(lay.visual_range_of_row(0).len(), 1);
        assert!(lay
            .visual_line(lay.visual_range_of_row(0).start)
            .unwrap()
            .is_empty());
        // tab counts as 1 col: "\tindented" = 9 cols → wraps at 8.
        assert!(lay.visual_range_of_row(1).len() >= 2);
    }

    #[test]
    fn logical_to_visual_maps_wrap_boundary_to_next_row() {
        let lines = buf(&["the quick brown fox"]);
        let lay = LineLayout::build(&lines, WrapMode::Word { width_cols: 10 });
        // col 10 is the wrap boundary (end of "the quick ") → start of visual row 1.
        let (vi, col) = lay.logical_to_visual((0, 10));
        assert_eq!((vi, col), (1, 0));
        // end of line stays on the last visual row.
        let end = lines[0].chars().count();
        let (vi, col) = lay.logical_to_visual((0, end));
        assert_eq!(vi, 1);
        assert_eq!(lay.visual_to_logical(vi, col), (0, end));
    }

    #[test]
    fn range_segments_span_multiple_visual_lines() {
        let lines = buf(&["the quick brown fox"]);
        let lay = LineLayout::build(&lines, WrapMode::Word { width_cols: 10 });
        // select chars [4, 13) = "quick bro" across the wrap at 10.
        let segs = lay.range_segments(TextRange::new((0, 4), (0, 13)));
        // visual 0 = [0,10): local cols 4..10 ; visual 1 = [10,19): local 0..3.
        assert_eq!(segs, vec![(0, 4, 10), (1, 0, 3)]);
        // collapsed range → no segments.
        assert!(lay
            .range_segments(TextRange::new((0, 5), (0, 5)))
            .is_empty());
    }

    #[test]
    fn range_segments_span_multiple_logical_rows() {
        let lines = buf(&["abc", "def", "ghi"]);
        let lay = LineLayout::build(&lines, WrapMode::None);
        // select (0,1)..(2,2) → "bc","def","gh".
        let segs = lay.range_segments(TextRange::new((0, 1), (2, 2)));
        assert_eq!(segs, vec![(0, 1, 3), (1, 0, 3), (2, 0, 2)]);
    }

    #[test]
    fn hit_test_within_visual_line_handles_cjk() {
        let lines = buf(&["the quick brown fox"]);
        let lay = LineLayout::build(&lines, WrapMode::Word { width_cols: 10 });
        // On visual row 1 ("brown fox"), x at 2.5 cols (char_w=10) → caret col 3
        // local → logical col 10+3 = 13.
        let cur = lay.hit_test(&lines, 1, 25.0, 10.0);
        assert_eq!(cur, (0, 13));
        // x past end clamps to the visual line end.
        let cur = lay.hit_test(&lines, 1, 9999.0, 10.0);
        assert_eq!(cur, (0, 19));
        // CJK hit-test: width 4 → "中文" on row 0. x at 1.5 cols → inside 中,
        // caret rounds left (idx 0).
        let cjk = buf(&["中文测试"]);
        let lay = LineLayout::build(&cjk, WrapMode::Word { width_cols: 4 });
        assert_eq!(lay.hit_test(&cjk, 0, 15.0, 10.0), (0, 1)); // 1.5 cols > mid 1.0 → 1
    }

    #[test]
    fn empty_document_layout_is_safe() {
        let lay = LineLayout::build(&[], WrapMode::None);
        assert_eq!(lay.visual_count(), 1);
        assert_eq!(lay.logical_to_visual((0, 0)), (0, 0));
        assert_eq!(lay.visual_to_logical(0, 0), (0, 0));
    }
}
