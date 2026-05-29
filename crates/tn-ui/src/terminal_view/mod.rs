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
use std::io::Write;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures::channel::mpsc;
use gpui::{
    canvas, div, point, prelude::*, px, relative, rgba, size, AsyncApp, Bounds,
    ClipboardItem, Context, Div, ElementInputHandler, EntityInputHandler, FocusHandle, FontWeight,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, SharedString, UTF16Selection, WeakEntity, Window,
};
use tn_ai::{AgentKind, AiUsage};
use tn_blocks::BlockModel;
use tn_config::Loaded;
use tn_core::{CellRun, GridSize, Palette, Rgb, SelectKind, Terminal};
use tn_pty::{LocalPty, PtyBackend, PtySize, SpawnSpec, SshBackend};
use tn_shell::Integration;

use crate::block_view;

/// Emitted when a pane's AI-usage readout changes, so the workspace status bar
/// (which renders the *focused* pane's usage) can repaint without re-rendering
/// on every terminal frame.
pub struct UsageUpdated;

/// Emitted once the pane's child process exits (detected via ConPTY `try_wait`,
/// since ConPTY doesn't reliably EOF the reader). The quick terminal listens for
/// this to fall back to its launcher when the hosted agent/shell exits.
pub struct ProcessExited;

/// Emitted when a changed-file card in the agent activity rail is clicked — the
/// workspace opens that file in Quick Look on the Diff tab (mockup `.ahint`
/// 「点卡片 = 速览全 diff」). Carries the absolute path.
pub struct OpenInQuickLook(pub std::path::PathBuf);

use crate::perf::PerfStats;
use crate::style::{col, cola, HOVER};

mod header; // agent pane header UI (avatar / model / usage ring)
mod io; // off-thread workers (reader / repaint / blink / exit-watcher / usage poller)
mod launch; // LaunchSpec: profile -> spawnable pane
pub use launch::LaunchSpec;

/// Cached per-frame render data (待优化清单 §2.1), keyed by the engine's
/// [`generation`](tn_core::Terminal::generation). A repaint that changed nothing
/// the grid renders (the ~530ms cursor blink, an unfocused-pane notify) reuses
/// this instead of rebuilding the snapshot + run batches. `rows` is `Rc` so the
/// hit path hands the renderer a cheap clone; the scalars are all the rest of
/// `render` needs (it never touches `snapshot.cells` after `row_runs`).
struct RenderCache {
    generation: u64,
    rows: Rc<Vec<Vec<CellRun>>>,
    cursor: (usize, usize),
    cursor_visible: bool,
    scroll_offset: usize,
    scroll_history: usize,
    fg: Rgb,
    bg: Rgb,
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

const ROWS: usize = 34;
const COLS: usize = 110;
/// Terminal body inset (mockup `.body { padding:11px 15px }`): the grid is drawn
/// `BODY_PAD_X`/`BODY_PAD_Y` in from the pane's content edge so text doesn't hug
/// the glass rim and aligns with the header's text inset. Applied uniformly to the
/// grid origin, the cursor, mouse hit-testing, AND the cols/rows fit (so the engine
/// sizes to the *inset* area) — all relative to `content_bounds`.
const BODY_PAD_X: f32 = 15.0;
const BODY_PAD_Y: f32 = 11.0;
/// Activity rail (mockup `.arail` 本次改动): cap the changed-file cards (the narrow
/// rail shows a short stack) and the first card's mini-diff preview lines.
pub(super) const RAIL_MAX_FILES: usize = 6;
pub(super) const RAIL_PREVIEW_LINES: usize = 3;
/// Debounce for the working-tree change watcher: coalesce a burst of file events
/// (a save touches several files, a build churns many) into one `git diff` refresh.
const RAIL_WATCH_DEBOUNCE_MS: u64 = 450;
/// Cursor blink half-period (待优化清单 §3.1). ~530ms matches common terminals.
const CURSOR_BLINK_MS: u64 = 530;
/// Visual-bell flash duration (待优化清单 §3.8): a short fade so a bell registers
/// without being a distraction. ~180ms ≈ a quick blink.
const BELL_FLASH_MS: u64 = 180;
/// Sentinel window title a hosted **agent** pane emits *after* the agent exits
/// (the `-NoExit` pwsh runs it on return). The reader sees this OSC, flags the
/// pane, and we clear the agent identity — so the header/tab stop pretending the
/// (now-gone) agent is still running. See [`launch::LaunchSpec`] + `io::spawn_reader`.
pub(super) const AGENT_EXIT_SENTINEL: &str = "TN::agent-exited";

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

pub struct TerminalView {
    terminal: Arc<Mutex<Terminal>>,
    writer: SharedWriter,
    // The pane's PTY backend (local ConPTY or remote SSH); used for resize +
    // exit detection, and kept alive (drop kills the child / disconnects).
    pty: Arc<Mutex<Box<dyn PtyBackend>>>,
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
    // Cursor blink (待优化清单 §3.1): `cursor_on` is the current blink phase,
    // toggled ~530ms by the blink task *only while focused*; `focused` caches the
    // last render's focus so the task knows whether to blink (and so an unfocused
    // pane stays idle — zero wakes). Typing forces `cursor_on = true`.
    cursor_on: bool,
    focused: bool,
    // While dragging the scrollbar thumb: the grab offset (cursor Y − thumb top,
    // px) so the thumb tracks under the cursor. `None` when not dragging.
    scrollbar_drag: Option<f32>,
    // AI usage for this pane (M4): the agent it hosts + its latest usage
    // snapshot, polled off-thread from the agent's session log.
    agent: Option<AgentKind>,
    usage: Option<AiUsage>,
    // Activity-rail data (mockup `.arail` 本次改动): real `git diff HEAD` for this
    // pane's cwd, refreshed by the usage poller off-thread (bounded). HONEST — it
    // comes from git, never from parsing the agent's terminal TUI. Empty = clean
    // working tree / not a git repo. `rail_preview` = the first file's mini diff.
    rail_files: Vec<crate::gitutil::FileChange>,
    rail_preview: Vec<(bool, String)>,
    /// Base dir git ran in for the rail (with `--relative`, `rail_files` paths are
    /// relative to this) — used to resolve a clicked card to an absolute path for
    /// Quick Look. `None` until the first refresh.
    rail_root: Option<std::path::PathBuf>,
    /// `true` when `agent` was inferred from a **typed shell command** (the user ran
    /// `claude`/`codex` at a plain-shell prompt — detected via shell-integration's
    /// command line, not a fragile process walk) rather than from launch intent.
    /// Such an agent is cleared when its command block finishes (vs launch-intent
    /// agents, which clear on the [`AGENT_EXIT_SENTINEL`]).
    agent_from_shell: bool,
    /// Working-tree change watcher for the activity rail (本次改动): fires `git diff`
    /// on file changes (变化即刷新). `Some` only while this pane is an agent; dropping
    /// it stops watching. Stored so it outlives `new` (a dropped watcher = no events).
    change_watcher: Option<notify::RecommendedWatcher>,
    // Set by the reader when a hosted agent emits [`AGENT_EXIT_SENTINEL`] on exit
    // (the `-NoExit` pwsh outlives it). The foreground then clears `agent`/`usage`
    // so the pane reverts to a plain shell (no stale header). Only agent panes
    // emit the sentinel, so a plain shell never trips this.
    agent_exited: Arc<AtomicBool>,
    // Set by the reader on a BEL byte (待优化清单 §3.8); the foreground turns the
    // false->true edge into a flash/beep, then clears it. An atomic (not a wake
    // event) so a bell during a quiet moment still rides the next repaint.
    bell: Arc<AtomicBool>,
    // When a visual bell is mid-fade: the instant it rang (drives the overlay
    // opacity). `None` when no flash is showing. `bell_fading` guards against
    // spawning more than one fade task at a time (a bell storm just refreshes
    // `bell_flash_at`).
    bell_flash_at: Option<Instant>,
    bell_fading: bool,
    // `[appearance]` bell prefs, resolved once at construction.
    visual_bell: bool,
    audio_bell: bool,
    // Theme accents for the per-pane header (Claude coral / Codex teal / UI blue).
    claude_accent: Rgb,
    codex_accent: Rgb,
    ui_accent: Rgb,
    // Chrome text colors for pane headers (mockup .phead/.nm/.model use ui.*, not
    // the terminal palette). fg-dim has no theme token → literal in header.rs.
    ui_fg: Rgb,
    ui_muted: Rgb,
    // Launch program (e.g. "powershell.exe") — for a clean shell label.
    program: String,
    // IME composition (preedit) text, set by the platform input handler while the
    // user is composing (e.g. pinyin → 中文). `Some` ⇒ gpui treats us as composing
    // and routes keys to the IME; on commit the result is written to the PTY and
    // this clears. Without an input handler, IME-composed text never arrives — the
    // root cause of "终端无法输入中文" (only ASCII `key_char` reached `encode_key`).
    ime_marked: Option<String>,
    // Cached render data + the engine generation it was built from (待优化清单
    // §2.1). Reused when a repaint changed nothing renderable (cursor blink).
    render_cache: Option<RenderCache>,
    // Opt-in render instrumentation (TN_PERF): render rate + cache hit-rate +
    // rebuild timing, logged to `tn::perf` ~1/s.
    perf: PerfStats,
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

/// Last-resort local pwsh, used when the intended backend can't spawn — keeps
/// pane construction infallible (it runs in GPUI's non-unwinding callback, where
/// a panic would abort the process).
fn fallback_pwsh(size: PtySize) -> LocalPty {
    LocalPty::spawn(&SpawnSpec::program("powershell.exe").arg("-NoLogo"), size)
        .expect("fallback pwsh spawn failed")
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>, launch: LaunchSpec) -> Self {
        let size = GridSize::new(ROWS, COLS);
        let pty_size = PtySize::new(size.rows as u16, size.cols as u16);
        // Pick the backend: a remote SSH session, or a local ConPTY. A bad
        // profile must NOT crash the app — pane construction runs inside GPUI's
        // window callback (non-unwinding), so a spawn panic aborts the whole
        // process; fall back to a plain pwsh instead.
        let mut pty: Box<dyn PtyBackend> = if let Some(cfg) = &launch.ssh {
            match SshBackend::spawn(cfg.clone(), pty_size) {
                Ok(b) => Box::new(b),
                Err(e) => {
                    tracing::error!(host = %cfg.host, "ssh spawn failed: {e}; falling back to pwsh");
                    Box::new(fallback_pwsh(pty_size))
                }
            }
        } else {
            // Build the spawn spec, then inject the pwsh OSC 133 shell-integration
            // script (pwsh only) via -EncodedCommand — no temp file, no echoed
            // input. Bypassable with TN_NO_SHELL_INTEGRATION.
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
            Box::new(LocalPty::spawn(&spec, pty_size).unwrap_or_else(|e| {
                tracing::error!(program = %launch.program, "spawn failed: {e}; falling back to pwsh");
                fallback_pwsh(pty_size)
            }))
        };
        let reader = pty.take_reader().expect("pty reader");
        let writer: SharedWriter = Arc::new(Mutex::new(pty.writer().expect("pty writer")));
        let pty = Arc::new(Mutex::new(pty));

        // Build the engine with the configured scrollback + theme palette.
        let palette = palette_from(&config.theme);
        let to_rgb = |c: tn_config::Color| Rgb::new(c.r, c.g, c.b);
        let claude_accent = to_rgb(config.theme.agents.claude);
        let codex_accent = to_rgb(config.theme.agents.codex);
        let ui_accent = to_rgb(config.theme.ui.accent);
        let ui_fg = to_rgb(config.theme.ui.foreground);
        let ui_muted = to_rgb(config.theme.ui.muted);
        let visual_bell = config.config.appearance.visual_bell;
        let audio_bell = config.config.appearance.audio_bell;
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
        let agent_exited = Arc::new(AtomicBool::new(false));
        let bell = Arc::new(AtomicBool::new(false));

        Self::spawn_reader(
            reader,
            terminal.clone(),
            writer.clone(),
            dirty.clone(),
            wake_tx,
            title.clone(),
            blocks.clone(),
            agent_exited.clone(),
            bell.clone(),
        );
        Self::spawn_repaint_loop(cx, dirty.clone(), wake_rx);
        Self::spawn_blink_loop(cx);
        // Watch the child so a pane (esp. the quick terminal) can react to its
        // shell/agent exiting. Harmless for the main window (no subscriber).
        Self::spawn_exit_watcher(cx, pty.clone());

        // Per-pane AI usage poller — ONLY for a pane launched AS an agent (launch
        // intent). A plain shell must not masquerade as Claude/Codex just because
        // a fresh agent session exists for this cwd: that agent is often a
        // *separate* process (e.g. the dev's own Claude Code editing this repo).
        // So a plain pwsh pane stays a shell (no agent header, no usage).
        let agent = launch.agent;
        let mut change_watcher = None;
        if agent.is_some() {
            if let Some(cwd) =
                std::env::current_dir().ok().and_then(|p| p.to_str().map(str::to_string))
            {
                Self::spawn_usage_poller(cx, cwd.clone(), agent);
                // 活动栏「本次改动」: watch the cwd → refresh `git diff` on file change
                // (变化即刷新). Also does the initial populate.
                change_watcher = Self::spawn_change_watcher(cx, cwd);
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
            cursor_on: true,
            focused: false,
            scrollbar_drag: None,
            agent,
            usage: None,
            rail_files: Vec::new(),
            rail_preview: Vec::new(),
            rail_root: None,
            agent_from_shell: false,
            change_watcher,
            agent_exited,
            bell,
            bell_flash_at: None,
            bell_fading: false,
            visual_bell,
            audio_bell,
            claude_accent,
            codex_accent,
            ui_accent,
            ui_fg,
            ui_muted,
            program: launch.program.clone(),
            ime_marked: None,
            render_cache: None,
            perf: PerfStats::new("pane.render"),
        }
    }

    /// The focus handle for this pane, so the workspace can route focus.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// This pane's agent (from launch intent, or detected from session logs).
    pub fn agent(&self) -> Option<AgentKind> {
        self.agent
    }

    /// If a hosted agent has signalled its exit (via [`AGENT_EXIT_SENTINEL`]),
    /// drop the agent identity + usage so the pane reverts to a plain shell (no
    /// stale header; the tab relabels to the shell name). Returns whether it
    /// just cleared, so the caller can repaint the workspace tab. Idempotent.
    pub(super) fn clear_agent_if_exited(&mut self) -> bool {
        if self.agent.is_some() && self.agent_exited.load(Ordering::Relaxed) {
            self.clear_agent();
            true
        } else {
            false
        }
    }

    /// Drop the agent identity + everything that hangs off it (usage, activity-rail
    /// data, the change watcher) so the pane reverts cleanly to a plain shell.
    fn clear_agent(&mut self) {
        self.agent = None;
        self.agent_from_shell = false;
        self.usage = None;
        self.rail_files.clear();
        self.rail_preview.clear();
        self.rail_root = None;
        self.change_watcher = None; // stop watching the working tree
    }

    /// Flip the pane to / from agent state based on what's **running** in the shell
    /// (shell-integration command line, OSC 633): typing `claude`/`codex` at a plain
    /// prompt shows the agent header + activity rail for the duration of that command,
    /// reverting when it finishes. Honest — the user literally ran that command (not a
    /// fragile process-tree walk / session-freshness guess, which mislabels; see坑).
    /// No-op for launch-intent agents (they own `agent` for the whole session).
    /// Called from the repaint loop (cheap: one lock + a first-token check).
    pub(super) fn sync_shell_agent(&mut self, cx: &mut Context<Self>) {
        // First token of the currently-running command → an agent? (Match the
        // PROGRAM, not the whole line, so `cd claude-proj` / `cat codex.md` don't trip.)
        let running_agent = {
            let bm = self.blocks.lock().unwrap();
            bm.current()
                .filter(|b| b.is_running())
                .and_then(|b| b.command.as_deref())
                .and_then(|cmd| cmd.split_whitespace().next())
                .and_then(tn_ai::agent_kind_for_command)
        };
        match (running_agent, self.agent) {
            // A typed agent command started in a plain (non-agent) shell.
            (Some(kind), None) => {
                self.agent = Some(kind);
                self.agent_from_shell = true;
                self.usage = None;
                if let Some(cwd) = self.cwd() {
                    Self::spawn_usage_poller(cx, cwd.clone(), Some(kind));
                    self.change_watcher = Self::spawn_change_watcher(cx, cwd);
                }
                cx.emit(UsageUpdated); // relabel the tab + repaint chrome
            }
            // The shell-inferred agent's command finished → revert to plain shell.
            // (Launch-intent agents have `agent_from_shell == false` → left alone;
            // they clear via the exit sentinel instead.)
            (None, Some(_)) if self.agent_from_shell => {
                self.clear_agent();
                cx.emit(UsageUpdated);
            }
            _ => {}
        }
    }

    /// Refresh the activity-rail「本次改动」from real `git diff HEAD` in the pane's
    /// cwd — off the UI thread, bounded. Triggered by the change watcher (变化即刷新)
    /// and once on agent start. No-op once the agent is gone.
    pub(super) fn refresh_changes(&mut self, cx: &mut Context<Self>) {
        if self.agent.is_none() {
            return;
        }
        let Some(cwd) = self.cwd() else { return };
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let (files, preview, root) = exec
                .spawn(async move {
                    let root = std::path::PathBuf::from(&cwd);
                    let mut files = crate::gitutil::changes_for(&root);
                    files.truncate(RAIL_MAX_FILES);
                    let preview = files
                        .first()
                        .map(|f| crate::gitutil::diff_preview(&root, &f.path, RAIL_PREVIEW_LINES))
                        .unwrap_or_default();
                    (files, preview, root)
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                v.rail_files = files;
                v.rail_preview = preview;
                v.rail_root = Some(root);
                cx.emit(UsageUpdated);
                cx.notify();
            });
        })
        .detach();
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
                    .bg(rgba(HOVER))
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

    /// Map a window-space position to a viewport `(row, col)`, clamped to the grid.
    fn cell_at(&self, pos: Point<Pixels>) -> (usize, usize) {
        let b = self.content_bounds.borrow();
        // Subtract the body inset (mockup .body padding) so a click maps to the cell
        // under the cursor — the grid is drawn at +BODY_PAD from content_bounds.
        let x = (f32::from(pos.x) - f32::from(b.origin.x) - BODY_PAD_X).max(0.0);
        let y = (f32::from(pos.y) - f32::from(b.origin.y) - BODY_PAD_Y).max(0.0);
        let col = (x / self.cell_width) as usize;
        let row = (y / self.line_height) as usize;
        (
            row.min(self.size.rows.saturating_sub(1)),
            col.min(self.size.cols.saturating_sub(1)),
        )
    }

    fn on_mouse_down(&mut self, event: &MouseDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let (row, col) = self.cell_at(event.position);
        // Click count picks the granularity: 1 = cell, 2 = word, 3+ = line
        // (待优化清单 §3.5). A following drag extends by that same granularity.
        let kind = match event.click_count {
            0 | 1 => SelectKind::Cell,
            2 => SelectKind::Word,
            _ => SelectKind::Line,
        };
        self.terminal.lock().unwrap().selection_start_kind(row, col, kind);
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.scrollbar_drag.is_some() {
            self.drag_scrollbar(event.position.y.into(), cx);
            return;
        }
        if !self.selecting || event.pressed_button != Some(MouseButton::Left) {
            return;
        }
        let (row, col) = self.cell_at(event.position);
        self.terminal.lock().unwrap().selection_update(row, col);
        cx.notify();
    }

    /// Begin dragging the scrollbar thumb; record the grab offset within the
    /// thumb so it tracks under the cursor.
    fn begin_scrollbar_drag(&mut self, cursor_y: f32, cx: &mut Context<Self>) {
        let b = *self.content_bounds.borrow();
        let track_h = f32::from(b.size.height);
        let (offset, history) = self.terminal.lock().unwrap().scroll_position();
        let total = (history + self.size.rows) as f32;
        if track_h <= 0.0 || total <= 0.0 {
            return;
        }
        let thumb_top = f32::from(b.origin.y)
            + (history.saturating_sub(offset)) as f32 / total * track_h;
        self.scrollbar_drag = Some(cursor_y - thumb_top);
        cx.notify();
    }

    /// Map the dragged thumb position to a scrollback offset and apply it.
    fn drag_scrollbar(&mut self, cursor_y: f32, cx: &mut Context<Self>) {
        let Some(grab_dy) = self.scrollbar_drag else { return };
        let b = *self.content_bounds.borrow();
        let track_h = f32::from(b.size.height);
        if track_h <= 0.0 {
            return;
        }
        let (_, history) = self.terminal.lock().unwrap().scroll_position();
        let total = (history + self.size.rows) as f32;
        let frac = ((cursor_y - f32::from(b.origin.y) - grab_dy) / track_h).clamp(0.0, 1.0);
        let offset = (history as f32 - frac * total).round().clamp(0.0, history as f32) as usize;
        self.terminal.lock().unwrap().scroll_to_offset(offset);
        cx.notify();
    }

    fn on_mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.scrollbar_drag.take().is_some() {
            cx.notify();
            return;
        }
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
        // Keep the cursor solid right as the user types (don't blink mid-keystroke).
        self.cursor_on = true;
        let m = &event.keystroke.modifiers;
        let key = event.keystroke.key.as_str();
        // Copy: Ctrl+Shift+C (reserved from the encoder).
        if m.control && m.shift && key == "c" {
            self.copy(cx);
            cx.stop_propagation();
            return;
        }
        // Paste: Ctrl+Shift+V or Shift+Insert (both reserved from the encoder).
        if (m.control && m.shift && key == "v") || (m.shift && !m.control && !m.alt && key == "insert")
        {
            self.paste(cx);
            cx.stop_propagation();
            return;
        }

        // **Plain text keys must NOT be consumed here** — defer them to the IME input
        // handler (no `stop_propagation`, no encode). A single-char `key` with no
        // Ctrl/Alt/Win is a text-producing key; letting it through means gpui runs
        // `translate_message`, so the platform routes it to the input handler:
        // English via WM_CHAR, **中文 via WM_IME_COMPOSITION** →
        // `replace_text_in_range` (which writes the committed bytes to the PTY).
        // Consuming + stopping these (the previous version) made gpui mark the
        // keydown handled and SKIP `translate_message`, so IME composition never
        // started — that was the "无法输入中文" root cause. Named/modified keys
        // (Enter, Tab, arrows, Ctrl-*, function keys, …) still encode below; during
        // an active composition gpui short-circuits keydown to the IME on its own.
        // `space` is a named key but it IS text input — and critically the IME's main
        // **commit** key. If we encode+stop it (as for other named keys), gpui marks
        // the keydown handled and skips `translate_message`, so the IME never sees the
        // space and can't commit the candidate → a literal space is typed instead of
        // 中文 (the reported bug). So treat space as a plain text key: defer it.
        let is_text_input =
            !m.control && !m.alt && !m.platform && (key.chars().count() == 1 || key == "space");
        if is_text_input {
            tracing::info!(target: "tn::ime", "term on_key DEFER key={key:?} ime_marked={:?}", self.ime_marked);
            return;
        }
        tracing::info!(target: "tn::ime", "term on_key ENCODE key={key:?} ctrl={} alt={} ime_marked={:?}", m.control, m.alt, self.ime_marked);

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
                // Not a terminal-input key (UI shortcut / unmapped): let it BUBBLE
                // (no stop) so workspace keybindings still fire. Crucially we also
                // DON'T consume it, so gpui's `translate_message` may turn a real
                // text key into WM_CHAR → the IME input handler (中文 via composition).
                None => return,
            }
        };
        self.send_bytes(&bytes);
        // We handled this key → stop it. This makes gpui mark the WM_KEYDOWN as
        // handled and skip `translate_message`, so no duplicate WM_CHAR reaches the
        // input handler (which would double every ASCII key once IME is wired). IME
        // composition (中文) arrives via WM_IME_COMPOSITION, not keydown, so it's
        // unaffected. UI shortcuts took the `None` path above and still bubble.
        cx.stop_propagation();
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
impl gpui::EventEmitter<OpenInQuickLook> for TerminalView {}

/// IME / text input (fixes "终端无法输入中文"). gpui only delivers IME-composed text
/// (pinyin → 中文) through an [`EntityInputHandler`]; without one, only ASCII
/// `key_char` from WM_KEYDOWN reached `encode_key`. The terminal has no editable
/// document, so the only "text" we model is the in-progress composition
/// (`ime_marked`): committed text (any language) is written straight to the PTY.
/// Plain ASCII keys still go through `on_key`/`encode_key` (which stops propagation
/// so gpui skips `translate_message` → no duplicate WM_CHAR); only IME results land
/// here. See `register_ime` (called in paint) + the events.rs dispatch notes.
impl EntityInputHandler for TerminalView {
    fn text_for_range(
        &mut self,
        range: std::ops::Range<usize>,
        adjusted: &mut Option<std::ops::Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        // We only expose the in-progress composition as addressable text.
        let units: Vec<u16> = self.ime_marked.as_deref().unwrap_or("").encode_utf16().collect();
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
        // Caret at the end of the composition (0 when not composing) — anchors the
        // IME candidate window via `bounds_for_range`.
        let end = self.ime_marked.as_deref().map(|s| s.encode_utf16().count()).unwrap_or(0);
        Some(UTF16Selection { range: end..end, reversed: false })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<std::ops::Range<usize>> {
        // `Some` ⇒ gpui knows we're composing and feeds keys to the IME (events.rs).
        let r = self.ime_marked.as_deref().map(|s| 0..s.encode_utf16().count());
        tracing::info!(target: "tn::ime", "term marked_text_range -> {r:?}");
        r
    }

    fn unmark_text(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.ime_marked = None;
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        _range: Option<std::ops::Range<usize>>,
        text: &str,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Committed text (IME result 中文, or any text the platform routes here) →
        // straight to the PTY, like a paste of one grapheme cluster.
        tracing::info!(target: "tn::ime", "term replace_text(commit) text={text:?}");
        if !text.is_empty() {
            self.terminal.lock().unwrap().scroll_to_bottom();
            self.send_bytes(text.as_bytes());
        }
        self.ime_marked = None;
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
        // Composition preedit (pinyin in progress): don't touch the PTY until commit;
        // just track it so we report composing state + position the candidate window.
        tracing::info!(target: "tn::ime", "term replace_and_mark text={new_text:?}");
        self.ime_marked = (!new_text.is_empty()).then(|| new_text.to_string());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _range_utf16: std::ops::Range<usize>,
        element_bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        // Place the IME candidate window at the cursor cell (grid is inset BODY_PAD).
        let (row, col) = self.render_cache.as_ref().map(|c| c.cursor).unwrap_or((0, 0));
        let x = f32::from(element_bounds.origin.x) + BODY_PAD_X + col as f32 * self.cell_width;
        let y = f32::from(element_bounds.origin.y) + BODY_PAD_Y + row as f32 * self.line_height;
        Some(Bounds {
            origin: point(px(x), px(y)),
            size: size(px(self.cell_width), px(self.line_height)),
        })
    }

    fn character_index_for_point(
        &mut self,
        _point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

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
            // Fit to the *inset* body area (mockup .body padding) — leave BODY_PAD on
            // each side so the grid matches where the cursor/grid are actually drawn.
            let avail_w = (bw - 2.0 * BODY_PAD_X).max(self.cell_width);
            let avail_h = (bh - 2.0 * BODY_PAD_Y).max(self.line_height);
            let cols = ((avail_w / self.cell_width).floor() as usize).max(1);
            let rows_n = ((avail_h / self.line_height).floor() as usize).max(1);
            let new_size = GridSize::new(rows_n, cols);
            // ConPTY tracks the visible grid EXACTLY (rows ≠ alacritty rows caused
            // worse, frequent blanking — once output scrolls alacritty but not
            // ConPTY their cursors diverge and the prompt mislands; the reverted
            // row-lock. See 踩过的坑). To stop a divider-drag-*grow* eating
            // scrollback, the engine top-anchors the grow (`resize_conpty`) so
            // ConPTY's top-anchored repaint can't clobber pulled-up history —
            // verified zero-loss by `TN_RESIZE_EXP=topgrow`.
            if new_size != self.size {
                self.size = new_size;
                self.terminal.lock().unwrap().resize_conpty(new_size);
                let _ = self.pty.lock().unwrap().resize(PtySize::new(rows_n as u16, cols as u16));
            }
        }

        // Render-data cache (待优化清单 §2.1): rebuild the snapshot + per-row run
        // batches only when the engine generation changed since the last paint.
        // A cursor-blink or unfocused-pane repaint changes nothing renderable, so
        // it reuses the cached `rows` (a cheap Rc clone) instead of re-walking the
        // whole grid. `perf` (TN_PERF) logs the hit-rate + rebuild cost.
        let generation = self.terminal.lock().unwrap().generation();
        let cache_hit = matches!(&self.render_cache, Some(c) if c.generation == generation);
        let rebuild = if cache_hit {
            None
        } else {
            let t0 = self.perf.enabled().then(Instant::now); // zero-cost when TN_PERF off
            let snap = self.terminal.lock().unwrap().snapshot();
            let rows = Rc::new(snap.row_runs());
            self.render_cache = Some(RenderCache {
                generation,
                rows,
                cursor: snap.cursor,
                cursor_visible: snap.cursor_visible,
                scroll_offset: snap.scroll_offset,
                scroll_history: snap.scroll_history,
                fg: snap.fg,
                bg: snap.bg,
            });
            t0.map(|t| t.elapsed())
        };
        self.perf.record(cache_hit, rebuild);
        let (rows, (cur_row, cur_col), cursor_visible, scroll_offset, scroll_history, fg, bg) = {
            let c = self.render_cache.as_ref().unwrap();
            (c.rows.clone(), c.cursor, c.cursor_visible, c.scroll_offset, c.scroll_history, c.fg, c.bg)
        };
        let bounds_cell = self.content_bounds.clone();
        // Captured into the canvas paint closure to register the IME input handler
        // (text input / 中文 composition) for this frame — see the `EntityInputHandler`
        // impl + `handle_input` below.
        let ime_focus = self.focus_handle.clone();
        let ime_entity = cx.entity();
        let block_bar = self.render_block_bar(cx);
        let header = self.render_pane_header();

        // Cursor (positioned over the grid, which starts at the term-area origin).
        // Hidden when the app hides it (vim) or the viewport is scrolled off the row.
        let focused = self.focus_handle.is_focused(window);
        self.focused = focused; // cache for the blink task (only blinks when focused)
        // Focused: solid block on the "on" half of the blink; nothing on the "off"
        // half. Unfocused: a steady slim outline (no blink).
        let draw_solid = focused && self.cursor_on;
        // The glyph under the cursor (≈1 col/char; cursor-on-wide-char is rare) so the
        // focused block can redraw it in the background color = a crisp **inverse**
        // cursor instead of a muddy translucent overlay. Whitespace → just the block.
        let cursor_char = rows.get(cur_row).and_then(|row| {
            let mut c = 0usize;
            for run in row {
                for ch in run.text.chars() {
                    if c == cur_col {
                        return (!ch.is_whitespace()).then_some(ch);
                    }
                    c += 1;
                }
            }
            None
        });
        let cursor_el = (cursor_visible
            && (draw_solid || !focused)
            && cur_row < self.size.rows
            && cur_col < self.size.cols)
            .then(|| {
                let base = div()
                    .absolute()
                    // +BODY_PAD: the grid is inset from the content edge (mockup .body)
                    .left(px(BODY_PAD_X + cur_col as f32 * self.cell_width))
                    .top(px(BODY_PAD_Y + cur_row as f32 * self.line_height))
                    .w(px(self.cell_width))
                    .h(px(self.line_height))
                    .rounded(px(2.));
                if draw_solid {
                    // Opaque block in the cursor color + the glyph redrawn in the bg
                    // color on top = sharp inverse cursor (the char stays crisp, not
                    // dimmed). The block sits over the grid glyph and hides it.
                    base.bg(col(self.palette.cursor))
                        .text_color(col(bg))
                        .when_some(cursor_char, |d, ch| d.child(SharedString::from(ch.to_string())))
                } else {
                    // Unfocused: a slim, calmer outline (thinner presence than a full block).
                    base.border_1().border_color(cola(self.palette.cursor, 0.55))
                }
            });

        // IME composition preedit (拼音 in progress): show it inline at the cursor —
        // an opaque box (covers the chars under it) + accent underline = the "正在合成"
        // affordance, so typing Chinese feels like normal inline input rather than
        // composing blind in the floating candidate window. Cleared on commit/cancel.
        let ime_preedit = self.ime_marked.clone().filter(|s| !s.is_empty()).map(|s| {
            div()
                .absolute()
                .left(px(BODY_PAD_X + cur_col as f32 * self.cell_width))
                .top(px(BODY_PAD_Y + cur_row as f32 * self.line_height))
                .h(px(self.line_height))
                .bg(col(bg)) // cover the cells underneath so the preedit is legible
                .text_color(col(fg))
                .border_b_2()
                .border_color(col(self.ui_accent))
                .child(SharedString::from(s))
        });

        // Scrollbar (待优化清单 §3.2): a thin right-edge indicator of the viewport's
        // position within scrollback. Shown only when there's history; brighter
        // while actually scrolled up. The thumb's size = viewport / total content.
        let scrollbar = (scroll_history > 0).then(|| {
            let total = (scroll_history + self.size.rows) as f32;
            let thumb_h = (self.size.rows as f32 / total).clamp(0.06, 1.0);
            let top = ((scroll_history.saturating_sub(scroll_offset)) as f32 / total)
                .clamp(0.0, 1.0 - thumb_h);
            let scrolled = scroll_offset > 0 || self.scrollbar_drag.is_some();
            div()
                .absolute()
                .top(relative(top))
                .right(px(2.))
                .w(px(5.))
                .h(relative(thumb_h))
                .rounded(px(2.))
                .bg(rgba(if scrolled { 0xffffff66 } else { HOVER }))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                        this.begin_scrollbar_drag(ev.position.y.into(), cx);
                    }),
                )
        });

        // Visual bell (待优化清单 §3.8): a brief translucent flash over the grid
        // that fades out, so a BEL registers without sound. `spawn_bell_fade`
        // drives the per-frame notifies and clears `bell_flash_at` when done.
        let bell_overlay = self.bell_flash_at.and_then(|t| {
            let frac = t.elapsed().as_millis() as f32 / BELL_FLASH_MS as f32;
            (frac < 1.0).then(|| {
                div()
                    .absolute()
                    .size_full()
                    .bg(cola(self.palette.fg, 0.18 * (1.0 - frac)))
            })
        });

        // Terminal area: the canvas captures THIS region's bounds (so the grid
        // fits the space above the block bar) and hosts the row runs. Mouse +
        // scroll handlers live here so clicks on the bar don't start selections.
        let term_area = div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .min_w(px(0.)) // mockup .abody .body min-width:0(agent 面板正文与活动栏同处 flex 行)
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
                    // Register the per-frame IME/text input handler so composed text
                    // (中文) reaches `replace_text_in_range`. No-op unless focused.
                    move |bounds, _state, window, cx| {
                        tracing::trace!(target: "tn::ime", "term canvas paint: handle_input focused={}", ime_focus.is_focused(window));
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
            .child(
                // Grid inset from the content edge by BODY_PAD (mockup .body padding);
                // absolute so it shares the cursor's coordinate origin exactly.
                div()
                    .absolute()
                    .left(px(BODY_PAD_X))
                    .top(px(BODY_PAD_Y))
                    .flex()
                    .flex_col()
                    .children(rows.iter().map(|runs| {
                        div()
                            .flex()
                            .flex_row()
                            .h(px(self.line_height))
                            .children(runs.iter().map(|r| {
                                div()
                                    // **Force the run box to its exact grid span**
                                    // (`cols × cell_width`) so cells stay aligned even
                                    // when a glyph's font advance ≠ cell_width — i.e.
                                    // CJK in a fallback font (CaskaydiaCove has no CJK).
                                    // Without this the row flex-flowed by natural glyph
                                    // width and Chinese drifted / spaced wrong. `flex_none`
                                    // + `overflow_hidden` keep the width authoritative.
                                    .flex_none()
                                    .w(px(r.cols as f32 * self.cell_width))
                                    .overflow_hidden()
                                    // 默认底色留空 → 透出面板 g1 玻璃(mockup:正文落在玻璃上);
                                    // 仅非默认底(选区/上色/反显)才实绘。
                                    .when(r.bg != bg, |d| d.bg(col(r.bg)))
                                    .text_color(col(r.fg))
                                    .when(r.bold, |d| d.font_weight(FontWeight::BOLD))
                                    .child(SharedString::from(r.text.clone()))
                            }))
                    })),
            )
            .when_some(cursor_el, |this, c| this.child(c))
            .when_some(ime_preedit, |this, p| this.child(p))
            .when_some(scrollbar, |this, s| this.child(s))
            .when_some(bell_overlay, |this, o| this.child(o));

        // agent 面板:正文 + 右侧活动栏并排(mockup .abody = .body + .arail);
        // shell 面板:正文满宽、无活动栏(mockup shell pane 无 .arail)。
        let body_region = if self.agent.is_some() {
            div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_row() // mockup .abody
                .child(term_area)
                .child(self.render_activity_rail(cx))
        } else {
            term_area
        };

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(13.)) // match the pane card's inner radius (R_PANEL - border)
            .bg(rgba(0x00000000)) // 透明:终端默认底透出 render_node 的 g1 玻璃(mockup .body on glass)
            .text_color(col(fg))
            .font_family(self.font_family.clone())
            .text_size(px(self.font_size))
            .line_height(px(self.line_height))
            .when_some(header, |this, h| this.child(h))
            .child(body_region)
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

    // ── Launch-path coverage (待优化清单 §6.3) ─────────────────────────────────
    // The SSH / native-pwsh / hosted-agent paths were previously untested; these
    // pin each one so the per-kind refactor (and future edits) stay honest.

    #[test]
    fn ssh_profile_builds_config_and_label() {
        let p = first_profile(
            "[[profiles]]\nname=\"box\"\nkind=\"ssh\"\nhost=\"example.com\"\nuser=\"alice\"\n",
        );
        let spec = LaunchSpec::from_profile(&p).expect("ssh profile is launchable");
        let cfg = spec.ssh.expect("ssh config present");
        assert_eq!(cfg.host, "example.com");
        assert_eq!(cfg.user, "alice");
        assert_eq!(spec.program, "alice@example.com"); // the pane label
        assert!(!spec.integrate_pwsh);
        assert!(spec.agent.is_none());
    }

    #[test]
    fn ssh_profile_without_host_is_none() {
        let p = first_profile("[[profiles]]\nname=\"box\"\nkind=\"ssh\"\nuser=\"alice\"\n");
        assert!(LaunchSpec::from_profile(&p).is_none(), "no host -> not launchable");
    }

    #[test]
    fn native_pwsh_runs_directly_with_integration() {
        let p = first_profile("[[profiles]]\nname=\"PS\"\ncommand=\"powershell.exe\"\n");
        let spec = LaunchSpec::from_profile(&p).expect("pwsh is launchable");
        assert_eq!(spec.program, "powershell.exe");
        assert!(spec.integrate_pwsh, "native pwsh gets OSC 133 integration");
        assert_eq!(spec.args, vec!["-NoLogo".to_string()]); // empty args defaulted
        assert!(spec.ssh.is_none());
        assert!(spec.agent.is_none());
    }

    #[test]
    fn agent_command_is_hosted_in_pwsh_with_noexit() {
        let p = first_profile("[[profiles]]\nname=\"Claude\"\ncommand=\"claude\"\n");
        let spec = LaunchSpec::from_profile(&p).expect("claude is launchable");
        assert_eq!(spec.program, "powershell.exe", "hosted inside pwsh");
        assert!(!spec.integrate_pwsh);
        assert_eq!(spec.agent, Some(AgentKind::ClaudeCode), "agent inferred from command");
        assert!(spec.args.contains(&"-NoExit".to_string()), "persistent keeps -NoExit");
        assert!(
            spec.args.iter().any(|a| a.contains("& 'claude'")),
            "command hosted via call operator, got {:?}",
            spec.args
        );
        // A persistent agent appends the exit sentinel so the view can drop the
        // header once the agent exits (the -NoExit pwsh runs it).
        assert!(
            spec.args.iter().any(|a| a.contains(AGENT_EXIT_SENTINEL)),
            "persistent agent emits the exit sentinel, got {:?}",
            spec.args
        );
    }

    #[test]
    fn ephemeral_hosted_agent_omits_noexit_and_sentinel() {
        let p = first_profile("[[profiles]]\nname=\"Codex\"\ncommand=\"codex\"\n");
        let spec = LaunchSpec::from_profile_ephemeral(&p).expect("codex is launchable");
        assert_eq!(spec.agent, Some(AgentKind::Codex));
        assert!(!spec.args.contains(&"-NoExit".to_string()), "ephemeral drops -NoExit");
        assert!(spec.args.iter().any(|a| a.contains("& 'codex'")));
        // No sentinel: the ephemeral pane exits pwsh outright (ProcessExited),
        // so it needn't (and shouldn't) emit the title marker.
        assert!(
            !spec.args.iter().any(|a| a.contains(AGENT_EXIT_SENTINEL)),
            "ephemeral agent must not append the sentinel, got {:?}",
            spec.args
        );
    }

    #[test]
    fn inner_catch_unwind_leaves_the_lock_unpoisoned() {
        // The reader (待优化清单 §8.1) catches an alacritty panic *inside* the lock
        // scope, so the stack unwinds only to the catch and the guard drops
        // normally — the Mutex is NOT poisoned, so the foreground (GPUI callbacks,
        // non-unwinding) can still lock it instead of aborting the whole process.
        // This models `spawn_reader`'s inner guard with a plain Mutex.
        let m = std::sync::Mutex::new(0i32);
        let caught = {
            let mut g = m.lock().unwrap();
            *g = 1;
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                *g = 2;
                panic!("simulated alacritty hiccup");
            }))
            // `g` drops here, normally, even though the closure panicked.
        };
        assert!(caught.is_err(), "the panic was caught, not propagated");
        assert!(m.lock().is_ok(), "the lock must survive a caught panic un-poisoned");
    }
}
