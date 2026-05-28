//! Warp-style command-block chrome (M3).
//!
//! Renders a compact "command block" bar at the bottom of a terminal pane,
//! driven by [`tn_blocks::BlockModel`] (fed real OSC 133/633 markers by the
//! shell-integration bypass). It shows the active or most-recent command: a
//! status stripe (running = blue / ok = green / fail = red), the command text,
//! duration, exit status, and cwd. Copy / rerun actions are wired by
//! `TerminalView` (they need its context), which appends them to [`bar_base`].
//!
//! Calm Glass: a translucent inset footer, no glow. `TerminalView` hides it on
//! the alternate screen (vim/less/...) — the full-screen app owns the viewport.
//! The full per-row overlay around each historical block is deferred; this
//! footer bar is the M3 cut.

use gpui::{div, prelude::*, px, rgba, Div, Rgba, SharedString};
use tn_blocks::{Block, BlockModel, BlockState};
use tn_core::Palette;

use crate::style::col;

/// Colors the bar needs, pulled from the live terminal palette.
pub(crate) struct BarPalette {
    pub fg: Rgba,
    pub dim: Rgba,
    pub green: Rgba,
    pub red: Rgba,
    pub blue: Rgba,
}

impl BarPalette {
    pub fn from_palette(p: &Palette) -> Self {
        Self {
            fg: col(p.fg),
            dim: col(p.ansi[8]), // bright black = muted
            green: col(p.ansi[2]),
            red: col(p.ansi[1]),
            blue: col(p.ansi[4]),
        }
    }
}

/// A flattened snapshot of the one block worth showing in the bar.
pub(crate) struct BlockBar {
    pub command: String,
    pub cwd: Option<String>,
    pub state: BlockState,
    pub exit: Option<i32>,
    pub duration_ms: Option<u64>,
}

impl BlockBar {
    /// Choose the block to display: a running / has-command current block,
    /// else the most recently finished one. `None` until the first command
    /// (so an idle bare prompt shows no bar).
    pub fn from_model(m: &BlockModel) -> Option<Self> {
        let chosen = match m.current() {
            Some(c) if c.state == BlockState::Running || c.command.is_some() => Some(c),
            _ => m.last_finished(),
        };
        let b: &Block = chosen?;
        Some(Self {
            command: b.command.clone().unwrap_or_default(),
            cwd: b.cwd.clone(),
            state: b.state,
            exit: b.exit,
            duration_ms: b.duration_ms(),
        })
    }
}

/// Stripe / status color for the block's state + exit code.
fn status_color(data: &BlockBar, pal: &BarPalette) -> Rgba {
    match (data.state, data.exit) {
        (BlockState::Running, _) => pal.blue,
        (BlockState::Finished, Some(0)) => pal.green,
        (BlockState::Finished, Some(_)) => pal.red,
        _ => pal.dim,
    }
}

/// The exit-status chip: a colored pill with a check/✗/diamond icon (mockup's
/// `.exit`), matching the block's state + exit code.
fn exit_chip(data: &BlockBar, pal: &BarPalette) -> Div {
    let color = status_color(data, pal);
    let bg = Rgba { a: 0.16, ..color };
    let chip = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.))
        .px_2()
        .py(px(1.))
        .rounded(px(999.))
        .bg(bg)
        .text_color(color);
    match (data.state, data.exit) {
        (BlockState::Running, _) => chip
            .child(crate::assets::icon("diamond", 10.).text_color(color))
            .child(SharedString::from("运行中")),
        (BlockState::Finished, Some(0)) => {
            chip.child(crate::assets::icon("check", 11.).text_color(color))
        }
        (BlockState::Finished, Some(n)) => chip
            .child(crate::assets::icon("close", 11.).text_color(color))
            .child(SharedString::from(format!("exit {n}"))),
        (BlockState::Finished, None) => chip.child(SharedString::from("结束")),
        _ => chip,
    }
}

fn fmt_duration(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1000.0)
    } else {
        let s = ms / 1000;
        format!("{}m{:02}s", s / 60, s % 60)
    }
}

/// Truncate from the head with a trailing ellipsis.
fn short(s: &str, max: usize) -> String {
    let t = s.trim();
    if t.chars().count() > max {
        let mut o: String = t.chars().take(max.saturating_sub(1)).collect();
        o.push('…');
        o
    } else {
        t.to_string()
    }
}

/// Keep the tail of a path (leading ellipsis), so the current dir stays visible.
fn short_path(s: &str, max: usize) -> String {
    let t = s.trim_end_matches(['/', '\\']);
    let n = t.chars().count();
    if n > max {
        let tail: String = t.chars().skip(n - (max.saturating_sub(1))).collect();
        format!("…{tail}")
    } else {
        t.to_string()
    }
}

/// Build the (non-interactive) bar row. `TerminalView` appends copy/rerun
/// buttons to the returned [`Div`].
pub(crate) fn bar_base(data: &BlockBar, pal: &BarPalette) -> Div {
    let stripe = status_color(data, pal);
    let cmd = if data.command.is_empty() {
        "(command…)".to_string()
    } else {
        short(&data.command, 64)
    };

    // A floating rounded "block card" (Calm Glass) rather than a flush shelf —
    // accent left-stripe, command, duration, exit chip, cwd.
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(9.)) // mockup .bh gap 9
        .mx_2()
        .mb(px(10.)) // mockup .block margin-bottom 10
        .py(px(8.)) // mockup .bh padding 8(上下)
        .pl(px(14.)) // mockup .bh padding-left 14
        .pr(px(12.)) // mockup .bh padding-right 12
        .rounded(px(11.)) // --r-card
        .text_size(px(12.)) // mockup .bh font-size 12
        // mockup .block:bg 白 @ .035,无边框(去掉原 glass rim)
        .bg(rgba(0xffffff09)) // round(.035×255)=9
        // left status stripe
        .child(div().w(px(3.)).h(px(16.)).rounded_full().bg(stripe))
        // command line (monospace, inherited from the pane root)
        .child(div().text_color(pal.fg).child(SharedString::from(cmd)))
        .child(div().flex_1());

    if let Some(ms) = data.duration_ms {
        // mockup .dur:10.5 · 640 · muted
        row = row.child(
            div()
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight(640.))
                .text_color(pal.dim)
                .child(SharedString::from(fmt_duration(ms))),
        );
    }
    row = row.child(exit_chip(data, pal));
    if let Some(cwd) = &data.cwd {
        row = row.child(div().text_color(pal.dim).child(SharedString::from(short_path(cwd, 22))));
    }
    row
}
