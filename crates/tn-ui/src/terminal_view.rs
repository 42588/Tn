//! Live terminal view: renders a `tn-core` [`Terminal`] driven by a `tn-pty`
//! ConPTY backend, with keyboard input routed back to the shell.
//!
//! Threading model:
//!   - A dedicated reader thread pumps PTY bytes into the shared [`Terminal`]
//!     and writes the engine's `PtyWrite` replies (DSR responses, etc.) back to
//!     the PTY — without this ConPTY stalls on startup.
//!   - The reader **pushes** a wake signal (coalesced via a `dirty` flag) down an
//!     unbounded channel; a GPUI foreground task awaits it and calls `notify()`.
//!     GPUI coalesces notifies to its vsync frame clock, so a burst of output
//!     paints once per frame and an idle terminal costs nothing (no poll).
//!   - DEC 2026 synchronized output (BSU/ESU) is handled inside the alacritty
//!     `vte` `Processor` (`StdSyncHandler`): the grid only mutates when an update
//!     completes or its timeout fires, so snapshots are always whole frames.

use std::cell::RefCell;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use futures::channel::mpsc;
use futures::StreamExt;
use gpui::{
    canvas, div, linear_color_stop, linear_gradient, prelude::*, px, rgba, AsyncApp, Bounds,
    ClipboardItem, Context, Div, FocusHandle, FontWeight, KeyDownEvent, MouseButton, MouseDownEvent,
    MouseMoveEvent, MouseUpEvent, Pixels, Point, Rgba, ScrollDelta, ScrollWheelEvent, SharedString,
    WeakEntity, Window,
};
use tn_ai::{AgentKind, AiUsage};
use tn_blocks::BlockModel;
use tn_config::Loaded;
use tn_core::{GridSize, Palette, Rgb, TermEvent, Terminal};
use tn_pty::{LocalPty, PtyBackend, PtySize, SpawnSpec};
use tn_shell::{Integration, ShellParser};

use crate::block_view;

/// Emitted when a pane's AI-usage readout changes, so the workspace status bar
/// (which renders the *focused* pane's usage) can repaint without re-rendering
/// on every terminal frame.
pub struct UsageUpdated;

/// Emitted once the pane's child process exits (detected via ConPTY `try_wait`,
/// since ConPTY doesn't reliably EOF the reader). The quick terminal listens for
/// this to fall back to its launcher when the hosted agent/shell exits.
pub struct ProcessExited;

/// Convert a tn-core RGB color to a GPUI color.
fn col(c: Rgb) -> Rgba {
    gpui::rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32)
}

/// An RGB color with explicit alpha (Calm Glass translucent fills).
fn cola(c: Rgb, a: f32) -> Rgba {
    Rgba { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a }
}

/// Map a config [`tn_config::Theme`]'s terminal-color subset into a
/// [`tn_core::Palette`]. `tn-config` stays free of `tn-core`, so the bridge
/// lives here in the GPUI layer.
pub(crate) fn palette_from(theme: &tn_config::Theme) -> Palette {
    let c = |x: tn_config::Color| Rgb::new(x.r, x.g, x.b);
    let a = &theme.ansi;
    let t = &theme.terminal;
    Palette {
        ansi: [
            c(a.black), c(a.red), c(a.green), c(a.yellow),
            c(a.blue), c(a.magenta), c(a.cyan), c(a.white),
            c(a.bright_black), c(a.bright_red), c(a.bright_green), c(a.bright_yellow),
            c(a.bright_blue), c(a.bright_magenta), c(a.bright_cyan), c(a.bright_white),
        ],
        fg: c(t.foreground),
        bg: c(t.background),
        cursor: c(t.cursor),
        selection_fg: c(t.selection_fg),
        selection_bg: c(t.selection_bg),
    }
}

/// How to launch a pane's process: program + args + whether to inject the pwsh
/// shell-integration script. Built from a `tn_config::Profile` (command-bearing
/// shell/agent profiles), or the default local PowerShell via [`LaunchSpec::pwsh`].
#[derive(Clone, Debug)]
pub struct LaunchSpec {
    pub program: String,
    pub args: Vec<String>,
    pub integrate_pwsh: bool,
    /// Which agent this pane hosts (launch-intent signal for per-pane usage).
    /// `None` for a plain shell — usage is then auto-detected by log freshness.
    pub agent: Option<AgentKind>,
}

impl LaunchSpec {
    /// Default local PowerShell pane, with OSC 133 shell integration.
    pub fn pwsh() -> Self {
        Self {
            program: "powershell.exe".into(),
            args: vec!["-NoLogo".into()],
            integrate_pwsh: true,
            agent: None,
        }
    }

    /// Derive from a config profile if it carries a command (shell + agent).
    /// WSL/SSH profiles (no command yet, M2) return `None`.
    ///
    /// Native pwsh runs directly (with integration). Any other command (Claude /
    /// Codex / scripts) is **hosted inside pwsh** via `-NoExit -Command "& '…'"`,
    /// because on Windows those are extensionless npm shims that `CreateProcessW`
    /// can't execute directly — pwsh resolves them via PATH + PATHEXT, and the
    /// shell survives the agent's exit (back to a prompt).
    pub fn from_profile(p: &tn_config::Profile) -> Option<Self> {
        Self::from_profile_inner(p, true)
    }

    /// Like [`from_profile`], but the pwsh hosting a non-pwsh agent omits
    /// `-NoExit`, so exiting the agent exits the PTY. The quick terminal uses
    /// this so "exit claude" returns to its launcher instead of leaving a
    /// lingering pwsh prompt under a stale agent header.
    pub fn from_profile_ephemeral(p: &tn_config::Profile) -> Option<Self> {
        Self::from_profile_inner(p, false)
    }

    fn from_profile_inner(p: &tn_config::Profile, persist: bool) -> Option<Self> {
        // WSL (M2): host the distro's login shell via `wsl.exe -d <distro>`.
        // ConPTY runs wsl.exe like any program, so no special backend is needed;
        // no pwsh integration (the distro runs bash/zsh). An empty/absent distro
        // launches WSL's default distro.
        if p.kind == tn_config::ProfileKind::Wsl {
            let mut args = Vec::new();
            if let Some(distro) = p.distro.as_deref().filter(|d| !d.is_empty()) {
                args.push("-d".to_string());
                args.push(distro.to_string());
            }
            return Some(Self {
                program: "wsl.exe".into(),
                args,
                integrate_pwsh: false,
                agent: None,
            });
        }
        let command = p.command.clone()?;
        // Agent identity: an explicit `agent = "..."` field wins, else infer from
        // the command (`claude` / `codex`). This is the launch-intent signal the
        // status bar reads, so a Codex pane never shows Claude's usage.
        let agent = p
            .agent
            .as_deref()
            .and_then(tn_ai::agent_kind_for_command)
            .or_else(|| tn_ai::agent_kind_for_command(&command));
        let lc = command.to_ascii_lowercase();
        if lc.contains("powershell") || lc.contains("pwsh") {
            let mut args = p.args.clone();
            if args.is_empty() {
                args.push("-NoLogo".into());
            }
            return Some(Self {
                program: command,
                args,
                integrate_pwsh: true,
                agent,
            });
        }
        // Host the command in pwsh (single-quote-escaped call operator). With
        // `persist` we keep `-NoExit` so the shell survives the agent's exit
        // (a prompt); without it, pwsh exits when the agent does.
        let mut invoke = format!("& '{}'", command.replace('\'', "''"));
        for a in &p.args {
            invoke.push_str(&format!(" '{}'", a.replace('\'', "''")));
        }
        let mut args = vec!["-NoLogo".to_string()];
        if persist {
            args.push("-NoExit".into());
        }
        args.push("-Command".into());
        args.push(invoke);
        Some(Self {
            program: "powershell.exe".into(),
            args,
            integrate_pwsh: false,
            agent,
        })
    }
}

const ROWS: usize = 34;
const COLS: usize = 110;

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct TerminalView {
    terminal: Arc<Mutex<Terminal>>,
    writer: SharedWriter,
    // Owns the ConPTY master + child; used for resize and kept alive.
    pty: Arc<Mutex<LocalPty>>,
    focus_handle: FocusHandle,
    size: GridSize,
    cell_width: f32,
    // Font, resolved from config once at construction.
    font_family: SharedString,
    font_size: f32,
    line_height: f32,
    // Latest OSC window title (OSC 0/2), captured off the reader thread. Kept
    // for future use (tooltips / meaningful program titles); tab labels use the
    // clean agent/shell name instead, since pwsh's title is the noisy exe path.
    #[allow(dead_code)]
    title: Arc<Mutex<Option<String>>>,
    // Screen-space bounds of the text content, captured each paint by a canvas
    // so mouse handlers can map pixels -> cells and resize fits the pane.
    content_bounds: Rc<RefCell<Bounds<Pixels>>>,
    // Warp-style command blocks, built from the shell-integration bypass.
    blocks: Arc<Mutex<BlockModel>>,
    // Live palette copy (for block-bar colors); kept in sync with the engine.
    palette: Palette,
    // True while a left-drag selection is in progress.
    selecting: bool,
    focused_once: bool,
    // AI usage for this pane (M4): the agent it hosts + its latest usage
    // snapshot, polled off-thread from the agent's session log.
    agent: Option<AgentKind>,
    usage: Option<AiUsage>,
    // Theme accents for the per-pane header (Claude coral / Codex teal / UI blue).
    claude_accent: Rgb,
    codex_accent: Rgb,
    ui_accent: Rgb,
    // Launch program (e.g. "powershell.exe") — for a clean shell label.
    program: String,
}

/// A clean shell name from a program path (`…\powershell.exe` → `pwsh`).
fn shell_name_of(program: &str) -> String {
    let base = program.rsplit(['/', '\\']).next().unwrap_or(program);
    let base = base
        .strip_suffix(".exe")
        .or_else(|| base.strip_suffix(".EXE"))
        .unwrap_or(base);
    match base.to_ascii_lowercase().as_str() {
        "powershell" | "pwsh" => "pwsh".to_string(),
        "cmd" => "cmd".to_string(),
        other if other.is_empty() => "shell".to_string(),
        other => other.to_string(),
    }
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>, launch: LaunchSpec) -> Self {
        let size = GridSize::new(ROWS, COLS);
        // Build the spawn spec from the launch profile, then inject the pwsh
        // OSC 133 shell-integration script (pwsh only) via -EncodedCommand — no
        // temp file, no echoed input. Bypassable with TN_NO_SHELL_INTEGRATION.
        let mut spec = SpawnSpec::program(&launch.program);
        for a in &launch.args {
            spec = spec.arg(a);
        }
        if launch.integrate_pwsh && std::env::var("TN_NO_SHELL_INTEGRATION").is_err() {
            spec = spec
                .arg("-NoExit")
                .arg("-EncodedCommand")
                .arg(Integration::new().encoded_command());
        }
        // A bad profile command must NOT crash the app: pane construction runs
        // inside GPUI's window callback (non-unwinding), so a spawn panic aborts
        // the whole process. Fall back to a plain pwsh instead.
        let pty_size = PtySize::new(size.rows as u16, size.cols as u16);
        let mut pty = LocalPty::spawn(&spec, pty_size).unwrap_or_else(|e| {
            tracing::error!(program = %launch.program, "spawn failed: {e}; falling back to pwsh");
            LocalPty::spawn(&SpawnSpec::program("powershell.exe").arg("-NoLogo"), pty_size)
                .expect("fallback pwsh spawn failed")
        });
        let reader = pty.take_reader().expect("pty reader");
        let writer: SharedWriter = Arc::new(Mutex::new(pty.writer().expect("pty writer")));
        let pty = Arc::new(Mutex::new(pty));

        // Build the engine with the configured scrollback + theme palette.
        let palette = palette_from(&config.theme);
        let to_rgb = |c: tn_config::Color| Rgb::new(c.r, c.g, c.b);
        let claude_accent = to_rgb(config.theme.agents.claude);
        let codex_accent = to_rgb(config.theme.agents.codex);
        let ui_accent = to_rgb(config.theme.ui.accent);
        let mut term = Terminal::with_scrollback(size, config.config.general.scrollback_lines);
        term.set_palette(palette);
        let terminal = Arc::new(Mutex::new(term));
        let blocks = Arc::new(Mutex::new(BlockModel::new()));
        // Starts false: the first read's false->true transition sends the first
        // wake. GPUI still paints the initial (empty) frame when the window opens.
        let dirty = Arc::new(AtomicBool::new(false));
        // Reader -> foreground wake channel. `dirty` dedupes so at most one wake
        // is in flight; the foreground drains it and notifies once per frame.
        let (wake_tx, wake_rx) = mpsc::unbounded::<()>();
        let title = Arc::new(Mutex::new(None));

        Self::spawn_reader(
            reader,
            terminal.clone(),
            writer.clone(),
            dirty.clone(),
            wake_tx,
            title.clone(),
            blocks.clone(),
        );
        Self::spawn_repaint_loop(cx, dirty.clone(), wake_rx);
        // Watch the child so a pane (esp. the quick terminal) can react to its
        // shell/agent exiting. Harmless for the main window (no subscriber).
        Self::spawn_exit_watcher(cx, pty.clone());

        // Per-pane AI usage poller — ONLY for a pane launched AS an agent (launch
        // intent). A plain shell must not masquerade as Claude/Codex just because
        // a fresh agent session exists for this cwd: that agent is often a
        // *separate* process (e.g. the dev's own Claude Code editing this repo).
        // So a plain pwsh pane stays a shell (no agent header, no usage).
        let agent = launch.agent;
        if agent.is_some() {
            if let Some(cwd) =
                std::env::current_dir().ok().and_then(|p| p.to_str().map(str::to_string))
            {
                Self::spawn_usage_poller(cx, cwd, agent);
            }
        }

        if std::env::var("TN_AUTOQUIT").is_ok() {
            Self::spawn_self_test(cx, terminal.clone(), writer.clone());
        }

        let font = config.font();
        let font_family = SharedString::from(font.family.clone());
        let font_size = font.size;
        let line_height = font.line_height_px();

        // Measure the monospace cell width once so we can fit the grid to the
        // window. Falls back to a ratio estimate if the glyph can't be measured.
        let font_id = cx.text_system().resolve_font(&gpui::font(&font_family));
        let cell_width = cx
            .text_system()
            .advance(font_id, px(font_size), 'm')
            .map(|s| f32::from(s.width))
            .unwrap_or(font_size * 0.6);

        Self {
            terminal,
            writer,
            pty,
            focus_handle: cx.focus_handle(),
            size,
            cell_width,
            font_family,
            font_size,
            line_height,
            title,
            content_bounds: Rc::new(RefCell::new(Bounds::default())),
            blocks,
            palette,
            selecting: false,
            focused_once: false,
            agent,
            usage: None,
            claude_accent,
            codex_accent,
            ui_accent,
            program: launch.program.clone(),
        }
    }

    /// Reader thread: PTY bytes -> engine; route engine `PtyWrite` replies back;
    /// capture title changes; push a (coalesced) wake to the foreground.
    fn spawn_reader(
        mut reader: Box<dyn Read + Send>,
        terminal: Arc<Mutex<Terminal>>,
        writer: SharedWriter,
        dirty: Arc<AtomicBool>,
        wake_tx: mpsc::UnboundedSender<()>,
        title: Arc<Mutex<Option<String>>>,
        blocks: Arc<Mutex<BlockModel>>,
    ) {
        thread::spawn(move || {
            // Shell-integration bypass parser + a session clock. The parser is
            // stateful (a sequence can split across reads), so it lives here.
            let mut shell = ShellParser::new();
            let start = Instant::now();
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let (replies, events, abs_line): (Vec<String>, _, u64) = {
                            let mut t = terminal.lock().unwrap();
                            t.advance(&buf[..n]);
                            let mut replies = Vec::new();
                            for e in t.drain_events() {
                                match e {
                                    TermEvent::PtyWrite(s) => replies.push(s),
                                    TermEvent::Title(s) => *title.lock().unwrap() = Some(s),
                                    TermEvent::ResetTitle => *title.lock().unwrap() = None,
                                    _ => {}
                                }
                            }
                            // Same bytes feed the bypass parser; the post-advance
                            // cursor line anchors this batch of block events.
                            let events = shell.advance(&buf[..n]);
                            (replies, events, t.cursor_abs_line())
                        };
                        if !replies.is_empty() {
                            let mut w = writer.lock().unwrap();
                            for r in replies {
                                let _ = w.write_all(r.as_bytes());
                            }
                            let _ = w.flush();
                        }
                        if !events.is_empty() {
                            let at_ms = start.elapsed().as_millis() as u64;
                            let mut bm = blocks.lock().unwrap();
                            for ev in events {
                                bm.on_event(ev, abs_line, at_ms);
                            }
                        }
                        // Wake the foreground only on the false->true transition,
                        // so a burst of reads enqueues at most one pending wake.
                        // (Relaxed: the terminal Mutex carries the data ordering.)
                        if !dirty.swap(true, Ordering::Relaxed) && wake_tx.unbounded_send(()).is_err()
                        {
                            break; // view dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }

    /// Foreground task: await reader wakes and repaint. GPUI coalesces the
    /// `notify()` calls onto its vsync frame clock; we render the final state.
    fn spawn_repaint_loop(
        cx: &mut Context<Self>,
        dirty: Arc<AtomicBool>,
        mut wake_rx: mpsc::UnboundedReceiver<()>,
    ) {
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // `dirty` dedup guarantees at most one wake is queued at a time, so a
            // single notify per wake already coalesces a burst of reads. GPUI
            // then folds repeated notifies into one paint at the next vsync.
            while wake_rx.next().await.is_some() {
                dirty.store(false, Ordering::Relaxed);
                if this.update(cx, |_view, cx| cx.notify()).is_err() {
                    break; // view dropped
                }
            }
        })
        .detach();
    }

    /// Poll the PTY child; emit [`ProcessExited`] once, when it exits. ConPTY
    /// doesn't reliably EOF the reader (see CLAUDE.md), so `try_wait` is the
    /// authoritative signal. Cheap (a brief lock every 400ms).
    fn spawn_exit_watcher(cx: &mut Context<Self>, pty: Arc<Mutex<LocalPty>>) {
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                exec.timer(Duration::from_millis(400)).await;
                let exited = pty
                    .lock()
                    .ok()
                    .and_then(|mut p| p.try_wait().ok().flatten())
                    .is_some();
                if exited {
                    let _ = this.update(cx, |_v, cx| cx.emit(ProcessExited));
                    break;
                }
                if this.update(cx, |_, _| ()).is_err() {
                    break; // view dropped
                }
            }
        })
        .detach();
    }

    /// Headless self-test (TN_AUTOQUIT=1): run a command, dump the rendered grid
    /// to stdout, then quit. Lets us verify live rendering without a human.
    fn spawn_self_test(cx: &mut Context<Self>, terminal: Arc<Mutex<Terminal>>, writer: SharedWriter) {
        {
            let mut w = writer.lock().unwrap();
            let _ = w.write_all(b"echo TN_GUI_OK\r\n");
            let _ = w.flush();
        }
        let executor = cx.background_executor().clone();
        cx.spawn(async move |_this: WeakEntity<Self>, cx: &mut AsyncApp| {
            executor.timer(Duration::from_secs(4)).await;
            let text = terminal.lock().unwrap().snapshot().to_text();
            println!("\n----- rendered terminal grid -----\n{text}\n----- end grid -----");
            let _ = cx.update(|cx| cx.quit());
        })
        .detach();
    }

    /// Poll this pane's agent usage off the main thread, re-parsing only when the
    /// resolved session file changes (path or mtime) — an idle agent costs a
    /// cheap `stat`, preserving the idle-zero-wakeup property. Emits
    /// [`UsageUpdated`] on change so the workspace status bar repaints.
    fn spawn_usage_poller(cx: &mut Context<Self>, cwd: String, hint: Option<AgentKind>) {
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut last: Option<(PathBuf, SystemTime)> = None;
            loop {
                let cwd2 = cwd.clone();
                let prev = last.clone();
                let res = exec
                    .spawn(async move {
                        let sref = tn_ai::resolve_session(&cwd2, hint)?;
                        let mtime = std::fs::metadata(&sref.path).ok()?.modified().ok()?;
                        if prev.as_ref() == Some(&(sref.path.clone(), mtime)) {
                            return None; // unchanged — skip the re-parse
                        }
                        let text = std::fs::read_to_string(&sref.path).ok()?;
                        let usage = tn_ai::parse_session(sref.kind, &text)?;
                        Some((sref.kind, sref.path, mtime, usage))
                    })
                    .await;
                if let Some((_kind, path, mtime, usage)) = res {
                    last = Some((path, mtime));
                    // `agent` is fixed from launch intent; the poller only updates
                    // the usage snapshot (never relabels the pane).
                    if this
                        .update(cx, |v, cx| {
                            v.usage = Some(usage);
                            cx.emit(UsageUpdated);
                            cx.notify();
                        })
                        .is_err()
                    {
                        break; // view dropped
                    }
                }
                exec.timer(Duration::from_secs(4)).await;
            }
        })
        .detach();
    }

    /// The focus handle for this pane, so the workspace can route focus.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// This pane's agent (from launch intent, or detected from session logs).
    pub fn agent(&self) -> Option<AgentKind> {
        self.agent
    }

    /// This pane's latest AI usage snapshot, if any has been parsed yet.
    pub fn usage(&self) -> Option<&AiUsage> {
        self.usage.as_ref()
    }

    /// This pane's current working directory (from OSC 7 / shell integration),
    /// if known — drives the tab path badge.
    pub fn cwd(&self) -> Option<String> {
        let m = self.blocks.lock().unwrap();
        m.current()
            .and_then(|b| b.cwd.clone())
            .or_else(|| m.last_finished().and_then(|b| b.cwd.clone()))
    }

    /// A clean tab label: the agent name for an agent pane, else the shell name
    /// (never the raw OSC title, which for pwsh is the noisy `…\powershell.exe`).
    pub fn tab_label(&self) -> String {
        match self.agent {
            Some(a) => a.label().to_string(),
            None => shell_name_of(&self.program),
        }
    }

    /// The latest OSC window title for this session, if the program set one.
    #[allow(dead_code)]
    pub fn title(&self) -> Option<String> {
        self.title.lock().unwrap().clone()
    }

    /// Re-apply a color palette to the live engine (config hot-reload). Font and
    /// scrollback are fixed at construction, so those changes affect new panes.
    pub fn apply_palette(&mut self, palette: Palette) {
        self.palette = palette;
        self.terminal.lock().unwrap().set_palette(palette);
    }

    /// Write raw bytes to the PTY (the shell's stdin), as if typed. Used by the
    /// scripted demo driver.
    pub fn send_bytes(&self, bytes: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }

    /// Demo: scroll the viewport by `lines` (positive = back into history).
    pub fn demo_scroll(&mut self, lines: i32, cx: &mut Context<Self>) {
        self.terminal.lock().unwrap().scroll(lines);
        cx.notify();
    }

    /// Demo: select a fixed visible region so the highlight is observable.
    pub fn demo_select(&mut self, cx: &mut Context<Self>) {
        let mut t = self.terminal.lock().unwrap();
        t.selection_start(1, 2);
        t.selection_update(4, 36);
        drop(t);
        cx.notify();
    }

    /// Demo: clear any selection and jump back to the live bottom.
    pub fn demo_reset_view(&mut self, cx: &mut Context<Self>) {
        let mut t = self.terminal.lock().unwrap();
        t.clear_selection();
        t.scroll_to_bottom();
        drop(t);
        cx.notify();
    }

    /// Paste clipboard text into the PTY, wrapped in bracketed-paste markers
    /// when the program enabled DEC 2004. Newlines are normalized to CR.
    fn paste(&mut self, cx: &mut Context<Self>) {
        let Some(text) = cx.read_from_clipboard().and_then(|i| i.text()) else {
            return;
        };
        if text.is_empty() {
            return;
        }
        let bracketed = {
            let mut t = self.terminal.lock().unwrap();
            t.scroll_to_bottom();
            t.input_mode().bracketed_paste
        };
        let normalized = text.replace("\r\n", "\r").replace('\n', "\r");
        let mut w = self.writer.lock().unwrap();
        if bracketed {
            let _ = w.write_all(b"\x1b[200~");
            let _ = w.write_all(normalized.as_bytes());
            let _ = w.write_all(b"\x1b[201~");
        } else {
            let _ = w.write_all(normalized.as_bytes());
        }
        let _ = w.flush();
        cx.notify();
    }

    /// Copy the current selection to the clipboard (Ctrl+Shift+C).
    fn copy(&mut self, cx: &mut Context<Self>) {
        if let Some(text) = self.terminal.lock().unwrap().selection_text() {
            if !text.is_empty() {
                cx.write_to_clipboard(ClipboardItem::new_string(text));
            }
        }
    }

    /// Copy a command block's command line to the clipboard (block-bar action).
    fn copy_command(&self, cmd: &str, cx: &mut Context<Self>) {
        if !cmd.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(cmd.to_string()));
        }
    }

    /// Re-run a command block: type its command line back into the shell.
    fn rerun_command(&self, cmd: &str, cx: &mut Context<Self>) {
        if cmd.is_empty() {
            return;
        }
        {
            let mut w = self.writer.lock().unwrap();
            let _ = w.write_all(cmd.as_bytes());
            let _ = w.write_all(b"\r");
            let _ = w.flush();
        }
        self.terminal.lock().unwrap().scroll_to_bottom();
        cx.notify();
    }

    /// Build the Warp-style command-block bar shown at the bottom of the pane,
    /// or `None` on the alternate screen (vim/less) or before any command runs.
    fn render_block_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.terminal.lock().unwrap().input_mode().alt_screen {
            return None; // full-screen app owns the viewport — no chrome
        }
        let data = block_view::BlockBar::from_model(&self.blocks.lock().unwrap())?;
        let pal = block_view::BarPalette::from_palette(&self.palette);
        let mut bar = block_view::bar_base(&data, &pal);
        if !data.command.is_empty() {
            let copy_cmd = data.command.clone();
            let rerun_cmd = data.command.clone();
            // Two equal-weight actions: same legible chip + hover brighten. (A
            // dim label read as "disabled", so both use the full foreground.)
            let btn = |label: &'static str| {
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgba(0xffffff14))
                    .text_color(pal.fg)
                    .hover(|s| s.bg(rgba(0xffffff2b)))
                    .child(label)
            };
            bar = bar
                .child(btn("复制").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                        this.copy_command(&copy_cmd, cx)
                    }),
                ))
                .child(btn("重跑").on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                        this.rerun_command(&rerun_cmd, cx)
                    }),
                ));
        }
        Some(bar)
    }

    /// This pane's identity accent: Claude coral / Codex teal, or the UI accent
    /// for a plain shell.
    fn agent_accent(&self) -> Rgb {
        match self.agent {
            Some(AgentKind::ClaudeCode) => self.claude_accent,
            Some(AgentKind::Codex) => self.codex_accent,
            None => self.ui_accent,
        }
    }

    /// A two-tone context-usage ring (grey track + agent-colored arc) with a
    /// centered percent label — the mockup's signature per-agent readout.
    fn usage_ring(&self, pct: u32, accent: Rgb) -> Div {
        div()
            .relative()
            .w(px(32.))
            .h(px(32.))
            .flex_none()
            .child(
                gpui::svg()
                    .path(crate::assets::ring_track_path())
                    .absolute()
                    .size_full()
                    .text_color(rgba(0xffffff1f)),
            )
            .child(
                gpui::svg()
                    .path(crate::assets::ring_path(pct))
                    .absolute()
                    .size_full()
                    .text_color(col(accent)),
            )
            .child(
                div()
                    .absolute()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_size(px(9.))
                    .font_weight(FontWeight::BOLD)
                    .text_color(col(self.palette.fg))
                    .child(SharedString::from(format!("{pct}%"))),
            )
    }

    /// Agent pane header: avatar + name/model + usage ring. No "Thinking…"
    /// indicator — we can't observe the agent's think state from the PTY, so we
    /// don't fake one (Calm Glass: honest chrome).
    fn render_agent_header(&self, agent: AgentKind) -> Div {
        let accent = self.agent_accent();
        let name = match agent {
            AgentKind::ClaudeCode => "Claude Code",
            AgentKind::Codex => "Codex",
        };
        let model = self
            .usage
            .as_ref()
            .map(|u| crate::workspace::short_model(&u.model))
            .filter(|m| !m.is_empty());
        let mut who = div().flex().flex_col().child(
            div()
                .text_size(px(13.))
                .font_weight(FontWeight::BOLD)
                .text_color(col(self.palette.fg))
                .child(SharedString::from(name)),
        );
        if let Some(m) = model {
            who = who.child(
                div()
                    .text_size(px(11.))
                    .text_color(col(self.palette.ansi[8]))
                    .child(SharedString::from(m)),
            );
        }
        let avatar = div()
            .w(px(28.))
            .h(px(28.))
            .rounded(px(9.))
            .flex()
            .items_center()
            .justify_center()
            .flex_none()
            .bg(cola(accent, 0.14))
            .child(crate::assets::icon("spark", 16.).text_color(col(accent)));
        let mut head = div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .flex_none()
            .rounded_t(px(13.)) // top corners follow the pane card
            .font_family(crate::workspace::UI_SANS) // chrome = sans, terminal = mono
            // faint vertical agent-color wash, fading out (refraction, no glow)
            .bg(linear_gradient(
                180.,
                linear_color_stop(cola(accent, 0.10), 0.),
                linear_color_stop(cola(accent, 0.0), 0.78),
            ))
            .child(avatar)
            .child(who)
            .child(div().flex_1());
        if let Some(u) = &self.usage {
            let pct = (u.context_frac() * 100.0).round() as u32;
            let meta = div()
                .flex()
                .flex_col()
                .items_end()
                .child(
                    div()
                        .text_size(px(11.))
                        .font_weight(FontWeight::BOLD)
                        .text_color(col(self.palette.ansi[8]))
                        .child(SharedString::from(format!(
                            "{} / {}",
                            crate::workspace::human_tokens(u.context_used as u64),
                            crate::workspace::human_tokens(u.context_max as u64)
                        ))),
                )
                .when(u.cost_usd > 0.0, |d| {
                    d.child(
                        div()
                            .text_size(px(10.5))
                            .font_weight(FontWeight::BOLD)
                            .text_color(col(self.palette.ansi[2]))
                            .child(SharedString::from(format!("${:.2}", u.cost_usd))),
                    )
                });
            head = head.child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(meta)
                    .child(self.usage_ring(pct, accent)),
            );
        }
        head
    }

    /// The per-pane header — an agent header for agent panes only. A plain shell
    /// gets none: its own prompt already shows the cwd, so a header would just
    /// duplicate it (and look sparse). The tab still labels the pane "pwsh".
    fn render_pane_header(&self) -> Option<Div> {
        self.agent.map(|a| self.render_agent_header(a))
    }

    /// Map a window-space position to a viewport `(row, col)`, clamped to the grid.
    fn cell_at(&self, pos: Point<Pixels>) -> (usize, usize) {
        let b = self.content_bounds.borrow();
        let x = (f32::from(pos.x) - f32::from(b.origin.x)).max(0.0);
        let y = (f32::from(pos.y) - f32::from(b.origin.y)).max(0.0);
        let col = (x / self.cell_width) as usize;
        let row = (y / self.line_height) as usize;
        (
            row.min(self.size.rows.saturating_sub(1)),
            col.min(self.size.cols.saturating_sub(1)),
        )
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let (row, col) = self.cell_at(event.position);
        self.terminal.lock().unwrap().selection_start(row, col);
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (row, col) = self.cell_at(event.position);
        self.terminal.lock().unwrap().selection_update(row, col);
        cx.notify();
    }

    fn on_mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if !self.selecting {
            return;
        }
        self.selecting = false;
        // A click with no drag leaves an empty selection — clear it so no stray
        // cell stays highlighted.
        let mut t = self.terminal.lock().unwrap();
        if t.selection_text().map_or(true, |s| s.is_empty()) {
            t.clear_selection();
            drop(t);
            cx.notify();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let m = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        // Copy: Ctrl+Shift+C (reserved from the encoder).
        if m.control && m.shift && key == "c" {
            self.copy(cx);
            return;
        }
        // Paste: Ctrl+Shift+V or Shift+Insert (both reserved from the encoder).
        if (m.control && m.shift && key == "v") || (m.shift && !m.control && !m.alt && key == "insert")
        {
            self.paste(cx);
            return;
        }

        // Encode against the engine's live modes (DECCKM, LNM, ...). Sending
        // input also snaps the viewport back to the live bottom.
        let bytes = {
            let mut t = self.terminal.lock().unwrap();
            let mode = t.input_mode();
            match crate::input::encode_key(&event.keystroke, mode) {
                Some(b) => {
                    t.scroll_to_bottom();
                    b
                }
                None => return,
            }
        };
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(&bytes);
        let _ = w.flush();
        cx.notify();
    }

    /// Mouse wheel: scroll the scrollback buffer on the main screen; on the
    /// alternate screen (vim/less/...) translate it into arrow keys so the app
    /// scrolls its own buffer.
    fn on_scroll(&mut self, event: &ScrollWheelEvent, _window: &mut Window, cx: &mut Context<Self>) {
        // Lines toward older output are positive.
        let lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / self.line_height,
        };
        if lines == 0.0 {
            return;
        }
        let mode = self.terminal.lock().unwrap().input_mode();
        if mode.alt_screen {
            let up = lines > 0.0;
            let arrow: &[u8] = match (up, mode.app_cursor) {
                (true, false) => b"\x1b[A",
                (true, true) => b"\x1bOA",
                (false, false) => b"\x1b[B",
                (false, true) => b"\x1bOB",
            };
            let n = (lines.abs().round() as usize).clamp(1, 100);
            let mut w = self.writer.lock().unwrap();
            for _ in 0..n {
                let _ = w.write_all(arrow);
            }
            let _ = w.flush();
        } else {
            self.terminal.lock().unwrap().scroll(lines.round() as i32);
            cx.notify();
        }
    }
}

impl gpui::EventEmitter<UsageUpdated> for TerminalView {}
impl gpui::EventEmitter<ProcessExited> for TerminalView {}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused_once {
            self.focus_handle.focus(window);
            self.focused_once = true;
        }

        // Fit the grid to the pane's own content bounds (captured by the canvas
        // below on the previous frame). Skipping while unset keeps the initial
        // size for one frame instead of collapsing to 1x1.
        let (bw, bh) = {
            let b = self.content_bounds.borrow();
            (f32::from(b.size.width), f32::from(b.size.height))
        };
        if bw > 1.0 && bh > 1.0 {
            let cols = ((bw / self.cell_width).floor() as usize).max(1);
            let rows_n = ((bh / self.line_height).floor() as usize).max(1);
            let new_size = GridSize::new(rows_n, cols);
            if new_size != self.size {
                self.size = new_size;
                self.terminal.lock().unwrap().resize(new_size);
                let _ = self
                    .pty
                    .lock()
                    .unwrap()
                    .resize(PtySize::new(rows_n as u16, cols as u16));
            }
        }

        let snapshot = self.terminal.lock().unwrap().snapshot();
        let rows = snapshot.row_runs();
        let bounds_cell = self.content_bounds.clone();
        let block_bar = self.render_block_bar(cx);
        let header = self.render_pane_header();

        // Cursor: a rounded block at the cursor cell (positioned over the grid,
        // which starts at the term-area origin). Solid + accent-tinted when the
        // pane is focused; a hollow outline when not. Hidden when the app hides
        // it (vim) or the viewport is scrolled off the cursor row.
        let (cur_row, cur_col) = snapshot.cursor;
        let focused = self.focus_handle.is_focused(window);
        let cursor_el = (snapshot.cursor_visible
            && cur_row < self.size.rows
            && cur_col < self.size.cols)
            .then(|| {
                let base = div()
                    .absolute()
                    .left(px(cur_col as f32 * self.cell_width))
                    .top(px(cur_row as f32 * self.line_height))
                    .w(px(self.cell_width))
                    .h(px(self.line_height))
                    .rounded(px(2.));
                if focused {
                    // translucent block so a character under the cursor stays legible
                    base.bg(cola(self.palette.cursor, 0.85))
                } else {
                    base.border_1().border_color(col(self.palette.cursor))
                }
            });

        // Terminal area: the canvas captures THIS region's bounds (so the grid
        // fits the space above the block bar) and hosts the row runs. Mouse +
        // scroll handlers live here so clicks on the bar don't start selections.
        let term_area = div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .overflow_hidden()
            .on_scroll_wheel(cx.listener(|this, ev: &ScrollWheelEvent, window, cx| {
                this.on_scroll(ev, window, cx)
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseDownEvent, window, cx| this.on_mouse_down(ev, window, cx)),
            )
            .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                this.on_mouse_move(ev, window, cx)
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, ev: &MouseUpEvent, window, cx| this.on_mouse_up(ev, window, cx)),
            )
            .child(
                canvas(
                    move |bounds, _window, _cx| *bounds_cell.borrow_mut() = bounds,
                    |_bounds, _state, _window, _cx| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .size_full()
                    .flex()
                    .flex_col()
                    .children(rows.into_iter().map(|runs| {
                        div()
                            .flex()
                            .flex_row()
                            .h(px(self.line_height))
                            .children(runs.into_iter().map(|r| {
                                div()
                                    .bg(col(r.bg))
                                    .text_color(col(r.fg))
                                    .when(r.bold, |d| d.font_weight(FontWeight::BOLD))
                                    .child(SharedString::from(r.text))
                            }))
                    })),
            )
            .when_some(cursor_el, |this, c| this.child(c));

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(13.)) // match the pane card's inner radius (R_PANEL - border)
            .bg(col(snapshot.bg))
            .text_color(col(snapshot.fg))
            .font_family(self.font_family.clone())
            .text_size(px(self.font_size))
            .line_height(px(self.line_height))
            .when_some(header, |this, h| this.child(h))
            .child(term_area)
            .when_some(block_bar, |this, bar| this.child(bar))
    }
}

// Key → byte encoding now lives in `crate::input` (see `input.rs`).

#[cfg(test)]
mod tests {
    use super::*;

    fn first_profile(toml: &str) -> tn_config::Profile {
        tn_config::Config::from_toml_str(toml)
            .expect("config parses")
            .profiles
            .into_iter()
            .next()
            .expect("a profile")
    }

    #[test]
    fn wsl_profile_launches_wsl_exe_with_distro() {
        let p = first_profile("[[profiles]]\nname = \"Ubuntu\"\nkind = \"wsl\"\ndistro = \"Ubuntu\"\n");
        let spec = LaunchSpec::from_profile(&p).expect("wsl profile is launchable");
        assert_eq!(spec.program, "wsl.exe");
        assert_eq!(spec.args, vec!["-d".to_string(), "Ubuntu".to_string()]);
        assert!(!spec.integrate_pwsh); // a distro runs bash/zsh, not pwsh
        assert!(spec.agent.is_none());
    }

    #[test]
    fn wsl_profile_without_distro_runs_default() {
        let p = first_profile("[[profiles]]\nname = \"WSL\"\nkind = \"wsl\"\n");
        let spec = LaunchSpec::from_profile(&p).expect("wsl profile is launchable");
        assert_eq!(spec.program, "wsl.exe");
        assert!(spec.args.is_empty()); // bare `wsl.exe` -> default distro
    }
}
