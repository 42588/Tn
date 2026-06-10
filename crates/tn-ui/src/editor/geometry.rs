//! Read-only layout/geometry model for the Tn editor renderer (TnE-08).
//!
//! These are **pure functions** — no GPUI, no window, no `&self`. They mirror the
//! geometry the current Quick Look `uniform_list` renderer computes inline
//! (`quick_look.rs`) so a future `EditorElement` (TnE-09+) can layout/prepaint
//! against one tested model shared by Quick Look and Diff Review.
//!
//! Conventions (matching the existing renderer):
//! - **Columns are char-based**; display width counts CJK / non-ASCII as 2 cols
//!   (fixed-cell rendering, see `disp_width` in `quick_look.rs`). The mismatch
//!   between a 2-col model and the CJK glyph advance is what made naive
//!   `rel/char_w` hit-testing drift on Chinese text — so everything here uses the
//!   1/2-col model, never a raw char count.
//! - **Pixels are logical** (`f32`); `char_w` is the single-column advance, the
//!   width of one ASCII glyph.
//! - The text area starts after a fixed line-number gutter; the y axis is the
//!   `uniform_list` scroll offset (negative as content scrolls up).

/// One visual row's height. Must stay fixed: `uniform_list` only measures row 0
/// and lays the rest out by this height (see CLAUDE.md uniform_list pit).
pub const ROW_H: f32 = 20.0;

/// Line-number gutter width: ln(38) + mr(14) + mk(14) = 66.
pub const CODE_GUTTER: f32 = 66.0;

/// Inner inset of the horizontal scrollbar track (left == right).
pub const HSCROLL_INSET: f32 = 6.0;

/// Minimum horizontal-scrollbar thumb width.
pub const HSCROLL_MIN_THUMB: f32 = 36.0;

/// Fixed cell metrics shared by every geometry computation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Metrics {
    /// Single-column advance (one ASCII glyph wide).
    pub char_w: f32,
    /// Visual row height.
    pub row_h: f32,
    /// Line-number gutter width before the text area.
    pub gutter: f32,
}

impl Metrics {
    pub fn new(char_w: f32) -> Self {
        Self {
            char_w,
            row_h: ROW_H,
            gutter: CODE_GUTTER,
        }
    }
}

/// Display width of a string in columns (CJK / non-ASCII = 2, ASCII = 1).
pub fn disp_width(s: &str) -> usize {
    s.chars().map(|c| if c.is_ascii() { 1 } else { 2 }).sum()
}

/// Display width (cols) of the first `col` characters of `line`. Used to place the
/// caret: a column index counts chars, but its x offset is in display columns.
pub fn prefix_cols(line: &str, col: usize) -> usize {
    line.chars()
        .take(col)
        .map(|c| if c.is_ascii() { 1 } else { 2 })
        .sum()
}

/// Widest line's display width (cols) across `lines` — drives content width.
pub fn max_cols(lines: &[String]) -> usize {
    lines.iter().map(|l| disp_width(l)).max().unwrap_or(0)
}

/// X of the caret within content space: `gutter + prefix_cols × char_w`. Matches
/// `caret_content_x` in the renderer (fixed-cell ⇒ column↔pixel is exact).
pub fn caret_x(line: &str, col: usize, m: Metrics) -> f32 {
    m.gutter + prefix_cols(line, col) as f32 * m.char_w
}

/// Total content width: widest line plus one trailing column for the end-of-line
/// caret, but **at least** the viewport so short content fills the view and no
/// spurious horizontal scrollbar appears. Mirrors `content_w` in the renderer.
pub fn content_width(max_cols: usize, m: Metrics, viewport_w: f32) -> f32 {
    (m.gutter + (max_cols as f32 + 1.0) * m.char_w).max(viewport_w)
}

/// Maximum horizontal scroll offset (0 when content fits the viewport).
pub fn max_h_offset(content_w: f32, viewport_w: f32) -> f32 {
    (content_w - viewport_w).max(0.0)
}

/// Map a horizontal pixel offset (relative to the glyph start, i.e. *after* the
/// gutter) to the char index **under the pointer** (floor). Used for drag extent.
pub fn hover_char_at_x(line: &str, rel_x: f32, char_w: f32) -> usize {
    if rel_x <= 0.0 || char_w <= 0.0 {
        return 0;
    }
    let target = rel_x / char_w;
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
/// position): past a glyph's midpoint the caret lands to its right. Used for
/// click-to-place-cursor.
pub fn caret_col_at_x(line: &str, rel_x: f32, char_w: f32) -> usize {
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

/// The range of rows currently visible given the (negative) scroll offset and the
/// viewport height. `first` is the topmost visible row index; `count` is how many
/// rows fit; `last` is the bottommost visible row index.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibleRows {
    pub first: usize,
    pub count: usize,
    pub last: usize,
}

/// Compute the visible row window. `offset_y` is the `uniform_list` scroll offset
/// (≤ 0 as content scrolls up). Mirrors the renderer's caret-follow row math.
pub fn visible_rows(offset_y: f32, viewport_h: f32, row_h: f32) -> VisibleRows {
    if row_h <= 0.0 || viewport_h <= 0.0 {
        return VisibleRows {
            first: 0,
            count: 0,
            last: 0,
        };
    }
    let first = (-offset_y / row_h).floor().max(0.0) as usize;
    let count = (viewport_h / row_h).floor() as usize;
    let last = first + count.saturating_sub(1);
    VisibleRows { first, count, last }
}

/// Whether `row` is outside the visible window — i.e. caret-follow should recenter.
pub fn row_out_of_view(row: usize, vis: VisibleRows) -> bool {
    row < vis.first || row > vis.last
}

/// New horizontal scroll offset so the caret stays visible with a margin. Returns
/// the current offset unchanged when the caret is already comfortably in view.
/// Mirrors the renderer's horizontal caret-follow (margin = 4 columns by default).
pub fn follow_h_offset(
    caret_x: f32,
    current_off: f32,
    viewport_w: f32,
    max_off: f32,
    margin: f32,
) -> f32 {
    let mut off = current_off;
    if caret_x < off + margin {
        off = (caret_x - margin).max(0.0);
    } else if caret_x > off + viewport_w - margin {
        off = caret_x - viewport_w + margin;
    }
    off.clamp(0.0, max_off)
}

/// Horizontal scrollbar thumb geometry within its track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HScrollThumb {
    /// Usable track width (viewport minus both insets).
    pub track_w: f32,
    /// Thumb width (proportional to viewport/content, clamped to a minimum).
    pub thumb_w: f32,
    /// Thumb left edge x, relative to the track origin (already includes inset).
    pub thumb_x: f32,
}

/// Compute the horizontal thumb geometry. Mirrors the renderer's thumb math and
/// `on_hscroll_move`'s inverse, so the model and the drag handler agree.
pub fn h_scroll_thumb(content_w: f32, viewport_w: f32, h_off: f32, max_off: f32) -> HScrollThumb {
    let track_w = (viewport_w - HSCROLL_INSET * 2.0).max(1.0);
    let thumb_w = if content_w > 0.0 {
        (track_w / content_w * track_w).clamp(HSCROLL_MIN_THUMB, track_w)
    } else {
        track_w
    };
    let thumb_x = if max_off > 0.0 {
        HSCROLL_INSET + h_off / max_off * (track_w - thumb_w)
    } else {
        HSCROLL_INSET
    };
    HScrollThumb {
        track_w,
        thumb_w,
        thumb_x,
    }
}

/// Inverse of the thumb geometry: given a drag of the thumb to `cursor_x` (absolute
/// mouse x), with `grab` the pointer's offset within the thumb at grab time and
/// `track_left` the track origin x, return the new horizontal scroll offset.
/// Mirrors `QuickLook::on_hscroll_move`.
pub fn h_offset_from_drag(
    cursor_x: f32,
    track_left: f32,
    grab: f32,
    content_w: f32,
    viewport_w: f32,
    max_off: f32,
) -> f32 {
    if max_off <= 0.0 || viewport_w <= 0.0 {
        return 0.0;
    }
    let thumb = h_scroll_thumb(content_w, viewport_w, 0.0, max_off);
    let usable = (thumb.track_w - thumb.thumb_w).max(1.0);
    let thumb_left = (cursor_x - track_left - HSCROLL_INSET - grab).clamp(0.0, usable);
    thumb_left / usable * max_off
}

/// IME candidate / caret rect origin in absolute space: the text-area element
/// origin x plus the gutter plus the caret's display-column offset. Use the same
/// `caret_x` model as the visible caret so the candidate window tracks the glyph.
pub fn caret_abs_x(element_origin_x: f32, line: &str, col: usize, m: Metrics) -> f32 {
    element_origin_x + caret_x(line, col, m)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buf(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|s| s.to_string()).collect()
    }

    const M: Metrics = Metrics {
        char_w: 8.0,
        row_h: ROW_H,
        gutter: CODE_GUTTER,
    };

    #[test]
    fn disp_width_counts_cjk_as_two() {
        assert_eq!(disp_width("abc"), 3);
        assert_eq!(disp_width("a中b"), 4);
        assert_eq!(disp_width("中文"), 4);
        assert_eq!(disp_width(""), 0);
    }

    #[test]
    fn prefix_cols_and_caret_x_use_display_columns() {
        // "a中b": col 0 → 0 cols, col 1 → 1, col 2 (after 中) → 3, col 3 → 4.
        assert_eq!(prefix_cols("a中b", 0), 0);
        assert_eq!(prefix_cols("a中b", 1), 1);
        assert_eq!(prefix_cols("a中b", 2), 3);
        assert_eq!(prefix_cols("a中b", 3), 4);
        // caret_x = gutter + prefix_cols × char_w
        assert_eq!(caret_x("a中b", 2, M), CODE_GUTTER + 3.0 * 8.0);
    }

    #[test]
    fn content_width_is_at_least_viewport_and_adds_trailing_col() {
        let lines = buf(&["abc", "a中b", "x"]);
        // widest = "a中b" = 4 cols.
        assert_eq!(max_cols(&lines), 4);
        // long line: gutter + (4+1)*8 = 66 + 40 = 106, viewport 50 → 106.
        assert_eq!(content_width(max_cols(&lines), M, 50.0), 106.0);
        // short content: max(106, 400) = 400 (fills viewport, no scrollbar).
        assert_eq!(content_width(max_cols(&lines), M, 400.0), 400.0);
        assert_eq!(max_h_offset(106.0, 400.0), 0.0);
        assert_eq!(max_h_offset(400.0, 106.0), 294.0);
    }

    #[test]
    fn hover_floor_and_caret_round_handle_cjk() {
        // char_w = 10 for easy mental math. "a中b": cols a[0,1) 中[1,3) b[3,4).
        let line = "a中b";
        // pointer at 1.5 cols → inside 中 (floor → idx 1).
        assert_eq!(hover_char_at_x(line, 15.0, 10.0), 1);
        // pointer past end → char count.
        assert_eq!(hover_char_at_x(line, 999.0, 10.0), 3);
        assert_eq!(hover_char_at_x(line, 0.0, 10.0), 0);
        // caret rounding: 中 spans cols [1,3), midpoint at 2.0. Before mid → idx 1,
        // after mid → idx 2.
        assert_eq!(caret_col_at_x(line, 15.0, 10.0), 1); // 1.5 cols < mid 2.0
        assert_eq!(caret_col_at_x(line, 25.0, 10.0), 2); // 2.5 cols > mid 2.0
    }

    #[test]
    fn visible_rows_window_and_out_of_view() {
        // offset 0, viewport 100, row 20 → rows 0..=4 (5 rows).
        let v = visible_rows(0.0, 100.0, 20.0);
        assert_eq!(
            v,
            VisibleRows {
                first: 0,
                count: 5,
                last: 4
            }
        );
        assert!(!row_out_of_view(4, v));
        assert!(row_out_of_view(5, v));
        // scrolled up by 50px → first = floor(50/20) = 2 → rows 2..=6.
        let v = visible_rows(-50.0, 100.0, 20.0);
        assert_eq!(v.first, 2);
        assert_eq!(v.last, 6);
        assert!(row_out_of_view(1, v));
        // degenerate viewport → empty window.
        assert_eq!(visible_rows(0.0, 0.0, 20.0).count, 0);
    }

    #[test]
    fn follow_h_offset_keeps_caret_in_margin() {
        // viewport 100, margin 10, max_off 500.
        // caret at 5 (< off 0 + margin 10) → scroll left to max(5-10,0)=0.
        assert_eq!(follow_h_offset(5.0, 0.0, 100.0, 500.0, 10.0), 0.0);
        // caret at 200 with off 0: 200 > 0 + 100 - 10 = 90 → off = 200-100+10 = 110.
        assert_eq!(follow_h_offset(200.0, 0.0, 100.0, 500.0, 10.0), 110.0);
        // caret comfortably in view → unchanged.
        assert_eq!(follow_h_offset(50.0, 0.0, 100.0, 500.0, 10.0), 0.0);
        // clamps to max_off.
        assert_eq!(follow_h_offset(9999.0, 0.0, 100.0, 500.0, 10.0), 500.0);
    }

    #[test]
    fn h_scroll_thumb_and_drag_are_inverse() {
        let content_w = 1000.0;
        let viewport_w = 200.0;
        let max_off = max_h_offset(content_w, viewport_w); // 800
                                                           // At offset 0 the thumb sits at the left inset.
        let t0 = h_scroll_thumb(content_w, viewport_w, 0.0, max_off);
        assert_eq!(t0.thumb_x, HSCROLL_INSET);
        assert!(t0.thumb_w >= HSCROLL_MIN_THUMB);
        // Dragging the thumb (grab=0) to track_left + inset + usable maps to max_off.
        let usable = (t0.track_w - t0.thumb_w).max(1.0);
        let track_left = 17.0;
        let at_end = h_offset_from_drag(
            track_left + HSCROLL_INSET + usable,
            track_left,
            0.0,
            content_w,
            viewport_w,
            max_off,
        );
        assert!((at_end - max_off).abs() < 0.001);
        // Dragging back to the start maps to 0.
        let at_start = h_offset_from_drag(
            track_left + HSCROLL_INSET,
            track_left,
            0.0,
            content_w,
            viewport_w,
            max_off,
        );
        assert_eq!(at_start, 0.0);
        // No scrollable range (max_off = 0) → offset 0 regardless of cursor.
        assert_eq!(h_offset_from_drag(500.0, 0.0, 0.0, 100.0, 100.0, 0.0), 0.0);
    }

    #[test]
    fn caret_abs_x_offsets_by_element_origin() {
        assert_eq!(
            caret_abs_x(40.0, "ab", 2, M),
            40.0 + CODE_GUTTER + 2.0 * 8.0
        );
    }
}
