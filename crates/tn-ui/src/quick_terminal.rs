//! Quick Terminal (M5): the Quake/Guake-style drop-down floating terminal.
//!
//! A separate borderless, topmost GPUI window (`WindowKind::PopUp`) that slides in
//! from the configured edge on a global hotkey (see [`crate::platform`]), takes
//! focus, and (optionally) slides away when it loses focus. On summon with no live
//! session it shows a **launcher** (Claude / Codex / pwsh — the command-bearing
//! `[[profiles]]`, mirroring the workspace command palette); the picked session is
//! a normal [`TerminalView`], so an agent gets its usual header + live usage ring.
//! Once launched, the session persists across hides; a small "switch" chip reopens
//! the launcher to pick a different one.
//!
//! The pure placement / slide math lives in `tn_config::quick_terminal` (headless,
//! tested); this file is the GPUI + Win32 driver.
//!
//! **Re-entrancy rule (hard-won):** Win32 `SetWindowPos`/`ShowWindow` dispatch
//! `WM_SIZE`/`WM_WINDOWPOSCHANGED` *synchronously* back into gpui's window proc,
//! which borrows the window-state `RefCell`. Calling them from inside a gpui
//! update / observer callback (which already holds that borrow) re-enters it and
//! the resize is silently dropped ("RefCell already borrowed"). So **all** window
//! manipulation is deferred onto a foreground `cx.spawn` task, never inline.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui::{
    div, linear_color_stop, linear_gradient, prelude::*, px, rgba, Context, Div, Entity,
    FocusHandle, FontWeight, KeyDownEvent, MouseButton, SharedString, Window,
};
use tn_config::{ease_out_cubic, lerp_rect, Loaded, Rect};

use crate::platform;
use crate::style::{col, cola, icon, specular_top, HOVER, INSET, RIM, R_CARD, R_PANEL, UI_SANS};
use crate::terminal_view::{LaunchSpec, ProcessExited, TerminalView, UsageUpdated};
use crate::welcome::{launch_agent_of, launch_tile_accent, launch_tile_sub};

/// Launcher → session cross-fade duration (待优化:手感真机调).
const TRANSITION_MS: u64 = 190;

/// Launcher card width in **logical** px: 4 tiles (128) + 3 gaps (11) + 2×22
/// padding + 2×1 border = 591, rounded up for a hair of slack. Scaled to physical
/// by the monitor DPI when sizing the window (see [`QuickTerminal::shown_for`]).
const CARD_W: f32 = 600.0;

pub struct QuickTerminal {
    config: Arc<Loaded>,
    /// The live session, if one has been launched. `None` => show the launcher.
    term: Option<Entity<TerminalView>>,
    /// Launcher overlay state.
    picker_open: bool,
    picker_sel: usize,
    picker_focus: FocusHandle,
    /// OS window handle, grabbed lazily on first toggle (needs a `&Window`).
    hwnd: Option<isize>,
    /// Logical visibility target (drives slide direction + the autohide guard).
    visible: bool,
    initialized: bool,
    topmost_done: bool,
    /// Focus the launcher (or the running terminal) on the next render — the one
    /// place we hold a `&mut Window` without also being mid-Win32-call.
    pending_focus: bool,
    anim_token: u64,
    /// When a launcher → session cross-fade is mid-flight: the instant it began,
    /// driving the dark wash that fades out over the new terminal. `None` = settled.
    transition_at: Option<Instant>,
    /// Launchable profiles (config `[[profiles]]` + installed WSL distros),
    /// resolved once (shares the workspace's discovery).
    launch_profiles: Vec<tn_config::Profile>,
}

impl QuickTerminal {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let launch_profiles = crate::workspace::discover_profiles(&config);
        Self {
            config,
            term: None,
            picker_open: false,
            picker_sel: 0,
            picker_focus: cx.focus_handle(),
            hwnd: None,
            visible: false,
            initialized: false,
            topmost_done: false,
            pending_focus: false,
            anim_token: 0,
            transition_at: None,
            launch_profiles,
        }
    }

    /// Launchable profiles (shell / agent / WSL distro) — the launcher's entries.
    /// Shares the command palette's predicate.
    fn launchable(&self) -> Vec<&tn_config::Profile> {
        self.launch_profiles
            .iter()
            .filter(|p| crate::workspace::is_launchable(p))
            .collect()
    }

    /// Toggle visibility — the action bound to the global hotkey.
    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_init(window, cx);
        let reveal = !self.visible;
        if reveal && self.term.is_none() {
            // Nothing running yet — summon straight into the launcher.
            self.picker_open = true;
            self.picker_sel = 0;
        }
        self.pending_focus = true;
        self.slide(reveal, cx);
    }

    /// First-toggle setup: grab the HWND and (if autohide is on) hide when the
    /// window loses focus. Both are borrow-safe (a pure handle read + a gpui
    /// observer registration), so they run inline — unlike the Win32 calls.
    fn ensure_init(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.initialized {
            return;
        }
        self.initialized = true;
        self.hwnd = platform::hwnd_of(window);
        if let Some(h) = self.hwnd {
            // Same IME key-routing fix as the main window (中文 composition in the
            // drop-down terminal). See platform.rs.
            platform::install_ime_keyfix(h);
        } else {
            tracing::warn!("quick terminal: no HWND; topmost/slide disabled");
        }
        if self.config.config.quick_terminal.autohide {
            cx.observe_window_activation(window, |qt, window, cx| {
                if qt.visible && !window.is_window_active() {
                    qt.slide(false, cx); // hide — Win32 deferred inside slide
                }
            })
            .detach();
        }
    }

    /// Launch the selected launcher entry as the hosted session (replacing any
    /// previous one — its process is killed by `LocalPty::Drop`), then show it.
    fn launch_selected(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let entries = self.launchable();
        // Ephemeral launch: an agent's pwsh host omits `-NoExit`, so exiting the
        // agent exits the PTY and we fall back to the launcher (below).
        let spec = if entries.is_empty() {
            LaunchSpec::pwsh()
        } else {
            let sel = self.picker_sel.min(entries.len() - 1);
            LaunchSpec::from_profile_ephemeral(entries[sel]).unwrap_or_else(LaunchSpec::pwsh)
        };
        let config = self.config.clone();
        let term = cx.new(|cx| TerminalView::new(cx, config, spec));
        // Repaint when this session's agent usage changes (keeps the ring live).
        cx.subscribe(&term, |_qt, _t, _ev: &UsageUpdated, cx| cx.notify()).detach();
        // When the session's process exits, return to the launcher (guard against
        // a stale watcher from a session we've since replaced).
        cx.subscribe(&term, |this, emitter, _ev: &ProcessExited, cx| {
            if this.term.as_ref().map(|t| t.entity_id()) == Some(emitter.entity_id()) {
                this.term = None;
                this.picker_open = true;
                this.picker_sel = 0;
                this.pending_focus = true;
                this.resnap(cx); // shrink the window back to the compact launcher card
                cx.notify();
            }
        })
        .detach();
        self.term = Some(term); // replaces any prior session (old one is dropped -> killed)
        self.picker_open = false;
        self.pending_focus = true; // focus happens in render
        self.resnap(cx); // grow the card-sized window to the session drop-down
        self.start_transition(cx); // cross-fade the launcher → session
        cx.notify();
    }

    /// Launcher keystrokes: ↑↓←→ select (the tiles are a 4-col grid: ←→ move by one,
    /// ↑↓ by a row), Enter launch, Esc back (to the running session, or hide the
    /// window if there's nothing to go back to).
    fn on_picker_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        const COLS: usize = 4; // mockup .tiles grid-template-columns: repeat(4,1fr)
        let n = self.launchable().len().max(1); // ≥1 (synthetic pwsh fallback)
        match ev.keystroke.key.as_str() {
            "escape" => {
                self.picker_open = false;
                if self.term.is_some() {
                    self.pending_focus = true; // back to the terminal
                    cx.notify();
                } else {
                    self.slide(false, cx); // nothing running — just hide
                }
            }
            "left" => {
                self.picker_sel = self.picker_sel.saturating_sub(1);
                cx.notify();
            }
            "up" => {
                self.picker_sel = self.picker_sel.saturating_sub(COLS);
                cx.notify();
            }
            "right" => {
                self.picker_sel = (self.picker_sel + 1).min(n - 1);
                cx.notify();
            }
            "down" => {
                self.picker_sel = (self.picker_sel + COLS).min(n - 1);
                cx.notify();
            }
            "enter" => self.launch_selected(window, cx),
            _ => {}
        }
    }

    /// Pixel height of the launcher card = `.lhead` + tiles (one row per 4) + footer.
    /// The launcher window is sized to exactly this (see [`shown_for`]) so the
    /// transparent window hugs the card — no surrounding window rectangle to peek.
    fn card_height(&self) -> f32 {
        let n = self.launchable().len().max(1);
        let rows = n.div_ceil(4) as f32; // 4 tiles per row
        // lhead(~42) + tiles(pad 28 + rows×110 + gaps) + divider(1) + footer(~40), a hair generous.
        116.0 + rows * 110.0 + (rows - 1.0) * 11.0
    }

    /// On-screen window rect for the current state: a bare launcher is a **card-sized**
    /// window, centered horizontally and dropped from the top, so the (transparent)
    /// window is exactly the card; a running session uses the configured drop-down size.
    ///
    /// `work`/the returned rect are **physical** px (what `set_bounds` speaks), but the
    /// card is laid out by gpui in **logical** px — so the card size is multiplied by
    /// `scale` (monitor DPI), else the window is `scale×` too small and clips the card
    /// on a HiDPI display. The session branch derives from `work` and is already physical.
    fn shown_for(&self, work: Rect, scale: f32) -> Rect {
        let qt = &self.config.config.quick_terminal;
        if self.term.is_none() {
            let s = scale.max(1.0);
            let w = (CARD_W * s).min(work.width * 0.94);
            let h = (self.card_height() * s).min(work.height * 0.94);
            let x = work.x + (work.width - w) / 2.0;
            let y = work.y + (work.height * 0.12).min(140.0 * s); // dropped a bit from the top
            Rect::new(x, y, w, h)
        } else {
            qt.shown_rect(work)
        }
    }

    /// Off-screen rect matching [`shown_for`] — same size, pushed above the top edge.
    fn hidden_for(&self, work: Rect, scale: f32) -> Rect {
        let qt = &self.config.config.quick_terminal;
        if self.term.is_none() {
            let s = self.shown_for(work, scale);
            Rect::new(s.x, work.y - s.height, s.width, s.height)
        } else {
            qt.hidden_rect(work)
        }
    }

    /// Re-snap the visible window to the current state's size (instant, no slide):
    /// after a launch (card → session window) or a session exit (back to the card).
    /// Win32 deferred onto a foreground task (module re-entrancy rule).
    fn resnap(&mut self, cx: &mut Context<Self>) {
        let Some(h) = self.hwnd else { return };
        if !self.visible {
            return;
        }
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            let Some(work) = platform::work_area(h) else { return };
            let scale = platform::scale_for(h);
            let Ok((rect, rounded)) =
                this.update(cx, |v, _| (v.shown_for(work, scale), v.term.is_none()))
            else {
                return;
            };
            platform::set_bounds(h, rect);
            // Round the launcher card window; a running session fills edge-to-edge.
            if rounded {
                platform::set_round_region(h, rect.width, rect.height, R_PANEL * scale);
            } else {
                platform::clear_region(h);
            }
            let _ = this.update(cx, |_, cx| cx.notify());
        })
        .detach();
    }

    /// Start the launcher → session cross-fade: a dark wash covers the freshly
    /// mounted terminal and fades out over [`TRANSITION_MS`], so the session
    /// "develops" in instead of snapping. Driven per-frame like the bell fade.
    fn start_transition(&mut self, cx: &mut Context<Self>) {
        self.transition_at = Some(Instant::now());
        let total = Duration::from_millis(TRANSITION_MS);
        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            loop {
                let done = this
                    .update(cx, |v, cx| {
                        let done = v.transition_at.is_none_or(|t| t.elapsed() >= total);
                        if done {
                            v.transition_at = None;
                        }
                        cx.notify();
                        done
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
                cx.background_executor().timer(Duration::from_millis(16)).await;
            }
        })
        .detach();
    }

    /// Reveal (`reveal = true`) or hide the window, sliding over `animation_ms`.
    /// Every Win32 call runs on a fresh foreground task (see module re-entrancy
    /// rule), so this is safe from a hotkey toggle *or* the autohide observer.
    fn slide(&mut self, reveal: bool, cx: &mut Context<Self>) {
        let Some(h) = self.hwnd else { return };
        self.visible = reveal;
        self.anim_token += 1;
        let token = self.anim_token;
        let first = !self.topmost_done;
        self.topmost_done = true;
        let anim_ms = self.config.config.quick_terminal.animation_ms;

        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            if first {
                platform::make_topmost(h);
            }
            let Some(work) = platform::work_area(h) else { return };
            let scale = platform::scale_for(h);
            // State-aware endpoints: card-sized for a bare launcher, full for a session.
            let Ok((hidden, shown, rounded)) = this.update(cx, |v, _| {
                (v.hidden_for(work, scale), v.shown_for(work, scale), v.term.is_none())
            }) else {
                return;
            };
            if reveal {
                platform::set_bounds(h, hidden);
                // Round the launcher window so its corners match the card (size is
                // constant across the slide, so set it once here). Square for a session.
                if rounded {
                    platform::set_round_region(h, hidden.width, hidden.height, R_PANEL * scale);
                } else {
                    platform::clear_region(h);
                }
                platform::show(h, true);
                let _ = this.update(cx, |_, cx| cx.notify()); // render -> consume focus
            }
            let dur = Duration::from_millis(anim_ms);
            let start = Instant::now();
            loop {
                if !this.update(cx, |v, _| v.anim_token == token).unwrap_or(false) {
                    return;
                }
                let elapsed = start.elapsed();
                let progress = if dur.is_zero() {
                    1.0
                } else {
                    (elapsed.as_secs_f32() / dur.as_secs_f32()).clamp(0.0, 1.0)
                };
                let t = if reveal { progress } else { 1.0 - progress };
                platform::set_bounds(h, lerp_rect(hidden, shown, ease_out_cubic(t)));
                if dur.is_zero() || elapsed >= dur {
                    if !reveal {
                        platform::show(h, false);
                    }
                    return;
                }
                cx.background_executor().timer(Duration::from_millis(16)).await;
            }
        })
        .detach();
        cx.notify();
    }

    /// The launcher (mockup `.quick` / `.launcher` / `.tiles`): a **centered** frosted
    /// card on a dim scrim — same shape as the command palette, so its rounded corners
    /// read clearly against the scrim (a window-filling strip looked unrefined and hid
    /// the radius). `None` when closed; a launched session fills the window behind it.
    fn render_picker(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.picker_open {
            return None;
        }
        let t = &self.config.theme;
        let ui = &t.ui;
        let entries = self.launchable();
        let sel = self.picker_sel.min(entries.len().saturating_sub(1));

        // Tiles: each launchable profile, or a single synthetic pwsh fallback.
        let tiles: Vec<Div> = if entries.is_empty() {
            vec![self.launcher_tile(0, true, "PowerShell".into(), "powershell.exe".into(), "term", ui.accent, cx)]
        } else {
            entries
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let agent = launch_agent_of(p);
                    let accent = launch_tile_accent(t, p, agent);
                    let glyph = if agent.is_some() { "spark" } else { "term" };
                    self.launcher_tile(i, i == sel, p.name.clone(), launch_tile_sub(p, agent), glyph, accent, cx)
                })
                .collect()
        };

        // The card **fills the whole (card-sized, transparent) window** — its rounded
        // corners show the desktop through, so it reads as just a floating card with no
        // surrounding window rectangle. specular wash + cool gradient + rim edge.
        let card = div()
            .size_full()
            .relative() // anchor the specular top wash
            .flex()
            .flex_col()
            .font_family(UI_SANS)
            .rounded(px(R_PANEL))
            .overflow_hidden()
            .border_1()
            .border_color(rgba(RIM))
            .track_focus(&self.picker_focus)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| this.on_picker_key(ev, w, cx)))
            // mockup .quick bg:#151622 → #0F1019(略带通透,无 token)
            .bg(linear_gradient(
                180.,
                linear_color_stop(rgba(0x151622e6), 0.),
                linear_color_stop(rgba(0x0f1019f2), 1.),
            ))
            .child(specular_top())
            .child(
                // .lhead:13 / 640 / fg-dim
                div()
                    .px(px(22.))
                    .pt(px(20.))
                    .pb(px(4.))
                    .text_size(px(13.))
                    .font_weight(FontWeight(640.))
                    .text_color(gpui::rgb(0xA6AFD4)) // fg-dim(无 token)
                    .child(SharedString::from("起一个会话 — Quick Terminal")),
            )
            .child(
                // .tiles:4 列网格(flex_wrap + 定宽磁贴,居中,>4 自动换行)
                div()
                    .px(px(22.))
                    .pt(px(10.))
                    .pb(px(18.))
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(11.))
                    .children(tiles),
            )
            .child(div().flex_1()) // body 留白:吸收窗口高度余量,提示贴底
            .child(div().h(px(1.)).bg(rgba(0xffffff0d))) // mockup .body border-top 白 .05
            .child(
                // .body 底部 ephemeral 提示
                div()
                    .px(px(22.))
                    .py(px(12.))
                    .text_size(px(11.5))
                    .text_color(col(ui.muted))
                    .child(SharedString::from(
                        "↑↓←→ 选择 · Enter 启动 · Esc 收起 · 退出当前会话即回到此启动器",
                    )),
            );

        Some(card)
    }

    /// One launcher tile (mockup `.tile` + `.tile.sel`): an agent-tinted icon chip,
    /// the profile name, and a sub-label. Click launches it.
    #[allow(clippy::too_many_arguments)]
    fn launcher_tile(
        &self,
        i: usize,
        is_sel: bool,
        name: String,
        sub: String,
        glyph: &'static str,
        accent: tn_config::Color,
        cx: &mut Context<Self>,
    ) -> Div {
        let t = &self.config.theme;
        let ui = &t.ui;
        div()
            .w(px(128.)) // 4 列定宽塞进 600 宽卡(4×128 + 3×11 gap ≤ 卡内宽),>4 自动换行
            .flex()
            .flex_col()
            .gap(px(9.)) // .tile gap 9
            .p(px(14.)) // .tile padding 14
            .rounded(px(R_CARD)) // --r-card
            .bg(rgba(INSET)) // .tile bg = g2(.04)
            .border_1()
            // .tile.sel:border claude@.4 + bg --g3;否则 rim 边 + hover 提亮
            .when(is_sel, |d| d.border_color(cola(t.agents.claude, 0.4)).bg(rgba(HOVER)))
            .when(!is_sel, |d| d.border_color(rgba(RIM)).hover(|s| s.bg(rgba(HOVER))))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, w, cx| {
                    this.picker_sel = i;
                    this.launch_selected(w, cx);
                }),
            )
            .child(
                // .ic:30×30 圆角 9,accent@.14 底 + accent 图标 18
                div()
                    .w(px(30.))
                    .h(px(30.))
                    .rounded(px(9.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(cola(accent, 0.14))
                    .child(icon(glyph, 18., accent)),
            )
            .child(
                // .tn:13 / 640 / fg
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight(640.))
                    .text_color(col(ui.foreground))
                    .child(SharedString::from(name)),
            )
            .child(
                // .td:11 / muted
                div().text_size(px(11.)).text_color(col(ui.muted)).child(SharedString::from(sub)),
            )
    }
}

impl Render for QuickTerminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus the launcher (if open) or the running terminal, now that the
        // element exists. gpui remembers element focus across the OS-level show,
        // so doing it on the notify-driven render is enough for keys to route.
        if self.pending_focus {
            self.pending_focus = false;
            if self.picker_open {
                self.picker_focus.focus(window);
            } else if let Some(t) = &self.term {
                let fh = t.read(cx).focus_handle();
                fh.focus(window);
            }
        }

        let theme = &self.config.theme;
        let mut root = div().size_full().overflow_hidden();
        // Launcher state leaves the window transparent so only the centered glass card
        // shows (floating on the desktop, no big opaque rectangle). A running session
        // needs an opaque dark fill behind the terminal (the TerminalView's own bg is
        // transparent — in the main window the pane's glass provides it; here we do).
        if self.term.is_some() {
            root = root.bg(cola(theme.terminal.background, 1.0));
        }

        // The live session fills the window (its own header shows the agent +
        // usage ring). The launcher overlays everything when open.
        if let Some(term) = &self.term {
            root = root.child(term.clone());
            // Launcher → session cross-fade: a dark wash over the fresh terminal that
            // eases out, so the session develops in instead of snapping.
            if let Some(at) = self.transition_at {
                let p = (at.elapsed().as_secs_f32() / (TRANSITION_MS as f32 / 1000.0)).clamp(0.0, 1.0);
                let a = (1.0 - ease_out_cubic(p)) * 0.96;
                if a > 0.004 {
                    root = root.child(
                        div().absolute().size_full().bg(cola(theme.terminal.background, a)),
                    );
                }
            }
        }
        if let Some(picker) = self.render_picker(cx) {
            root = root.child(picker);
        }
        root
    }
}
