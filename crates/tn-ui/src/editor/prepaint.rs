//! Read-only prepaint model (TnE-09 foundation).
//!
//! Given a document, a viewport and the current scroll offsets, this computes the
//! complete layout a read-only `EditorElement::prepaint` needs before painting:
//! which rows are visible, where each row and its gutter label sit, the content
//! width / clamped horizontal offset, and the scrollbar thumb geometry. The
//! actual GPUI `paint` (shape_line + ShapedLine::paint at these origins) and the
//! `TN_QL_ELEMENT` wiring into Quick Look's File tab are deferred to a real-machine
//! round — they cannot be visually verified headless. Everything here is pure and
//! unit-tested so the paint layer is a thin,低风险 wrapper.
//!
//! All positions are relative to the **text-area element origin** unless noted;
//! the paint layer adds the element's absolute origin. Mirrors the geometry the
//! current Quick Look `uniform_list` File renderer computes inline.

use std::ops::Range;

use super::geometry::{
    content_width, h_scroll_thumb, max_cols, max_h_offset, visible_rows, HScrollThumb, Metrics,
};

/// Visible row range, clamped to the document line count, with one extra row at
/// the bottom so a partially-scrolled row doesn't leave a gap. Empty when the
/// document is empty.
pub fn visible_row_indices(
    offset_y: f32,
    viewport_h: f32,
    row_h: f32,
    total: usize,
) -> Range<usize> {
    let vis = visible_rows(offset_y, viewport_h, row_h);
    let first = vis.first.min(total);
    let last_exclusive = (vis.first + vis.count + 1).min(total);
    first..last_exclusive.max(first)
}

/// Y offset (relative to the text-area top) where `row` is painted. `offset_y` is
/// the scroll offset (≤ 0 as content scrolls up), so this is negative for rows
/// above the viewport.
pub fn row_top(row: usize, offset_y: f32, row_h: f32) -> f32 {
    offset_y + row as f32 * row_h
}

/// 1-based line-number label for the gutter.
pub fn gutter_label(row: usize) -> String {
    (row + 1).to_string()
}

/// X (relative to the text-area origin) where column 0 of every line begins, after
/// the line-number gutter and the horizontal scroll offset.
pub fn content_origin_x(h_offset: f32, m: Metrics) -> f32 {
    m.gutter - h_offset
}

/// Everything a read-only File-preview Element needs to paint one frame.
#[derive(Clone, Debug, PartialEq)]
pub struct ReadOnlyPrepaint {
    /// Total content width (≥ viewport; drives the horizontal scroll range).
    pub content_w: f32,
    /// Maximum horizontal scroll offset (0 when content fits).
    pub max_off: f32,
    /// Horizontal scroll offset, clamped to `[0, max_off]`.
    pub h_offset: f32,
    /// Rows to paint this frame (clamped to the document; bottom-padded by one).
    pub rows: Range<usize>,
    /// Horizontal scrollbar thumb, or `None` when content fits the viewport.
    pub thumb: Option<HScrollThumb>,
    /// X (relative to element origin) where column 0 begins (`gutter - h_offset`).
    pub content_x: f32,
}

/// Compute the read-only prepaint for the File preview. Mirrors the renderer:
/// content width is at least the viewport, the thumb only appears once there is
/// more than 8px of scrollable range, and `h_offset` is clamped.
pub fn prepaint_readonly(
    lines: &[String],
    viewport_w: f32,
    viewport_h: f32,
    offset_y: f32,
    h_offset: f32,
    m: Metrics,
) -> ReadOnlyPrepaint {
    let cols = max_cols(lines);
    let content_w = content_width(cols, m, viewport_w);
    let max_off = max_h_offset(content_w, viewport_w);
    let h_offset = h_offset.clamp(0.0, max_off);
    let rows = visible_row_indices(offset_y, viewport_h, m.row_h, lines.len());
    let thumb = (max_off > 8.0 && viewport_w > 0.0)
        .then(|| h_scroll_thumb(content_w, viewport_w, h_offset, max_off));
    ReadOnlyPrepaint {
        content_w,
        max_off,
        h_offset,
        rows,
        thumb,
        content_x: content_origin_x(h_offset, m),
    }
}

#[cfg(test)]
mod tests {
    use super::super::geometry::{CODE_GUTTER, ROW_H};
    use super::*;

    fn buf(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("line {i}")).collect()
    }

    const M: Metrics = Metrics {
        char_w: 8.0,
        row_h: ROW_H,
        gutter: CODE_GUTTER,
    };

    #[test]
    fn visible_indices_clamp_to_document_and_pad_one() {
        // viewport 100 / row 20 → 5 rows visible from offset 0; +1 pad = 0..6.
        assert_eq!(visible_row_indices(0.0, 100.0, 20.0, 50), 0..6);
        // near the end, clamps to total.
        assert_eq!(visible_row_indices(-940.0, 100.0, 20.0, 50), 47..50);
        // empty document → empty range.
        assert_eq!(visible_row_indices(0.0, 100.0, 20.0, 0), 0..0);
        // fewer lines than fit → clamps to total.
        assert_eq!(visible_row_indices(0.0, 100.0, 20.0, 3), 0..3);
    }

    #[test]
    fn row_top_and_gutter_label() {
        assert_eq!(row_top(0, 0.0, 20.0), 0.0);
        assert_eq!(row_top(3, 0.0, 20.0), 60.0);
        assert_eq!(row_top(3, -50.0, 20.0), 10.0);
        assert_eq!(gutter_label(0), "1");
        assert_eq!(gutter_label(41), "42");
    }

    #[test]
    fn prepaint_short_content_fills_viewport_no_thumb() {
        let lines = buf(3);
        let p = prepaint_readonly(&lines, 400.0, 100.0, 0.0, 0.0, M);
        assert_eq!(p.content_w, 400.0); // max(content, viewport)
        assert_eq!(p.max_off, 0.0);
        assert_eq!(p.h_offset, 0.0);
        assert!(p.thumb.is_none());
        assert_eq!(p.rows, 0..3);
        assert_eq!(p.content_x, CODE_GUTTER); // gutter - 0
    }

    #[test]
    fn prepaint_long_line_enables_thumb_and_clamps_offset() {
        // One very long line forces content beyond a 100px viewport.
        let lines = vec!["x".repeat(200)];
        let p = prepaint_readonly(&lines, 100.0, 100.0, 0.0, 9999.0, M);
        assert!(p.max_off > 8.0);
        assert!(p.thumb.is_some());
        // requested h_offset way past the end → clamped to max_off.
        assert_eq!(p.h_offset, p.max_off);
        // content_x shifts left by the clamped offset.
        assert_eq!(p.content_x, CODE_GUTTER - p.max_off);
    }

    #[test]
    fn prepaint_offset_within_eight_px_hides_thumb() {
        // Content only a few px wider than viewport (≤ 8px) → no scrollbar, matching
        // the renderer's `max_off > 8.0` gate.
        let m = Metrics {
            char_w: 1.0,
            row_h: ROW_H,
            gutter: 0.0,
        };
        // max_cols=5 → content = (5+1)*1 = 6; viewport 2 → max(6,2)=6, max_off=4 (≤8).
        let lines = vec!["abcde".to_string()];
        let p = prepaint_readonly(&lines, 2.0, 100.0, 0.0, 0.0, m);
        assert_eq!(p.max_off, 4.0);
        assert!(p.thumb.is_none());
    }
}
