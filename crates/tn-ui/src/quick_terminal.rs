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
    div, prelude::*, px, rgba, Context, Div, Entity, FocusHandle, KeyDownEvent, MouseButton,
    SharedString, Window,
};
use tn_config::Loaded;

use crate::platform;
use crate::style::{col, cola, shadowed, soft_shadow, HOVER, INSET, RIM, R_CARD, R_WINDOW, SHEEN, UI_SANS};
use crate::terminal_view::{LaunchSpec, ProcessExited, TerminalView, UsageUpdated};

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
        if self.hwnd.is_none() {
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
                cx.notify();
            }
        })
        .detach();
        self.term = Some(term); // replaces any prior session (old one is dropped -> killed)
        self.picker_open = false;
        self.pending_focus = true; // focus happens in render
        cx.notify();
    }

    /// Launcher keystrokes: ↑↓ select, Enter launch, Esc back (to the running
    /// session, or hide the window if there's nothing to go back to).
    fn on_picker_key(&mut self, ev: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
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
            "up" => {
                self.picker_sel = self.picker_sel.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                self.picker_sel = (self.picker_sel + 1).min(n - 1);
                cx.notify();
            }
            "enter" => self.launch_selected(window, cx),
            _ => {}
        }
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
        let qt = self.config.config.quick_terminal.clone();

        cx.spawn(async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
            if first {
                platform::make_topmost(h);
            }
            let Some(work) = platform::work_area(h) else { return };
            if reveal {
                platform::set_bounds(h, qt.hidden_rect(work));
                platform::show(h, true);
                let _ = this.update(cx, |_, cx| cx.notify()); // render -> consume focus
            }
            let dur = Duration::from_millis(qt.animation_ms);
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
                platform::set_bounds(h, qt.frame_rect(work, t));
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

    /// The launcher overlay (a frosted, centered panel listing the launchable
    /// profiles), or `None` when closed. Mirrors the workspace command palette.
    fn render_picker(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.picker_open {
            return None;
        }
        let t = &self.config.theme;
        let ui = &t.ui;
        let entries = self.launchable();
        let sel = self.picker_sel.min(entries.len().saturating_sub(1));

        let header = div()
            .px_3()
            .py_2()
            .text_size(px(12.5))
            .text_color(col(ui.muted))
            .child(SharedString::from(
                "启动会话   ↑↓ 选择 · Enter 启动 · Esc 取消   (退出当前会话即回到这里)",
            ));

        // Rows: each launchable profile, or a single synthetic pwsh fallback.
        let rows: Vec<Div> = if entries.is_empty() {
            vec![self.picker_row(0, true, "PowerShell", "powershell.exe", t.agents.claude, cx)]
        } else {
            entries
                .iter()
                .enumerate()
                .map(|(i, p)| {
                    let accent = p.accent.unwrap_or(t.agents.claude);
                    let hint = p.command.clone().unwrap_or_default();
                    self.picker_row(i, i == sel, &p.name, &hint, accent, cx)
                })
                .collect()
        };

        let panel = shadowed(
            div()
                .flex()
                .flex_col()
                .w(px(440.))
                .max_w(px(640.))
                .font_family(UI_SANS)
                .rounded(px(R_WINDOW))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(RIM))
                .bg(cola(ui.palette_bg, 0.92))
                .child(div().h(px(1.)).bg(rgba(SHEEN)))
                .child(header)
                .child(div().h(px(1.)).bg(rgba(RIM)))
                .child(div().flex().flex_col().p_1().gap_1().children(rows)),
            vec![soft_shadow(24.0, 64.0, -36.0, 0.6)],
        );

        Some(
            div()
                .absolute()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .bg(rgba(0x0a0b11cc)) // dim scrim
                .track_focus(&self.picker_focus)
                .on_key_down(cx.listener(|this, ev: &KeyDownEvent, w, cx| this.on_picker_key(ev, w, cx)))
                .child(panel),
        )
    }

    fn picker_row(
        &self,
        i: usize,
        is_sel: bool,
        name: &str,
        hint: &str,
        accent: tn_config::Color,
        cx: &mut Context<Self>,
    ) -> Div {
        let ui = &self.config.theme.ui;
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_1()
            .rounded(px(R_CARD))
            .when(is_sel, |d| d.bg(rgba(HOVER)))
            .when(!is_sel, |d| d.hover(|s| s.bg(rgba(INSET))))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, w, cx| {
                    this.picker_sel = i;
                    this.launch_selected(w, cx);
                }),
            )
            .child(div().w(px(7.)).h(px(7.)).rounded_full().bg(col(accent)))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(col(ui.foreground))
                    .child(SharedString::from(name.to_string())),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(col(ui.muted))
                    .child(SharedString::from(hint.to_string())),
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
        let mut root = div()
            .size_full()
            .overflow_hidden()
            .bg(cola(theme.terminal.background, 0.98))
            .border_1()
            .border_color(cola(theme.agents.claude, 0.35));

        // The live session fills the window (its own header shows the agent +
        // usage ring). The launcher overlays everything when open.
        if let Some(term) = &self.term {
            root = root.child(term.clone());
        }
        if let Some(picker) = self.render_picker(cx) {
            root = root.child(picker);
        }
        root
    }
}
