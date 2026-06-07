//! Quick Terminal (M5): the Quake/Guake-style drop-down floating terminal.
//!
//! A separate borderless, topmost GPUI window (`WindowKind::PopUp`) that slides in
//! from the configured edge on a global hotkey (see [`crate::platform`]), takes
//! focus, and (optionally) slides away when it loses focus. On summon with no live
//! session it shows a **launcher** (configured Agent / pwsh — the command-bearing
//! `[[profiles]]`, mirroring the workspace command palette); the picked session is
//! a normal [`TerminalView`], so an agent gets its usual header and capability slots.
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
    canvas, div, linear_color_stop, linear_gradient, prelude::*, px, rgba, Bounds, Context, Div,
    ElementInputHandler, Entity, EntityInputHandler, FocusHandle, FontWeight, KeyDownEvent,
    MouseButton, Pixels, SharedString, UTF16Selection, Window,
};
use tn_config::{ease_out_cubic, lerp_rect, Loaded, Rect};

use crate::platform;
use crate::ssh_recents::{AuthBadge, SshRecents};
use crate::style::{col, cola, icon, HOVER, INSET, RIM, R_CARD, R_PANEL, UI_SANS};
use crate::terminal_view::{
    FileNamespace, LaunchSpec, ProcessExited, SshCloseRequested, SshConnected, SshRememberPassword,
    SshRetryRequested, TerminalView, UsageUpdated,
};
use crate::welcome::{
    launch_entries, profile_card, ssh_card, wsl_card, wsl_distros, CardId, LaunchEntry,
};

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
    /// Last launch spec for the hosted session; SSH retry / remember-password update it.
    term_spec: Option<LaunchSpec>,
    /// Launcher overlay state.
    picker_open: bool,
    picker_sel: usize,
    /// Within the launcher, drilled into the WSL group's distro sub-picker.
    wsl_drill: bool,
    picker_focus: FocusHandle,
    /// Compact SSH connector shown after activating the SSH launcher card.
    ssh_prompt_open: bool,
    ssh_prompt_input: String,
    ssh_prompt_sel: usize,
    ssh_prompt_focus: FocusHandle,
    ssh_recents: SshRecents,
    ssh_config_hosts: Vec<tn_pty::SshHostEntry>,
    ssh_rename: Option<QuickSshRenameDraft>,
    ssh_rename_marked: Option<String>,
    ssh_ime_disabled: bool,
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

/// What a launcher tile does when activated. Built from [`launch_entries`] + the
/// `wsl_drill` flag so render, keyboard nav, and click all agree on the current view.
#[derive(Clone, Copy)]
enum PickerItem {
    /// Launch the profile at this index into `launch_profiles`.
    Launch(usize),
    /// Synthetic fallback when nothing is launchable (default local PowerShell).
    Pwsh,
    /// The aggregated WSL card: drill into the distro sub-picker (or launch the lone one).
    DrillWsl,
    /// Interactive SSH prompt launcher.
    SshPrompt,
}

#[derive(Clone)]
enum QuickSshRow {
    Profile {
        name: String,
        target: String,
        resolved: String,
    },
    Recent {
        host: String,
        user: String,
        port: u16,
        name: Option<String>,
        target: String,
        favorite: bool,
        auth: AuthBadge,
        last_used: u64,
    },
    Config {
        alias: String,
        target: String,
    },
}

#[derive(Clone)]
struct QuickSshRenameDraft {
    host: String,
    user: String,
    port: u16,
    name: String,
}

impl QuickSshRow {
    fn connect_target(&self) -> String {
        match self {
            QuickSshRow::Profile { target, .. }
            | QuickSshRow::Recent { target, .. }
            | QuickSshRow::Config { alias: target, .. } => target.clone(),
        }
    }
}

impl QuickTerminal {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let launch_profiles = crate::workspace::discover_profiles(&config);
        Self {
            config,
            term: None,
            term_spec: None,
            picker_open: false,
            picker_sel: 0,
            wsl_drill: false,
            picker_focus: cx.focus_handle(),
            ssh_prompt_open: false,
            ssh_prompt_input: String::new(),
            ssh_prompt_sel: 0,
            ssh_prompt_focus: cx.focus_handle(),
            ssh_recents: SshRecents::load(),
            ssh_config_hosts: tn_pty::list_ssh_config_hosts(),
            ssh_rename: None,
            ssh_rename_marked: None,
            ssh_ime_disabled: false,
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

    /// Indices (into `launch_profiles`) of all discovered WSL distros, in order.
    fn wsl_indices(&self) -> Vec<usize> {
        wsl_distros(&self.launch_profiles)
    }

    /// The launcher's tiles grouped into visual **rows**: a bare launcher shows agents
    /// configured agents on top and shells + WSL + SSH below (用户要的两行排版); a drill
    /// shows the WSL distros in one row. Render, card sizing, and keyboard nav all read
    /// this so `picker_sel` (a flat index across the rows) stays consistent.
    fn picker_rows(&self) -> Vec<Vec<PickerItem>> {
        if self.wsl_drill {
            // Just the distros — back is via the clickable "‹" header or Esc.
            return vec![self
                .wsl_indices()
                .into_iter()
                .map(PickerItem::Launch)
                .collect()];
        }
        let mut agents = Vec::new();
        let mut others = Vec::new();
        for e in launch_entries(&self.launch_profiles) {
            match e {
                LaunchEntry::Profile(i) => {
                    if crate::welcome::is_agent_profile(&self.launch_profiles[i]) {
                        agents.push(PickerItem::Launch(i)); // agents → top row
                    } else {
                        others.push(PickerItem::Launch(i)); // pwsh → bottom row
                    }
                }
                LaunchEntry::Wsl(_) => others.push(PickerItem::DrillWsl), // WSL → bottom
                LaunchEntry::SshPrompt => others.push(PickerItem::SshPrompt), // SSH → bottom
            }
        }
        // Defensive: nothing launchable (only the SSH placeholder) → offer pwsh.
        if agents.is_empty() && others.iter().all(|it| matches!(it, PickerItem::SshPrompt)) {
            others.insert(0, PickerItem::Pwsh);
        }
        let mut rows = Vec::new();
        if !agents.is_empty() {
            rows.push(agents);
        }
        if !others.is_empty() {
            rows.push(others);
        }
        rows
    }

    /// Flat tile list across all rows — `picker_sel` indexes this (for click + activate).
    fn picker_items(&self) -> Vec<PickerItem> {
        self.picker_rows().into_iter().flatten().collect()
    }

    /// The card identity (name / sub / glyph / accent) for a picker tile.
    fn item_card(&self, item: &PickerItem, cx: &gpui::App) -> CardId {
        let t = &self.config.theme;
        match item {
            PickerItem::Launch(i) => {
                let reg = crate::agent_host::agent_registry(cx);
                profile_card(t, &self.launch_profiles[*i], &reg)
            }
            PickerItem::Pwsh => CardId {
                name: "PowerShell".into(),
                sub: "powershell.exe".into(),
                glyph: "term",
                accent: t.ui.accent,
            },
            PickerItem::DrillWsl => wsl_card(t, self.wsl_indices().len()),
            PickerItem::SshPrompt => ssh_card(t),
        }
    }

    /// Toggle visibility — the action bound to the global hotkey.
    pub fn toggle(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.ensure_init(window, cx);
        let reveal = !self.visible;
        if reveal && self.term.is_none() && !self.ssh_prompt_open {
            // Nothing running yet — summon straight into the launcher (root view).
            self.picker_open = true;
            self.picker_sel = 0;
            self.wsl_drill = false;
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

    /// Activate the currently-selected tile: launch a profile, drill into / out of the
    /// WSL sub-picker, or open the compact SSH connector.
    fn activate_sel(&mut self, cx: &mut Context<Self>) {
        let items = self.picker_items();
        let Some(item) = items.get(self.picker_sel) else {
            return;
        };
        match *item {
            PickerItem::Launch(i) => self.launch_profile(i, cx),
            PickerItem::Pwsh => self.launch_spec(LaunchSpec::pwsh(), cx),
            PickerItem::DrillWsl => {
                let distros = self.wsl_indices();
                if distros.len() == 1 {
                    self.launch_profile(distros[0], cx); // lone distro → skip the sub-picker
                } else {
                    self.wsl_drill = true;
                    self.picker_sel = 0;
                    self.resnap(cx); // resize the card to fit the distro list
                    cx.notify();
                }
            }
            PickerItem::SshPrompt => self.open_ssh_prompt(cx),
        }
    }

    fn open_ssh_prompt(&mut self, cx: &mut Context<Self>) {
        self.picker_open = false;
        self.wsl_drill = false;
        self.ssh_prompt_open = true;
        self.ssh_prompt_input.clear();
        self.ssh_prompt_sel = 0;
        self.ssh_recents = SshRecents::load();
        self.ssh_config_hosts = tn_pty::list_ssh_config_hosts();
        self.ssh_rename = None;
        self.ssh_rename_marked = None;
        self.pending_focus = true;
        self.resnap(cx);
        cx.notify();
    }

    fn close_ssh_prompt_to_picker(&mut self, cx: &mut Context<Self>) {
        self.ssh_prompt_open = false;
        self.picker_open = true;
        self.picker_sel = 0;
        self.wsl_drill = false;
        self.ssh_rename = None;
        self.ssh_rename_marked = None;
        self.pending_focus = true;
        self.resnap(cx);
        cx.notify();
    }

    /// Launch the profile at `idx` (ephemeral) as the hosted session.
    fn launch_profile(&mut self, idx: usize, cx: &mut Context<Self>) {
        let reg = crate::agent_host::agent_registry(cx);
        let spec = self
            .launch_profiles
            .get(idx)
            .and_then(|p| LaunchSpec::from_profile_ephemeral(p, &reg))
            .unwrap_or_else(LaunchSpec::pwsh);
        self.launch_spec(spec, cx);
    }

    /// Launch `spec` as the hosted session (replacing any previous one — its process is
    /// killed by `LocalPty::Drop`), grow the card-sized window to the drop-down, fade in.
    fn launch_spec(&mut self, spec: LaunchSpec, cx: &mut Context<Self>) {
        // Ephemeral launch: an agent's pwsh host omits `-NoExit`, so exiting the agent
        // exits the PTY and we fall back to the launcher (the ProcessExited sub below).
        let config = self.config.clone();
        let term_spec = spec.clone();
        let term = cx.new(|cx| TerminalView::new(cx, config, spec));
        // Repaint when this session's agent usage changes (keeps the ring live).
        cx.subscribe(&term, |_qt, _t, _ev: &UsageUpdated, cx| cx.notify())
            .detach();
        cx.subscribe(&term, |this, emitter, ev: &SshConnected, _cx| {
            if this.term.as_ref().map(|t| t.entity_id()) != Some(emitter.entity_id()) {
                return;
            }
            if let Some(cfg) = this.term_spec.as_ref().and_then(|s| s.ssh.as_ref()) {
                this.ssh_recents
                    .record(&cfg.host, &cfg.user, cfg.port, AuthBadge::from_pty(ev.0));
                this.ssh_recents.save();
            }
        })
        .detach();
        cx.subscribe(&term, |this, emitter, _ev: &SshRetryRequested, cx| {
            if this.term.as_ref().map(|t| t.entity_id()) != Some(emitter.entity_id()) {
                return;
            }
            if let Some(spec) = this.term_spec.clone() {
                this.launch_spec(spec, cx);
            }
        })
        .detach();
        cx.subscribe(&term, |this, emitter, _ev: &SshCloseRequested, cx| {
            if this.term.as_ref().map(|t| t.entity_id()) != Some(emitter.entity_id()) {
                return;
            }
            this.term = None;
            this.term_spec = None;
            this.picker_open = true;
            this.ssh_prompt_open = false;
            this.ssh_rename = None;
            this.ssh_rename_marked = None;
            this.picker_sel = 0;
            this.wsl_drill = false;
            this.pending_focus = true;
            this.resnap(cx);
            cx.notify();
        })
        .detach();
        cx.subscribe(&term, |this, emitter, ev: &SshRememberPassword, _cx| {
            if this.term.as_ref().map(|t| t.entity_id()) != Some(emitter.entity_id()) {
                return;
            }
            if let Some(ssh) = this.term_spec.as_mut().and_then(|s| s.ssh.as_mut()) {
                ssh.password = Some(ev.0.clone());
            }
        })
        .detach();
        // When the session's process exits, return to the launcher (guard against a
        // stale watcher from a session we've since replaced).
        cx.subscribe(&term, |this, emitter, _ev: &ProcessExited, cx| {
            if this.term.as_ref().map(|t| t.entity_id()) == Some(emitter.entity_id()) {
                this.term = None;
                this.term_spec = None;
                this.picker_open = true;
                this.ssh_prompt_open = false;
                this.ssh_rename = None;
                this.ssh_rename_marked = None;
                this.picker_sel = 0;
                this.wsl_drill = false;
                this.pending_focus = true;
                this.resnap(cx); // shrink the window back to the compact launcher card
                cx.notify();
            }
        })
        .detach();
        self.term = Some(term); // replaces any prior session (old one is dropped -> killed)
        self.term_spec = Some(term_spec);
        self.picker_open = false;
        self.ssh_prompt_open = false;
        self.ssh_rename = None;
        self.ssh_rename_marked = None;
        self.wsl_drill = false;
        self.pending_focus = true; // focus happens in render
        self.resnap(cx); // grow the card-sized window to the session drop-down
        self.start_transition(cx); // cross-fade the launcher → session
        cx.notify();
    }

    /// Launcher keystrokes: ←→ walk the flat order, ↑↓ move between the visual rows
    /// (agents ↔ shells/WSL/SSH) at the same column, Enter activates, Esc backs out of
    /// the WSL sub-picker (or, at the root, returns to the session / hides the window).
    fn on_picker_key(&mut self, ev: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let lens: Vec<usize> = self.picker_rows().iter().map(|r| r.len()).collect();
        let n: usize = lens.iter().sum::<usize>().max(1);
        // Resolve the flat `picker_sel` to its (row, column).
        let (mut cur_row, mut col, mut acc) = (0usize, 0usize, 0usize);
        for (ri, &len) in lens.iter().enumerate() {
            if self.picker_sel < acc + len {
                cur_row = ri;
                col = self.picker_sel - acc;
                break;
            }
            acc += len;
        }
        let row_start = |ri: usize| lens[..ri].iter().sum::<usize>();
        match ev.keystroke.key.as_str() {
            "escape" => {
                if self.wsl_drill {
                    self.wsl_drill = false; // back to the root launcher
                    self.picker_sel = 0;
                    self.resnap(cx);
                    cx.notify();
                } else {
                    self.picker_open = false;
                    if self.term.is_some() {
                        self.pending_focus = true; // back to the terminal
                        cx.notify();
                    } else {
                        self.slide(false, cx); // nothing running — just hide
                    }
                }
            }
            "left" => {
                self.picker_sel = self.picker_sel.saturating_sub(1);
                cx.notify();
            }
            "right" => {
                self.picker_sel = (self.picker_sel + 1).min(n - 1);
                cx.notify();
            }
            "up" => {
                if cur_row > 0 {
                    let tr = cur_row - 1;
                    self.picker_sel = row_start(tr) + col.min(lens[tr].saturating_sub(1));
                }
                cx.notify();
            }
            "down" => {
                if cur_row + 1 < lens.len() {
                    let tr = cur_row + 1;
                    self.picker_sel = row_start(tr) + col.min(lens[tr].saturating_sub(1));
                }
                cx.notify();
            }
            "enter" => self.activate_sel(cx),
            _ => {}
        }
    }

    fn ssh_rows(&self) -> Vec<QuickSshRow> {
        let q = self.ssh_prompt_input.trim();
        let ql = q.to_ascii_lowercase();
        let matches = |s: &str| ql.is_empty() || s.to_ascii_lowercase().contains(&ql);
        let mut seen_eps: std::collections::HashSet<(String, String, u16)> =
            std::collections::HashSet::new();
        let mut rows = Vec::new();

        for p in &self.launch_profiles {
            if p.kind != tn_config::ProfileKind::Ssh {
                continue;
            }
            let Some(host) = p.host.as_deref().filter(|h| !h.trim().is_empty()) else {
                continue;
            };
            let target = if let Some(user) = p.user.as_deref().filter(|u| !u.trim().is_empty()) {
                format!("{user}@{host}")
            } else {
                host.to_string()
            };
            let cfg = tn_pty::SshConfig::parse(&target, None);
            let resolved = crate::ssh_recents::format_target(&cfg.user, &cfg.host, cfg.port);
            if !(matches(&p.name) || matches(&target) || matches(&resolved)) {
                continue;
            }
            seen_eps.insert((cfg.host.to_ascii_lowercase(), cfg.user.clone(), cfg.port));
            rows.push(QuickSshRow::Profile {
                name: p.name.clone(),
                target,
                resolved,
            });
        }

        for r in self.ssh_recents.filtered(q) {
            seen_eps.insert((r.host.to_ascii_lowercase(), r.user.clone(), r.port));
            rows.push(QuickSshRow::Recent {
                host: r.host.clone(),
                user: r.user.clone(),
                port: r.port,
                name: r.name.clone(),
                target: r.target(),
                favorite: r.favorite,
                auth: r.auth,
                last_used: r.last_used,
            });
        }

        for h in &self.ssh_config_hosts {
            let user = h.user.clone().unwrap_or_default();
            let target = crate::ssh_recents::format_target(&user, &h.host, h.port);
            if !(matches(&h.alias) || matches(&target) || matches(&h.host)) {
                continue;
            }
            if seen_eps.contains(&(h.host.to_ascii_lowercase(), user.clone(), h.port)) {
                continue;
            }
            rows.push(QuickSshRow::Config {
                alias: h.alias.clone(),
                target,
            });
        }

        rows.truncate(5);
        rows
    }

    fn ssh_commit_rename(&mut self, cx: &mut Context<Self>) {
        let Some(draft) = self.ssh_rename.take() else {
            return;
        };
        self.ssh_rename_marked = None;
        self.ssh_recents
            .rename(&draft.host, &draft.user, draft.port, &draft.name);
        self.ssh_recents.save();
        cx.notify();
    }

    fn ssh_cancel_rename(&mut self, cx: &mut Context<Self>) {
        self.ssh_rename = None;
        self.ssh_rename_marked = None;
        cx.notify();
    }

    fn on_ssh_prompt_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        let key = ev.keystroke.key.as_str();
        let printable = || -> Option<String> {
            ev.keystroke
                .key_char
                .as_ref()
                .filter(|c| {
                    !ev.keystroke.modifiers.control
                        && !ev.keystroke.modifiers.alt
                        && !ev.keystroke.modifiers.platform
                        && c.chars().all(|ch| ch.is_ascii_graphic() || ch == ' ')
                })
                .cloned()
        };

        if self.ssh_rename.is_some() {
            match key {
                "escape" => self.ssh_cancel_rename(cx),
                "enter" => self.ssh_commit_rename(cx),
                "backspace" => {
                    if self.ssh_rename_marked.take().is_some() {
                        cx.notify();
                    } else if let Some(d) = self.ssh_rename.as_mut() {
                        d.name.pop();
                        cx.notify();
                    }
                }
                _ => {
                    if let Some(c) = printable() {
                        if let Some(d) = self.ssh_rename.as_mut() {
                            d.name.push_str(&c);
                        }
                        cx.notify();
                    }
                }
            }
            cx.stop_propagation();
            return;
        }

        let rows = self.ssh_rows();
        let n = rows.len();
        match key {
            "escape" => self.close_ssh_prompt_to_picker(cx),
            "down" => {
                if n > 0 {
                    self.ssh_prompt_sel = (self.ssh_prompt_sel + 1).min(n - 1);
                    cx.notify();
                }
            }
            "up" => {
                self.ssh_prompt_sel = self.ssh_prompt_sel.saturating_sub(1);
                cx.notify();
            }
            "enter" => {
                let target = if n > 0 {
                    rows[self.ssh_prompt_sel.min(n - 1)].connect_target()
                } else {
                    let typed = self.ssh_prompt_input.trim();
                    if typed.is_empty() || crate::workspace::validate_ssh_target(typed).is_err() {
                        cx.stop_propagation();
                        return;
                    }
                    typed.to_string()
                };
                self.ssh_connect(&target, cx);
            }
            "backspace" => {
                self.ssh_prompt_input.pop();
                self.ssh_prompt_sel = 0;
                cx.notify();
            }
            _ => {
                if let Some(c) = printable() {
                    self.ssh_prompt_input.push_str(&c);
                    self.ssh_prompt_sel = 0;
                    cx.notify();
                }
            }
        }
        cx.stop_propagation();
    }

    fn ssh_connect(&mut self, target: &str, cx: &mut Context<Self>) {
        let target = target.trim();
        if target.is_empty() || crate::workspace::validate_ssh_target(target).is_err() {
            return;
        }
        let cfg = tn_pty::SshConfig::parse(target, None);
        let program = if cfg.user.is_empty() {
            cfg.host.clone()
        } else {
            format!("{}@{}", cfg.user, cfg.host)
        };
        self.launch_spec(
            LaunchSpec {
                program,
                args: Vec::new(),
                integrate_pwsh: false,
                shell_integration: None,
                agent: None,
                ssh: Some(cfg),
                cwd: None,
                file_namespace: FileNamespace::Ssh,
            },
            cx,
        );
    }

    /// Pixel height of the launcher card = `.lhead` + tiles (one row per 4) + footer.
    /// The launcher window is sized to exactly this (see [`shown_for`]) so the
    /// transparent window hugs the card — no surrounding window rectangle to peek.
    fn card_height(&self) -> f32 {
        if self.ssh_prompt_open {
            return 500.0;
        }
        // Sum each logical row's wrapped height (4 tiles per visual row).
        let rows = self
            .picker_rows()
            .iter()
            .map(|r| r.len().max(1).div_ceil(4))
            .sum::<usize>()
            .max(1) as f32;
        // lhead + footer + tiles-padding + divider + border (~126) + each tile row
        // (~114) + 11px inter-row gaps. Deliberately generous so the bottom hint never
        // clips (CJK line heights run taller than ASCII estimates); any surplus is
        // absorbed by the `flex_1` body spacer rather than cutting the footer.
        126.0 + rows * 114.0 + (rows - 1.0) * 11.0
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
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                let Some(work) = platform::work_area(h) else {
                    return;
                };
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
            },
        )
        .detach();
    }

    /// Start the launcher → session cross-fade: a dark wash covers the freshly
    /// mounted terminal and fades out over [`TRANSITION_MS`], so the session
    /// "develops" in instead of snapping. Driven per-frame like the bell fade.
    fn start_transition(&mut self, cx: &mut Context<Self>) {
        self.transition_at = Some(Instant::now());
        let total = Duration::from_millis(TRANSITION_MS);
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| loop {
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
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
            },
        )
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

        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                if first {
                    platform::make_topmost(h);
                }
                let Some(work) = platform::work_area(h) else {
                    return;
                };
                let scale = platform::scale_for(h);
                // State-aware endpoints: card-sized for a bare launcher, full for a session.
                let Ok((hidden, shown, rounded)) = this.update(cx, |v, _| {
                    (
                        v.hidden_for(work, scale),
                        v.shown_for(work, scale),
                        v.term.is_none(),
                    )
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
                    if !this
                        .update(cx, |v, _| v.anim_token == token)
                        .unwrap_or(false)
                    {
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
                    cx.background_executor()
                        .timer(Duration::from_millis(16))
                        .await;
                }
            },
        )
        .detach();
        cx.notify();
    }

    /// The launcher card (mockup `.quick`): fills the (card-sized, transparent) window
    /// so its rounded corners float on the desktop. Tiles come from [`picker_items`] —
    /// the aggregated root (profiles + WSL card + SSH placeholder), or, when drilled, a
    /// Back tile + the WSL distros. `None` when closed.
    fn render_picker(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.picker_open {
            return None;
        }
        let t = &self.config.theme;
        let ui = &t.ui;
        let rows = self.picker_rows();
        let total: usize = rows.iter().map(|r| r.len()).sum();
        let sel = self.picker_sel.min(total.saturating_sub(1));
        // One flex-wrap row per visual row; a running flat index keeps `picker_sel` +
        // click in step with `picker_items`.
        let mut row_divs: Vec<Div> = Vec::new();
        let mut flat = 0usize;
        for row in &rows {
            let mut tiles: Vec<Div> = Vec::new();
            for item in row {
                let i = flat;
                flat += 1;
                let c = self.item_card(item, cx);
                tiles.push(self.launcher_tile(i, i == sel, c.name, c.sub, c.glyph, c.accent, cx));
            }
            row_divs.push(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .justify_center()
                    .gap(px(11.))
                    .children(tiles),
            );
        }

        let head = if self.wsl_drill {
            "‹ 选择 WSL 发行版"
        } else {
            "起一个会话 — Quick Terminal"
        };
        let hint = if self.wsl_drill {
            "↑↓←→ 选择 · Enter 启动 · Esc 返回"
        } else {
            "↑↓←→ 选择 · Enter 启动 · Esc 收起 · 退出当前会话即回到此启动器"
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
            .on_key_down(
                cx.listener(|this, ev: &KeyDownEvent, w, cx| this.on_picker_key(ev, w, cx)),
            )
            // mockup .quick bg:#151622 → #0F1019(略带通透,无 token)
            .bg(linear_gradient(
                180.,
                linear_color_stop(rgba(0x151622e6), 0.),
                linear_color_stop(rgba(0x0f1019f2), 1.),
            ))
            .child(crate::style::specular_wash(true, ui.accent))
            .child(
                // .lhead:13 / 640 / fg-dim;drilled 时整行可点 = 返回
                div()
                    .px(px(22.))
                    .pt(px(20.))
                    .pb(px(4.))
                    .text_size(px(13.))
                    .font_weight(FontWeight(640.))
                    .text_color(gpui::rgb(0xA6AFD4)) // fg-dim(无 token)
                    .when(self.wsl_drill, |d| {
                        d.hover(|s| s.text_color(col(ui.foreground))).on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                this.wsl_drill = false;
                                this.picker_sel = 0;
                                this.resnap(cx);
                                cx.notify();
                            }),
                        )
                    })
                    .child(SharedString::from(head)),
            )
            .child(
                // .tiles:两行(agents 上 / shells·WSL·SSH 下),每行 4 列定宽磁贴居中、>4 换行
                div()
                    .px(px(22.))
                    .pt(px(10.))
                    .pb(px(18.))
                    .flex()
                    .flex_col()
                    .gap(px(11.))
                    .children(row_divs),
            )
            .child(div().flex_1()) // body 留白:吸收窗口高度余量,提示贴底
            .child(div().h(px(1.)).bg(rgba(0xffffff0d))) // mockup .body border-top 白 .05
            .child(
                // .body 底部提示(随视图变)
                div()
                    .px(px(22.))
                    .py(px(12.))
                    .text_size(px(11.5))
                    .text_color(col(ui.muted))
                    .child(SharedString::from(hint)),
            );

        Some(card)
    }

    fn render_ssh_prompt(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.ssh_prompt_open {
            return None;
        }
        let t = &self.config.theme;
        let ui = &t.ui;
        let mono = SharedString::from(self.config.font().family.clone());
        let typed = self.ssh_prompt_input.trim().to_string();
        let ssh_err = (!typed.is_empty())
            .then(|| crate::workspace::validate_ssh_target(&typed).err())
            .flatten();
        let rows = self.ssh_rows();
        let sel = self.ssh_prompt_sel.min(rows.len().saturating_sub(1));
        let placeholder = "user@host[:port]";

        let chips = crate::workspace::parse_ssh_target_chips(&typed);
        let has_error = ssh_err.is_some();
        let chip = |label: &str, val: String| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(999.))
                .bg(rgba(HOVER))
                .text_size(px(10.))
                .child(
                    div()
                        .text_color(col(ui.muted))
                        .child(SharedString::from(label.to_string())),
                )
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_color(col(ui.accent))
                        .child(SharedString::from(val)),
                )
        };
        let chips_row = chips.as_ref().map(|(user, host, port)| {
            let mut row = div().flex().flex_row().items_center().gap(px(5.));
            if let Some(user) = user {
                row = row.child(chip("user", user.clone()));
            }
            row = row.child(chip("host", host.clone()));
            if let Some(port) = port {
                row = row.child(chip("port", port.clone()));
            }
            row
        });
        let err_chip = ssh_err.map(|msg| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(999.))
                .bg(cola(t.ansi.red, 0.12))
                .text_size(px(10.))
                .child(icon("alert", 11., t.ansi.red))
                .child(
                    div()
                        .text_color(col(t.ansi.red))
                        .child(SharedString::from(msg)),
                )
        });

        let mut list = div().px(px(22.)).pb(px(13.)).flex().flex_col().gap(px(7.));

        if rows.is_empty() {
            let copy = if typed.is_empty() {
                "输入 user@host 后回车，或在 ~/.ssh/config 添加 Host alias"
            } else if has_error {
                "目标格式需要先修正"
            } else {
                "没有匹配的记录；回车连接当前输入"
            };
            list = list.child(
                div()
                    .h(px(74.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(12.))
                    .text_color(col(ui.muted))
                    .child(SharedString::from(copy)),
            );
        } else {
            for (i, row) in rows.iter().enumerate() {
                let selected = i == sel;
                let connect_target = row.connect_target();
                let row_el = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .px(px(11.))
                    .py(px(8.))
                    .rounded(px(9.))
                    .border_1()
                    .border_color(if selected {
                        cola(ui.accent, 0.32)
                    } else {
                        rgba(RIM)
                    })
                    .bg(if selected {
                        cola(ui.accent, 0.10)
                    } else {
                        rgba(INSET)
                    })
                    .when(!selected, |d| d.hover(|s| s.bg(cola(ui.accent, 0.07))))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _e, _w, cx| {
                            this.ssh_prompt_sel = i;
                            this.ssh_connect(&connect_target, cx);
                        }),
                    );

                let row_el = match row {
                    QuickSshRow::Profile {
                        name,
                        target: _,
                        resolved,
                    } => row_el
                        .child(icon("external", 15., ui.accent))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(1.))
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .font_weight(FontWeight(640.))
                                        .text_color(col(ui.foreground))
                                        .child(SharedString::from(name.clone())),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(col(ui.muted))
                                        .child(SharedString::from(resolved.clone())),
                                ),
                        )
                        .child(div().flex_1())
                        .child(self.ssh_row_chip("profile", ui.accent)),
                    QuickSshRow::Recent {
                        host,
                        user,
                        port,
                        name,
                        target,
                        favorite,
                        auth,
                        last_used,
                    } => {
                        let (host, user, port, favorite, auth) =
                            (host.clone(), user.clone(), *port, *favorite, *auth);
                        let name = name.clone();
                        let rename_active = self.ssh_rename.as_ref().is_some_and(|d| {
                            d.port == port && d.user == user && d.host.eq_ignore_ascii_case(&host)
                        });
                        let title_text = self
                            .ssh_rename
                            .as_ref()
                            .filter(|_| rename_active)
                            .map(|d| d.name.clone())
                            .or_else(|| name.clone())
                            .unwrap_or_else(|| host.clone());
                        let marked = rename_active
                            .then(|| self.ssh_rename_marked.clone())
                            .flatten();
                        let (badge_icon, badge_label, badge_color) = match auth {
                            AuthBadge::Key => ("key", "密钥", t.ansi.green),
                            AuthBadge::Password => ("lock", "密码", t.ansi.yellow),
                            AuthBadge::Unknown => ("external", "最近", ui.muted),
                        };
                        let (fav_host, fav_user) = (host.clone(), user.clone());
                        let star = div()
                            .child(icon(
                                "star",
                                14.,
                                if favorite { t.ansi.yellow } else { ui.muted },
                            ))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    cx.stop_propagation();
                                    this.ssh_recents.toggle_favorite(&fav_host, &fav_user, port);
                                    this.ssh_recents.save();
                                    cx.notify();
                                }),
                            );
                        let (rename_host, rename_user) = (host.clone(), user.clone());
                        let initial_name = name.clone().unwrap_or_else(|| rename_host.clone());
                        let rename_btn = div()
                            .child(icon("pen", 13., ui.muted))
                            .hover(|s| s.text_color(col(ui.accent)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _e, _w, cx| {
                                    cx.stop_propagation();
                                    this.ssh_rename = Some(QuickSshRenameDraft {
                                        host: rename_host.clone(),
                                        user: rename_user.clone(),
                                        port,
                                        name: initial_name.clone(),
                                    });
                                    this.ssh_rename_marked = None;
                                    cx.notify();
                                }),
                            );
                        row_el
                            .child(star)
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap(px(1.))
                                    .min_w(px(0.))
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .text_size(px(13.))
                                            .font_weight(FontWeight(640.))
                                            .text_color(if rename_active {
                                                col(ui.accent)
                                            } else {
                                                col(ui.foreground)
                                            })
                                            .child(SharedString::from(title_text))
                                            .when_some(marked, |d, m| {
                                                d.child(
                                                    div()
                                                        .text_color(col(ui.muted))
                                                        .child(SharedString::from(m)),
                                                )
                                            })
                                            .when(rename_active, |d| {
                                                d.child(
                                                    div()
                                                        .text_color(col(ui.muted))
                                                        .child(SharedString::from("▏")),
                                                )
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .text_color(col(ui.muted))
                                            .child(SharedString::from(target.clone())),
                                    ),
                            )
                            .child(div().flex_1())
                            .child(self.ssh_row_chip_with_icon(
                                badge_icon,
                                badge_label,
                                badge_color,
                            ))
                            .child(rename_btn)
                            .child(
                                div()
                                    .min_w(px(42.))
                                    .text_size(px(10.5))
                                    .text_color(rgba(0x8f96b880))
                                    .child(SharedString::from(crate::ssh_recents::rel_time(
                                        *last_used,
                                    ))),
                            )
                    }
                    QuickSshRow::Config { alias, target } => row_el
                        .child(icon("external", 15., ui.accent_alt))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap(px(1.))
                                .min_w(px(0.))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .font_weight(FontWeight(640.))
                                        .text_color(col(ui.foreground))
                                        .child(SharedString::from(alias.clone())),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.))
                                        .text_color(col(ui.muted))
                                        .child(SharedString::from(target.clone())),
                                ),
                        )
                        .child(div().flex_1())
                        .child(self.ssh_row_chip("ssh-config", ui.accent_alt)),
                };
                list = list.child(row_el);
            }
        }

        let footer = if self.ssh_rename.is_some() {
            "Enter 保存名称 · Esc 取消 · 支持中文输入"
        } else if rows.is_empty() {
            "Enter 连接 · Esc 返回启动器"
        } else {
            "↑↓ 选择 · Enter 连接 · ★ 收藏/取消收藏 · Esc 返回启动器"
        };
        let ime_focus = self.ssh_prompt_focus.clone();
        let ime_entity = cx.entity();
        let rename_ime_active = self.ssh_rename.is_some();

        Some(
            div()
                .size_full()
                .relative()
                .flex()
                .flex_col()
                .font_family(UI_SANS)
                .rounded(px(R_PANEL))
                .overflow_hidden()
                .border_1()
                .border_color(rgba(RIM))
                .track_focus(&self.ssh_prompt_focus)
                .on_key_down(
                    cx.listener(|this, ev: &KeyDownEvent, _w, cx| this.on_ssh_prompt_key(ev, cx)),
                )
                .bg(linear_gradient(
                    180.,
                    linear_color_stop(rgba(0x151622e6), 0.),
                    linear_color_stop(rgba(0x0f1019f2), 1.),
                ))
                .child(crate::style::specular_wash(true, ui.accent))
                .child(
                    div()
                        .px(px(22.))
                        .pt(px(18.))
                        .pb(px(7.))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(9.))
                        .text_size(px(13.))
                        .font_weight(FontWeight(640.))
                        .text_color(gpui::rgb(0xA6AFD4))
                        .hover(|s| s.text_color(col(ui.foreground)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| this.close_ssh_prompt_to_picker(cx)),
                        )
                        .child(SharedString::from("‹ SSH 快速连接")),
                )
                .child(
                    div()
                        .mx(px(22.))
                        .mb(px(12.))
                        .px(px(12.))
                        .py(px(10.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(if has_error {
                            cola(t.ansi.red, 0.50)
                        } else {
                            rgba(RIM)
                        })
                        .bg(rgba(INSET))
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(8.))
                        .child(icon("external", 15., ui.accent))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .min_w(px(0.))
                                .font_family(mono.clone())
                                .text_size(px(13.))
                                .when(!self.ssh_prompt_input.is_empty(), |d| {
                                    d.child(
                                        div().text_color(col(ui.foreground)).child(
                                            SharedString::from(self.ssh_prompt_input.clone()),
                                        ),
                                    )
                                })
                                .child(
                                    div()
                                        .text_color(col(ui.muted))
                                        .child(SharedString::from("▏")),
                                )
                                .when(self.ssh_prompt_input.is_empty(), |d| {
                                    d.child(
                                        div()
                                            .ml(px(2.))
                                            .text_color(col(ui.muted))
                                            .child(SharedString::from(placeholder)),
                                    )
                                }),
                        )
                        .child(div().flex_1())
                        .when_some(err_chip, |d, chip| d.child(chip))
                        .when(!has_error, |d| {
                            d.when_some(chips_row, |d, chips| d.child(chips))
                        }),
                )
                .child(list)
                .child(div().flex_1())
                .child(div().h(px(1.)).bg(rgba(0xffffff0d)))
                .child(
                    div()
                        .px(px(22.))
                        .py(px(12.))
                        .text_size(px(11.5))
                        .text_color(col(ui.muted))
                        .child(SharedString::from(footer)),
                )
                .when(rename_ime_active, |d| {
                    d.child(
                        canvas(
                            |_bounds, _window, _cx| {},
                            move |bounds, _state, window, cx| {
                                window.handle_input(
                                    &ime_focus,
                                    ElementInputHandler::new(bounds, ime_entity.clone()),
                                    cx,
                                );
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                }),
        )
    }

    fn ssh_row_chip(&self, label: &'static str, color: tn_config::Color) -> Div {
        div()
            .px(px(8.))
            .py(px(2.))
            .rounded(px(999.))
            .bg(cola(color, 0.12))
            .text_size(px(10.))
            .text_color(col(color))
            .child(SharedString::from(label))
    }

    fn ssh_row_chip_with_icon(
        &self,
        glyph: &'static str,
        label: &'static str,
        color: tn_config::Color,
    ) -> Div {
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(5.))
            .px(px(8.))
            .py(px(2.))
            .rounded(px(999.))
            .bg(cola(color, 0.12))
            .child(icon(glyph, 11., color))
            .child(
                div()
                    .text_size(px(10.))
                    .text_color(col(color))
                    .child(SharedString::from(label)),
            )
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
            // .tile.sel: dynamic agent color border + bg; 否则 rim 边 + 动态 agent color hover 提亮
            .when(is_sel, |d| {
                d.border_color(cola(accent, 0.4)).bg(cola(accent, 0.12))
            })
            .when(!is_sel, |d| {
                d.border_color(rgba(RIM))
                    .hover(|s| s.bg(cola(accent, 0.08)).border_color(cola(accent, 0.30)))
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _e, _w, cx| {
                    this.picker_sel = i;
                    this.activate_sel(cx);
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
                div()
                    .text_size(px(11.))
                    .text_color(col(ui.muted))
                    .child(SharedString::from(sub)),
            )
    }
}

impl EntityInputHandler for QuickTerminal {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        adjusted: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let units: Vec<u16> = self
            .ssh_rename_marked
            .as_deref()
            .unwrap_or("")
            .encode_utf16()
            .collect();
        let start = range.start.min(units.len());
        let end = range.end.min(units.len());
        *adjusted = Some(start..end);
        Some(String::from_utf16_lossy(&units[start..end]))
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        self.ssh_rename.as_ref()?;
        let end = self
            .ssh_rename_marked
            .as_deref()
            .map(|s| s.encode_utf16().count())
            .unwrap_or(0);
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        self.ssh_rename_marked
            .as_deref()
            .map(|s| 0..s.encode_utf16().count())
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ssh_rename_marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft) = self.ssh_rename.as_mut() {
            draft.name.push_str(text);
        }
        self.ssh_rename_marked = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        new_text: &str,
        _new_selected: Option<std::ops::Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.ssh_rename_marked = (!new_text.is_empty()).then(|| new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(Bounds {
            origin: gpui::point(
                element_bounds.origin.x + px(72.),
                element_bounds.origin.y + px(168.),
            ),
            size: gpui::size(px(260.), px(28.)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: gpui::Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

impl Render for QuickTerminal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Focus the launcher (if open) or the running terminal, now that the
        // element exists. gpui remembers element focus across the OS-level show,
        // so doing it on the notify-driven render is enough for keys to route.
        if self.pending_focus {
            self.pending_focus = false;
            if self.ssh_prompt_open {
                self.ssh_prompt_focus.focus(window);
            } else if self.picker_open {
                self.picker_focus.focus(window);
            } else if let Some(t) = &self.term {
                let fh = t.read(cx).focus_handle();
                fh.focus(window);
            }
        }

        // Disable IME while editing the SSH target (ASCII user/host/port), but let
        // it work in rename mode so Chinese nicknames can be committed normally.
        let ssh_should_disable_ime =
            self.visible && self.ssh_prompt_open && self.ssh_rename.is_none();
        if ssh_should_disable_ime != self.ssh_ime_disabled {
            if let Some(hwnd) = self.hwnd.or_else(|| crate::platform::hwnd_of(window)) {
                crate::platform::set_ime_enabled(hwnd, !ssh_should_disable_ime);
            }
            self.ssh_ime_disabled = ssh_should_disable_ime;
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
                let p =
                    (at.elapsed().as_secs_f32() / (TRANSITION_MS as f32 / 1000.0)).clamp(0.0, 1.0);
                let a = (1.0 - ease_out_cubic(p)) * 0.96;
                if a > 0.004 {
                    root = root.child(
                        div()
                            .absolute()
                            .size_full()
                            .bg(cola(theme.terminal.background, a)),
                    );
                }
            }
        }
        if let Some(picker) = self.render_picker(cx) {
            root = root.child(picker);
        }
        if let Some(ssh_prompt) = self.render_ssh_prompt(cx) {
            root = root.child(ssh_prompt);
        }
        root
    }
}
