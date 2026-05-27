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
    canvas, div, prelude::*, px, relative, rgba, Bounds,
    ClipboardItem, Context, Div, FocusHandle, FontWeight, KeyDownEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ScrollDelta, ScrollWheelEvent,
    SharedString, Window,
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
/// Cursor blink half-period (待优化清单 §3.1). ~530ms matches common terminals.
const CURSOR_BLINK_MS: u64 = 530;
/// Floor for a plain shell's ConPTY row count (see `pty_target_rows`). Chosen to
/// exceed any realistic full-window pane (a 1440p window at a readable font is
/// ~100 rows) so dragging a divider never grows ConPTY's rows — which would let
/// its resize-repaint eat scrollback. Tall ConPTY rows are harmless for a shell.
const SHELL_PTY_ROWS_FLOOR: usize = 120;
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
    // Set by the reader when a hosted agent emits [`AGENT_EXIT_SENTINEL`] on exit
    // (the `-NoExit` pwsh outlives it). The foreground then clears `agent`/`usage`
    // so the pane reverts to a plain shell (no stale header). Only agent panes
    // emit the sentinel, so a plain shell never trips this.
    agent_exited: Arc<AtomicBool>,
    // Theme accents for the per-pane header (Claude coral / Codex teal / UI blue).
    claude_accent: Rgb,
    codex_accent: Rgb,
    ui_accent: Rgb,
    // Launch program (e.g. "powershell.exe") — for a clean shell label.
    program: String,
    // Row-lock for plain shells (see `pty_target_rows`): the monotonic, floored
    // ConPTY row count for this pane on the main screen. It only ever grows and
    // starts above any realistic full-window pane, so dragging a divider bigger
    // never triggers a ConPTY row-grow — which would let ConPTY's resize-repaint
    // clobber the scrollback alacritty pulls into the viewport (Windows quirk).
    shell_pty_rows: usize,
    // The last (rows, cols) actually sent to the PTY, so we only resize the PTY
    // when the target genuinely changes (the row-lock decouples it from `size`).
    last_pty_size: Option<PtySize>,
    // Cached render data + the engine generation it was built from (待优化清单
    // §2.1). Reused when a repaint changed nothing renderable (cursor blink).
    render_cache: Option<RenderCache>,
    // Opt-in render instrumentation (TN_PERF): render rate + cache hit-rate +
    // rebuild timing, logged to `tn::perf` ~1/s.
    perf: PerfStats,
}

/// The ConPTY row count for a pane, given whether it hosts an agent, whether
/// the alternate screen is active, the previous shell high-water mark, and the
/// current visible grid rows. Pure so it can be unit-tested; see
/// [`TerminalView::pty_target_rows`] for the rationale (the row-lock that stops
/// ConPTY's resize-repaint from eating scrollback on a divider drag).
fn pty_rows_for(is_agent: bool, alt_screen: bool, locked_rows: usize, grid_rows: usize) -> usize {
    if is_agent || alt_screen {
        grid_rows // exact: agents redraw by height, fullscreen apps position by row
    } else {
        // Monotonic: stay at the locked height (seeded to the spawn height, so it
        // never *grows* in normal use — that would repaint/blank). Only a pane
        // taller than the lock (huge monitor) bumps it, a rare one-time grow.
        locked_rows.max(grid_rows)
    }
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
        // A local shell spawns its ConPTY at the row-lock height (the floor)
        // directly, so its rows NEVER grow afterward — a ConPTY row-grow repaints
        // and repositions content, blanking the prompt / eating scrollback (see
        // `pty_target_rows` + 踩过的坑). Agents spawn at the visible rows (they
        // redraw their TUI fully and want a real height). alacritty's grid still
        // starts at `ROWS` and tracks the pane; only ConPTY's rows are pinned.
        let is_local_shell = launch.agent.is_none() && launch.ssh.is_none();
        let spawn_rows = if is_local_shell { SHELL_PTY_ROWS_FLOOR } else { ROWS } as u16;
        let pty_size = PtySize::new(spawn_rows, COLS as u16);
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

        Self::spawn_reader(
            reader,
            terminal.clone(),
            writer.clone(),
            dirty.clone(),
            wake_tx,
            title.clone(),
            blocks.clone(),
            agent_exited.clone(),
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
            cursor_on: true,
            focused: false,
            scrollbar_drag: None,
            agent,
            usage: None,
            agent_exited,
            claude_accent,
            codex_accent,
            ui_accent,
            program: launch.program.clone(),
            // Seed the lock to the spawn height so a local shell's ConPTY rows
            // stay put (no grow). Agents leave it 0 (they use exact rows until,
            // on exit, the pane relocks at its then-current height).
            shell_pty_rows: if is_local_shell { SHELL_PTY_ROWS_FLOOR } else { 0 },
            // Seed to the spawn size so the first render doesn't redundantly
            // resize ConPTY's rows (only its columns, to fit the pane).
            last_pty_size: Some(pty_size),
            render_cache: None,
            perf: PerfStats::new("pane.render"),
        }
    }

    /// The row count to give ConPTY for this pane (columns are always tracked
    /// exactly — they drive line wrapping). The catch is a Windows ConPTY quirk:
    /// when ConPTY's *rows* grow, it repaints its viewport, and that repaint
    /// overwrites the scrollback lines alacritty just pulled up into the grown
    /// viewport → they're lost from both screen and history (verified with
    /// `tn-cli TN_RESIZE_EXP`). We can't stop alacritty pulling them up, so we
    /// stop ConPTY from growing its rows for plain shells:
    ///
    /// * **Agent panes** (Claude/Codex are Ink TUIs that redraw by terminal
    ///   height) → exact rows always. They manage their own redraw; correct
    ///   layout matters more than shell-style scrollback.
    /// * **Alt-screen** (vim/less) → exact rows; a fullscreen app positions by
    ///   absolute row and has no scrollback to lose anyway.
    /// * **Plain shell on the main screen** → a monotonic count floored at
    ///   [`SHELL_PTY_ROWS_FLOOR`], which exceeds any realistic full-window pane.
    ///   So a divider drag never grows ConPTY's rows and its repaint can't
    ///   clobber. alacritty still resizes exactly and reflows its own (intact)
    ///   scrollback, so growing the pane losslessly reveals more history. A
    ///   shell ignores its row count, and a tall ConPTY (rows ≫ the visible
    ///   grid) still streams output coherently and bottom-anchored — verified
    ///   with `tn-cli TN_RESIZE_EXP=interactive` up to a 10× ratio.
    fn pty_target_rows(&mut self, grid_rows: usize, alt_screen: bool) -> usize {
        let rows = pty_rows_for(self.agent.is_some(), alt_screen, self.shell_pty_rows, grid_rows);
        if self.agent.is_none() && !alt_screen {
            self.shell_pty_rows = rows; // remember the (monotonic) high-water mark
        }
        rows
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
            self.agent = None;
            self.usage = None;
            // Lock the now-shell at its CURRENT height, not the floor — growing
            // ConPTY rows here (to 120) would repaint and blank the fresh prompt.
            // (A later divider-drag bigger can still grow it, but that's rare for
            // a just-exited agent and far less jarring than a blank-out on exit.)
            self.shell_pty_rows = self.size.rows;
            true
        } else {
            false
        }
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
            // alacritty always matches the pane exactly (it renders these rows
            // and reflows its own scrollback losslessly).
            if new_size != self.size {
                self.size = new_size;
                self.terminal.lock().unwrap().resize(new_size);
            }
            // ConPTY's rows are row-locked for plain shells (see
            // `pty_target_rows`) — its columns always track exactly. Resize the
            // PTY only when this target actually changes (it's decoupled from
            // `self.size`, and the alt-screen state can flip it without a resize).
            let alt = self.terminal.lock().unwrap().input_mode().alt_screen;
            let pty_rows = self.pty_target_rows(rows_n, alt);
            let pty_size = PtySize::new(pty_rows as u16, cols as u16);
            if self.last_pty_size != Some(pty_size) {
                self.last_pty_size = Some(pty_size);
                let _ = self.pty.lock().unwrap().resize(pty_size);
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
        let block_bar = self.render_block_bar(cx);
        let header = self.render_pane_header();

        // Cursor: a rounded block at the cursor cell (positioned over the grid,
        // which starts at the term-area origin). Solid + accent-tinted when the
        // pane is focused; a hollow outline when not. Hidden when the app hides
        // it (vim) or the viewport is scrolled off the cursor row.
        let focused = self.focus_handle.is_focused(window);
        self.focused = focused; // cache for the blink task (only blinks when focused)
        // Focused: solid block on the "on" half of the blink; nothing on the "off"
        // half. Unfocused: a steady hollow outline (no blink).
        let draw_solid = focused && self.cursor_on;
        let cursor_el = (cursor_visible
            && (draw_solid || !focused)
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
                if draw_solid {
                    // translucent block so a character under the cursor stays legible
                    base.bg(cola(self.palette.cursor, 0.85))
                } else {
                    base.border_1().border_color(col(self.palette.cursor))
                }
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
                    .children(rows.iter().map(|runs| {
                        div()
                            .flex()
                            .flex_row()
                            .h(px(self.line_height))
                            .children(runs.iter().map(|r| {
                                div()
                                    .bg(col(r.bg))
                                    .text_color(col(r.fg))
                                    .when(r.bold, |d| d.font_weight(FontWeight::BOLD))
                                    .child(SharedString::from(r.text.clone()))
                            }))
                    })),
            )
            .when_some(cursor_el, |this, c| this.child(c))
            .when_some(scrollbar, |this, s| this.child(s));

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .rounded(px(13.)) // match the pane card's inner radius (R_PANEL - border)
            .bg(col(bg))
            .text_color(col(fg))
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

    // ── Row-lock (ConPTY resize-repaint scrollback fix) ───────────────────────
    // Background: growing ConPTY's *rows* makes it repaint and clobber the
    // scrollback alacritty pulled into the grown viewport — verified losing
    // exactly `delta` lines with `tn-cli TN_RESIZE_EXP`. `pty_rows_for` pins a
    // plain shell's ConPTY rows so a divider drag never grows them.

    #[test]
    fn shell_pty_rows_never_grow_on_a_divider_drag() {
        // A local shell's ConPTY is spawned at the floor, so the lock starts at
        // `SHELL_PTY_ROWS_FLOOR`. Dragging the pane smaller or bigger (within the
        // lock) must NOT change the ConPTY row count — no repaint, no blank, no
        // scrollback loss. Columns are handled separately and always exact.
        let lock = SHELL_PTY_ROWS_FLOOR; // seeded at spawn
        assert_eq!(pty_rows_for(false, false, lock, 50), lock, "shrunk pane keeps the lock");
        assert_eq!(pty_rows_for(false, false, lock, 12), lock, "shrinking does not lower it");
        assert_eq!(pty_rows_for(false, false, lock, 90), lock, "growing within the lock keeps it");
    }

    #[test]
    fn shell_pty_rows_only_grow_beyond_the_lock() {
        // Only a pane taller than the current lock (huge monitor) bumps it — a
        // rare one-time grow; thereafter shrinking keeps the new mark.
        let mut lock = SHELL_PTY_ROWS_FLOOR;
        lock = pty_rows_for(false, false, lock, 200); // taller than the floor
        assert_eq!(lock, 200, "exceeds the lock -> raises it");
        assert_eq!(pty_rows_for(false, false, lock, 60), 200, "then shrinking keeps the mark");
    }

    #[test]
    fn agents_and_alt_screen_get_exact_rows() {
        // Agent panes (Ink redraws by height) and fullscreen apps (absolute
        // positioning) must always get the real grid rows, not the lock.
        assert_eq!(pty_rows_for(true, false, 999, 30), 30, "agent: exact");
        assert_eq!(pty_rows_for(false, true, 999, 30), 30, "alt-screen: exact");
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
