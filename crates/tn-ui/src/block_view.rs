//! Warp-style command-block chrome (M3).
//!
//! Renders a compact "command block" bar at the bottom of a terminal pane,
//! driven by [`tn_blocks::BlockModel`] (fed real OSC 133/633 markers by the
//! shell-integration bypass). It shows the active or most-recent command: a
//! status spine (running = 磷光 / ok = green / fail = red), the command text,
//! duration, exit status, and cwd. Copy / rerun actions are wired by
//! `TerminalView` (they need its context), which appends them to [`bar_base`].
//!
//! 磷光 `.blockbar`(SHEET 02):高 30 · L2 · 顶 1px h0 · 左 2px 状态脊 —— 脊色是
//! 块的唯一状态语言;正文永不被块卡染色。`TerminalView` hides it on the alternate
//! screen (vim/less/...) — the full-screen app owns the viewport.

use gpui::{div, prelude::*, px, rgba, Div, Rgba, SharedString};
use tn_blocks::{Block, BlockModel, BlockState};
use tn_core::{Palette, Rgb};

use crate::style::{col, ERR_SOFT, H0, OK_SOFT, PH_SOFT, R_CHIP};

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
    ///
    /// `now_ms` = 会话时钟当前值(与块事件的 at_ms 同源):运行中的块以
    /// `now − started_at` 给出**实时累加耗时**(SHEET 07-B「运行中耗时实时
    /// 累加,即最朴素的进度仪」;差异总结 1-12)。
    #[allow(dead_code)]
    pub fn from_model(m: &BlockModel, now_ms: u64) -> Option<Self> {
        Self::from_model_with_override(m, None, now_ms)
    }

    pub fn from_model_with_override(m: &BlockModel, selected_id: Option<u64>, now_ms: u64) -> Option<Self> {
        let chosen = if let Some(id) = selected_id {
            m.iter().find(|b| b.id == id)
        } else {
            None
        };
        let chosen = chosen.or_else(|| match m.current() {
            Some(c) if c.state == BlockState::Running || c.command.is_some() => Some(c),
            _ => m.last_finished(),
        });
        let b: &Block = chosen?;
        let duration_ms = b.duration_ms().or_else(|| {
            (b.state == BlockState::Running)
                .then(|| b.started_at.map(|s| now_ms.saturating_sub(s)))
                .flatten()
        });
        Some(Self {
            command: b
                .command
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_default(),
            cwd: b.cwd.as_ref().map(|s| s.to_string()),
            state: b.state,
            exit: b.exit,
            duration_ms,
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

/// The exit-status chip(SHEET 07 `.bx`):状态色字 + 同色 soft 底 · r3 · mono 11。
fn exit_chip(data: &BlockBar, pal: &BarPalette) -> Div {
    let color = status_color(data, pal);
    let soft = match (data.state, data.exit) {
        (BlockState::Running, _) => PH_SOFT,
        (BlockState::Finished, Some(0)) => OK_SOFT,
        (BlockState::Finished, Some(_)) => ERR_SOFT,
        _ => 0x00000000,
    };
    let chip = div()
        .flex()
        .flex_row()
        .items_center()
        .gap(px(3.))
        .px(px(7.))
        .py(px(1.))
        .rounded(px(R_CHIP))
        .bg(rgba(soft))
        .text_size(px(crate::style::FS_MICRO))
        .font_weight(gpui::FontWeight(600.))
        .text_color(color);
    match (data.state, data.exit) {
        (BlockState::Running, _) => chip.child(SharedString::from("RUN")),
        (BlockState::Finished, Some(0)) => chip.child(SharedString::from("✓ 0")),
        (BlockState::Finished, Some(n)) => chip.child(SharedString::from(format!("✕ {n}"))),
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

    // SHEET 02 `.blockbar`:贴底全宽仪表条 — 高 30 · L2 · 顶 1px h0 · 左 2px
    // 状态脊。脊走 border_l(单色);顶发丝走内层 1px 线(border_color 单色限制)。
    let mut row = div()
        .flex()
        .flex_row()
        .items_center()
        .relative()
        .gap(px(10.))
        .h(px(30.))
        .flex_none()
        .pl(px(10.)) // 12 − 2px 脊
        .pr(px(12.))
        .border_l(px(2.)) // 状态脊:块的唯一状态语言
        .border_color(stripe)
        .bg(gpui::rgb(crate::style::L2))
        .text_size(px(crate::style::FS_MICRO))
        .child(
            div()
                .absolute()
                .top(px(0.))
                .left(px(0.))
                .right(px(0.))
                .h(px(1.))
                .bg(rgba(H0)),
        )
        .child(cmd_el)
        .child(div().flex_1());

    if let Some(ms) = data.duration_ms {
        row = row.child(
            div()
                .text_size(px(crate::style::FS_MICRO))
                .text_color(pal.muted)
                .child(SharedString::from(fmt_duration(ms))),
        );
    }
    row = row.child(exit_chip(data, pal));
    if let Some(cwd) = &data.cwd {
        row = row.child(
            div()
                .text_size(px(crate::style::FS_MICRO))
                .text_color(pal.muted)
                .child(SharedString::from(short_path(cwd, 22))),
        );
    }
    row
}

#[cfg(test)]
mod tests {
    use super::*;
    use tn_blocks::BlockModel;
    use tn_shell::BlockEvent;

    #[test]
    fn test_from_model_with_override() {
        let mut m = BlockModel::new();
        m.on_event(BlockEvent::PromptStart, 0, 0);
        m.on_event(BlockEvent::CommandLine("cargo test".into()), 0, 0);
        m.on_event(BlockEvent::CommandStart, 0, 100);
        m.on_event(BlockEvent::OutputStart, 1, 120);
        m.on_event(BlockEvent::CommandFinished { exit: Some(0) }, 5, 200);

        m.on_event(BlockEvent::PromptStart, 6, 300);
        m.on_event(BlockEvent::CommandLine("git status".into()), 6, 300);
        m.on_event(BlockEvent::CommandStart, 6, 400);

        let blocks: Vec<_> = m.iter().collect();
        assert_eq!(blocks.len(), 2);
        let first_id = blocks[0].id;

        // Without override, it should pick the current/running block (git status)
        let bar_normal = BlockBar::from_model(&m, 500).unwrap();
        assert_eq!(bar_normal.command, "git status");

        // With override of first block (cargo test)
        let bar_overridden = BlockBar::from_model_with_override(&m, Some(first_id), 500).unwrap();
        assert_eq!(bar_overridden.command, "cargo test");
        assert_eq!(bar_overridden.exit, Some(0));

        // With override of non-existent block, falls back to normal (git status)
        let bar_fallback = BlockBar::from_model_with_override(&m, Some(9999), 500).unwrap();
        assert_eq!(bar_fallback.command, "git status");
    }
}
