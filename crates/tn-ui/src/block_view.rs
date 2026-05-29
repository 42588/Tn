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
use tn_core::{Palette, Rgb};

use crate::style::col;

/// Colors the bar needs. The block card is **chrome** (Calm Glass), so its text
/// follows the UI tokens (mockup `.block` uses `--fg` / `--muted` / `--accent`),
/// NOT the terminal ANSI palette — the muted `--muted` (#6E76A0) is much lighter
/// than ANSI bright-black, which read as too dim for the duration/cwd. The status
/// stripe + exit chip use ANSI green/red (= mockup `--green`/`--red`) with the UI
/// accent for "running" (= mockup `.block.run` / `.exit.run` `--accent`).
pub(crate) struct BarPalette {
    pub fg: Rgba,     // command text = mockup --fg (ui.foreground)
    pub muted: Rgba,  // duration / cwd = mockup --muted (ui.muted)
    pub accent: Rgba, // program name + running status = mockup --accent (ui.accent)
    pub green: Rgba,  // success = mockup --green (ansi.green)
    pub red: Rgba,    // failure = mockup --red (ansi.red)
}

impl BarPalette {
    /// `ui_*` are the chrome tokens (from the theme `[ui]` table); `p` supplies the
    /// ANSI green/red for the success/fail status.
    pub fn new(ui_fg: Rgb, ui_muted: Rgb, ui_accent: Rgb, p: &Palette) -> Self {
        Self {
            fg: col(ui_fg),
            muted: col(ui_muted),
            accent: col(ui_accent),
            green: col(p.ansi[2]),
            red: col(p.ansi[1]),
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
        (BlockState::Running, _) => pal.accent, // mockup .block.run / .exit.run = --accent
        (BlockState::Finished, Some(0)) => pal.green,
        (BlockState::Finished, Some(_)) => pal.red,
        _ => pal.muted,
    }
}

/// The exit-status chip: a colored pill with a check/✗/diamond icon (mockup's
/// `.exit`), matching the block's state + exit code.
fn exit_chip(data: &BlockBar, pal: &BarPalette) -> Div {
    let color = status_color(data, pal);
    // mockup .exit:内联 · gap 3 · 10px · weight 680 · 状态色(无药丸底/无边)
    let chip = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(3.))
        .text_size(px(10.))
        .font_weight(gpui::FontWeight(680.))
        .text_color(color);
    match (data.state, data.exit) {
        (BlockState::Running, _) => chip
            .child(crate::assets::icon("diamond", 11.).text_color(color))
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
    // Command as `❯ <accent>prog</accent> rest` (mockup .bh + .blu): program name in
    // the UI accent, args/prompt in fg. Empty (a bare prompt, no command yet) → muted ❯.
    let cmd = short(&data.command, 64);
    let cmd_el = if cmd.is_empty() {
        div().text_color(pal.muted).child(SharedString::from("❯"))
    } else {
        let (prog, rest) = match cmd.split_once(char::is_whitespace) {
            Some((p, r)) => (p.to_string(), format!(" {r}")),
            None => (cmd.clone(), String::new()),
        };
        div()
            .flex()
            .flex_row()
            .child(div().text_color(pal.fg).child(SharedString::from("❯ ")))
            .child(div().text_color(pal.accent).child(SharedString::from(prog)))
            .child(div().text_color(pal.fg).child(SharedString::from(rest)))
    };

    // A floating rounded "block card" (Calm Glass, mockup .block): a glass panel with a
    // 3px left status stripe. The stripe is the card's own **left border**, not an
    // absolute child — gpui's `overflow_hidden` clips children RECTANGULARLY, not by
    // `corner_radii` (踩过的坑), so a full-height absolute stripe would poke square
    // corners past the rounded card (左两角变方). A border follows the rounded corners
    // natively → the stripe curves into the 11px radius like mockup `.block::before`
    // does when the browser clips it. `pl` drops 14→11 so the 3px border + 11 padding
    // keeps the command text at the mockup's 14px from the card edge.
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(9.)) // mockup .bh gap 9
        .mx_2()
        .mb(px(10.)) // mockup .block margin-bottom 10
        .py(px(8.)) // mockup .bh padding 8(上下)
        .pl(px(11.)) // mockup .bh padding-left 14 − 3px border
        .pr(px(12.)) // mockup .bh padding-right 12
        .rounded(px(11.)) // --r-card
        .border_l(px(3.)) // mockup .block::before 3px 左缘状态条 → 走边框跟随圆角
        .border_color(stripe)
        .overflow_hidden() // clip the (truncated) command text to the card radius
        .text_size(px(12.)) // mockup .bh font-size 12
        .bg(rgba(0xffffff09)) // .035×255≈9 白叠加(mockup .block,无边框)
        .child(cmd_el)
        .child(div().flex_1());

    if let Some(ms) = data.duration_ms {
        // mockup .dur:10.5 · 640 · muted
        row = row.child(
            div()
                .text_size(px(10.5))
                .font_weight(gpui::FontWeight(640.))
                .text_color(pal.muted)
                .child(SharedString::from(fmt_duration(ms))),
        );
    }
    row = row.child(exit_chip(data, pal));
    if let Some(cwd) = &data.cwd {
        row = row.child(div().text_color(pal.muted).child(SharedString::from(short_path(cwd, 22))));
    }
    row
}
