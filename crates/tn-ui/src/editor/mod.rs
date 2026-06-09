//! Editor renderer scaffolding (TnE-08).
//!
//! Read-only layout/geometry model that a future `EditorElement` (TnE-09+) will
//! use for `layout`/`prepaint`/`paint`, replacing the inline geometry the Quick
//! Look `uniform_list` renderer computes today. This is **not yet wired into any
//! render path** — it exists so the geometry can be unit-tested in isolation and
//! later shared by Quick Look, the Editor Pane and Diff Review against one model.
//!
//! See [`geometry`] for the pure functions. Nothing here depends on a live GPUI
//! window; everything is plain `f32` / `usize` so `cargo test -p tn-ui --lib`
//! covers it headless.

// Scaffolding: not wired into a render path yet (TnE-09 will consume it), so the
// public geometry API is intentionally not all called from the crate yet.
#![allow(dead_code)]

pub mod diff;
pub mod geometry;
pub mod motion;
pub mod pane;
pub mod prepaint;
pub mod session;

#[allow(unused_imports)]
pub use diff::{classify_diff_line, hunk_header_rows, next_hunk, prev_hunk, DiffRowKind};
#[allow(unused_imports)]
pub use geometry::{
    caret_abs_x, caret_col_at_x, caret_x, content_width, disp_width, follow_h_offset,
    h_offset_from_drag, h_scroll_thumb, hover_char_at_x, max_cols, max_h_offset, prefix_cols,
    row_out_of_view, visible_rows, HScrollThumb, Metrics, VisibleRows, CODE_GUTTER, ROW_H,
};
#[allow(unused_imports)]
pub use motion::{
    motion_snapshot, CaretMotionInput, CaretMotionState, MotionKind, MotionSnapshot, MotionTrigger,
    SettleGlyph,
};
#[allow(unused_imports)]
pub use prepaint::{
    content_origin_x, gutter_label, prepaint_readonly, row_top, visible_row_indices,
    ReadOnlyPrepaint,
};
