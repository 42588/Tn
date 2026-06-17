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
use tn_config::{ease_out_cubic, ease_out_back, ease_in_back, lerp_rect, Loaded, Rect};

use crate::local_dir_picker::{
    read_local_dirs, windows_virtual_root, LocalDirAction, LocalDirFocus, LocalDirPicker,
    WorkdirRecents,
};
use crate::platform;
use crate::ssh_recents::{AuthBadge, SshRecents};
use crate::style::{col, cola, icon, R_CARD, UI_SANS};
use crate::terminal_view::{
    FileNamespace, LaunchSpec, ProcessExited, SshCloseRequested, SshConnected, SshRememberPassword,
    SshRetryRequested, TerminalView, UsageUpdated,
};
use crate::welcome::{
    launch_entries, profile_card, ssh_card, wsl_card, wsl_distros, CardId, LaunchEntry,
};

/// Launcher → session cross-fade duration (待优化:手感真机调).
const TRANSITION_MS: u64 = 190;

/// Launcher card width in **logical** px: matches the 760px mockup card. Scaled to physical
/// by the monitor DPI when sizing the window (see [`QuickTerminal::shown_for`]).
const CARD_W: f32 = 760.0;
const LOCAL_DIR_CARD_H: f32 = 500.0;
const LOCAL_DIR_RECENTS_H: f32 = 158.0;
const LOCAL_DIR_LIST_H: f32 = 176.0;

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
    /// In-place second level shown after activating an agent tile.
    local_dir_picker: Option<LocalDirPicker>,
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
    slide_progress: Option<f32>,
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
            local_dir_picker: None,
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
            slide_progress: None,
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

    /// 启动器默认选中位:第一枚 **shell** 磁贴(SHEET 04 板 B「pwsh · 默认
    /// PROFILE」)。agents 行排在上方但不抢默认位(差异总结 4-5);没有 shell
    /// 行时退回 0。
    fn default_picker_sel(&self) -> usize {
        self.picker_items()
            .iter()
            .position(|it| match it {
                PickerItem::Launch(i) => {
                    !crate::welcome::is_agent_profile(&self.launch_profiles[*i])
                }
                PickerItem::Pwsh | PickerItem::DrillWsl | PickerItem::SshPrompt => true,
            })
            .unwrap_or(0)
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
            self.picker_sel = self.default_picker_sel(); // pwsh 默认 PROFILE(SHEET 04)
            self.wsl_drill = false;
            self.local_dir_picker = None;
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
            PickerItem::Launch(i) if crate::welcome::is_agent_profile(&self.launch_profiles[i]) => {
                self.open_agent_dir_picker(i, cx)
            }
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
        self.local_dir_picker = None;
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

    fn open_agent_dir_picker(&mut self, idx: usize, cx: &mut Context<Self>) {
        let Some(profile) = self.launch_profiles.get(idx) else {
            return;
        };
        let recents = WorkdirRecents::load().sorted_with_seed(None);
        let initial = recents
            .first()
            .map(|r| r.path.clone())
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(windows_virtual_root);
        let mut picker = LocalDirPicker::new(idx, profile.name.clone(), initial, recents);
        self.load_local_dir_picker_dirs(&mut picker);
        self.local_dir_picker = Some(picker);
        self.picker_open = true;
        self.wsl_drill = false;
        self.ssh_prompt_open = false;
        self.ssh_rename = None;
        self.ssh_rename_marked = None;
        self.pending_focus = true;
        self.resnap(cx);
        cx.notify();
    }

    fn load_local_dir_picker_dirs(&mut self, picker: &mut LocalDirPicker) {
        match read_local_dirs(&picker.current) {
            Ok(dirs) => picker.apply_dirs(dirs),
            Err(e) => {
                tracing::warn!(path = %picker.current.display(), error = %e, "read local workdir failed");
                picker.apply_dirs(Vec::new());
            }
        }
    }

    fn refresh_local_dir_picker(&mut self, cx: &mut Context<Self>) {
        if let Some(mut picker) = self.local_dir_picker.take() {
            self.load_local_dir_picker_dirs(&mut picker);
            self.local_dir_picker = Some(picker);
            self.resnap(cx);
            cx.notify();
        }
    }

    fn close_local_dir_picker_to_launcher(&mut self, cx: &mut Context<Self>) {
        self.local_dir_picker = None;
        self.picker_open = true;
        self.wsl_drill = false;
        self.pending_focus = true;
        self.resnap(cx);
        cx.notify();
    }

    fn confirm_local_dir_picker(&mut self, cx: &mut Context<Self>) {
        let Some(picker) = self.local_dir_picker.take() else {
            return;
        };
        let cwd = picker.launch_cwd();
        let mut recents = WorkdirRecents::load();
        recents.record(cwd.clone());
        recents.save();
        self.launch_profile_with_cwd(picker.agent_index, cwd, cx);
    }

    fn browse_local_dir_picker(&mut self, cx: &mut Context<Self>) {
        let recv = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn(
            async move |this: gpui::WeakEntity<Self>, cx: &mut gpui::AsyncApp| {
                if let Ok(Ok(Some(paths))) = recv.await {
                    if let Some(path) = paths.into_iter().next() {
                        let _ = this.update(cx, |this, cx| {
                            if let Some(mut picker) = this.local_dir_picker.take() {
                                picker.current = path.clone();
                                picker.selected = path;
                                picker.focus = LocalDirFocus::Directories;
                                picker.dir_sel = 0;
                                this.load_local_dir_picker_dirs(&mut picker);
                                this.local_dir_picker = Some(picker);
                                this.resnap(cx);
                                cx.notify();
                            }
                        });
                    }
                }
            },
        )
        .detach();
    }

    fn close_ssh_prompt_to_picker(&mut self, cx: &mut Context<Self>) {
        self.ssh_prompt_open = false;
        self.picker_open = true;
        self.picker_sel = 0;
        self.wsl_drill = false;
        self.local_dir_picker = None;
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

    fn launch_profile_with_cwd(
        &mut self,
        idx: usize,
        cwd: std::path::PathBuf,
        cx: &mut Context<Self>,
    ) {
        let reg = crate::agent_host::agent_registry(cx);
        let spec = self
            .launch_profiles
            .get(idx)
            .and_then(|p| LaunchSpec::from_profile_ephemeral(p, &reg))
            .map(|spec| spec.with_cwd(cwd))
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
        let term = cx.new(|cx| {
            let mut t = TerminalView::new(cx, config, spec);
            t.set_ghost_chrome(true); // 幽灵窗自带 GHOST_ 头,抑制 shell 板头
            t
        });
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
            this.local_dir_picker = None;
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
                this.local_dir_picker = None;
                this.ssh_rename = None;
                this.ssh_rename_marked = None;
                this.picker_sel = this.default_picker_sel();
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
        self.local_dir_picker = None;
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
        if self.local_dir_picker.is_some() {
            self.on_local_dir_picker_key(ev, cx);
            return;
        }
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

    fn on_local_dir_picker_key(&mut self, ev: &KeyDownEvent, cx: &mut Context<Self>) {
        cx.stop_propagation();
        match ev.keystroke.key.as_str() {
            "escape" => self.close_local_dir_picker_to_launcher(cx),
            "tab" => {
                if let Some(picker) = self.local_dir_picker.as_mut() {
                    if ev.keystroke.modifiers.shift {
                        picker.focus_prev();
                    } else {
                        picker.focus_next();
                    }
                    cx.notify();
                }
            }
            "up" => {
                if let Some(picker) = self.local_dir_picker.as_mut() {
                    picker.move_selection(-1);
                    cx.notify();
                }
            }
            "down" => {
                if let Some(picker) = self.local_dir_picker.as_mut() {
                    picker.move_selection(1);
                    cx.notify();
                }
            }
            "left" => {
                if self
                    .local_dir_picker
                    .as_mut()
                    .and_then(LocalDirPicker::go_focused_parent)
                    .is_some()
                {
                    self.refresh_local_dir_picker(cx);
                }
            }
            "right" => {
                let action = self
                    .local_dir_picker
                    .as_mut()
                    .and_then(LocalDirPicker::open_focused_for_navigation);
                match action {
                    Some(LocalDirAction::Open(_)) => self.refresh_local_dir_picker(cx),
                    Some(LocalDirAction::Browse) => self.browse_local_dir_picker(cx),
                    None => {}
                }
            }
            "enter" => self.confirm_local_dir_picker(cx),
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
                env: Vec::new(),
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
        if self.local_dir_picker.is_some() {
            return LOCAL_DIR_CARD_H;
        }
        // Sum each logical row's wrapped height (5 tiles per visual row, SHEET 04).
        let rows = self
            .picker_rows()
            .iter()
            .map(|r| r.len().max(1).div_ceil(5))
            .sum::<usize>()
            .max(1) as f32;
        // 顶缘磷光 2 + GHOST 头 38 + 页脚 30 + tiles padding 24(~100)
        // + 每行磁贴 (~92) + 8px 行间隙。残影已收编为卡内刻线(P0-2),窗高
        // 即卡高 —— 不再为窗外残影留透明下沉区。Deliberately generous so the
        // footer never clips; any surplus is absorbed by the `flex_1` body spacer.
        100.0 + rows * 92.0 + (rows - 1.0) * 8.0
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
        let s = scale.max(1.0);
        // 幽灵窗在两态都是同一「760 顶垂窗」(SHEET 04 — 上直角下圆角)。
        // 宽固定 Ghost 卡宽、水平居中、顶贴屏;运行态不按屏宽拉伸成横条
        //(原型与真机差异总结 P0:运行态比例/外部背景)。
        let w = (CARD_W * s).min(work.width * 0.94);
        let x = work.x + (work.width - w) / 2.0;
        let h = if self.term.is_none() {
            // 启动器:卡片自身高度(磁贴行数撑出)。
            (self.card_height() * s).min(work.height * 0.94)
        } else {
            // 运行态:沿用配置的下拉高度,只把宽度收回 Ghost 规格。
            qt.shown_rect(work).height.min(work.height * 0.94)
        };
        Rect::new(x, work.y, w, h)
    }

    /// Off-screen rect matching [`shown_for`] — same size, pushed above the top edge.
    fn hidden_for(&self, work: Rect, scale: f32) -> Rect {
        // 两态统一从屏顶滑入/滑出(幽灵顶垂语义),隐藏位 = 同尺寸推到屏顶之上。
        let s = self.shown_for(work, scale);
        Rect::new(s.x, work.y - s.height, s.width, s.height)
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
                // 顶垂形窗形:窗体不透明(P0-2,Windows 无真透明),用 Win32
                // region 裁出「上直角下圆角」,圆角外不留任何可见 surface。
                let _ = rounded;
                platform::set_ghost_region(
                    h,
                    rect.width,
                    rect.height,
                    crate::style::R_WINDOW * scale,
                );
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
                    // 顶垂形 region(上直角下圆角);hidden/shown 同尺寸,滑动中
                    // region 始终贴合(P0-2:不透明窗 + region,零白区)。
                    let _ = rounded;
                    platform::set_ghost_region(
                        h,
                        hidden.width,
                        hidden.height,
                        crate::style::R_WINDOW * scale,
                    );
                    platform::show(h, true);
                    let _ = this.update(cx, |_, cx| cx.notify()); // render -> consume focus
                }
                let dur = Duration::from_millis(anim_ms);
                let start = Instant::now();
                loop {
                    let mut is_anim_active = false;
                    let mut progress = 1.0;
                    let ok = this
                        .update(cx, |v, cx| {
                            if v.anim_token != token {
                                return false;
                            }
                            let elapsed = start.elapsed();
                            progress = if dur.is_zero() {
                                1.0
                            } else {
                                (elapsed.as_secs_f32() / dur.as_secs_f32()).clamp(0.0, 1.0)
                            };
                            v.slide_progress = Some(progress);
                            cx.notify();
                            is_anim_active = !dur.is_zero() && elapsed < dur;
                            true
                        })
                        .unwrap_or(false);
                    if !ok {
                        return;
                    }
                    let eased = if reveal {
                        ease_out_back(progress)
                    } else {
                        1.0 - ease_in_back(progress)
                    };
                    platform::set_bounds(h, lerp_rect(hidden, shown, eased));
                    if !is_anim_active {
                        let _ = this.update(cx, |v, cx| {
                            v.slide_progress = None;
                            cx.notify();
                        });
                        if !reveal {
                            platform::show(h, false);
                        }
                        return;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(8))
                        .await;
                }
            },
        )
        .detach();
        cx.notify();
    }

    /// 像素幽灵标(SHEET 04 `.gmark`):16×15 圆顶方底磷光块 + 双墨眼 — 与宠物
    /// 像素语言同源。
    fn ghost_mark(&self) -> Div {
        let eye = || {
            div()
                .absolute()
                .top(px(5.))
                .w(px(3.))
                .h(px(3.))
                .bg(gpui::rgb(crate::style::PH_INK))
        };
        div()
            .w(px(16.))
            .h(px(15.))
            .flex_none()
            .relative()
            .rounded_t(px(7.))
            .rounded_b(px(2.))
            .bg(gpui::rgb(crate::style::PH))
            .child(eye().left(px(3.)))
            .child(eye().right(px(3.)))
    }

    /// 幽灵窗外壳(SHEET 04):上直角下圆角(顶垂)· L1 · 1px h2 边(无顶边)·
    /// 顶缘 2px 磷光线(中央峰、两端渐隐)。
    ///
    /// 残影签名(P0-2,差异总结 §6):gpui 0.2 在 Windows 上的 Transparent 是
    /// SetWindowCompositionAttribute 的 accent 渐变,**不是逐像素透明** —— 窗外
    /// 残影区曾渲染成纯白「材质」,隐藏滑出时整块暴露。兜底方案:窗高收回卡高
    /// (零未绘制区),残影改为**卡内刻线** —— 底缘上方 1px ph-dim 圆角弧线
    /// (启动器 1 道 / 运行态 2 道,·45/·18 与原型同档);真透明可达后再外移。
    fn ghost_frame(&self, inner: Div) -> Div {
        let progress = self.slide_progress.unwrap_or(1.0);
        let left_w = 0.5 - 0.5 * progress;
        let right_w = 0.5 - 0.5 * progress;

        // 卡内残影弧:贴底一条 R_WINDOW 高的圆角描边带(底线 + 两角弧 + 角旁
        // 短侧线),只描不填,读作幽灵身后的轮廓残像。
        let echo_arc = |lift: f32, inset: f32, alpha: u32| {
            div()
                .absolute()
                .left(px(inset))
                .right(px(inset))
                .bottom(px(lift))
                .h(px(crate::style::R_WINDOW))
                .rounded_b(px(crate::style::R_WINDOW))
                .border_b(px(1.))
                .border_l(px(1.))
                .border_r(px(1.))
                .border_color(rgba((crate::style::PH << 8) | alpha))
        };
        // 顶缘磷光:两段镜像渐变在中点会合;绝对定位 + 中心 2px 负边距重叠,
        // 堵掉 flex 像素取整在正中漏出的 1px 缺口(差异总结 4-2)。渐变停靠
        // 0.4 起坡 = 原型「两端 20% 全透平台」按半段折算。
        let top_edge = div()
            .h(px(2.))
            .flex_none()
            .relative()
            .child(
                div()
                    .absolute()
                    .top(px(0.))
                    .bottom(px(0.))
                    .left(gpui::relative(left_w))
                    .right(gpui::relative(0.5))
                    .mr(px(-2.))
                    .bg(linear_gradient(
                        90.,
                        linear_color_stop(rgba(0x5BE7C400), 0.4),
                        linear_color_stop(gpui::rgb(crate::style::PH), 1.),
                    )),
            )
            .child(
                div()
                    .absolute()
                    .top(px(0.))
                    .bottom(px(0.))
                    .left(gpui::relative(0.5))
                    .right(gpui::relative(right_w))
                    .ml(px(-2.))
                    .bg(linear_gradient(
                        90.,
                        linear_color_stop(gpui::rgb(crate::style::PH), 0.),
                        linear_color_stop(rgba(0x5BE7C400), 0.6),
                    )),
            );
        let session_alive = self.term.is_some();
        div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .rounded_b(px(crate::style::R_WINDOW))
            .overflow_hidden()
            .border_b(px(1.))
            .border_l(px(1.))
            .border_r(px(1.))
            .border_color(rgba(crate::style::H2))
            .bg(gpui::rgb(crate::style::L1)) // 不透明 L1(契约 1)
            .child(top_edge)
            .child(inner)
            // 残影刻线压在页脚区上层(SHEET 04:启动器 1 道,运行态 2 道)
            .child(echo_arc(3., 10., 0x24)) // ph ·14%(= ph-dim × .45)
            .when(session_alive, |d| d.child(echo_arc(7., 20., 0x10))) // ph ·6%
    }

    /// The launcher card(SHEET 04 板 B):垂下启动器 — GHOST_ 头 + 磁贴 + 页脚。
    /// Tiles come from [`picker_items`] — the aggregated root (profiles + WSL card
    /// + SSH placeholder), or, when drilled, a Back tile + the WSL distros.
    /// `None` when closed.
    fn render_picker(&self, cx: &mut Context<Self>) -> Option<Div> {
        if !self.picker_open {
            return None;
        }
        if let Some(picker) = self.local_dir_picker.as_ref() {
            return Some(self.render_local_dir_picker(cx, picker));
        }
        let t = &self.config.theme;
        let ui = &t.ui;
        let rows = self.picker_rows();
        let total: usize = rows.iter().map(|r| r.len()).sum();
        let sel = self.picker_sel.min(total.saturating_sub(1));
        // SHEET 04 `.tiles`:同一张牌桌 — 全部磁贴入一个流式网格(左起、gap 8、
        // 填满卡宽),不再按 agents/others 分行居中。flat 索引与 `picker_items` 对齐。
        let mut tiles: Vec<Div> = Vec::new();
        let mut flat = 0usize;
        for row in &rows {
            for item in row {
                let i = flat;
                flat += 1;
                let c = self.item_card(item, cx);
                tiles.push(self.launcher_tile(i, i == sel, c.name, c.sub, c.glyph, c.accent, cx));
            }
        }
        let row_divs: Vec<Div> = vec![div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap(px(8.))
            .children(tiles)];

        let _ = ui;
        let mono = SharedString::from(self.config.font().family.clone());
        let head_sub = if self.wsl_drill {
            "‹ 选择 WSL 发行版"
        } else {
            "幽灵终端"
        };

        // GHOST_ 头(SHEET 04):gmark + GHOST_ + 副标 + AUTOHIDE chip — L2 抬升。
        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .h(px(38.))
            .px(px(14.))
            .flex_none()
            .bg(gpui::rgb(crate::style::L2))
            .border_b(px(1.))
            .border_color(rgba(crate::style::H1))
            .font_family(mono.clone())
            .child(self.ghost_mark())
            .child(
                div()
                    .flex()
                    .flex_row()
                    .text_size(px(crate::style::FS_CAPTION))
                    .font_weight(FontWeight(600.))
                    .child(div().text_color(gpui::rgb(crate::style::T0)).child("GHOST"))
                    .child(div().text_color(gpui::rgb(crate::style::PH)).child("_")),
            )
            .child(
                div()
                    .text_size(px(crate::style::FS_MICRO))
                    .text_color(gpui::rgb(crate::style::T2))
                    .when(self.wsl_drill, |d| {
                        d.hover(|s| s.text_color(gpui::rgb(crate::style::T0)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _e, _w, cx| {
                                    this.wsl_drill = false;
                                    this.picker_sel = 0;
                                    this.resnap(cx);
                                    cx.notify();
                                }),
                            )
                    })
                    .child(SharedString::from(head_sub)),
            )
            .child(div().flex_1())
            .child(
                div()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(crate::style::R_CHIP))
                    .border_1()
                    .border_color(rgba(crate::style::H1))
                    .text_size(px(crate::style::FS_MICRO))
                    .text_color(gpui::rgb(crate::style::T1))
                    .child("AUTOHIDE · ON"),
            );

        // float-foot:kbd 提示 + 「会话常驻」tag。
        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(12.))
            .h(px(30.))
            .px(px(14.))
            .flex_none()
            .border_t(px(1.))
            .border_color(rgba(crate::style::H1))
            .font_family(mono.clone())
            .text_size(px(crate::style::FS_MICRO))
            .text_color(gpui::rgb(crate::style::T2))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.))
                    .child(crate::style::kbd("⇥", mono.clone()))
                    .child(div().child("选择")),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.))
                    .child(crate::style::kbd("↵", mono.clone()))
                    .child(div().child("启动")),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(5.))
                    .child(crate::style::kbd("Esc", mono.clone()))
                    .child(div().child("隐匿")),
            )
            .child(div().flex_1())
            .child(div().child(if self.wsl_drill {
                "WSL"
            } else {
                "会话常驻 · 隐而不灭"
            }));

        let inner = div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(UI_SANS)
            .track_focus(&self.picker_focus)
            .on_key_down(
                cx.listener(|this, ev: &KeyDownEvent, w, cx| this.on_picker_key(ev, w, cx)),
            )
            .child(header)
            .child(
                div()
                    .p(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .children(row_divs),
            )
            .child(div().flex_1()) // body 留白:吸收窗口高度余量,页脚贴底
            .child(footer);

        Some(self.ghost_frame(inner))
    }

    fn render_local_dir_picker(&self, cx: &mut Context<Self>, picker: &LocalDirPicker) -> Div {
        let t = &self.config.theme;
        let ui = &t.ui;
        let mono = SharedString::from(self.config.font().family.clone());
        let selected = picker.launch_cwd().display().to_string();
        let current = picker.current_label();
        let focused = picker.focus;

        let section_label = |label: &'static str, active: bool| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(7.))
                .h(px(24.))
                .text_size(px(crate::style::FS_MICRO))
                .font_family(mono.clone())
                .font_weight(FontWeight(650.))
                .text_color(if active {
                    gpui::rgb(crate::style::T0)
                } else {
                    gpui::rgb(crate::style::T2)
                })
                .child(div().w(px(5.)).h(px(5.)).rounded(px(1.)).bg(if active {
                    gpui::rgb(crate::style::PH)
                } else {
                    gpui::rgb(crate::style::T3)
                }))
                .child(label)
        };

        let recent_rows = if picker.recents.is_empty() {
            div()
                .flex()
                .items_center()
                .h(px(LOCAL_DIR_RECENTS_H))
                .overflow_hidden()
                .px(px(11.))
                .text_size(px(crate::style::FS_CAPTION))
                .text_color(gpui::rgb(crate::style::T2))
                .child("暂无最近工作目录")
        } else {
            let mut list = div().flex().flex_col().gap(px(3.));
            let start = if focused == LocalDirFocus::Recent {
                picker.recent_sel.saturating_sub(4)
            } else {
                0
            };
            for (offset, item) in picker.recents.iter().enumerate().skip(start).take(5) {
                let i = offset;
                let is_sel = focused == LocalDirFocus::Recent && picker.recent_sel == i;
                let path = item.path.clone();
                let label = item.label.clone();
                let sub = item.path.display().to_string();
                list = list.child(
                    div()
                        .relative()
                        .flex()
                        .flex_row()
                        .items_center()
                        .h(px(38.))
                        .gap(px(9.))
                        .px(px(11.))
                        .rounded(px(R_CARD))
                        .bg(if is_sel {
                            col(ui.palette_selected)
                        } else {
                            rgba(0x00000000)
                        })
                        .border_1()
                        .border_color(if is_sel {
                            rgba(crate::style::PH_DIM)
                        } else {
                            rgba(crate::style::H0)
                        })
                        .hover(|s| s.bg(gpui::rgb(crate::style::L2)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                if let Some(picker) = this.local_dir_picker.as_mut() {
                                    picker.focus = LocalDirFocus::Recent;
                                    picker.recent_sel = i;
                                    picker.current = path.clone();
                                    picker.selected = path.clone();
                                }
                                this.refresh_local_dir_picker(cx);
                            }),
                        )
                        .child(icon("folder", 14., ui.accent))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .flex()
                                .flex_col()
                                .gap(px(1.))
                                .child(
                                    div()
                                        .text_size(px(crate::style::FS_CAPTION))
                                        .font_weight(FontWeight(620.))
                                        .text_color(col(ui.foreground))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(SharedString::from(label)),
                                )
                                .child(
                                    div()
                                        .text_size(px(crate::style::FS_MICRO))
                                        .text_color(col(ui.muted))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(SharedString::from(sub)),
                                ),
                        ),
                );
            }
            list.h(px(LOCAL_DIR_RECENTS_H)).overflow_hidden()
        };

        let mut dir_rows = div()
            .h(px(LOCAL_DIR_LIST_H))
            .overflow_hidden()
            .flex()
            .flex_col()
            .gap(px(3.));
        if picker.dirs.is_empty() {
            dir_rows = dir_rows.child(
                div()
                    .flex()
                    .items_center()
                    .h(px(46.))
                    .px(px(11.))
                    .text_size(px(crate::style::FS_CAPTION))
                    .text_color(gpui::rgb(crate::style::T2))
                    .child("没有可进入的子目录"),
            );
        } else {
            let start = if focused == LocalDirFocus::Directories {
                picker.dir_sel.saturating_sub(6)
            } else {
                0
            };
            for (offset, item) in picker.dirs.iter().enumerate().skip(start).take(7) {
                let i = offset;
                let is_sel = focused == LocalDirFocus::Directories && picker.dir_sel == i;
                let path = item.path.clone();
                let name = item.name.clone();
                let is_git = item.is_git;
                let is_drive = item.is_drive;
                dir_rows = dir_rows.child(
                    div()
                        .relative()
                        .flex()
                        .flex_row()
                        .items_center()
                        .h(px(34.))
                        .gap(px(9.))
                        .px(px(11.))
                        .rounded(px(R_CARD))
                        .bg(if is_sel {
                            col(ui.palette_selected)
                        } else {
                            rgba(0x00000000)
                        })
                        .border_1()
                        .border_color(if is_sel {
                            rgba(crate::style::PH_DIM)
                        } else {
                            rgba(crate::style::H0)
                        })
                        .hover(|s| s.bg(gpui::rgb(crate::style::L2)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _e, _w, cx| {
                                if let Some(picker) = this.local_dir_picker.as_mut() {
                                    picker.focus = LocalDirFocus::Directories;
                                    picker.dir_sel = i;
                                    picker.current = path.clone();
                                    picker.selected = path.clone();
                                }
                                this.refresh_local_dir_picker(cx);
                            }),
                        )
                        .child(icon("folder", 14., ui.accent_alt))
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(0.))
                                .text_size(px(crate::style::FS_CAPTION))
                                .font_weight(FontWeight(600.))
                                .text_color(col(ui.foreground))
                                .overflow_hidden()
                                .text_ellipsis()
                                .child(SharedString::from(name)),
                        )
                        .when(is_git, |d| {
                            d.child(
                                div()
                                    .px(px(7.))
                                    .py(px(1.))
                                    .rounded(px(crate::style::R_CHIP))
                                    .border_1()
                                    .border_color(cola(t.ansi.yellow, 0.35))
                                    .text_size(px(crate::style::FS_MICRO))
                                    .text_color(col(t.ansi.yellow))
                                    .child("git"),
                            )
                        })
                        .when(is_drive, |d| {
                            d.child(
                                div()
                                    .px(px(7.))
                                    .py(px(1.))
                                    .rounded(px(crate::style::R_CHIP))
                                    .border_1()
                                    .border_color(cola(ui.accent_alt, 0.35))
                                    .text_size(px(crate::style::FS_MICRO))
                                    .text_color(col(ui.accent_alt))
                                    .child("盘符"),
                            )
                        }),
                );
            }
        }

        let browse_active = focused == LocalDirFocus::Browse;
        let browse = div()
            .relative()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .h(px(44.))
            .px(px(12.))
            .rounded(px(R_CARD))
            .border_1()
            .border_color(if browse_active {
                rgba(crate::style::PH_DIM)
            } else {
                rgba(crate::style::H1)
            })
            .bg(if browse_active {
                col(ui.palette_selected)
            } else {
                gpui::rgb(crate::style::L1)
            })
            .hover(|s| s.bg(gpui::rgb(crate::style::L2)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    if let Some(picker) = this.local_dir_picker.as_mut() {
                        picker.focus = LocalDirFocus::Browse;
                    }
                    this.browse_local_dir_picker(cx);
                }),
            )
            .child(icon("external", 14., ui.accent))
            .child(
                div()
                    .flex_1()
                    .text_size(px(crate::style::FS_CAPTION))
                    .font_weight(FontWeight(620.))
                    .text_color(col(ui.foreground))
                    .child("浏览本地文件夹"),
            )
            .child(
                div()
                    .text_size(px(crate::style::FS_MICRO))
                    .text_color(col(ui.muted))
                    .child("→"),
            );

        let header = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .h(px(38.))
            .px(px(14.))
            .flex_none()
            .bg(gpui::rgb(crate::style::L2))
            .border_b(px(1.))
            .border_color(rgba(crate::style::H1))
            .font_family(mono.clone())
            .child(self.ghost_mark())
            .child(
                div()
                    .text_size(px(crate::style::FS_CAPTION))
                    .font_weight(FontWeight(650.))
                    .text_color(gpui::rgb(crate::style::T0))
                    .hover(|s| s.text_color(gpui::rgb(crate::style::PH)))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _e, _w, cx| this.close_local_dir_picker_to_launcher(cx)),
                    )
                    .child("GHOST_"),
            )
            .child(
                div()
                    .text_size(px(crate::style::FS_MICRO))
                    .text_color(gpui::rgb(crate::style::T2))
                    .child(SharedString::from(format!(
                        "{} 工作目录",
                        picker.agent_name
                    ))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .px(px(8.))
                    .py(px(2.))
                    .rounded(px(crate::style::R_CHIP))
                    .border_1()
                    .border_color(cola(ui.accent, 0.35))
                    .text_size(px(crate::style::FS_MICRO))
                    .text_color(col(ui.accent))
                    .child("AGENT"),
            );

        let footer = div()
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.))
            .h(px(34.))
            .px(px(14.))
            .flex_none()
            .border_t(px(1.))
            .border_color(rgba(crate::style::H1))
            .font_family(mono.clone())
            .text_size(px(crate::style::FS_MICRO))
            .text_color(gpui::rgb(crate::style::T2))
            .child(crate::style::kbd("Tab", mono.clone()))
            .child(div().child("焦点"))
            .child(crate::style::kbd("↑↓", mono.clone()))
            .child(div().child("选择"))
            .child(crate::style::kbd("←", mono.clone()))
            .child(div().child("上级"))
            .child(crate::style::kbd("→", mono.clone()))
            .child(div().child("进入"))
            .child(crate::style::kbd("Enter", mono.clone()))
            .child(div().child("启动"))
            .child(div().flex_1())
            .child(crate::style::btn_primary("启动").on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    this.confirm_local_dir_picker(cx);
                }),
            ));

        let inner = div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(UI_SANS)
            .track_focus(&self.picker_focus)
            .on_key_down(
                cx.listener(|this, ev: &KeyDownEvent, w, cx| this.on_picker_key(ev, w, cx)),
            )
            .child(header)
            .child(
                div()
                    .p(px(12.))
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .h(px(428.))
                    .child(
                        div()
                            .px(px(11.))
                            .py(px(9.))
                            .rounded(px(R_CARD))
                            .border_1()
                            .border_color(rgba(crate::style::H1))
                            .bg(gpui::rgb(crate::style::L0))
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .child(
                                div()
                                    .text_size(px(crate::style::FS_MICRO))
                                    .font_family(mono.clone())
                                    .text_color(gpui::rgb(crate::style::T2))
                                    .child("当前工作目录"),
                            )
                            .child(
                                div()
                                    .text_size(px(crate::style::FS_CAPTION))
                                    .text_color(col(ui.foreground))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(SharedString::from(selected)),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(10.))
                            .child(
                                div()
                                    .w(px(260.))
                                    .flex_none()
                                    .flex()
                                    .flex_col()
                                    .gap(px(5.))
                                    .child(section_label(
                                        "最近工作目录",
                                        focused == LocalDirFocus::Recent,
                                    ))
                                    .child(recent_rows),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w(px(0.))
                                    .flex()
                                    .flex_col()
                                    .gap(px(5.))
                                    .child(section_label(
                                        "当前目录",
                                        focused == LocalDirFocus::Directories,
                                    ))
                                    .child(
                                        div()
                                            .text_size(px(crate::style::FS_MICRO))
                                            .text_color(col(ui.muted))
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .child(SharedString::from(current)),
                                    )
                                    .child(dir_rows),
                            ),
                    )
                    .child(section_label("浏览", browse_active))
                    .child(browse),
            )
            .child(div().flex_1())
            .child(footer);

        self.ghost_frame(inner)
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
        // `.chip`:1px h1 · r3 · mono 10(磷光芯片)
        let chip = |label: &str, val: String| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(4.))
                .px(px(8.))
                .py(px(2.))
                .rounded(px(crate::style::R_CHIP))
                .border_1()
                .border_color(rgba(crate::style::H1))
                .font_family(mono.clone())
                .text_size(px(crate::style::FS_MICRO))
                .child(
                    div()
                        .text_color(gpui::rgb(crate::style::T2))
                        .child(SharedString::from(label.to_string())),
                )
                .child(
                    div()
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
                .rounded(px(crate::style::R_CHIP))
                .border_1()
                .border_color(cola(t.ansi.red, 0.3))
                .bg(rgba(crate::style::ERR_SOFT))
                .text_size(px(crate::style::FS_MICRO))
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
                    .text_size(px(crate::style::FS_CAPTION))
                    .text_color(col(ui.muted))
                    .child(SharedString::from(copy)),
            );
        } else {
            for (i, row) in rows.iter().enumerate() {
                let selected = i == sel;
                let connect_target = row.connect_target();
                // `.prow` 语法:选中 = L4 + ph-dim 边 + 左 2px 磷光脊(浮层家族)。
                let row_el = div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .px(px(11.))
                    .py(px(8.))
                    .rounded(px(R_CARD))
                    .relative()
                    .border_1()
                    .border_color(if selected {
                        rgba(crate::style::PH_DIM)
                    } else {
                        rgba(crate::style::H0)
                    })
                    .bg(if selected {
                        gpui::rgb(crate::style::L4)
                    } else {
                        gpui::rgb(crate::style::L2)
                    })
                    .when(selected, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left(px(-1.))
                                .top(px(8.))
                                .bottom(px(8.))
                                .w(px(2.))
                                .rounded(px(1.))
                                .bg(gpui::rgb(crate::style::PH)),
                        )
                    })
                    .when(!selected, |d| {
                        d.hover(|s| s.bg(gpui::rgb(crate::style::L4)))
                    })
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
                                        .text_size(px(crate::style::FS_BODY))
                                        .font_weight(FontWeight(640.))
                                        .text_color(col(ui.foreground))
                                        .child(SharedString::from(name.clone())),
                                )
                                .child(
                                    div()
                                        .text_size(px(crate::style::FS_MICRO))
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
                                            .text_size(px(crate::style::FS_BODY))
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
                                            .text_size(px(crate::style::FS_MICRO))
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
                                    .text_size(px(crate::style::FS_MICRO))
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
                                        .text_size(px(crate::style::FS_BODY))
                                        .font_weight(FontWeight(640.))
                                        .text_color(col(ui.foreground))
                                        .child(SharedString::from(alias.clone())),
                                )
                                .child(
                                    div()
                                        .text_size(px(crate::style::FS_MICRO))
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

        // float-foot 键帽化(与 workspace SSH 连接器同语法,差异总结 §7)。
        let khint = |k: &'static str, label: &'static str| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(5.))
                .child(crate::style::kbd(k, mono.clone()))
                .child(div().child(label))
        };
        let footer_hints: Vec<Div> = if self.ssh_rename.is_some() {
            vec![
                khint("↵", "保存名称"),
                khint("Esc", "取消"),
                div().child("支持中文输入"),
            ]
        } else if rows.is_empty() {
            vec![khint("↵", "连接"), khint("Esc", "返回启动器")]
        } else {
            vec![
                khint("↑↓", "选择"),
                khint("↵", "连接"),
                div().child("★ 收藏/取消收藏"),
                khint("Esc", "返回启动器"),
            ]
        };
        let ime_focus = self.ssh_prompt_focus.clone();
        let ime_entity = cx.entity();
        let rename_ime_active = self.ssh_rename.is_some();

        let inner = div()
            .size_full()
            .relative()
            .flex()
            .flex_col()
            .font_family(UI_SANS)
            .track_focus(&self.ssh_prompt_focus)
            .on_key_down(
                cx.listener(|this, ev: &KeyDownEvent, _w, cx| this.on_ssh_prompt_key(ev, cx)),
            )
            .child(
                // GHOST_ 头变体:‹ 返回 + SSH 快速连接
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .h(px(38.))
                    .px(px(14.))
                    .flex_none()
                    .bg(gpui::rgb(crate::style::L2))
                    .border_b(px(1.))
                    .border_color(rgba(crate::style::H1))
                    .font_family(mono.clone())
                    .child(self.ghost_mark())
                    .child(
                        div()
                            .text_size(px(crate::style::FS_CAPTION))
                            .font_weight(FontWeight(600.))
                            .text_color(gpui::rgb(crate::style::T0))
                            .hover(|s| s.text_color(gpui::rgb(crate::style::PH)))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _e, _w, cx| this.close_ssh_prompt_to_picker(cx)),
                            )
                            .child(SharedString::from("‹ SSH 快速连接")),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .px(px(8.))
                            .py(px(2.))
                            .rounded(px(crate::style::R_CHIP))
                            .border_1()
                            .border_color(cola(t.ansi.yellow, 0.3))
                            .text_size(px(crate::style::FS_MICRO))
                            .text_color(col(t.ansi.yellow))
                            .child("SSH"),
                    ),
            )
            .child(
                // 输入井:L0 凹井 + h1 边(error = 红边)
                div()
                    .mx(px(14.))
                    .mt(px(12.))
                    .mb(px(10.))
                    .px(px(12.))
                    .py(px(10.))
                    .rounded(px(R_CARD))
                    .border_1()
                    .border_color(if has_error {
                        cola(t.ansi.red, 0.50)
                    } else {
                        rgba(crate::style::H1)
                    })
                    .bg(gpui::rgb(crate::style::L0))
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
                            .text_size(px(crate::style::FS_BODY))
                            .when(!self.ssh_prompt_input.is_empty(), |d| {
                                d.child(
                                    div()
                                        .text_color(col(ui.foreground))
                                        .child(SharedString::from(self.ssh_prompt_input.clone())),
                                )
                            })
                            .child(
                                // 磷光块光标(`.cur`,浮层输入行统一块形;差异总结 §7)
                                div()
                                    .w(px(7.))
                                    .h(px(15.))
                                    .flex_none()
                                    .bg(gpui::rgb(crate::style::PH))
                                    .rounded(px(1.)),
                            )
                            .when(self.ssh_prompt_input.is_empty(), |d| {
                                d.child(
                                    div()
                                        .ml(px(4.))
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
            .child(
                // float-foot:mono 10 t2 · kbd 键帽
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(12.))
                    .h(px(30.))
                    .px(px(14.))
                    .flex_none()
                    .border_t(px(1.))
                    .border_color(rgba(crate::style::H1))
                    .font_family(mono.clone())
                    .text_size(px(crate::style::FS_MICRO))
                    .text_color(gpui::rgb(crate::style::T2))
                    .children(footer_hints),
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
            });
        Some(self.ghost_frame(inner))
    }

    /// 行尾来源 chip(`.chip`):1px 同色 ·30% 边 + r3。
    fn ssh_row_chip(&self, label: &'static str, color: tn_config::Color) -> Div {
        div()
            .px(px(8.))
            .py(px(2.))
            .rounded(px(crate::style::R_CHIP))
            .border_1()
            .border_color(cola(color, 0.3))
            .text_size(px(crate::style::FS_MICRO))
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
            .rounded(px(crate::style::R_CHIP))
            .border_1()
            .border_color(cola(color, 0.3))
            .child(icon(glyph, 11., color))
            .child(
                div()
                    .text_size(px(crate::style::FS_MICRO))
                    .text_color(col(color))
                    .child(SharedString::from(label)),
            )
    }

    /// One launcher tile — 共用 [`launch_tile_shape`](crate::welcome::launch_tile_shape)
    /// (140 宽:5 列定宽塞进 760 宽卡,5×140 + 4×8 + 2×12 ≈ 756,>5 换行),与欢迎页
    /// 同一 tile 家族;选中态(键盘游标)交给共享壳画 L4 + ph-dim 边 + 左 2px 磷光脊。
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
        let mono = SharedString::from(self.config.font().family.clone());
        let card = crate::welcome::CardId {
            name,
            sub,
            glyph,
            accent,
        };
        crate::welcome::launch_tile_shape(mono, &card, 140., is_sel).on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, _e, _w, cx| {
                this.picker_sel = i;
                this.activate_sel(cx);
            }),
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
        let ssh_should_disable_ime = self.visible
            && ((self.ssh_prompt_open && self.ssh_rename.is_none())
                || self.local_dir_picker.is_some());
        if ssh_should_disable_ime != self.ssh_ime_disabled {
            if let Some(hwnd) = self.hwnd.or_else(|| crate::platform::hwnd_of(window)) {
                crate::platform::set_ime_enabled(hwnd, !ssh_should_disable_ime);
            }
            self.ssh_ime_disabled = ssh_should_disable_ime;
        }

        let theme = &self.config.theme;
        let ui = &theme.ui;
        let mut root = div().size_full().overflow_hidden();

        // The live session fills the window (its own header shows the agent +
        // usage ring). The launcher overlays everything when open.
        let _ = ui;
        if let Some(term) = &self.term {
            let mono = SharedString::from(self.config.font().family.clone());
            let hotkey = self
                .config
                .config
                .quick_terminal
                .hotkey
                .to_ascii_uppercase()
                .replace('+', " + ");
            let mut session_body = div()
                .flex_1()
                .min_h(px(0.))
                .relative()
                .overflow_hidden()
                .child(term.clone());
            // Launcher → session cross-fade: a dark wash over the fresh terminal that
            // eases out, so the session develops in instead of snapping.
            if let Some(at) = self.transition_at {
                let p =
                    (at.elapsed().as_secs_f32() / (TRANSITION_MS as f32 / 1000.0)).clamp(0.0, 1.0);
                let a = (1.0 - ease_out_cubic(p)) * 0.96;
                if a > 0.004 {
                    session_body = session_body.child(
                        div()
                            .absolute()
                            .size_full()
                            .bg(cola(theme.terminal.background, a)),
                    );
                }
            }
            // SHEET 04 板 C:运行态幽灵头 —— gmark + GHOST_ + 磷光会话 chip +
            // 失焦提示 + 磷光点。shell 会话由本头标识(TerminalView 的板头被
            // ghost_chrome 抑制);agent 会话沿用自带 agent 头(用量环不可丢),
            // 不再叠加幽灵头。差异总结 4-3:运行态身份消失的修复。
            let ghost_head = (!term.read(cx).is_agent()).then(|| {
                let label = term.read(cx).tab_label();
                let title = term.read(cx).title();
                let chip_text = match title {
                    Some(t) if !t.is_empty() && t != label => format!("{label} · {t}"),
                    _ => label,
                };
                let autohide = self.config.config.quick_terminal.autohide;
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap(px(10.))
                    .h(px(38.))
                    .px(px(14.))
                    .flex_none()
                    .bg(gpui::rgb(crate::style::L2))
                    .border_b(px(1.))
                    .border_color(rgba(crate::style::H1))
                    .font_family(mono.clone())
                    .child(self.ghost_mark())
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .text_size(px(crate::style::FS_CAPTION))
                            .font_weight(FontWeight(600.))
                            .child(div().text_color(gpui::rgb(crate::style::T0)).child("GHOST"))
                            .child(div().text_color(gpui::rgb(crate::style::PH)).child("_")),
                    )
                    .child(
                        // 会话 chip(磷光语法):`pwsh · ~/cwd`
                        div()
                            .px(px(8.))
                            .py(px(2.))
                            .rounded(px(crate::style::R_CHIP))
                            .border_1()
                            .border_color(rgba(crate::style::PH_DIM))
                            .bg(rgba(crate::style::PH_SOFT))
                            .text_size(px(crate::style::FS_MICRO))
                            .text_color(gpui::rgb(crate::style::PH))
                            .max_w(px(420.))
                            .overflow_hidden()
                            .child(SharedString::from(chip_text)),
                    )
                    .child(div().flex_1())
                    .when(autohide, |d| {
                        d.child(
                            div()
                                .text_size(px(crate::style::FS_MICRO))
                                .text_color(gpui::rgb(crate::style::T2))
                                .child("失焦 → 上滑隐匿"),
                        )
                    })
                    .child(
                        div()
                            .w(px(5.))
                            .h(px(5.))
                            .rounded_full()
                            .bg(gpui::rgb(crate::style::PH)),
                    )
            });
            // 会话态 = 幽灵头(shell)+ 终端正文 + float-foot(Esc 隐匿 · 再召唤 ·
            // SESSION ALIVE 磷光 tag)。
            let session = div()
                .size_full()
                .flex()
                .flex_col()
                .bg(gpui::rgb(crate::style::L1)) // 不透明 L1 板面(契约 1)
                .when_some(ghost_head, |d, h| d.child(h))
                .child(session_body)
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .gap(px(12.))
                        .h(px(26.))
                        .px(px(14.))
                        .flex_none()
                        .border_t(px(1.))
                        .border_color(rgba(crate::style::H1))
                        .font_family(mono.clone())
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(gpui::rgb(crate::style::T2))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(5.))
                                .child(crate::style::kbd("Esc", mono.clone()))
                                .child(div().child("隐匿(会话保留)")),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(5.))
                                .child(crate::style::kbd(hotkey, mono.clone()))
                                .child(div().child("再召唤")),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .text_color(gpui::rgb(crate::style::PH))
                                .child("SESSION ALIVE"),
                        ),
                );
            // 幽灵窗外壳:顶垂 + 顶缘磷光 + 残影签名(SHEET 04 板 C)。
            root = root.child(self.ghost_frame(session));
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
