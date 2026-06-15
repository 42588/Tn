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
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use futures::channel::mpsc;
use gpui::{
    canvas, div, point, prelude::*, px, relative, rgba, size, AsyncApp, Bounds, ClipboardItem,
    Context, Div, ElementInputHandler, EntityInputHandler, ExternalPaths, FocusHandle, FontWeight,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point,
    ScrollDelta, ScrollWheelEvent, SharedString, UTF16Selection, WeakEntity, Window,
};
use tn_agent::{
    AgentAdapter, AgentCapabilities, AgentDescriptor, AgentEvent, AgentId, AgentRegistry,
    AgentStatus, AiUsage, ExternalProcessAdapter, SidecarLaunch,
};
use tn_blocks::BlockModel;
use tn_config::Loaded;
use tn_core::{CellRun, GridSize, Palette, ResizeAnchoring, Rgb, SelectKind, Terminal};
use tn_pty::{
    remote_cmd::SshCommandService, remote_fs::RemotePath, LocalPty, PtyBackend, PtySize, SpawnSpec,
    SshBackend,
};
use tn_shell::Integration;

use crate::block_view;

/// Emitted when a pane's AI-usage readout changes, so the workspace status bar
/// (which renders the *focused* pane's usage) can repaint without re-rendering
/// on every terminal frame.
pub struct UsageUpdated;

/// Emitted when the change watcher detects filesystem modifications in the pane's
/// cwd — the workspace uses this to refresh the explorer's git-status tags.
pub struct FilesChanged;

/// Emitted when the pane's current working directory changes.
pub struct CwdChanged;

/// Emitted once the pane's child process exits (detected via ConPTY `try_wait`,
/// since ConPTY doesn't reliably EOF the reader). The quick terminal listens for
/// this to fall back to its launcher when the hosted agent/shell exits.
pub struct ProcessExited;

/// Emitted when a changed-file card in the agent activity rail is clicked — the
/// workspace opens that file in Quick Look on the Diff tab (mockup `.ahint`
/// 「点卡片 = 速览全 diff」). Carries the absolute path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum RailFileTarget {
    Local(std::path::PathBuf),
    Remote(crate::remote_git::RemoteGitFile),
}

pub(crate) struct OpenInQuickLook(pub(crate) RailFileTarget);

/// Emitted when this pane's SSH session finishes authenticating and opens its
/// shell — the workspace records the pane's target as a recent connection (A1),
/// tagged with the method that worked.
pub struct SshConnected(pub tn_pty::AuthKind);

/// Emitted when the SSH error card's 重试 is clicked — the workspace re-spawns
/// this pane in place with the same target (C1 retry).
pub struct SshRetryRequested;

/// Emitted when an SSH progress/error card's 取消 / 关闭 is clicked — the
/// workspace closes this pane (B1 cancel / C1 close).
pub struct SshCloseRequested;

/// SSH connection failure detail for the actionable error card (C1).
#[derive(Clone)]
pub(crate) struct SshErrorInfo {
    pub kind: tn_pty::SshErrorKind,
    pub detail: String,
    pub offered: String,
}

/// An in-flight SSH password request (B3 password card): the prompt, an optional
/// previous-attempt error (in-place retry), and the reply channel.
pub(crate) struct SshPasswordPrompt {
    pub prompt: String,
    pub error: Option<String>,
    pub reply: std::sync::mpsc::Sender<tn_pty::PasswordReply>,
}

/// Emitted when the user checks "记住密码" and submits — the workspace caches the
/// password in this pane's spec (session RAM only) so a reconnect/retry skips the
/// prompt (B3).
pub struct SshRememberPassword(pub String);

/// An in-flight host-key trust request (B2 TOFU): the host, its SHA256
/// fingerprint, and the verdict reply channel.
pub(crate) struct SshHostKeyPrompt {
    pub host: String,
    pub fingerprint: String,
    pub reply: std::sync::mpsc::Sender<tn_pty::HostKeyVerdict>,
}

/// An SSH pane's live connection state — the four-phase dot in the header +
/// reconnect banner (B4).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SshConnState {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
}

use crate::perf::PerfStats;
use crate::style::{col, cola};

mod header; // agent pane header UI (avatar / model / usage ring)
mod io; // off-thread workers (reader / repaint / blink / exit-watcher / usage poller)
mod launch; // LaunchSpec: profile -> spawnable pane
pub use launch::is_host_process_path;
pub use launch::FileNamespace;
pub use launch::LaunchSpec;
pub use launch::ShellIntegration;

/// Cached per-frame render data (see docs/修复与优化/基础性能与审查勘误.md), keyed by the engine's
/// [`generation`](tn_core::Terminal::generation). A repaint that changed nothing
/// the grid renders (the ~530ms cursor blink, an unfocused-pane notify) reuses
/// this instead of rebuilding the snapshot + run batches. `rows` is `Rc` so the
/// hit path hands the renderer a cheap clone; the scalars are all the rest of
/// `render` needs (it never touches `snapshot.cells` after `row_runs`).
struct RenderCache {
    generation: u64,
    rows: Rc<Vec<Vec<CellRun>>>,
    pub cursor: (usize, usize),
    pub cursor_shape: tn_core::CursorShape,
    pub cursor_visible: bool,
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
            c(a.black),
            c(a.red),
            c(a.green),
            c(a.yellow),
            c(a.blue),
            c(a.magenta),
            c(a.cyan),
            c(a.white),
            c(a.bright_black),
            c(a.bright_red),
            c(a.bright_green),
            c(a.bright_yellow),
            c(a.bright_blue),
            c(a.bright_magenta),
            c(a.bright_cyan),
            c(a.bright_white),
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
const ACTIVITY_RAIL_W: f32 = 212.0;
/// Below this pane content width the activity rail (本次改动) is dropped so the agent
/// body keeps a usable width instead of being squeezed to nothing / visually covered
/// (窄面板自适应:正文为主,rail 为辅). The 248px rail shell plus a comfortable body
/// minimum — anything narrower and the rail stops earning its space. Restores the
/// instant the pane widens past it; see [`rail_layout_for_width`].
const RAIL_MIN_PANE_W: f32 = 560.0;

fn fit_grid_size_from_bounds(
    bounds_w: f32,
    bounds_h: f32,
    cell_width: f32,
    line_height: f32,
) -> Option<GridSize> {
    if !bounds_w.is_finite()
        || !bounds_h.is_finite()
        || !cell_width.is_finite()
        || !line_height.is_finite()
        || cell_width <= 0.0
        || line_height <= 0.0
    {
        return None;
    }

    let avail_w = bounds_w - 2.0 * BODY_PAD_X;
    let avail_h = bounds_h - 2.0 * BODY_PAD_Y;
    if avail_w < cell_width || avail_h < line_height {
        return None;
    }

    let cols = (avail_w / cell_width).floor() as usize;
    let rows = (avail_h / line_height).floor() as usize;
    Some(GridSize::new(rows, cols))
}

fn cell_at_from_bounds(
    pos_x: f32,
    pos_y: f32,
    bounds_x: f32,
    bounds_y: f32,
    bounds_w: f32,
    bounds_h: f32,
    cell_width: f32,
    line_height: f32,
    size: GridSize,
) -> Option<(usize, usize)> {
    if fit_grid_size_from_bounds(bounds_w, bounds_h, cell_width, line_height).is_none()
        || !pos_x.is_finite()
        || !pos_y.is_finite()
        || !bounds_x.is_finite()
        || !bounds_y.is_finite()
        || pos_x < bounds_x
        || pos_y < bounds_y
        || pos_x > bounds_x + bounds_w
        || pos_y > bounds_y + bounds_h
    {
        return None;
    }
    let grid_x = bounds_x + BODY_PAD_X;
    let grid_y = bounds_y + BODY_PAD_Y;
    let grid_w = size.cols as f32 * cell_width;
    let grid_h = size.rows as f32 * line_height;
    if grid_w <= 0.0
        || grid_h <= 0.0
        || pos_x < grid_x
        || pos_y < grid_y
        || pos_x >= grid_x + grid_w
        || pos_y >= grid_y + grid_h
    {
        return None;
    }
    let x = pos_x - grid_x;
    let y = pos_y - grid_y;
    let col = (x / cell_width) as usize;
    let row = (y / line_height) as usize;
    Some((
        row.min(size.rows.saturating_sub(1)),
        col.min(size.cols.saturating_sub(1)),
    ))
}

fn should_scroll_to_bottom_before_input(offset: usize, _history: usize) -> bool {
    offset > 0
}

/// Width **in cells** of the glyph the cursor sits on at `col`: 2 for a
/// double-width (CJK / wide-emoji) glyph, else 1. `row_runs` emits every wide
/// char as its own `cols == 2` single-char run (it never merges a wide cell with
/// its neighbours), so walking the runs and matching the leading cell is
/// exhaustive. The block cursor uses this so it covers the **whole** glyph — a
/// fixed one-cell block only covers the left half of a wide char, the
/// "光标只覆盖一半 CJK 字" bug seen when moving the cursor back over CJK text.
fn cursor_cell_cols(row: &[tn_core::CellRun], col: usize) -> usize {
    let mut c = 0usize;
    for run in row {
        let step = if run.cols == 2 && run.text.chars().count() == 1 {
            2
        } else {
            1
        };
        for _ in run.text.chars() {
            if c == col {
                return step;
            }
            c += step;
        }
    }
    1
}

fn resize_anchoring_for_pane(agent_active: bool, alt_screen: bool) -> ResizeAnchoring {
    if agent_active || alt_screen {
        ResizeAnchoring::Bottom
    } else {
        ResizeAnchoring::Top
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivityRailLayout {
    None,
    Flex,
    Overlay,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyRegionContainerLayout {
    Direct,
    FlexRow,
    OverlayFlexColumn,
}

fn activity_rail_layout(
    agent_active: bool,
    agent_from_shell: bool,
    git_diff_capable: bool,
) -> ActivityRailLayout {
    if !agent_active || !git_diff_capable {
        ActivityRailLayout::None
    } else if agent_from_shell {
        ActivityRailLayout::Overlay
    } else {
        ActivityRailLayout::Flex
    }
}

/// 窄面板自适应折叠:drop the rail once the pane is too narrow to host it without
/// crushing the agent body. `pane_w` is the *stable* pane content width (independent
/// of whether the rail is currently shown), so this is a single hard threshold with
/// no hysteresis dance — it only flips when the user actually drags across it, and
/// restores automatically on widen.
fn rail_layout_for_width(base: ActivityRailLayout, pane_w: f32) -> ActivityRailLayout {
    if base != ActivityRailLayout::None && pane_w < RAIL_MIN_PANE_W {
        ActivityRailLayout::None
    } else {
        base
    }
}

fn body_region_container_layout(rail_layout: ActivityRailLayout) -> BodyRegionContainerLayout {
    match rail_layout {
        ActivityRailLayout::None => BodyRegionContainerLayout::Direct,
        ActivityRailLayout::Flex => BodyRegionContainerLayout::FlexRow,
        ActivityRailLayout::Overlay => BodyRegionContainerLayout::OverlayFlexColumn,
    }
}

fn terminal_app_owns_viewport(agent_active: bool, alt_screen: bool) -> bool {
    agent_active || alt_screen
}

fn should_render_block_bar(agent_active: bool, alt_screen: bool) -> bool {
    !terminal_app_owns_viewport(agent_active, alt_screen)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScrollWheelRoute {
    MouseReport,
    AppArrows,
    Scrollback,
}

fn scroll_wheel_route(mode: tn_core::InputMode) -> ScrollWheelRoute {
    if mode.mouse_report {
        ScrollWheelRoute::MouseReport
    } else if mode.alt_screen {
        ScrollWheelRoute::AppArrows
    } else {
        ScrollWheelRoute::Scrollback
    }
}

/// `Some(relative)` when `path` lives inside `base`, comparing components
/// case-insensitively so Windows drive-letter / casing differences (`D:\` vs
/// `d:\`) still match. `None` when `path` is outside `base` (or equals it). The
/// remainder keeps the original path's casing and separators. Pure → tested.
fn relative_under(path: &Path, base: &Path) -> Option<String> {
    let mut bc = base.components();
    let mut pc = path.components();
    loop {
        match bc.next() {
            // base exhausted → whatever remains of `pc` is the relative path.
            None => break,
            Some(b) => {
                let p = pc.next()?;
                if !b.as_os_str().eq_ignore_ascii_case(p.as_os_str()) {
                    return None;
                }
            }
        }
    }
    let rel = pc.as_path().to_string_lossy().into_owned();
    (!rel.is_empty()).then_some(rel)
}

/// Render one dropped OS path for injection into the input line. Agent panes pass
/// their working dir as `base` → a path inside the repo becomes **relative**
/// (cleaner for a repo-scoped agent), anything outside falls back to absolute.
/// Plain shells pass `base = None` → always absolute. No quoting (per spec). Pure.
fn render_drop_path(path: &Path, base: Option<&Path>) -> String {
    base.and_then(|b| relative_under(path, b))
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Join dropped paths into the single space-separated string written to the input
/// line (multi-drop → one line, per spec). Empty renders are dropped.
fn render_dropped_paths(paths: &[PathBuf], base: Option<&Path>) -> String {
    paths
        .iter()
        .map(|p| render_drop_path(p, base))
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Encode `count` mouse-wheel events as terminal mouse-report bytes for an app
/// that has enabled mouse tracking (so a TUI like codex scrolls its **own**
/// transcript — its content lives on the alt screen, not in our scrollback).
/// `up` chooses wheel-up (button 64) vs wheel-down (65). `col`/`row` are the
/// 1-based cell coordinates of the pointer. With `sgr` (DEC 1006) each event is
/// `ESC[<b;col;rowM`; otherwise the legacy X10 form `ESC[M` + three `+32` bytes
/// (coordinates capped at 223, the byte-encoding limit). Wheel events are press
/// only — there is no matching release. Pure → unit-tested.
fn encode_wheel_report(up: bool, col: usize, row: usize, sgr: bool, count: usize) -> Vec<u8> {
    let button = if up { 64 } else { 65 };
    let mut out = Vec::new();
    for _ in 0..count.max(1) {
        if sgr {
            out.extend_from_slice(format!("\x1b[<{button};{col};{row}M").as_bytes());
        } else {
            out.extend_from_slice(b"\x1b[M");
            out.push((button + 32) as u8);
            out.push((col.clamp(1, 223) + 32) as u8);
            out.push((row.clamp(1, 223) + 32) as u8);
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ScrollbarThumbStyle {
    width: f32,
    margin_right: f32,
    idle_bg: u32,
    active_bg: u32,
}

fn scrollbar_thumb_style(rail_layout: ActivityRailLayout) -> ScrollbarThumbStyle {
    if rail_layout == ActivityRailLayout::Flex {
        ScrollbarThumbStyle {
            width: 3.,
            margin_right: 3.,
            idle_bg: 0xffffff08,
            active_bg: 0xffffff3a,
        }
    } else {
        ScrollbarThumbStyle {
            width: 5.,
            margin_right: 2.,
            idle_bg: crate::style::H2, // SHEET 02 SPEC:thumb = h2
            active_bg: 0xffffff66,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SelectionDragMove {
    Idle,
    EndLostMouseUp,
    Update,
}

fn selection_drag_move(selecting: bool, pressed: Option<MouseButton>) -> SelectionDragMove {
    if !selecting {
        SelectionDragMove::Idle
    } else if pressed == Some(MouseButton::Left) {
        SelectionDragMove::Update
    } else {
        SelectionDragMove::EndLostMouseUp
    }
}

/// Trailing-edge debounce(静默窗口) for the working-tree change watcher: refresh the
/// rail `git diff` only after file events have been quiet this long. A single save fires
/// ~one window later (responsive); a long build's continuous event stream keeps pushing
/// it back, so the rail refreshes once after the build settles instead of every window.
/// 300ms balances responsiveness with coalescing (was 1000ms 固定窗口 — 审查③: 1000ms
/// 钝化手动编辑响应, 且固定窗口在长构建时每窗口刷一次).
const RAIL_WATCH_DEBOUNCE_MS: u64 = 300;
/// Cursor blink half-period (see docs/产品体验/终端交互体验.md). ~530ms matches common terminals.
const CURSOR_BLINK_MS: u64 = 530;
/// Smooth cursor glide (see docs/产品体验/终端交互体验.md): the cursor eases toward its new cell over
/// this window instead of teleporting, so typing/deleting reads as fluid. Short
/// enough to feel responsive (the glyph is already there; only the block catches up).
const CURSOR_GLIDE_MS: u64 = 90;
/// Only glide *small* same-row moves (typing / deleting / local nav). Bigger jumps
/// (line wrap, prompt redraw, screen clear, vertical nav) snap — a long swoosh
/// across the grid looks worse than an honest jump.
const CURSOR_GLIDE_MAX_COLS: i64 = 12;
/// Visual-bell flash duration (see docs/产品体验/终端交互体验.md): a short fade so a bell registers
/// without being a distraction. ~180ms ≈ a quick blink.
const BELL_FLASH_MS: u64 = 180;
/// Sentinel window title a hosted **agent** pane emits *after* the agent exits
/// (the `-NoExit` pwsh runs it on return). The reader sees this OSC, flags the
/// pane, and we clear the agent identity — so the header/tab stop pretending the
/// (now-gone) agent is still running. See [`launch::LaunchSpec`] + `io::spawn_reader`.
pub(super) const AGENT_EXIT_SENTINEL: &str = "TN::agent-exited";

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Activity-rail「本次改动」state machine — keeps the UI render path a pure
/// read of an already-resolved state; no git/io inside `render()`. The enum
/// replaces ad-hoc `Vec` + `bool` flags so the render can distinguish between
/// "haven't run yet" (Idle), "background is computing" (Loading → skeleton),
/// and "data is ready" (Ready → real cards).
#[derive(Debug, Clone)]
pub enum RailState {
    /// No agent present (plain shell) → rail not rendered at all.
    Idle,
    /// Background git diff is in flight; UI draws a skeleton placeholder.
    Loading,
    /// Fresh data has arrived. `root` is the git working directory (paths in
    /// `files` are relative to it; used to resolve click→QuickLook absolute paths).
    Ready {
        files: Vec<crate::gitutil::FileChange>,
        source: RailSource,
    },
}

#[derive(Debug, Clone)]
pub enum RailSource {
    Local {
        root: std::path::PathBuf,
    },
    Remote {
        cfg: tn_pty::SshConfig,
        root: RemotePath,
    },
}

impl RailSource {
    fn target_for(&self, path: &str) -> RailFileTarget {
        match self {
            Self::Local { root } => RailFileTarget::Local(root.join(path)),
            Self::Remote { cfg, root } => {
                RailFileTarget::Remote(crate::remote_git::RemoteGitFile {
                    cfg: cfg.clone(),
                    root: root.clone(),
                    path: path.to_string(),
                })
            }
        }
    }
}

/// A network-reaching telemetry sidecar awaiting user confirmation before it
/// spawns — the default-deny network gate ([`SidecarLaunch::Confirm`]). Carries
/// the descriptor the confirm action needs to start the [`ExternalProcessAdapter`].
#[derive(Clone)]
struct SidecarConfirm {
    descriptor: AgentDescriptor,
}

pub struct TerminalView {
    terminal: Arc<Mutex<Terminal>>,
    writer: SharedWriter,
    writer_tx: std::sync::mpsc::Sender<Vec<u8>>,
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
    // Screen-space width of the whole pane, captured each paint. Stable w.r.t. the
    // activity rail's presence (the pane is full-width inside a flex-col, so showing
    // or hiding the rail never changes it), so the rail's窄面板自适应折叠 can gate on
    // it without oscillating frame-to-frame. Default `f32::MAX` = "unmeasured →
    // keep the rail" so the first frame never flashes the body full-width.
    pane_width: Rc<RefCell<f32>>,
    // Warp-style command blocks, built from the shell-integration bypass.
    blocks: Arc<Mutex<BlockModel>>,
    // The last CWD sent to the workspace/explorer tree (to filter redundant updates).
    last_cwd: Option<String>,
    // Live palette copy (for block-bar colors); kept in sync with the engine.
    palette: Palette,
    // True while a left-drag selection is in progress.
    selecting: bool,
    focused_once: bool,
    // Cursor blink: `cursor_on` is the current blink phase,
    // toggled ~530ms by the blink task *only while focused*; `focused` caches the
    // last render's focus so the task knows whether to blink (and so an unfocused
    // pane stays idle — zero wakes). Typing forces `cursor_on = true`.
    cursor_on: bool,
    focused: bool,
    // Smooth cursor glide: `cursor_px` is the cursor block's
    // currently-drawn top-left (term-area coords, incl. BODY_PAD); it eases toward
    // the target cell instead of teleporting. `cursor_cell` caches the last target
    // (so a move is detected), `cursor_glide_from`/`cursor_glide_start` define the
    // in-flight ease, and `cursor_gliding` guards the per-frame driver task. Init
    // `cursor_cell` to a sentinel so the first frame snaps (no glide-from-origin).
    cursor_px: (f32, f32),
    cursor_cell: (usize, usize),
    cursor_anim_start: Option<Instant>,
    cursor_action_forward: bool,
    cursor_gliding: bool,
    // While dragging the scrollbar thumb: the grab offset (cursor Y − thumb top,
    // px) so the thumb tracks under the cursor. `None` when not dragging.
    scrollbar_drag: Option<f32>,
    // AI usage for this pane (M4): the agent it hosts + its latest usage
    // snapshot, polled off-thread from the agent's session log. `AgentId` is the
    // open identity resolved through the registry — no closed enum.
    agent: Option<AgentId>,
    /// Hosted inside the ghost terminal (SHEET 04):幽灵窗自带 GHOST_ 头,
    /// shell 板头由幽灵头取代(agent 头保留 — 用量环不可丢)。
    ghost_chrome: bool,
    usage: Option<AiUsage>,
    /// Realtime event state from external/sidecar adapters. Built-in log-only
    /// adapters leave these empty; the header renders them only when present.
    agent_status: Option<AgentStatus>,
    agent_model: Option<String>,
    agent_transcript_tail: Option<String>,
    agent_permission_prompt: Option<String>,
    agent_error: Option<String>,
    /// Per-pane realtime telemetry adapter — a sidecar [`ExternalProcessAdapter`]
    /// spawned when this agent's manifest declared `sidecar`. Owned here so the
    /// child process is killed on `clear_agent` / view drop; the agent-event
    /// poller drains it into [`reduce_agent_event`](Self::reduce_agent_event).
    realtime_adapter: Option<Arc<dyn AgentAdapter>>,
    /// A networked sidecar pending user confirmation before spawning (default-deny
    /// network gate). `None` once spawned, denied, or for local sidecars.
    sidecar_confirm: Option<SidecarConfirm>,
    // Activity-rail「本次改动」state machine (mockup `.arail`).
    // Replaces ad-hoc `Vec` + `Option<PathBuf>` — the render path reads this
    // pure enum; zero computation inside `render()`. The `files`/`preview`/`root`
    // live inside `RailState::Ready` so they are always consistent with each other.
    pub(super) rail_state: RailState,
    /// Monotonic generation counter: incremented each time a background refresh
    /// is kicked off. The task captures the generation at spawn; on completion
    /// it is checked against `rail_generation` — stale results (from a previous
    /// refresh that finished after a newer one was already dispatched) are
    /// silently dropped. Wrapping on overflow (32-bit on 64-bit hosts → fine).
    rail_generation: usize,
    /// The directory the change watcher was started on (app cwd at launch, or
    /// the shell cwd for shell-typed agents). Used as a fallback in
    /// `refresh_changes` when the blocks model has no known cwd (launched
    /// agent panes carry no shell integration, so OSC 7 never fires).
    rail_cwd: Option<std::path::PathBuf>,
    spawn_cwd: Option<std::path::PathBuf>,
    file_namespace: FileNamespace,
    ssh_cfg: Option<tn_pty::SshConfig>,
    /// `true` when `agent` was inferred from a **typed shell command** (the user ran
    /// `claude`/`codex` at a plain-shell prompt — detected via shell-integration's
    /// command line, not a fragile process walk) rather than from launch intent.
    /// Such an agent is cleared when its command block finishes (vs launch-intent
    /// agents, which clear on the [`AGENT_EXIT_SENTINEL`]).
    agent_from_shell: bool,
    /// When true, the render reserves 212 px for the activity rail from
    /// the start so the terminal never resizes when sync_shell_agent
    /// promotes a shell to an agent, avoiding input lag and the
    /// stuck-first-character bug.
    #[allow(dead_code)]
    integrate_pwsh: bool,
    /// Working-tree change watcher for the activity rail (本次改动): fires `git diff`
    /// on file changes (变化即刷新). `Some` only while this pane is an agent; dropping
    /// it stops watching. Stored so it outlives `new` (a dropped watcher = no events).
    change_watcher: Option<notify::RecommendedWatcher>,
    // Set by the reader when a hosted agent emits [`AGENT_EXIT_SENTINEL`] on exit
    // (the `-NoExit` pwsh outlives it). The foreground then clears `agent`/`usage`
    // so the pane reverts to a plain shell (no stale header). Only agent panes
    // emit the sentinel, so a plain shell never trips this.
    agent_exited: Arc<AtomicBool>,
    // While true, the PTY reader strips CSI 3 J (erase saved lines) before it
    // reaches alacritty, so main-screen agent repaints can clear the visible grid
    // without deleting Tn's scrollback.
    preserve_scrollback_clears: Arc<AtomicBool>,
    // Set by the reader on a BEL byte; the foreground turns the
    // false->true edge into a flash/beep, then clears it. An atomic (not a wake
    // event) so a bell during a quiet moment still rides the next repaint.
    bell: Arc<AtomicBool>,
    // When a visual bell is mid-fade: the instant it rang (drives the overlay
    // opacity). `None` when no flash is showing. `bell_fading` guards against
    // spawning more than one fade task at a time (a bell storm just refreshes
    // `bell_flash_at`).
    bell_flash_at: Option<Instant>,
    bell_fading: bool,
    /// 会话时钟原点(≈ reader 线程的块事件时钟):RUN 块实时耗时 = 现在 − started_at。
    session_clock: Instant,
    /// 运行中块的 200ms 重绘 ticker 是否在跑(防重复 spawn;静默长命令也走表)。
    block_ticking: bool,
    // `[appearance]` bell prefs, resolved once at construction.
    visual_bell: bool,
    audio_bell: bool,
    // Loaded config kept so a shell-typed agent (sync_shell_agent) can re-resolve
    // its billing override (`General::billing_for`) and themed accent when it
    // appears at runtime — agent-agnostic, via the AgentId, no per-agent field.
    config: Arc<Loaded>,
    // Global starting usage-pill mode (`[general].billing_mode`); per-agent override
    // is resolved on demand from `config` by AgentId.
    billing_mode: tn_config::BillingMode,
    // Live per-pane usage-pill display mode ($ / % / tokens). Starts from the
    // config default for this pane's agent (auto-resolved via usage_display) and
    // is cycled in memory when the user clicks the pill — independent per pane.
    usage_mode: tn_config::BillingMode,
    // 额度面板:clicking the usage readout opens a popover with windowed spend
    // (5h/日/周, from `tn_ai::usage_windows`, aggregated across all Claude sessions —
    // account-wide, not per-pane). Computed off-thread on open; `quota_gen` drops
    // stale background results. `None` = not yet computed.
    quota_open: bool,
    quota: Option<tn_ai::usage_windows::UsageWindows>,
    quota_gen: usize,
    // Resolved agent presentation, recomputed whenever `agent` changes (construction,
    // sync_shell_agent, clear_agent) from the descriptor + theme — so the render path
    // never names a concrete agent. `agent_accent` falls back to `ui_accent` for a
    // plain shell; `agent_label` is the descriptor label; `agent_manages_cursor` is
    // the Ink-cursor quirk (replaces the Claude-only `force_hide_cursor`).
    agent_accent: Rgb,
    agent_label: Option<SharedString>,
    agent_short: Option<SharedString>,
    agent_manages_cursor: bool,
    // Declared capabilities of the current agent (descriptor) → which Universal
    // Agent Surface slots render: `usage` gates the header ring/pill, `git_diff`
    // the activity rail. A config-level agent (no adapter) hosts as a terminal
    // with the rail but no usage. All-false for a plain shell.
    agent_caps: AgentCapabilities,
    ui_accent: Rgb,
    // Chrome text colors for pane headers (mockup .phead/.nm/.model use ui.*, not
    // the terminal palette). fg-dim has no theme token → literal in header.rs.
    ui_fg: Rgb,
    ui_muted: Rgb,
    // ANSI accents for the SSH connection cards (done ✓ green / error red / hint
    // yellow). Pulled from the theme like ui_accent.
    ui_green: Rgb,
    ui_red: Rgb,
    ui_yellow: Rgb,
    // Launch program (e.g. "powershell.exe") — for a clean shell label.
    program: String,
    // IME composition (preedit) text, set by the platform input handler while the
    // user is composing (e.g. pinyin → 中文). `Some` ⇒ gpui treats us as composing
    // and routes keys to the IME; on commit the result is written to the PTY and
    // this clears. Without an input handler, IME-composed text never arrives — the
    // root cause of "终端无法输入中文" (only ASCII `key_char` reached `encode_key`).
    ime_marked: Option<String>,
    // Cached render data + the engine generation it was built from (see
    // docs/修复与优化/基础性能与审查勘误.md). Reused when a repaint changed nothing
    // renderable (cursor blink).
    render_cache: Option<RenderCache>,
    // Opt-in render instrumentation (TN_PERF): render rate + cache hit-rate +
    // rebuild timing, logged to `tn::perf` ~1/s.
    perf: PerfStats,
    // SSH password prompt state (M2b). When present, the UI renders a GPUI
    // floating input above the terminal, routing keystrokes to `ssh_password_input`.
    ssh_password_prompt: Option<SshPasswordPrompt>,
    ssh_password_input: String,
    // B3: reveal (eye) toggle + "remember for this session" checkbox state.
    ssh_password_reveal: bool,
    ssh_password_remember: bool,
    // SSH target label (`user@host:port`) shown on the connection cards.
    ssh_target: String,
    // Current SSH phase while connecting (B1 progress card); cleared once the
    // shell opens (`Connected`) or the attempt fails.
    ssh_progress: Option<(tn_pty::SshPhase, String)>,
    // SSH failure detail for the actionable error card (C1); `None` = no error.
    ssh_error: Option<SshErrorInfo>,
    // B2 TOFU: pending host-key trust prompt + the "记住(写 known_hosts)" toggle.
    ssh_hostkey: Option<SshHostKeyPrompt>,
    ssh_hostkey_remember: bool,
    // B4: live connection state for SSH panes (None = not an SSH pane). Drives the
    // header's four-phase dot + the reconnect banner.
    ssh_conn: Option<SshConnState>,
    // C3 success feedback: the auth method that succeeded, appended to the
    // header's "已连接" label (密钥 / 密码 / 交互).
    ssh_conn_method: Option<tn_pty::AuthKind>,
    // ── 渲染重复诊断探针(`TN_AGENT_DUP=1`,默认关) ─────────────────────────
    // 内联 agent(Claude/Codex)正文重复/堆叠的归因工具:打印每次 ConPTY
    // resize 与每次 resize_conpty 长高时 `scroll_up` 把活帧推进 scrollback
    // 的细节。env 门控,关时零成本。
    dup_probe: bool,
    // 上一帧活动栏是否在场(`agent.is_some() && caps.git_diff`),用于探针检测
    // agent rail 出现/消失。shell-detected agent 走 overlay,不应改变正文宽度。
    dup_last_rail: bool,
    // 上一帧光标位置(row, col),用于探针直接捕捉「光标跳回 row 0」这个 bug3 现象,
    // 无论它是 resize / swap 恢复 / ConPTY 自发 repaint 引起。仅探针开启时有意义。
    dup_last_cursor: (usize, usize),
    // 探针:本帧是否触发了 resize(供光标跳变那行日志标注「是不是 resize 引起的」)。
    dup_resized_this_frame: bool,
    // Diagnostic frames after a shell command is promoted to an agent surface.
    // Kept tiny and one-shot so default info logging stays useful without noise.
    agent_surface_probe_frames: u8,
}

/// A clean shell name from a program path (`…\powershell.exe` → `pwsh`).
/// Convert a Windows path to its WSL `/mnt/...` equivalent.
/// `C:\Users\Gua\..` becomes `/mnt/c/Users/Gua/..`.
fn windows_to_wsl_path(path: &std::path::Path) -> String {
    let s = path.to_string_lossy();
    let s = s.replace('\\', "/");
    if s.len() >= 2 && s.as_bytes()[1] == b':' {
        let drive = (s.as_bytes()[0] as char).to_ascii_lowercase();
        format!("/mnt/{}{}", drive, &s[2..])
    } else {
        s
    }
}

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

fn tail_chars(s: &str, max_chars: usize) -> String {
    let total = s.chars().count();
    if total <= max_chars {
        s.to_string()
    } else {
        s.chars().skip(total - max_chars).collect()
    }
}

/// Last-resort local pwsh, used when the intended backend can't spawn — keeps
/// pane construction infallible (it runs in GPUI's non-unwinding callback, where
/// a panic would abort the process).
fn fallback_pwsh(size: PtySize) -> LocalPty {
    LocalPty::spawn(&SpawnSpec::program("powershell.exe").arg("-NoLogo"), size)
        .expect("fallback pwsh spawn failed")
}

/// Resolved per-pane agent presentation (from descriptor + theme), produced by
/// [`TerminalView::resolve_agent_view`] and cached on the view so the render path
/// never re-resolves or names a concrete agent.
struct AgentView {
    accent: Rgb,
    /// Full descriptor label for the agent header (e.g. "Claude Code").
    label: SharedString,
    /// Short descriptor label for the tab (e.g. "Claude").
    short: SharedString,
    /// The agent paints its own cursor (Ink TUI) → hide ours.
    manages_cursor: bool,
    /// Declared capabilities → which surface slots render.
    caps: AgentCapabilities,
    usage_mode: tn_config::BillingMode,
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>, launch: LaunchSpec) -> Self {
        // Captured before spawning the agent so its session log (created moments
        // later) is reliably newer — that's how the usage poller binds to THIS
        // pane's session and never a pre-existing one (see spawn_usage_poller).
        let launched_at = SystemTime::now();
        // Where this pane runs (AgentRuntimeKind) — distinct from its file
        // namespace. PTY family only for now; logged so "which runtime hosts this
        // agent" is visible without assuming a local process.
        tracing::debug!(
            program = %launch.program,
            runtime = ?launch.runtime(),
            namespace = ?launch.file_namespace,
            "spawning pane",
        );
        let size = GridSize::new(ROWS, COLS);
        let pty_size = PtySize::new(size.rows as u16, size.cols as u16);
        // Pick the backend: a remote SSH session, or a local ConPTY. A bad
        // profile must NOT crash the app — pane construction runs inside GPUI's
        // window callback (non-unwinding), so a spawn panic aborts the whole
        // process; fall back to a plain pwsh instead.
        let mut pty: Box<dyn PtyBackend> = if let Some(cfg) = &launch.ssh {
            let mut ssh_cfg = cfg.clone();
            if std::env::var("TN_NO_SHELL_INTEGRATION").is_err() {
                ssh_cfg.shell_init = Some(Integration::new().ssh_init_cmd());
            }
            match SshBackend::spawn(ssh_cfg, pty_size) {
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
            for (k, v) in &launch.env {
                spec = spec.env(k, v);
            }
            if launch.file_namespace == FileNamespace::Host {
                if let Some(cwd) = &launch.cwd {
                    spec = spec.cwd(cwd);
                }
            }
            if std::env::var("TN_NO_SHELL_INTEGRATION").is_err() {
                match launch.shell_integration {
                    Some(ShellIntegration::Pwsh) => {
                        spec = spec
                            .arg("-NoExit")
                            .arg("-EncodedCommand")
                            .arg(Integration::new().encoded_command());
                    }
                    Some(ShellIntegration::Bash) => {
                        let integration = Integration::new();
                        let script = integration.bash();
                        let temp_dir = std::env::temp_dir();
                        let temp_file = temp_dir.join(format!("tn-bash-{}.sh", integration.nonce));
                        std::fs::write(&temp_file, script.as_bytes())
                            .expect("write bash integration temp file");
                        let wsl_path = windows_to_wsl_path(&temp_file);
                        spec = spec.arg("--").arg("bash").arg("--rcfile").arg(wsl_path);
                    }
                    None => {}
                }
            }
            Box::new(LocalPty::spawn(&spec, pty_size).unwrap_or_else(|e| {
                tracing::error!(program = %launch.program, "spawn failed: {e}; falling back to pwsh");
                fallback_pwsh(pty_size)
            }))
        };
        // Starts false: the first read's false->true transition sends the first
        // wake. SSH backend-only events use the same path so password/TOFU cards
        // appear even when no terminal bytes arrive.
        let dirty = Arc::new(AtomicBool::new(false));
        // Reader/backend -> foreground wake channel. `dirty` dedupes so at most one wake
        // is in flight; the foreground drains it and notifies once per frame.
        let (wake_tx, wake_rx) = mpsc::unbounded::<()>();
        {
            let dirty = dirty.clone();
            let wake_tx = wake_tx.clone();
            pty.set_waker(Box::new(move || {
                if !dirty.swap(true, Ordering::Relaxed) {
                    let _ = wake_tx.unbounded_send(());
                }
            }));
        }
        let reader = pty.take_reader().expect("pty reader");
        let writer: SharedWriter = Arc::new(Mutex::new(pty.writer().expect("pty writer")));
        let (writer_tx, writer_rx) = std::sync::mpsc::channel::<Vec<u8>>();
        let w_thread_writer = writer.clone();
        std::thread::spawn(move || {
            while let Ok(bytes) = writer_rx.recv() {
                if let Ok(mut w) = w_thread_writer.lock() {
                    let _ = w.write_all(&bytes);
                    let _ = w.flush();
                }
            }
        });
        let pty = Arc::new(Mutex::new(pty));

        // Build the engine with the configured scrollback + theme palette.
        let palette = palette_from(&config.theme);
        let to_rgb = |c: tn_config::Color| Rgb::new(c.r, c.g, c.b);
        let ui_accent = to_rgb(config.theme.ui.accent);
        let ui_fg = to_rgb(config.theme.ui.foreground);
        let ui_muted = to_rgb(config.theme.ui.muted);
        let ui_green = to_rgb(config.theme.ansi.green);
        let ui_red = to_rgb(config.theme.ansi.red);
        let ui_yellow = to_rgb(config.theme.ansi.yellow);
        let ssh_target = launch
            .ssh
            .as_ref()
            .map(|c| crate::ssh_recents::format_target(&c.user, &c.host, c.port))
            .unwrap_or_default();
        let visual_bell = config.config.appearance.visual_bell;
        let audio_bell = config.config.appearance.audio_bell;
        let billing_mode = config.config.general.billing_mode;
        let mut term = Terminal::with_scrollback(size, config.config.general.scrollback_lines);
        term.set_palette(palette);
        let terminal = Arc::new(Mutex::new(term));
        let blocks = Arc::new(Mutex::new(BlockModel::new()));
        let title = Arc::new(Mutex::new(None));
        let agent = launch.agent.clone();
        let registry = crate::agent_host::agent_registry(cx);
        let agent_exited = Arc::new(AtomicBool::new(false));
        let preserve_scrollback_clears = Arc::new(AtomicBool::new(agent.is_some()));
        let bell = Arc::new(AtomicBool::new(false));
        let cmd_done = Arc::new(AtomicBool::new(false));

        Self::spawn_reader(
            reader,
            terminal.clone(),
            writer_tx.clone(),
            dirty.clone(),
            wake_tx,
            title.clone(),
            blocks.clone(),
            agent_exited.clone(),
            bell.clone(),
            cmd_done.clone(),
            preserve_scrollback_clears.clone(),
            registry.clone(),
        );
        Self::spawn_repaint_loop(cx, dirty.clone(), wake_rx, cmd_done);
        Self::spawn_blink_loop(cx);
        // Watch the child so a pane (esp. the quick terminal) can react to its
        // shell/agent exiting. Harmless for the main window (no subscriber).
        Self::spawn_exit_watcher(cx, pty.clone());

        // Per-pane AI usage poller — ONLY for a pane launched AS an agent (launch
        // intent). A plain shell must not masquerade as Claude/Codex just because
        // a fresh agent session exists for this cwd: that agent is often a
        // *separate* process (e.g. the dev's own Claude Code editing this repo).
        // So a plain pwsh pane stays a shell (no agent header, no usage).
        // The app-wide registry resolves this agent's descriptor (presentation +
        // capabilities + cursor quirk) and adapter (usage telemetry). Everything
        // below reads the descriptor/adapter — no concrete agent is named.
        // Resolve the agent's presentation + capabilities + starting pill mode once.
        let (agent_accent, agent_label, agent_short, agent_manages_cursor, agent_caps, usage_mode) =
            match &agent {
                Some(id) => {
                    let v =
                        Self::resolve_agent_view(id, &config, &registry, ui_accent, billing_mode);
                    (
                        v.accent,
                        Some(v.label),
                        Some(v.short),
                        v.manages_cursor,
                        v.caps,
                        v.usage_mode,
                    )
                }
                None => (
                    ui_accent,
                    None,
                    None,
                    false,
                    AgentCapabilities::default(),
                    tn_config::BillingMode::default(),
                ),
            };
        // For launched agents: stash the launch cwd so refresh_changes has a fallback
        // when the blocks model returns no cwd (no shell integration -> no OSC 7).
        let rail_cwd = if launch.file_namespace == FileNamespace::Host {
            launch.cwd.clone().or_else(|| std::env::current_dir().ok())
        } else {
            None
        };
        let mut change_watcher = None;
        let mut realtime_adapter: Option<Arc<dyn AgentAdapter>> = None;
        let mut sidecar_confirm: Option<SidecarConfirm> = None;
        if let Some(id) = &agent {
            // Usage binds to the session THIS pane launches (newest log created
            // at/after `launched_at`), not whatever's newest in the cwd — a dev
            // Claude editing this very repo must not hijack a fresh pane's readout
            // (see tn_ai::resolve_pane_session). cwd-independent: hosted agent panes
            // carry no shell integration, so the agent's cwd is unknowable. Only an
            // agent with a usage adapter is polled — a config-level agent (no
            // adapter) hosts fine but reports no usage.
            if let Some(adapter) = registry.adapter(id) {
                Self::spawn_usage_poller(cx, adapter.clone(), launched_at);
                if adapter.has_realtime_events() {
                    Self::spawn_agent_event_poller(cx, adapter.clone());
                }
            }
            // 活动栏「本次改动」still needs a working dir for `git diff` (变化即刷新).
            if let Some(cwd) = rail_cwd.clone() {
                change_watcher = Self::spawn_change_watcher(cx, cwd);
            }
            // Config-declared realtime sidecar (the observation tier without a
            // built-in adapter): a local stdio sidecar spawns now; a networked one
            // stages a user confirmation (default-deny). The per-pane adapter owns
            // the child (killed on clear_agent / drop); its event poller feeds the
            // agent header. Only for launched agents — shell-promoted agents
            // (sync_shell_agent) don't spawn sidecars.
            if let Some(desc) = registry.get(id) {
                match desc.sidecar_launch() {
                    SidecarLaunch::SpawnNow => {
                        realtime_adapter = Self::spawn_sidecar(cx, desc.clone());
                    }
                    SidecarLaunch::Confirm => {
                        sidecar_confirm = Some(SidecarConfirm {
                            descriptor: desc.clone(),
                        });
                    }
                    SidecarLaunch::None => {}
                }
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
            writer_tx,
            pty,
            focus_handle: cx.focus_handle(),
            size,
            cell_width,
            font_family,
            font_size,
            line_height,
            title,
            content_bounds: Rc::new(RefCell::new(Bounds::default())),
            pane_width: Rc::new(RefCell::new(f32::MAX)),
            blocks,
            last_cwd: None,
            palette,
            selecting: false,
            focused_once: false,
            cursor_on: true,
            focused: false,
            cursor_px: (0.0, 0.0),
            cursor_cell: (usize::MAX, usize::MAX), // sentinel → first frame snaps
            cursor_anim_start: None,
            cursor_action_forward: true,
            cursor_gliding: false,
            scrollbar_drag: None,
            agent,
            ghost_chrome: false,
            usage: None,
            agent_status: None,
            agent_model: None,
            agent_transcript_tail: None,
            agent_permission_prompt: None,
            agent_error: None,
            realtime_adapter,
            sidecar_confirm,
            rail_state: RailState::Idle,
            rail_generation: 0,
            quota_open: false,
            quota: None,
            quota_gen: 0,
            rail_cwd,
            agent_from_shell: false,
            spawn_cwd: launch.cwd.clone(),
            file_namespace: launch.file_namespace.clone(),
            ssh_cfg: launch.ssh.clone(),
            integrate_pwsh: launch.integrate_pwsh,
            change_watcher,
            agent_exited,
            preserve_scrollback_clears,
            bell,
            bell_flash_at: None,
            bell_fading: false,
            session_clock: Instant::now(),
            block_ticking: false,
            visual_bell,
            audio_bell,
            config: config.clone(),
            billing_mode,
            usage_mode,
            agent_accent,
            agent_label,
            agent_short,
            agent_manages_cursor,
            agent_caps,
            ui_accent,
            ui_fg,
            ui_muted,
            ui_green,
            ui_red,
            ui_yellow,
            program: launch.program.clone(),
            ime_marked: None,
            render_cache: None,
            perf: PerfStats::new("pane.render"),
            ssh_password_prompt: None,
            ssh_password_input: String::new(),
            ssh_password_reveal: false,
            ssh_password_remember: false,
            ssh_target,
            ssh_progress: None,
            ssh_error: None,
            ssh_hostkey: None,
            ssh_hostkey_remember: true,
            ssh_conn: launch.ssh.as_ref().map(|_| SshConnState::Connecting),
            ssh_conn_method: None,
            dup_probe: std::env::var("TN_AGENT_DUP").is_ok(),
            dup_last_rail: false,
            dup_last_cursor: (usize::MAX, usize::MAX),
            dup_resized_this_frame: false,
            agent_surface_probe_frames: 0,
        }
    }

    /// The focus handle for this pane, so the workspace can route focus.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// This pane's agent (from launch intent, or detected from a typed shell
    /// command). An open [`AgentId`] resolved through the registry.
    pub fn agent(&self) -> Option<AgentId> {
        self.agent.clone()
    }

    /// Explicitly clear the GPUI render cache to free the massive `gpui::Div` trees
    /// and row states when this terminal tab goes into the background (inactive).
    pub fn clear_render_cache(&mut self, cx: &mut Context<Self>) {
        // First reap any prior async swap: if the off-thread serialize finished and
        // no new output arrived since the clone, this blanks the live grid to finally
        // free it. Unconditional — `render_cache` may already be None from an earlier
        // eviction, so it can't sit inside the `is_some()` guard below. (项34 缺陷①)
        self.terminal.lock().unwrap().try_finish_swap();
        if self.render_cache.is_some() {
            self.render_cache = None;

            // Kick off an async grid swap: clone under the lock (~ms memcpy), then
            // serialize + write off-thread (the slow part leaves the UI thread, so
            // switching tabs no longer stalls on a big grid). The grid is actually
            // freed by the next eviction's `try_finish_swap` above; until then it
            // stays live, so restore/advance never see a half-swapped state.
            let id = cx.entity_id().as_u64();
            let mut path = std::env::temp_dir();
            path.push("tn");
            let _ = std::fs::create_dir_all(&path);
            path.push(format!("scrollback_{id}.bin"));
            self.terminal.lock().unwrap().swap_out_async(path);

            cx.notify();
        }
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

    /// Process a background event from the PTY backend.
    pub(super) fn handle_pty_event(&mut self, ev: tn_pty::PtyEvent, cx: &mut Context<Self>) {
        match ev {
            tn_pty::PtyEvent::NeedPassword {
                prompt,
                error,
                reply,
            } => {
                // A re-prompt (error.is_some()) keeps the connection alive (B3); only
                // the typed text resets. reveal/remember persist across retries.
                self.ssh_password_prompt = Some(SshPasswordPrompt {
                    prompt,
                    error,
                    reply,
                });
                self.ssh_password_input.clear();
                self.ssh_progress = None; // password card takes over from the progress card
                cx.notify();
            }
            tn_pty::PtyEvent::SshProgress { phase, detail } => {
                // B1 progress card: advance the connection step. A new attempt
                // clears any stale error card.
                self.ssh_progress = Some((phase, detail));
                self.ssh_error = None;
                // B4: progress after a prior connect/drop = a reconnect (slim
                // banner, no big card); the very first connect = Connecting.
                self.ssh_conn = self.ssh_conn.map(|s| match s {
                    SshConnState::Connecting => SshConnState::Connecting,
                    _ => SshConnState::Reconnecting,
                });
                cx.notify();
            }
            tn_pty::PtyEvent::SshFailed {
                kind,
                detail,
                offered,
            } => {
                // C1 error card: stop the progress card, show the actionable error.
                self.ssh_progress = None;
                self.ssh_error = Some(SshErrorInfo {
                    kind,
                    detail,
                    offered,
                });
                cx.notify();
            }
            tn_pty::PtyEvent::NeedHostKeyConfirm {
                host,
                fingerprint,
                reply,
            } => {
                // B2 TOFU: pause on the trust panel (the progress card hides).
                self.ssh_hostkey = Some(SshHostKeyPrompt {
                    host,
                    fingerprint,
                    reply,
                });
                self.ssh_hostkey_remember = true;
                self.ssh_progress = None;
                cx.notify();
            }
            tn_pty::PtyEvent::Connected { method } => {
                // Authenticated + shell open → drop the progress card, and let the
                // workspace record this as a recent SSH connection (A1). Workspace
                // knows the target via pane_specs.
                self.ssh_progress = None;
                self.ssh_error = None;
                self.ssh_conn = self.ssh_conn.map(|_| SshConnState::Connected);
                self.ssh_conn_method = Some(method); // C3: surface 密钥/密码 in header

                // Inject prompt command for remote bash/zsh to report CWD changes
                // Define `__tn_pc` (emits OSC 633;P;Cwd), hook it into the prompt,
                // **and call it once immediately** so the remote cwd is reported the
                // moment this runs — without waiting for the next prompt (the first
                // prompt may already be drawn). `TerminalView::cwd()` reads the
                // model-level cwd from this bare P;Cwd (no A/B/C/D markers needed).
                let integration_cmd = " if [ -n \"$BASH_VERSION\" ]; then __tn_pc() { printf '\\033]633;P;Cwd=%s\\007' \"$PWD\"; }; PROMPT_COMMAND=\"__tn_pc;${PROMPT_COMMAND:-}\"; __tn_pc; elif [ -n \"$ZSH_VERSION\" ]; then __tn_pc() { printf '\\033]633;P;Cwd=%s\\007' \"$PWD\"; }; typeset -ag precmd_functions; if [[ -z ${(M)precmd_functions:#__tn_pc} ]]; then precmd_functions+=(__tn_pc); fi; __tn_pc; fi\r";
                self.send_bytes(integration_cmd.as_bytes());

                cx.emit(SshConnected(method));
                cx.notify();
            }
            tn_pty::PtyEvent::Disconnected => {
                // B4: connection dropped — the backend auto-reconnects after 5s
                // (a SshProgress will flip us to Reconnecting). Show the banner.
                self.ssh_conn = self.ssh_conn.map(|_| SshConnState::Disconnected);
                cx.notify();
            }
        }
    }

    /// Submit the typed SSH password (B3): cache it if "记住密码" is checked, send
    /// it to the backend, and close the card. Shared by Enter and the 连接 button.
    fn submit_ssh_password(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.ssh_password_prompt.take() else {
            return;
        };
        let pw = std::mem::take(&mut self.ssh_password_input);
        if self.ssh_password_remember && !pw.is_empty() {
            cx.emit(SshRememberPassword(pw.clone()));
        }
        let _ = p.reply.send(tn_pty::PasswordReply {
            password: pw,
            remember: self.ssh_password_remember,
        });
        cx.notify();
    }

    /// Cancel the SSH password prompt (Esc / 取消): release the backend prompt and
    /// ask the workspace to close this SSH pane, matching the other SSH cards.
    fn cancel_ssh_password(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.ssh_password_prompt.take() else {
            return;
        };
        let _ = p.reply.send(tn_pty::PasswordReply {
            password: String::new(),
            remember: false,
        });
        self.ssh_password_input.clear();
        cx.emit(SshCloseRequested);
        cx.notify();
    }

    /// Trust the pending host key (B2): save to known_hosts if "记住" is checked,
    /// else accept for this session only.
    fn trust_host_key(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.ssh_hostkey.take() else {
            return;
        };
        let verdict = if self.ssh_hostkey_remember {
            tn_pty::HostKeyVerdict::AcceptAndSave
        } else {
            tn_pty::HostKeyVerdict::AcceptOnce
        };
        let _ = p.reply.send(verdict);
        cx.notify();
    }

    /// Reject the pending host key (B2): abort the connection and close this SSH
    /// pane, matching the card's "取消" label.
    fn reject_host_key(&mut self, cx: &mut Context<Self>) {
        let Some(p) = self.ssh_hostkey.take() else {
            return;
        };
        let _ = p.reply.send(tn_pty::HostKeyVerdict::Reject);
        cx.emit(SshCloseRequested);
        cx.notify();
    }

    /// Drop the agent identity + everything that hangs off it (usage, activity-rail
    /// data, the change watcher) so the pane reverts cleanly to a plain shell.
    fn clear_agent(&mut self) {
        self.agent = None;
        self.agent_from_shell = false;
        self.preserve_scrollback_clears
            .store(false, Ordering::Relaxed);
        self.usage = None;
        self.agent_status = None;
        self.agent_model = None;
        self.agent_transcript_tail = None;
        self.agent_permission_prompt = None;
        self.agent_error = None;
        self.rail_state = RailState::Idle;
        self.rail_cwd = None;
        self.quota_open = false; // close the 额度 popover (its pill is gone)
        self.change_watcher = None; // stop watching the working tree
        self.realtime_adapter = None; // drop sidecar → its child process is killed
        self.sidecar_confirm = None;
        // Reset resolved presentation back to the plain-shell defaults.
        self.agent_accent = self.ui_accent;
        self.agent_label = None;
        self.agent_short = None;
        self.agent_manages_cursor = false;
        self.agent_caps = AgentCapabilities::default();
    }

    /// Spawn a per-pane telemetry sidecar from a descriptor's `realtime_command`
    /// and start its event poller. Returns the owned adapter (the view keeps it so
    /// the child dies on drop), or `None` if there's no command / the spawn failed.
    /// Never panics — a spawn error degrades to "no telemetry" (pane construction
    /// runs in a non-unwinding GPUI callback).
    fn spawn_sidecar(
        cx: &mut Context<Self>,
        descriptor: AgentDescriptor,
    ) -> Option<Arc<dyn AgentAdapter>> {
        match ExternalProcessAdapter::from_descriptor(descriptor) {
            Some(Ok(adapter)) => {
                let arc: Arc<dyn AgentAdapter> = Arc::new(adapter);
                Self::spawn_agent_event_poller(cx, arc.clone());
                Some(arc)
            }
            Some(Err(e)) => {
                tracing::error!(error = %e, "agent sidecar spawn failed");
                None
            }
            None => None,
        }
    }

    /// Confirm a pending networked sidecar (user clicked 允许) → spawn it now.
    fn confirm_sidecar(&mut self, cx: &mut Context<Self>) {
        let Some(c) = self.sidecar_confirm.take() else {
            return;
        };
        self.realtime_adapter = Self::spawn_sidecar(cx, c.descriptor);
        cx.notify();
    }

    /// Deny a pending networked sidecar (user clicked 拒绝) → host without it.
    fn deny_sidecar(&mut self, cx: &mut Context<Self>) {
        self.sidecar_confirm = None;
        cx.notify();
    }

    /// Reduce one [`AgentEvent`] into this pane's view state — the single funnel
    /// for all agent telemetry/lifecycle, so producers (the usage poller, the
    /// shell-agent sync, the exit watcher) speak `AgentEvent` instead of poking
    /// fields directly. Keeps the UI's agent input to one stream (the Agent Host
    /// contract) without disturbing the off-thread poll cadence.
    pub(super) fn reduce_agent_event(&mut self, ev: AgentEvent, cx: &mut Context<Self>) {
        match ev {
            AgentEvent::UsageUpdated(u) => {
                self.usage = Some(u);
                cx.emit(UsageUpdated); // relabel tab + repaint status bar
            }
            AgentEvent::DiffChanged => self.refresh_changes(cx),
            AgentEvent::SessionStarted => {
                self.agent_status = Some(AgentStatus::Starting);
            }
            AgentEvent::SessionEnded => {
                self.clear_agent();
                cx.emit(UsageUpdated);
            }
            AgentEvent::StatusChanged(s) => {
                self.agent_status = Some(s);
            }
            AgentEvent::CwdChanged(cwd) => {
                self.last_cwd = Some(cwd.clone());
                if let Some(root) = self.file_namespace.browsable_path_from_cwd(&cwd) {
                    self.rail_cwd = Some(root.clone());
                    if self.agent.is_some() && self.agent_caps.git_diff {
                        self.change_watcher = Self::spawn_change_watcher(cx, root);
                        self.refresh_changes(cx);
                    }
                }
                cx.emit(CwdChanged);
            }
            AgentEvent::ModelChanged(model) => {
                self.agent_model = Some(model);
                cx.emit(UsageUpdated); // status bar/tab model text may repaint
            }
            AgentEvent::TranscriptAppended(text) => {
                self.agent_transcript_tail = Some(tail_chars(&text, 180));
            }
            AgentEvent::PermissionRequested(prompt) => {
                self.agent_permission_prompt = Some(prompt);
                self.agent_status = Some(AgentStatus::Running);
            }
            AgentEvent::ErrorReported(err) => {
                self.agent_error = Some(err);
                self.agent_status = Some(AgentStatus::Error);
            }
        }
        cx.notify();
    }

    /// Resolve an agent id's presentation (accent / label / own-cursor quirk) and
    /// its starting usage-pill mode from the registry descriptor + this view's
    /// config. Agent-agnostic — the single place the UI turns an [`AgentId`] into
    /// display data, shared by construction and [`sync_shell_agent`]. Accent =
    /// theme `[agents.<id>]` override → descriptor default → UI accent; pill mode
    /// = config override → global, with `auto` resolved from the adapter's login.
    fn resolve_agent_view(
        id: &AgentId,
        config: &Loaded,
        registry: &AgentRegistry,
        ui_accent: Rgb,
        billing_mode: tn_config::BillingMode,
    ) -> AgentView {
        let desc = registry.descriptor_or_generic(id, id.as_str());
        let accent = config
            .theme
            .agents
            .accent_for(id.as_str())
            .or(desc.accent)
            .map(|c| Rgb::new(c.r, c.g, c.b))
            .unwrap_or(ui_accent);
        let override_mode = config.config.general.billing_for(id.as_str());
        let is_sub = registry
            .adapter(id)
            .map(|a| a.is_subscription())
            .unwrap_or(false);
        let usage_mode = crate::usage_display::starting_mode(billing_mode, override_mode, is_sub);
        AgentView {
            accent,
            label: SharedString::from(desc.label.clone()),
            short: SharedString::from(desc.short.clone()),
            manages_cursor: desc.manages_own_cursor,
            caps: desc.capabilities,
            usage_mode,
        }
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
        let registry = crate::agent_host::agent_registry(cx);
        let running_agent = {
            let bm = self.blocks.lock().unwrap();
            bm.current()
                .filter(|b| b.is_running())
                .and_then(|b| b.command.as_deref())
                .and_then(|cmd| {
                    let tok = cmd.split_whitespace().next()?;
                    registry
                        .match_command(tok)
                        .map(|id| (id, tok.to_string(), cmd.to_string()))
                })
        };
        match (running_agent, self.agent.is_some()) {
            // A typed agent command started in a plain (non-agent) shell.
            (Some((id, token, command)), false) => {
                self.agent = Some(id.clone());
                self.agent_from_shell = true;
                self.preserve_scrollback_clears
                    .store(true, Ordering::Relaxed);
                self.usage = None;
                // Resolve this pane's presentation + starting pill mode for the
                // now-known agent (agent-agnostic, via the registry).
                let v = Self::resolve_agent_view(
                    &id,
                    &self.config,
                    &registry,
                    self.ui_accent,
                    self.billing_mode,
                );
                self.agent_accent = v.accent;
                self.agent_label = Some(v.label);
                self.agent_short = Some(v.short);
                self.agent_manages_cursor = v.manages_cursor;
                self.agent_caps = v.caps;
                self.usage_mode = v.usage_mode;
                self.agent_surface_probe_frames = 4;
                tracing::info!(
                    target: "tn::agent_surface",
                    agent = %id,
                    token = %token,
                    command = %command,
                    git_diff_cap = self.agent_caps.git_diff,
                    usage_cap = self.agent_caps.usage,
                    manages_cursor = self.agent_manages_cursor,
                    "shell command promoted to agent surface"
                );
                // Bind usage to the session this just-typed command starts (created
                // ~now); the grace in resolve_pane_session absorbs detection lag.
                // Only an agent with a usage adapter is polled.
                if let Some(adapter) = registry.adapter(&id) {
                    let started_at = SystemTime::now();
                    Self::spawn_usage_poller(cx, adapter.clone(), started_at);
                    if adapter.has_realtime_events() {
                        Self::spawn_agent_event_poller(cx, adapter.clone());
                    }
                }
                if let Some(root) = self.effective_browsable_cwd() {
                    self.change_watcher = Self::spawn_change_watcher(cx, root.clone());
                    self.rail_cwd = Some(root);
                }
                cx.emit(UsageUpdated); // relabel the tab + repaint chrome
            }
            // The shell-inferred agent's command finished → revert to plain shell.
            // (Launch-intent agents have `agent_from_shell == false` → left alone;
            // they clear via the exit sentinel instead.)
            (None, true) if self.agent_from_shell => {
                tracing::info!(
                    target: "tn::agent_surface",
                    agent = ?self.agent.as_ref(),
                    "shell agent command finished; reverting pane to shell surface"
                );
                self.clear_agent();
                cx.emit(UsageUpdated);
            }
            _ => {}
        }
    }

    /// Refresh the activity-rail「本次改动」from real `git diff HEAD` in the pane's
    /// cwd — off the UI thread, bounded. Triggered by the change watcher (变化即刷新)
    /// and once on agent start. No-op once the agent is gone.
    ///
    /// ## Stale-result prevention
    /// Each call bumps `rail_generation`; the spawned task captures the generation at
    /// dispatch time. When the task completes (potentially out of order — a slow git
    /// run can finish AFTER a faster run that was dispatched later), the generation
    /// is compared: stale results are silently dropped. This guarantees the UI never
    /// regresses to an earlier diff snapshot.
    pub(super) fn refresh_changes(&mut self, cx: &mut Context<Self>) {
        if self.agent.is_none() {
            return;
        }
        let Some(source) = self.effective_rail_source() else {
            return;
        };

        // Bump generation. If we don't have any ready data yet, show the Loading skeleton.
        self.rail_generation = self.rail_generation.wrapping_add(1);
        let gen = self.rail_generation;
        if !matches!(self.rail_state, RailState::Ready { .. }) {
            self.rail_state = RailState::Loading;
            cx.notify();
        }

        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // ── Background: expensive git ops (may block for >100ms) ──
            let (files, source) = exec
                .spawn(async move {
                    let (tx, rx) = futures::channel::oneshot::channel();
                    let fallback_source = source.clone();
                    std::thread::spawn(move || {
                        let files = match &fallback_source {
                            RailSource::Local { root } => crate::gitutil::changes_for(root),
                            RailSource::Remote { cfg, root } => {
                                let service = SshCommandService::shared();
                                crate::remote_git::changes_for_remote(service.as_ref(), cfg, root)
                            }
                        };
                        let _ = tx.send((files, fallback_source));
                    });
                    rx.await.unwrap_or_else(|_| (Vec::new(), source))
                })
                .await;
            // ── Back on UI thread: only apply if still current ──
            let _ = this.update(cx, |v, cx| {
                if v.rail_generation != gen {
                    // A newer refresh was dispatched while this one was in flight;
                    // drop these stale results so the UI doesn't regress.
                    return;
                }
                v.rail_state = RailState::Ready { files, source };
                cx.emit(UsageUpdated);
                cx.emit(FilesChanged);
                cx.notify(); // skeleton exits, real cards render
            });
        })
        .detach();
    }

    /// Toggle the 额度 popover anchored at the usage readout. On open, kick a
    /// background recompute of the windowed spend.
    pub(super) fn toggle_quota(&mut self, cx: &mut Context<Self>) {
        self.quota_open = !self.quota_open;
        if self.quota_open {
            self.refresh_quota(cx);
        }
        cx.notify();
    }

    /// Recompute the 5h/day/week windows off the UI thread (reads the last week of
    /// Claude session logs). `quota_gen` drops a stale result if a newer refresh
    /// landed first — same guard as `refresh_changes`.
    fn refresh_quota(&mut self, cx: &mut Context<Self>) {
        self.quota_gen = self.quota_gen.wrapping_add(1);
        let gen = self.quota_gen;
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let windows = exec
                .spawn(async move {
                    let (tx, rx) = futures::channel::oneshot::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(tn_ai::usage_windows::current_windows(SystemTime::now()));
                    });
                    rx.await.ok()
                })
                .await;
            let _ = this.update(cx, |v, cx| {
                if v.quota_gen != gen {
                    return;
                }
                if let Some(w) = windows {
                    v.quota = Some(w);
                    cx.notify();
                }
            });
        })
        .detach();
    }

    fn effective_rail_source(&self) -> Option<RailSource> {
        match self.file_namespace {
            FileNamespace::Ssh => {
                let cfg = self.ssh_cfg.clone()?;
                let cwd = self.cwd()?;
                let cwd = cwd.trim();
                cwd.starts_with('/').then(|| RailSource::Remote {
                    cfg,
                    root: RemotePath::new(cwd),
                })
            }
            _ => self
                .effective_browsable_cwd()
                .or_else(|| self.rail_cwd.clone())
                .map(|root| RailSource::Local { root }),
        }
    }

    /// This pane's latest AI usage snapshot, if any has been parsed yet.

    /// Update the rail directory for existing agent panes after "Open Folder".
    /// Only affects panes that already host an agent; running agent processes
    /// keep their original cwd (PTY constraint), so a full restart is needed
    /// to land in the new directory -- but the rail follows the tree immediately.
    pub fn set_rail_root(&mut self, root: &std::path::Path, cx: &mut Context<Self>) {
        if self.agent.is_none() {
            return;
        }
        self.rail_cwd = Some(root.to_path_buf());
        self.change_watcher = Self::spawn_change_watcher(cx, root.to_path_buf());
        self.refresh_changes(cx);
    }
    /// Find the index of `path` within the activity rail's file list.
    /// Used by the workspace to remember which card was clicked so ↑↓ nav
    /// can stay within the rail's changed-file scope (not the explorer tree).
    pub(crate) fn rail_find_idx(&self, target: &RailFileTarget) -> Option<usize> {
        if let RailState::Ready { files, source } = &self.rail_state {
            files
                .iter()
                .position(|f| source.target_for(&f.path) == *target)
        } else {
            None
        }
    }

    /// Navigate the activity rail by `delta` (-1 = previous, +1 = next) from
    /// `current_idx`, wrapping around. Returns `(new_index, target)`.
    pub(crate) fn rail_nav(
        &self,
        current_idx: usize,
        delta: i32,
    ) -> Option<(usize, RailFileTarget)> {
        if let RailState::Ready { files, source } = &self.rail_state {
            if files.is_empty() {
                return None;
            }
            let n = files.len() as i32;
            let new_idx = ((current_idx as i32 + delta).rem_euclid(n)) as usize;
            Some((new_idx, source.target_for(&files[new_idx].path)))
        } else {
            None
        }
    }

    /// Number of changed files in this pane's activity rail (0 when not Ready) —
    /// drives QuickLook's `RAIL · n/N` footer read when opened from the rail.
    pub(crate) fn rail_len(&self) -> usize {
        if let RailState::Ready { files, .. } = &self.rail_state {
            files.len()
        } else {
            0
        }
    }

    pub fn usage(&self) -> Option<&AiUsage> {
        self.usage.as_ref()
    }

    /// This pane's current working directory (from OSC 7 / shell integration),
    /// if known — drives the tab path badge.
    pub fn cwd(&self) -> Option<String> {
        let m = self.blocks.lock().unwrap();
        m.current()
            .and_then(|b| b.cwd.as_ref().map(|s| s.to_string()))
            .or_else(|| {
                m.last_finished()
                    .and_then(|b| b.cwd.as_ref().map(|s| s.to_string()))
            })
            // Model-level cwd: a bare `OSC 633;P;Cwd` with **no** A/B/C/D prompt
            // markers (as the remote bash/zsh injection emits — see PtyEvent::
            // Connected) never creates a block, so `current()`/`last_finished()`
            // stay None. Without this fallback the SSH pane's cwd is never known,
            // and the file tree / 「打开文件夹」 can't re-root to the remote FS.
            .or_else(|| m.cwd().map(|s| s.to_string()))
            .or_else(|| self.last_cwd.clone())
    }

    pub fn file_namespace(&self) -> FileNamespace {
        self.file_namespace.clone()
    }

    /// Current cwd as a host-browsable path. Host shells produce Windows paths;
    /// WSL cwd is mapped to its `\\wsl$\<distro>` namespace; SSH/macOS/Linux
    /// remote cwd deliberately returns `None` until a remote file backend exists.
    pub fn effective_browsable_cwd(&self) -> Option<std::path::PathBuf> {
        self.cwd()
            .and_then(|cwd| self.file_namespace.browsable_path_from_cwd(&cwd))
            .or_else(|| {
                (self.file_namespace == FileNamespace::Host)
                    .then(|| self.spawn_cwd.clone())
                    .flatten()
            })
    }

    /// A cwd safe to pass as the Windows process cwd for newly spawned local
    /// processes. WSL and SSH cwd are intentionally excluded.
    pub fn effective_host_process_cwd(&self) -> Option<std::path::PathBuf> {
        self.cwd()
            .and_then(|cwd| self.file_namespace.host_process_path_from_cwd(&cwd))
            .or_else(|| {
                (self.file_namespace == FileNamespace::Host)
                    .then(|| self.spawn_cwd.clone())
                    .flatten()
            })
    }

    /// A clean tab label: the agent name for an agent pane, else the shell name
    /// (never the raw OSC title, which for pwsh is the noisy `…\powershell.exe`).
    pub fn tab_label(&self) -> String {
        match &self.agent_short {
            Some(s) => s.to_string(),
            None => shell_name_of(&self.program),
        }
    }

    /// This pane hosts an agent session (vs a plain shell).
    pub fn is_agent(&self) -> bool {
        self.agent.is_some()
    }

    /// Mark this pane as ghost-terminal hosted: the ghost window draws its own
    /// GHOST_ head, so the plain-shell pane header is suppressed (agent header
    /// stays — it carries the usage ring). 差异总结 4-3。
    pub fn set_ghost_chrome(&mut self, on: bool) {
        self.ghost_chrome = on;
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
        if self.ssh_input_blocked() {
            return;
        }
        let _ = self.writer_tx.send(bytes.to_vec());
    }

    fn ssh_input_blocked(&self) -> bool {
        self.ssh_hostkey.is_some()
            || self.ssh_password_prompt.is_some()
            || self.ssh_error.is_some()
            || self.ssh_progress.is_some()
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
        if self.ssh_input_blocked() {
            return;
        }
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

    /// External OS file/dir drop (从文件管理器拖文件/目录进窗格). Writes the dropped
    /// path(s) into the PTY's current input line — **no Enter**, so the user reviews
    /// and sends. Agent panes get paths relative to the agent's working dir (repo-
    /// scoped; falls back to absolute outside it); plain shells get absolute paths.
    /// Multiple drops join with spaces; no quoting (per spec). Wrapped in bracketed-
    /// paste markers when the program enabled DEC 2004 so it lands as input, not keys.
    fn drop_external_paths(
        &mut self,
        paths: &[PathBuf],
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ssh_input_blocked() || paths.is_empty() {
            return;
        }
        // Agent → relative to its working dir (rail_cwd = the launch/Open-Folder dir);
        // shell → absolute (base None).
        let base = self
            .agent
            .is_some()
            .then(|| self.rail_cwd.clone())
            .flatten();
        let text = render_dropped_paths(paths, base.as_deref());
        self.insert_input_text(&text, window, cx);
    }

    /// Drop a file dragged out of Tn's own explorer (left-button GPUI-native drag)
    /// into this pane's input line — no Enter. The inserted string is rendered for
    /// **this pane's namespace**: a WSL `\\wsl$\…` file becomes the Linux path a WSL
    /// shell can open; an SSH file uses its remote path; a local file follows the
    /// same agent-relative / shell-absolute rule as an external OS drop.
    pub fn drop_dragged_file(
        &mut self,
        file: &crate::explorer::ExplorerFile,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.ssh_input_blocked() {
            return;
        }
        let Some(text) = self.dropped_file_text(file) else {
            return;
        };
        self.insert_input_text(&text, window, cx);
    }

    /// Resolve the input string for a dragged explorer file against this pane's
    /// namespace. Pure aside from reading `self`; the formatting branches mirror
    /// `render_drop_path` (local) and add WSL-UNC→Linux + SSH-remote handling.
    fn dropped_file_text(&self, file: &crate::explorer::ExplorerFile) -> Option<String> {
        use crate::explorer::ExplorerFile;
        match file {
            ExplorerFile::Local(p) => {
                // WSL pane + a `\\wsl$\…` UNC → the Linux path the shell understands.
                if matches!(self.file_namespace, FileNamespace::Wsl { .. }) {
                    if let Some(linux) = crate::explorer::wsl_unc_to_linux(p) {
                        return (!linux.is_empty()).then_some(linux);
                    }
                }
                let base = self
                    .agent
                    .is_some()
                    .then(|| self.rail_cwd.clone())
                    .flatten();
                let s = render_drop_path(p, base.as_deref());
                (!s.is_empty()).then_some(s)
            }
            // SSH file → its remote absolute path (the remote shell's own namespace).
            ExplorerFile::Remote { id, .. } => {
                let s = id.path.as_str().to_string();
                (!s.is_empty()).then_some(s)
            }
        }
    }

    /// Write `text` into the PTY input line (no Enter), wrapped in bracketed-paste
    /// markers when the program enabled DEC 2004 so it lands as input, not keys, and
    /// focus the pane so the user's following Enter reaches this terminal. Shared by
    /// external OS drops and internal explorer drags.
    fn insert_input_text(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        if text.is_empty() {
            return;
        }
        let bracketed = {
            let mut t = self.terminal.lock().unwrap();
            t.scroll_to_bottom();
            t.input_mode().bracketed_paste
        };
        {
            let mut w = self.writer.lock().unwrap();
            if bracketed {
                let _ = w.write_all(b"\x1b[200~");
                let _ = w.write_all(text.as_bytes());
                let _ = w.write_all(b"\x1b[201~");
            } else {
                let _ = w.write_all(text.as_bytes());
            }
            let _ = w.flush();
        }
        // A drop doesn't focus the pane on its own — focus it so the next keystroke
        // (the user's Enter) reaches this terminal.
        self.focus_handle.focus(window);
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
        if cmd.is_empty() || self.ssh_input_blocked() {
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
    /// or `None` when a full-screen app or detected agent owns the viewport.
    /// 运行中块的走表 ticker:200ms 重绘一次,让 RUN 耗时在静默长命令(无输出
    /// 不触发 reader wake)下也实时累加;块结束自行停表。
    fn ensure_block_ticker(&mut self, cx: &mut Context<Self>) {
        let running = self
            .blocks
            .lock()
            .unwrap()
            .current()
            .is_some_and(|b| b.is_running());
        if !running || self.block_ticking {
            return;
        }
        self.block_ticking = true;
        cx.spawn(async move |this, cx: &mut gpui::AsyncApp| loop {
            cx.background_executor()
                .timer(Duration::from_millis(200))
                .await;
            let keep = this
                .update(cx, |v, cx| {
                    let running = v
                        .blocks
                        .lock()
                        .unwrap()
                        .current()
                        .is_some_and(|b| b.is_running());
                    if !running {
                        v.block_ticking = false;
                    }
                    cx.notify();
                    running
                })
                .unwrap_or(false);
            if !keep {
                break;
            }
        })
        .detach();
    }

    fn render_block_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let alt_screen = self.terminal.lock().unwrap().input_mode().alt_screen;
        if !should_render_block_bar(self.agent.is_some(), alt_screen) {
            return None; // Terminal apps own the viewport, so shell chrome stays out.
        }
        let now_ms = self.session_clock.elapsed().as_millis() as u64;
        let data = block_view::BlockBar::from_model(&self.blocks.lock().unwrap(), now_ms)?;
        // Chrome tokens (mockup .block uses --fg/--muted/--accent), + ANSI green/red for status.
        let pal =
            block_view::BarPalette::new(self.ui_fg, self.ui_muted, self.ui_accent, &self.palette);
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
                    .rounded(px(crate::style::R_CARD))
                    .bg(gpui::rgb(crate::style::L2))
                    .border_1()
                    .border_color(rgba(crate::style::H1))
                    .text_color(pal.fg)
                    .hover(|s| s.bg(gpui::rgb(crate::style::L4)))
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
    fn cell_at(&self, pos: Point<Pixels>) -> Option<(usize, usize)> {
        let b = self.content_bounds.borrow();
        // Subtract the body inset (mockup .body padding) so a click maps to the cell
        // under the cursor — the grid is drawn at +BODY_PAD from content_bounds.
        cell_at_from_bounds(
            f32::from(pos.x),
            f32::from(pos.y),
            f32::from(b.origin.x),
            f32::from(b.origin.y),
            f32::from(b.size.width),
            f32::from(b.size.height),
            self.cell_width,
            self.line_height,
            self.size,
        )
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((row, col)) = self.cell_at(event.position) else {
            return;
        };
        // Click count picks the granularity: 1 = cell, 2 = word, 3+ = line
        // A following drag extends by that same granularity.
        let kind = match event.click_count {
            0 | 1 => SelectKind::Cell,
            2 => SelectKind::Word,
            _ => SelectKind::Line,
        };
        self.terminal
            .lock()
            .unwrap()
            .selection_start_kind(row, col, kind);
        self.selecting = true;
        cx.notify();
    }

    fn on_mouse_move(
        &mut self,
        event: &MouseMoveEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.scrollbar_drag.is_some() {
            // 防粘连:鼠标在元素外松开时 on_mouse_up 收不到 → 拖动态残留、滚动条跟着鼠标走。
            // 兜底——左键一旦已松(pressed_button != Left)即清除拖动、不再跟随(同 Quick Look)。
            if event.pressed_button != Some(MouseButton::Left) {
                self.scrollbar_drag = None;
                cx.notify();
                return;
            }
            self.drag_scrollbar(event.position.y.into(), cx);
            return;
        }
        match selection_drag_move(self.selecting, event.pressed_button) {
            SelectionDragMove::Idle => return,
            SelectionDragMove::EndLostMouseUp => {
                self.finish_selection_drag(cx);
                return;
            }
            SelectionDragMove::Update => {}
        }
        let Some((row, col)) = self.cell_at(event.position) else {
            return;
        };
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
        let thumb_top =
            f32::from(b.origin.y) + (history.saturating_sub(offset)) as f32 / total * track_h;
        self.scrollbar_drag = Some(cursor_y - thumb_top);
        cx.notify();
    }

    /// Map the dragged thumb position to a scrollback offset and apply it.
    fn drag_scrollbar(&mut self, cursor_y: f32, cx: &mut Context<Self>) {
        let Some(grab_dy) = self.scrollbar_drag else {
            return;
        };
        let b = *self.content_bounds.borrow();
        let track_h = f32::from(b.size.height);
        if track_h <= 0.0 {
            return;
        }
        let (_, history) = self.terminal.lock().unwrap().scroll_position();
        let total = (history + self.size.rows) as f32;
        let frac = ((cursor_y - f32::from(b.origin.y) - grab_dy) / track_h).clamp(0.0, 1.0);
        let offset = (history as f32 - frac * total)
            .round()
            .clamp(0.0, history as f32) as usize;
        self.terminal.lock().unwrap().scroll_to_offset(offset);
        cx.notify();
    }

    fn on_mouse_up(&mut self, _event: &MouseUpEvent, _window: &mut Window, cx: &mut Context<Self>) {
        if self.scrollbar_drag.take().is_some() {
            cx.notify();
            return;
        }
        self.finish_selection_drag(cx);
    }

    fn finish_selection_drag(&mut self, cx: &mut Context<Self>) {
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
        // 宠物 Typing 信号:KeyPress 进入专注态(只写原子量,零成本,不影响输入)。
        crate::pet::signal_typing();
        // SSH host-key trust panel (B2): Enter = trust, Esc = reject. Highest
        // precedence — it gates the rest of the connection.
        if self.ssh_hostkey.is_some() {
            match event.keystroke.key.as_str() {
                "enter" => self.trust_host_key(cx),
                "escape" => self.reject_host_key(cx),
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        // SSH error card (C1): Enter = retry, Esc = close. Takes precedence over
        // the terminal so the dead session's keys don't leak through.
        if self.ssh_error.is_some() {
            match event.keystroke.key.as_str() {
                "enter" => {
                    cx.emit(SshRetryRequested);
                    cx.stop_propagation();
                    return;
                }
                "escape" => {
                    cx.emit(SshCloseRequested);
                    cx.stop_propagation();
                    return;
                }
                _ => {}
            }
            cx.stop_propagation();
            return;
        }
        // SSH progress card (B1): Esc = cancel the in-flight connection; all
        // other keys are swallowed so they can't queue and replay into the remote
        // shell once it opens.
        if self.ssh_progress.is_some() {
            if event.keystroke.key.as_str() == "escape" {
                cx.emit(SshCloseRequested);
            }
            cx.stop_propagation();
            return;
        }
        // Handle SSH password prompt input intercept
        if self.ssh_password_prompt.is_some() {
            let keystroke = &event.keystroke;
            let key = keystroke.key.as_str();
            if key == "escape" {
                self.cancel_ssh_password(cx);
                cx.stop_propagation();
                return;
            } else if key == "enter" {
                self.submit_ssh_password(cx);
                cx.stop_propagation();
                return;
            } else if key == "backspace" {
                self.ssh_password_input.pop();
                cx.notify();
                cx.stop_propagation();
                return;
            } else if !keystroke.modifiers.control
                && !keystroke.modifiers.alt
                && !keystroke.modifiers.platform
            {
                if let Some(c) = &keystroke.key_char {
                    if !c.is_empty() && c.chars().count() == 1 {
                        self.ssh_password_input.push_str(c);
                        cx.notify();
                    }
                } else if key.chars().count() == 1 {
                    self.ssh_password_input.push_str(key);
                    cx.notify();
                }
                cx.stop_propagation();
                return;
            }
            // Swallow other keys while prompt is active
            cx.stop_propagation();
            return;
        }

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
        if (m.control && m.shift && key == "v")
            || (m.shift && !m.control && !m.alt && key == "insert")
        {
            self.paste(cx);
            cx.stop_propagation();
            return;
        }
        // NOTE: keys the IME is actively composing arrive as `VK_PROCESSKEY` and are
        // intercepted + routed to the IME by the window subclass (`platform::
        // install_ime_keyfix`) BEFORE reaching here — so by the time a key hits this
        // handler the IME does NOT want it (we're not composing, or it's a real
        // terminal key). That's what lets backspace/enter/arrows be encoded below
        // without stealing them from a 中文 composition. See platform.rs for the why.
        //
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
        // Defer only keys that gpui's `translate_message` turns into a WM_CHAR — i.e.
        // **printable** keys — so they flow to the IME input handler (English via
        // WM_CHAR, 中文 via composition → `replace_text_in_range`). Background: some
        // IMEs (e.g. MS Pinyin) never send a composition string, so gpui's
        // `is_composing` is always false and never short-circuits keydown to the IME
        // (tn.log: `marked_text_range -> None`, `replace_and_mark` never fires). So WE
        // keep printable keys un-consumed; the IME eats them while composing (no
        // WM_CHAR) or they return as WM_CHAR otherwise:
        //   • single-char keys (letters/digits/punctuation) — pinyin + candidate select.
        //   • `space` — the IME **commit** key (a real WM_CHAR 0x20 when not composing).
        // **`backspace` is NOT deferred** (it's encoded below): `translate_message` does
        // **not** emit a WM_CHAR for VK_BACK (a control key), so deferring it dropped the
        // key entirely — terminal backspace stopped deleting (tn.log: `DEFER backspace`
        // with no following `replace_text`). Same logic excludes all non-printable named
        // keys (Enter/Tab/Escape/arrows/Home/End/PageUp·Down/F*): they have no WM_CHAR to
        // fall back on, so they must be encoded. (Cost: those keys can't reach the IME
        // mid-composition — a gpui IMM32 limitation, see CLAUDE.md / WT-uses-TSF note.)
        // Exception: if the IME is active and not empty, we previously deferred backspace.
        // However, this caused backspace to permanently break if IME composition got stuck.
        // We now ALWAYS let Backspace pass through to the PTY if it reaches `on_key_down`.
        let is_text_input =
            !m.control && !m.alt && !m.platform && (key.chars().count() == 1 || key == "space");
        if is_text_input {
            return;
        }

        // ── IME preedit 残留兜底(修复「偶尔有个删不掉的符号」)─────────────────
        // 能走到这里的都是命名键/带修饰键 —— 真正合成中的键会被 VK_PROCESSKEY 窗口子类
        // (platform::install_ime_keyfix)在抵达 gpui 前路由给 IME,根本不进 on_key。所以
        // 此刻 `ime_marked` 若仍非空,它是微软拼音等 IME 状态机卡死遗留的「幽灵 preedit」
        // (replace_and_mark 设了 preedit 却没等到 commit/unmark 清掉)—— render 会把它画
        // 成停在光标处、删不掉的字符框(见 ime_preedit)。在此清掉。退格优先用来清它:屏幕
        // 上那个删不掉的字符正是这个 preedit,清掉即满足删除意图,并消费这次退格(不再误把
        // 退格发给 PTY 删一个真实字符);其余命名键(回车/方向等)清掉残留后继续正常编码。
        if self.ime_marked.take().is_some() {
            cx.notify();
            if key == "backspace" {
                cx.stop_propagation();
                return;
            }
        }

        // Encode against the engine's live modes (DECCKM, LNM, ...). Sending
        // input also snaps the viewport back to the live bottom. Skip the
        // scroll AND the repaint if already at the bottom (common case when
        // typing) — the PTY echo triggers a repaint via the reader with the
        // actual new terminal state. Only notify when we scrolled (bumped
        // generation) so the viewport change is painted.
        let (bytes, did_scroll) = {
            let mut t = self.terminal.lock().unwrap();
            let mode = t.input_mode();
            match crate::input::encode_key(&event.keystroke, mode) {
                Some(b) => {
                    let (offset, hist) = t.scroll_position();
                    let did_scroll = should_scroll_to_bottom_before_input(offset, hist);
                    if did_scroll {
                        t.scroll_to_bottom();
                    }
                    (b, did_scroll)
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
        if did_scroll {
            cx.notify();
        }
    }

    /// Mouse wheel, in priority order:
    /// 1. **App owns the mouse** (TUIs that set DECSET 1000/1002/1003): forward the
    ///    wheel as a mouse button-64/65 report so the app scrolls its own viewport.
    /// 2. **Alt screen without mouse capture** (`less`/`vim` with alternate-scroll):
    ///    translate the wheel into cursor-key presses so the pager pages.
    /// 3. **Main screen**: scroll our own scrollback buffer.
    fn on_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Lines toward older output are positive.
        let lines = match event.delta {
            ScrollDelta::Lines(p) => p.y,
            ScrollDelta::Pixels(p) => f32::from(p.y) / self.line_height,
        };
        if lines == 0.0 {
            return;
        }
        let mode = self.terminal.lock().unwrap().input_mode();
        let up = lines > 0.0;
        let n = (lines.abs().round() as usize).clamp(1, 100);
        let (offset, history) = self.terminal.lock().unwrap().scroll_position();
        let route = scroll_wheel_route(mode);
        cx.stop_propagation();

        {
            let branch = match route {
                ScrollWheelRoute::MouseReport => "mouse_report",
                ScrollWheelRoute::AppArrows => "arrows",
                ScrollWheelRoute::Scrollback => "scrollback",
            };
            tracing::info!(
                target: "tn::scroll",
                agent = ?self.agent_short.as_deref(),
                alt_screen = mode.alt_screen,
                mouse_report = mode.mouse_report,
                sgr_mouse = mode.sgr_mouse,
                history,
                offset,
                lines,
                branch,
                "wheel tick"
            );
        }

        // (1) App-driven mouse: a TUI (codex/ratatui) set DECSET 1000/1002/1003,
        // so forward a wheel button report at the pointer's cell and let the app
        // scroll its own viewport. Requires the child's mode to actually reach us —
        // see the bundled modern ConPTY (`conpty.dll`); the old system conhost
        // strips alt-screen/mouse modes so this branch would never trigger.
        if route == ScrollWheelRoute::MouseReport {
            if self.ssh_input_blocked() {
                return;
            }
            let (row, col) = self
                .cell_at(event.position)
                .map(|(r, c)| (r + 1, c + 1))
                .unwrap_or((1, 1));
            let sgr = mode.sgr_mouse;
            let bytes = encode_wheel_report(up, col, row, sgr, n);
            let mut w = self.writer.lock().unwrap();
            let _ = w.write_all(&bytes);
            let _ = w.flush();
            return;
        }

        // (2) Alt-screen pager without mouse capture: synthesize cursor keys.
        if route == ScrollWheelRoute::AppArrows {
            if self.ssh_input_blocked() {
                return;
            }
            let arrow: &[u8] = match (up, mode.app_cursor) {
                (true, false) => b"\x1b[A",
                (true, true) => b"\x1bOA",
                (false, false) => b"\x1b[B",
                (false, true) => b"\x1bOB",
            };
            let mut w = self.writer.lock().unwrap();
            for _ in 0..n {
                let _ = w.write_all(arrow);
            }
            let _ = w.flush();
            return;
        }

        // (3) Main screen: scroll our scrollback.
        self.terminal.lock().unwrap().scroll(lines.round() as i32);
        cx.notify();
    }
}

impl gpui::EventEmitter<UsageUpdated> for TerminalView {}
impl gpui::EventEmitter<FilesChanged> for TerminalView {}
impl gpui::EventEmitter<CwdChanged> for TerminalView {}
impl gpui::EventEmitter<ProcessExited> for TerminalView {}
impl gpui::EventEmitter<OpenInQuickLook> for TerminalView {}
impl gpui::EventEmitter<SshConnected> for TerminalView {}
impl gpui::EventEmitter<SshRetryRequested> for TerminalView {}
impl gpui::EventEmitter<SshCloseRequested> for TerminalView {}
impl gpui::EventEmitter<SshRememberPassword> for TerminalView {}

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
        let units: Vec<u16> = self
            .ime_marked
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
        // Caret at the end of the composition (0 when not composing) — anchors the
        // IME candidate window via `bounds_for_range`.
        let end = self
            .ime_marked
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
        // `Some` ⇒ gpui knows we're composing and feeds keys to the IME (events.rs).
        self.ime_marked
            .as_deref()
            .map(|s| 0..s.encode_utf16().count())
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
        // (Backspace is encoded in `on_key`, never routed here — `translate_message`
        // emits no WM_CHAR for it, see the on_key note.)
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
        let (row, col) = self
            .render_cache
            .as_ref()
            .map(|c| c.cursor)
            .unwrap_or((0, 0));
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
        self.dup_resized_this_frame = false; // 探针:每帧清零,resize 块命中时置位

        // Fit the grid to the pane's own content bounds (captured by the canvas
        // below on the previous frame). Skipping while unset keeps the initial
        // size for one frame instead of collapsing to 1x1.
        let (bw, bh) = {
            let b = self.content_bounds.borrow();
            (f32::from(b.size.width), f32::from(b.size.height))
        };
        if let Some(new_size) = fit_grid_size_from_bounds(bw, bh, self.cell_width, self.line_height)
        {
            // Fit to the *inset* body area (mockup .body padding) — leave BODY_PAD on
            // each side so the grid matches where the cursor/grid are actually drawn.
            // ConPTY tracks the visible grid EXACTLY (rows ≠ alacritty rows caused
            // worse, frequent blanking — once output scrolls alacritty but not
            // ConPTY their cursors diverge and the prompt mislands; the reverted
            // row-lock. See 踩过的坑). To stop a divider-drag-*grow* eating
            // scrollback, the engine top-anchors the grow (`resize_conpty`) so
            // ConPTY's top-anchored repaint can't clobber pulled-up history —
            // verified zero-loss by `TN_RESIZE_EXP=topgrow`.
            if new_size != self.size {
                let old = self.size;
                // 探针(机制 2):长高时 resize_conpty 会 `scroll_up(min(history, delta))`
                // 把视口顶部那么多行推进 scrollback。对内联 agent 那几行就是「活帧」,
                // ConPTY 随后又 repaint 一遍 → 同一帧在 scrollback + 视口各留一份(重复)。
                if self.dup_probe {
                    let history = self.terminal.lock().unwrap().scroll_position().1;
                    let grow = new_size.rows > old.rows;
                    let pulled = if grow {
                        history.min(new_size.rows - old.rows)
                    } else {
                        0
                    };
                    tracing::info!(
                        target: "tn::agent_dup",
                        "resize {}x{} -> {}x{} grow={} pulled_into_scrollback={} \
                         history_before={} inline_agent={} alt_screen={}",
                        old.cols, old.rows, new_size.cols, new_size.rows, grow, pulled, history,
                        self.agent_manages_cursor,
                        self.terminal.lock().unwrap().input_mode().alt_screen,
                    );
                }
                self.dup_resized_this_frame = true;
                self.size = new_size;
                let (anchoring, alt_screen) = {
                    let mut terminal = self.terminal.lock().unwrap();
                    let alt_screen = terminal.input_mode().alt_screen;
                    let anchoring = resize_anchoring_for_pane(self.agent.is_some(), alt_screen);
                    terminal.resize_conpty_with_anchoring(new_size, anchoring);
                    (anchoring, alt_screen)
                };
                if self.dup_probe {
                    tracing::info!(
                        target: "tn::agent_dup",
                        "resize anchoring={:?} agent_active={} alt_screen={}",
                        anchoring,
                        self.agent.is_some(),
                        alt_screen
                    );
                }
                let _ = self
                    .pty
                    .lock()
                    .unwrap()
                    .resize(PtySize::new(new_size.rows as u16, new_size.cols as u16));
            }
        }

        // 探针:活动栏出现/消失。shell-detected agent 现在走 overlay,所以这不应
        // 再伴随终端正文宽度变化;若仍有 resize 日志,说明另有布局源在改 bounds。
        if self.dup_probe {
            let rail = self.agent.is_some() && self.agent_caps.git_diff;
            if rail != self.dup_last_rail {
                let layout = activity_rail_layout(
                    self.agent.is_some(),
                    self.agent_from_shell,
                    self.agent_caps.git_diff,
                );
                tracing::info!(
                    target: "tn::agent_dup",
                    "rail {} layout={:?} (agent={:?} git_diff_cap={} from_shell={})",
                    if rail { "appeared" } else { "removed" },
                    layout,
                    self.agent_short.as_deref(),
                    self.agent_caps.git_diff,
                    self.agent_from_shell,
                );
                self.dup_last_rail = rail;
            }
        }

        // Render-data cache: rebuild the snapshot + per-row run
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
                cursor_shape: snap.cursor_shape,
                cursor_visible: snap.cursor_visible,
                scroll_offset: snap.scroll_offset,
                scroll_history: snap.scroll_history,
                fg: snap.fg,
                bg: snap.bg,
            });
            t0.map(|t| t.elapsed())
        };
        self.perf.record(cache_hit, rebuild);
        let (
            rows,
            (cur_row, cur_col),
            cursor_shape,
            cursor_visible,
            scroll_offset,
            scroll_history,
            fg,
            bg,
        ) = {
            let c = self.render_cache.as_ref().unwrap();
            (
                c.rows.clone(),
                c.cursor,
                c.cursor_shape,
                c.cursor_visible,
                c.scroll_offset,
                c.scroll_history,
                c.fg,
                c.bg,
            )
        };
        // ── 探针:直接捕捉「光标跳回 row 0」(bug3 现象本身) ──────────────────
        if self.dup_probe {
            // 启动确认:第一帧打一行,证明探针确实在跑(上次日志里啥都没有 = 没开)。
            if self.dup_last_cursor == (usize::MAX, usize::MAX) {
                tracing::info!(
                    target: "tn::agent_dup",
                    "probe ON for pane (agent={:?} git_diff_cap={})",
                    self.agent_short.as_deref(), self.agent_caps.git_diff,
                );
            } else if cur_row == 0 && self.dup_last_cursor.0 > 0 {
                // 光标从非顶部跳到了 row 0 —— 正是 bug3。把这一帧的成因证据全打出来:
                // resize?generation 变没变(变=有新输出/restore)?滚动位置?
                tracing::info!(
                    target: "tn::agent_dup",
                    "CURSOR JUMPED to row0 (was {:?} -> ({},{})) resized_this_frame={} \
                     generation={} scroll_offset={} scroll_history={} agent={:?}",
                    self.dup_last_cursor, cur_row, cur_col, self.dup_resized_this_frame,
                    generation, scroll_offset, scroll_history, self.agent_short.as_deref(),
                );
            }
            self.dup_last_cursor = (cur_row, cur_col);
        }
        let bounds_cell = self.content_bounds.clone();
        // Stable pane-width probe (drives 窄面板自适应折叠 of the activity rail next
        // frame). Captured on the pane outer below — full-width in the flex-col, so it
        // never moves when the rail shows/hides.
        let pane_width_cell = self.pane_width.clone();
        // Captured into the canvas paint closure to register the IME input handler
        // (text input / 中文 composition) for this frame — see the `EntityInputHandler`
        // impl + `handle_input` below.
        let ime_focus = self.focus_handle.clone();
        let ime_entity = cx.entity();
        self.ensure_block_ticker(cx); // RUN 实时耗时走表(SHEET 07-B)
        let block_bar = self.render_block_bar(cx);
        // Pane header (agent pill / shell chip). The pill's click handler needs a
        // handle to THIS pane to cycle usage_mode at event time, so pass a weak
        // ref (cheap; the pane outlives its own render).
        let header = self.render_pane_header(cx.entity().downgrade());
        // 窄面板自适应:正文(agent 主内容)永远优先。pane 太窄就收起活动栏,把整宽
        // 让给正文,避免 248px rail 把正文挤到不可用 / 视觉上遮挡。门限基于上一帧测到的
        // 稳定 pane 宽度(与 rail 是否显示无关 → 不抖动);拉宽后自动恢复并排。
        let rail_layout = rail_layout_for_width(
            activity_rail_layout(
                self.agent.is_some(),
                self.agent_from_shell,
                self.agent_caps.git_diff,
            ),
            *self.pane_width.borrow(),
        );

        // Cursor (positioned over the grid, which starts at the term-area origin).
        // Hidden when the app hides it (vim) or the viewport is scrolled off the row.
        let focused = self.focus_handle.is_focused(window);
        self.focused = focused; // cache for the blink task (only blinks when focused)
                                // 失焦不可能在合成中:顺手清掉可能残留的 IME preedit,避免它跨焦点切换时卡在
                                // 光标处删不掉(与 on_key 的残留兜底互为双保险;修「偶尔删不掉的符号」)。
        if !focused {
            self.ime_marked = None;
        }
        // Focused: solid block on the "on" half of the blink; nothing on the "off"
        // half. Unfocused: a steady slim outline (no blink).
        let draw_solid = focused && self.cursor_on;
        // Claude Code(Ink)自绘虚拟光标 + ConPTY 常丢 `\e[?25l` → 我们强制隐藏物理光标
        // (见下 cursor_el)。既然 Claude 态根本不画光标,glide/Q弹动画毫无意义,别 spawn
        // 那个每 16ms notify 的驱动任务(否则 Claude 每移一次光标就白拉起一轮重绘循环)。
        let force_hide_cursor = self.agent_manages_cursor;

        // ── Smooth cursor glide ─────────────────────────────────────────────
        // Ease the drawn block toward the target cell instead of teleporting. Only
        // small same-row moves glide (typing / deleting / local nav); bigger jumps
        // (line wrap, prompt redraw, screen clear, vertical nav) and the first frame
        // snap. The glyph is already at the target — only the block trails, so input
        // reads as fluid. `spawn_cursor_glide` notifies each frame during the ease.
        let target_px = (
            BODY_PAD_X + cur_col as f32 * self.cell_width,
            BODY_PAD_Y + cur_row as f32 * self.line_height,
        );
        if (cur_row, cur_col) != self.cursor_cell {
            let same_row = self.cursor_cell.0 == cur_row;
            let dcol = cur_col as i64 - self.cursor_cell.1 as i64;
            let first = self.cursor_cell == (usize::MAX, usize::MAX);
            let small = same_row && dcol.abs() <= CURSOR_GLIDE_MAX_COLS;
            self.cursor_cell = (cur_row, cur_col);
            if focused && small && !first && !force_hide_cursor {
                self.cursor_anim_start = Some(Instant::now());
                self.cursor_action_forward = dcol > 0;
                self.spawn_cursor_glide(cx);
            } else {
                self.cursor_anim_start = None; // snap
                self.cursor_px = target_px;
            }
        }

        // Exponential decay for smooth chasing (replaces fixed duration ease_out)
        let (mut cur_x, mut cur_y) = self.cursor_px;
        let dx = target_px.0 - cur_x;
        let dy = target_px.1 - cur_y;

        if dx.abs() > 0.5 || dy.abs() > 0.5 {
            // Lerp by a factor per frame. At 60fps, 0.4 is a nice fast ease.
            cur_x += dx * 0.4;
            cur_y += dy * 0.4;
            // When close enough, snap to target
            if (cur_x - target_px.0).abs() < 0.5 {
                cur_x = target_px.0;
            }
            if (cur_y - target_px.1).abs() < 0.5 {
                cur_y = target_px.1;
            }
        } else {
            cur_x = target_px.0;
            cur_y = target_px.1;
        }
        self.cursor_px = (cur_x, cur_y);

        // Calculate pop/squish animation offsets
        let mut width_offset = 0.0;
        let mut height_offset = 0.0;
        if let Some(start) = self.cursor_anim_start {
            let t =
                (start.elapsed().as_secs_f32() / (CURSOR_GLIDE_MS as f32 / 1000.0)).clamp(0.0, 1.0);
            if t >= 1.0 {
                self.cursor_anim_start = None;
            } else {
                // Parabola: 4 * t * (1 - t) goes 0 -> 1 -> 0
                let pop = 4.0 * t * (1.0 - t);
                if self.cursor_action_forward {
                    // Typing: widen obviously, shrink height obviously
                    width_offset = self.cell_width * 0.7 * pop;
                    height_offset = -self.line_height * 0.3 * pop;
                } else {
                    // Deleting: squeeze width obviously, stretch height obviously
                    width_offset = -self.cell_width * 0.45 * pop;
                    height_offset = self.line_height * 0.45 * pop;
                }
            }
        }

        // 字符淡入淡出(原 §3.1)已删:fade-in 在快速打字 / Claude spinner 下像多光标
        // 残块、fade-out 像删除延迟残影 —— 产品上已禁用(push 早被注释)。连同每帧
        // cache-miss 时对全网格的 rows_to_cells + 逐格 diff 一并删去:Claude 高频整屏
        // 重绘时那是纯 CPU 浪费,debug 下 opt-level=0 更会放大成可感卡顿。

        // The glyph under the cursor (≈1 col/char; cursor-on-wide-char is rare) so the
        // focused block can redraw it in the background color = a crisp **inverse**
        // cursor instead of a muddy translucent overlay. Whitespace → just the block.
        let cursor_char = rows.get(cur_row).and_then(|row| {
            let mut c = 0usize;
            for run in row {
                let is_wide = run.cols == 2 && run.text.chars().count() == 1;
                for ch in run.text.chars() {
                    // Match either the leading cell or the trailing phantom cell of a wide char
                    if c == cur_col || (is_wide && c + 1 == cur_col) {
                        return (!ch.is_whitespace()).then_some(ch);
                    }
                    c += if is_wide { 2 } else { 1 };
                }
            }
            None
        });
        // (force_hide_cursor 已在上方光标动画前算出:ConPTY 常丢 `\e[?25l`,Claude/Ink
        // 自绘虚拟光标,这里据此剔除物理光标,避免"双光标"重影。)
        let cursor_el = (cursor_visible
            && !force_hide_cursor
            && (draw_solid || !focused)
            && cur_row < self.size.rows
            && cur_col < self.size.cols)
            .then(|| {
                let is_block = cursor_shape == tn_core::CursorShape::Block;
                let is_underline = cursor_shape == tn_core::CursorShape::Underline;

                // A double-width (CJK / wide-emoji) glyph spans two cells; the
                // block/underline must cover both or it sits over only the left
                // half of the character (visible when moving back over CJK text).
                let cursor_cols = rows
                    .get(cur_row)
                    .map(|row| cursor_cell_cols(row, cur_col))
                    .unwrap_or(1);
                let anim_w = (if is_block || is_underline {
                    cursor_cols as f32 * self.cell_width
                } else {
                    2.0
                }) + width_offset;
                let anim_h = self.line_height + height_offset;
                let anim_x = cur_x - width_offset / 2.0;
                let anim_y = cur_y - height_offset / 2.0;

                let base = div()
                    .absolute()
                    // Glided position (eases toward the target cell; see above). Already
                    // includes BODY_PAD — the grid is inset from the content edge.
                    .left(px(anim_x))
                    .top(px(anim_y))
                    .w(px(anim_w))
                    .h(px(anim_h))
                    .rounded(px(1.));
                if draw_solid {
                    if is_block {
                        // Opaque block in the cursor color + the glyph redrawn in the bg
                        // color on top = sharp inverse cursor (the char stays crisp, not
                        // dimmed). The block sits over the grid glyph and hides it.
                        base.bg(col(self.palette.cursor))
                            .text_color(col(bg))
                            .when_some(cursor_char, |d, ch| {
                                d.child(SharedString::from(ch.to_string()))
                            })
                    } else if is_underline {
                        // Underline cursor: a thin bar at the bottom
                        div()
                            .absolute()
                            .left(px(anim_x))
                            .top(px(anim_y + anim_h - 2.0))
                            .w(px(anim_w))
                            .h(px(2.0))
                            .bg(col(self.palette.cursor))
                    } else {
                        // Beam cursor: a thin bar on the left
                        base.bg(col(self.palette.cursor))
                    }
                } else {
                    // Unfocused: a slim, calmer outline (thinner presence than a full block).
                    if is_block {
                        base.border_1()
                            .border_color(cola(self.palette.cursor, 0.55))
                    } else if is_underline {
                        div()
                            .absolute()
                            .left(px(anim_x))
                            .top(px(anim_y + anim_h - 2.0))
                            .w(px(anim_w))
                            .h(px(2.0))
                            .bg(cola(self.palette.cursor, 0.55))
                    } else {
                        base.bg(cola(self.palette.cursor, 0.55))
                    }
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

        // Scrollbar: a thin right-edge indicator of the viewport's
        // position within scrollback. Shown only when there's history; brighter
        // while actually scrolled up. The thumb's size = viewport / total content.
        let scrollbar = (scroll_history > 0).then(|| {
            let total = (scroll_history + self.size.rows) as f32;
            let thumb_h = (self.size.rows as f32 / total).clamp(0.06, 1.0);
            let top = ((scroll_history.saturating_sub(scroll_offset)) as f32 / total)
                .clamp(0.0, 1.0 - thumb_h);
            let scrolled = scroll_offset > 0 || self.scrollbar_drag.is_some();
            let thumb = scrollbar_thumb_style(rail_layout);
            let thumb_bg = if scrolled {
                thumb.active_bg
            } else {
                thumb.idle_bg
            };
            // 命中区比可见条宽得多(14px),透明、贴右缘、承接拖拽;里头一条细可见 bar
            // 靠右显示 → 视觉仍纤细、但好抓(修「滚动条可交互区域太小」)。
            div()
                .absolute()
                .top(relative(top))
                .right(px(0.))
                .w(px(14.))
                .h(relative(thumb_h))
                .flex()
                .justify_end()
                .items_center()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, ev: &MouseDownEvent, _w, cx| {
                        this.begin_scrollbar_drag(ev.position.y.into(), cx);
                    }),
                )
                .child(
                    div()
                        .w(px(thumb.width))
                        .h_full()
                        .mr(px(thumb.margin_right))
                        .rounded(px(2.))
                        .bg(rgba(thumb_bg)),
                )
        });

        // Visual bell: a brief translucent flash over the grid
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
        let term_area =
            div()
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
                    cx.listener(|this, ev: &MouseDownEvent, window, cx| {
                        this.on_mouse_down(ev, window, cx)
                    }),
                )
                .on_mouse_move(cx.listener(|this, ev: &MouseMoveEvent, window, cx| {
                    this.on_mouse_move(ev, window, cx)
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, ev: &MouseUpEvent, window, cx| {
                        this.on_mouse_up(ev, window, cx)
                    }),
                )
                .child(
                    canvas(
                        move |bounds, _window, _cx| *bounds_cell.borrow_mut() = bounds,
                        // Register the per-frame IME/text input handler so composed text
                        // (中文) reaches `replace_text_in_range`. No-op unless focused.
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
                            div().flex().flex_row().h(px(self.line_height)).children(
                                runs.iter().map(|r| {
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
                                        .child(SharedString::from(r.text.to_string()))
                                }),
                            )
                        })),
                )
                .when_some(cursor_el, |this, c| this.child(c))
                .when_some(ime_preedit, |this, p| this.child(p))
                .when_some(scrollbar, |this, s| this.child(s))
                .when_some(bell_overlay, |this, o| this.child(o));

        // agent:正文 + 右侧活动栏并排(mockup .abody = .body + .arail).
        // Launch-intent agents get a side-by-side rail from the first frame. Agents
        // detected from a running shell command use an overlay rail so the terminal
        // body width does not change mid-command and force a ConPTY repaint over
        // the agent's startup screen.
        // The activity rail is a capability slot (`git_diff`): an agent that
        // declares it gets「本次改动」; one that doesn't hosts full-width.
        if self.agent_surface_probe_frames > 0 {
            let nonblank_rows = rows
                .iter()
                .filter(|runs| runs.iter().any(|r| !r.text.trim().is_empty()))
                .count();
            tracing::info!(
                target: "tn::agent_surface",
                frames_left = self.agent_surface_probe_frames,
                rail_layout = ?rail_layout,
                body_container = ?body_region_container_layout(rail_layout),
                agent_from_shell = self.agent_from_shell,
                agent = ?self.agent.as_ref(),
                bounds_w = bw,
                bounds_h = bh,
                grid_cols = self.size.cols,
                grid_rows = self.size.rows,
                nonblank_rows,
                generation,
                scroll_offset,
                scroll_history,
                block_bar_visible = block_bar.is_some(),
                "agent surface render probe"
            );
            self.agent_surface_probe_frames -= 1;
        }
        let body_region = match rail_layout {
            ActivityRailLayout::None => term_area,
            ActivityRailLayout::Flex => div()
                .flex_1()
                .min_h(px(0.))
                .flex()
                .flex_row() // mockup .abody
                .child(term_area)
                .child(self.render_activity_rail(cx)),
            ActivityRailLayout::Overlay => div()
                .relative()
                .flex_1()
                .min_h(px(0.))
                .min_w(px(0.))
                .flex()
                .flex_col()
                .overflow_hidden()
                .child(term_area)
                .child(
                    div()
                        .absolute()
                        .top(px(0.))
                        .right(px(0.))
                        .bottom(px(0.))
                        .w(px(ACTIVITY_RAIL_W))
                        .child(self.render_activity_rail(cx).h_full()),
                ),
        };

        // ── SSH connection cards (B1 progress / C1 error / B3 password). Only one
        // is ever active at a time (password > error > progress, gated below). ──
        // 浮层家族(SHEET 06):L3 不透明浮板 + 1px h2 + r6 + float 投影 + 纯色 scrim。
        let card_chrome = |inner: gpui::Div| -> gpui::Div {
            let panel = crate::style::shadowed(
                inner
                    .flex()
                    .flex_col()
                    .w(px(460.)) // SHEET 06/07 SSH 卡:浮层家族 460 宽
                    .max_w(relative(0.92))
                    .rounded(px(crate::style::R_PANEL))
                    .overflow_hidden()
                    .border_1()
                    .border_color(rgba(crate::style::H2))
                    .bg(gpui::rgb(crate::style::L3))
                    .on_mouse_down(gpui::MouseButton::Left, |_e, _w, cx| cx.stop_propagation()),
                crate::style::shadow_float(),
            );
            div()
                .absolute()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .bg(rgba(crate::style::SCRIM))
                .child(panel)
        };
        // SHEET 06/07 `.float-head`:38 高 · L4 顶面 · 底 1px h1 · mono — 彩色字形记号
        // + 标题 + 右侧身份 chip(原副标信息)。SSH 过程/失败/TOFU/密码/联网卡共用同一
        // 浮层头语法(原型与真机差异总结:SSH 卡片沿用浮层家族,不再用图标徽章头)。
        let card_header = |icon_name: &'static str, accent: Rgb, title: &str, chip: &str| {
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .h(px(38.))
                .px(px(14.))
                .flex_none()
                .bg(gpui::rgb(crate::style::L4))
                .border_b(px(1.))
                .border_color(rgba(crate::style::H1))
                .font_family(self.font_family.clone())
                .child(crate::style::icon(icon_name, 14., accent))
                .child(
                    div()
                        .flex_none()
                        .text_size(px(12.5))
                        .font_weight(FontWeight(600.))
                        .text_color(col(self.ui_fg))
                        .child(SharedString::from(title.to_string())),
                )
                .child(div().flex_1().min_w(px(0.)))
                .child(
                    // 右侧身份 chip(`.chip`):accent 边 + soft 底 + accent 字,mono 10
                    div()
                        .max_w(px(250.))
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .px(px(8.))
                        .py(px(2.))
                        .rounded(px(crate::style::R_CHIP))
                        .border_1()
                        .border_color(cola(accent, 0.3))
                        .bg(cola(accent, 0.10))
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(col(accent))
                        .child(SharedString::from(chip.to_string())),
                )
        };

        // B1: connection progress card. Suppressed during a *reconnect* (B4 shows
        // the slim banner instead of the big card).
        let ssh_progress_card = (self.ssh_password_prompt.is_none()
            && self.ssh_error.is_none()
            && self.ssh_hostkey.is_none()
            && self.ssh_conn != Some(SshConnState::Reconnecting))
        .then_some(self.ssh_progress.as_ref())
        .flatten()
        .map(|(phase, detail)| {
            let cur = phase.ordinal();
            let steps = [
                (tn_pty::SshPhase::Connecting, "连接"),
                (tn_pty::SshPhase::Authenticating, "认证"),
                (tn_pty::SshPhase::OpeningShell, "打开 shell"),
            ];
            // B1 progress: three equal columns so long hosts never push the card
            // wider than the SSH float panel.
            let mut steps_row = div().flex().flex_row().items_start().gap(px(8.));
            for (p, label) in steps {
                let o = p.ordinal();
                // .dotn:18×18 圆 · 1px 边 · mono 10 字形
                let dotn = div()
                    .w(px(18.))
                    .h(px(18.))
                    .flex_none()
                    .rounded_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .border_1()
                    .text_size(px(crate::style::FS_MICRO));
                let dotn = if o < cur {
                    dotn.border_color(cola(self.ui_green, 0.4))
                        .bg(gpui::rgb(crate::style::L2))
                        .text_color(col(self.ui_green))
                        .child("✓")
                } else if o == cur {
                    dotn.border_color(gpui::rgb(crate::style::PH))
                        .bg(gpui::rgb(crate::style::PH))
                        .text_color(gpui::rgb(crate::style::PH_INK))
                        .child("●")
                } else {
                    dotn.border_color(rgba(crate::style::H1))
                        .bg(gpui::rgb(crate::style::L2))
                        .text_color(gpui::rgb(crate::style::T3))
                        .child("○")
                };
                // .stp 文字:busy = t0,done/pending = t2
                let label_color = if o == cur { self.ui_fg } else { self.ui_muted };
                let detail_owned = detail.clone();
                steps_row = steps_row.child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .font_family(self.font_family.clone())
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(col(label_color))
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(7.))
                                .child(dotn)
                                .child(
                                    div()
                                        .min_w(px(0.))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .child(SharedString::from(label.to_string())),
                                ),
                        )
                        .when(o == cur && !detail_owned.is_empty(), |d| {
                            d.child(
                                div()
                                    .min_w(px(0.))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .text_color(col(self.ui_muted))
                                    .child(SharedString::from(detail_owned)),
                            )
                        }),
                );
            }
            // body:p16 容器 — 横排步骤 + 认证顺序提示(SHEET 07 板 C)
            let steps_col = div()
                .p(px(16.))
                .flex()
                .flex_col()
                .gap(px(10.))
                .child(steps_row)
                .child(
                    div()
                        .font_family(self.font_family.clone())
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(gpui::rgb(crate::style::T2))
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(SharedString::from(
                            "id_ed25519 → agent → password; 密码框支持显隐 / 记住本会话 / 重试",
                        )),
                );
            let cancel = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(12.))
                .py(px(7.))
                .rounded(px(8.))
                .bg(gpui::rgb(crate::style::L2))
                .text_color(col(self.ui_fg))
                .hover(|s| s.bg(gpui::rgb(crate::style::L4)))
                .child(crate::style::icon("close", 13., self.ui_muted))
                .child(div().text_size(px(crate::style::FS_CAPTION)).child(SharedString::from("取消")))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|_this, _e, _w, cx| {
                        cx.stop_propagation();
                        cx.emit(SshCloseRequested);
                    }),
                );
            card_chrome(
                div()
                    .child(card_header(
                        "globe",
                        self.ui_accent,
                        "正在连接",
                        &self.ssh_target,
                    ))
                    .child(steps_col)
                    .child(div().h(px(1.)).bg(rgba(crate::style::H1)))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_end()
                            .p(px(11.))
                            .child(cancel),
                    ),
            )
        });

        // C1: actionable error card.
        let ssh_error_card = self
            .ssh_password_prompt
            .is_none()
            .then_some(self.ssh_error.as_ref())
            .flatten()
            .map(|info| {
                let is_auth = info.kind == tn_pty::SshErrorKind::Auth;
                let can_retry = matches!(
                    info.kind,
                    tn_pty::SshErrorKind::Network | tn_pty::SshErrorKind::Auth
                );
                let title = match info.kind {
                    tn_pty::SshErrorKind::Network => "连接失败",
                    tn_pty::SshErrorKind::Auth => "认证失败",
                    tn_pty::SshErrorKind::HostKeyMismatch => "主机密钥已更改",
                };
                let body_text = match info.kind {
                    tn_pty::SshErrorKind::Network => {
                        if info.detail.is_empty() {
                            "无法连接到目标主机,已停止自动重试。".to_string()
                        } else {
                            info.detail.clone()
                        }
                    }
                    tn_pty::SshErrorKind::Auth => {
                        if info.offered.is_empty() || info.offered == "(未知)" {
                            "密钥被拒或密码错误。".to_string()
                        } else {
                            format!("密钥被拒或密码错误。服务器开放的方式:{}。", info.offered)
                        }
                    }
                    tn_pty::SshErrorKind::HostKeyMismatch => {
                        if info.detail.is_empty() {
                            "服务器指纹与 ~/.ssh/known_hosts 记录不符 —— 可能是服务器重装,也可能是中间人攻击。已中止连接。".to_string()
                        } else {
                            format!("服务器指纹与 ~/.ssh/known_hosts 记录不符 —— 可能是服务器重装,也可能是中间人攻击。已中止连接。\n服务器本次指纹:{}", info.detail)
                        }
                    }
                };
                // Yellow hint box (auth only): the backend's server-config hint, or a generic one.
                let hint = if is_auth {
                    Some(if info.detail.is_empty() {
                        "提示:若确信密码正确,服务器可能设了 PermitRootLogin prohibit-password 或 PasswordAuthentication no,需在服务端放开。".to_string()
                    } else {
                        info.detail.clone()
                    })
                } else {
                    None
                };
                let retry = div()
                    .flex().flex_row().items_center().gap(px(6.)).px(px(12.)).py(px(7.)).rounded(px(8.))
                    .bg(cola(self.ui_accent, 0.16)).text_color(col(self.ui_accent))
                    .hover(|s| s.bg(cola(self.ui_accent, 0.24)))
                    .child(crate::style::icon("refresh", 13., self.ui_accent))
                    .child(div().text_size(px(crate::style::FS_CAPTION)).child(SharedString::from("重试")))
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(|_this, _e, _w, cx| {
                        cx.stop_propagation();
                        cx.emit(SshRetryRequested);
                    }));
                let close = div()
                    .flex().flex_row().items_center().gap(px(6.)).px(px(12.)).py(px(7.)).rounded(px(8.))
                    .bg(gpui::rgb(crate::style::L2)).text_color(col(self.ui_fg))
                    .hover(|s| s.bg(gpui::rgb(crate::style::L4)))
                    .child(crate::style::icon("close", 13., self.ui_muted))
                    .child(div().text_size(px(crate::style::FS_CAPTION)).child(SharedString::from("关闭")))
                    .on_mouse_down(gpui::MouseButton::Left, cx.listener(|_this, _e, _w, cx| {
                        cx.stop_propagation();
                        cx.emit(SshCloseRequested);
                }));
                let mut btnrow = div().flex().flex_row().gap(px(8.)).justify_end().p(px(11.));
                if can_retry {
                    btnrow = btnrow.child(retry);
                }
                btnrow = btnrow.child(close);
                card_chrome(
                    div()
                        .child(card_header("alert", self.ui_red, title, &self.ssh_target))
                        .child(
                            div()
                                .px(px(14.))
                                .pt(px(11.))
                                .text_size(px(12.5))
                                .text_color(col(self.ui_fg))
                                .overflow_hidden()
                                .child(SharedString::from(body_text)),
                        )
                        .when_some(hint, |d, h| {
                            d.child(
                                div().mx(px(14.)).mt(px(11.)).p(px(10.)).rounded(px(8.))
                                    .bg(cola(self.ui_yellow, 0.08)).border_1().border_color(cola(self.ui_yellow, 0.22))
                                    .text_size(px(11.5)).text_color(col(self.ui_yellow))
                                    .child(SharedString::from(h)),
                            )
                        })
                        .child(div().h(px(1.)).bg(rgba(crate::style::H1)).mt(px(12.)))
                        .child(btnrow),
                )
            });

        // B3: password card — masked input + eye reveal + remember checkbox + error
        // line (in-place retry) + 连接/取消. Reuses the shared card chrome.
        let ssh_password_card = self.ssh_password_prompt.as_ref().map(|p| {
            let mono = self.font_family.clone();
            let revealed = self.ssh_password_reveal;
            let shown: SharedString = if self.ssh_password_input.is_empty() {
                SharedString::from("")
            } else if revealed {
                SharedString::from(self.ssh_password_input.clone())
            } else {
                SharedString::from("•".repeat(self.ssh_password_input.chars().count()))
            };
            let input_row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.))
                .px(px(14.))
                .py(px(11.))
                .child(crate::style::icon("lock", 15., self.ui_muted))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_row()
                        .items_center()
                        .font_family(mono.clone())
                        .text_size(px(crate::style::FS_LABEL))
                        .when(!self.ssh_password_input.is_empty(), |d| {
                            d.child(div().text_color(col(self.ui_fg)).child(shown))
                        })
                        .child(div().text_color(col(self.ui_muted)).child("▏"))
                        .when(self.ssh_password_input.is_empty(), |d| {
                            d.child(
                                div()
                                    .ml(px(2.))
                                    .text_color(col(self.ui_muted))
                                    .child("输入密码"),
                            )
                        }),
                )
                // 👁 reveal toggle
                .child(
                    div()
                        .flex_none()
                        .p(px(2.))
                        .rounded(px(6.))
                        .hover(|s| s.bg(gpui::rgb(crate::style::L2)))
                        .child(crate::style::icon(
                            "eye",
                            15.,
                            if revealed {
                                self.ui_accent
                            } else {
                                self.ui_muted
                            },
                        ))
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|this, _e, _w, cx| {
                                cx.stop_propagation();
                                this.ssh_password_reveal = !this.ssh_password_reveal;
                                cx.notify();
                            }),
                        ),
                );
            // remember checkbox
            let remembered = self.ssh_password_remember;
            let remember_row = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .px(px(14.))
                .py(px(9.))
                .child(
                    div()
                        .w(px(16.))
                        .h(px(16.))
                        .flex_none()
                        .rounded(px(5.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(remembered, |d| d.bg(cola(self.ui_accent, 0.9)))
                        .when(!remembered, |d| {
                            d.border_1().border_color(cola(self.ui_muted, 0.6))
                        })
                        .when(remembered, |d| {
                            d.child(crate::style::icon("check", 12., self.palette.bg))
                        }),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(col(self.ui_muted))
                        .child("记住密码(仅本会话)"),
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.ssh_password_remember = !this.ssh_password_remember;
                        cx.notify();
                    }),
                );
            // buttons
            let connect = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(12.))
                .py(px(7.))
                .rounded(px(8.))
                .bg(cola(self.ui_accent, 0.16))
                .text_color(col(self.ui_accent))
                .hover(|s| s.bg(cola(self.ui_accent, 0.24)))
                .child(crate::style::icon("enter", 13., self.ui_accent))
                .child(div().text_size(px(crate::style::FS_CAPTION)).child("连接"))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.submit_ssh_password(cx);
                    }),
                );
            let cancel = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(12.))
                .py(px(7.))
                .rounded(px(8.))
                .bg(gpui::rgb(crate::style::L2))
                .text_color(col(self.ui_fg))
                .hover(|s| s.bg(gpui::rgb(crate::style::L4)))
                .child(div().text_size(px(crate::style::FS_CAPTION)).child("取消"))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.cancel_ssh_password(cx);
                    }),
                );
            card_chrome(
                div()
                    .child(card_header("lock", self.ui_accent, "输入密码", &p.prompt))
                    .when_some(p.error.clone(), |d, err| {
                        d.child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap(px(6.))
                                .px(px(14.))
                                .pt(px(10.))
                                .child(crate::style::icon("alert", 13., self.ui_red))
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(col(self.ui_red))
                                        .child(SharedString::from(err)),
                                ),
                        )
                    })
                    .child(input_row)
                    .child(remember_row)
                    .child(div().h(px(1.)).bg(rgba(crate::style::H1)))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .justify_end()
                            .p(px(11.))
                            .child(cancel)
                            .child(connect),
                    ),
            )
        });

        // B2: host-key trust panel (TOFU) — shown on first contact with an
        // unrecognized host, before auth. SHEET 06-C:warn ⇄ 头 + 指纹凹井 +
        // 三按钮「仅本次信任 / 写入 known_hosts(primary 磷光)/ 取消」
        // (差异总结遗留:曾是 checkbox + 两按钮)。
        let ssh_hostkey_card = self.ssh_hostkey.as_ref().map(|hk| {
            // SHEET 06 板 C `.fp`:指纹块 = L0 凹井 + 1px h0 + r4 + mono 磷光指纹。
            let fp_box = div()
                .mx(px(14.)).mt(px(11.)).p(px(10.)).rounded(px(crate::style::R_CARD))
                .bg(gpui::rgb(crate::style::L0)).border_1().border_color(rgba(crate::style::H0))
                .child(div().text_size(px(crate::style::FS_MICRO)).text_color(col(self.ui_muted)).child("ED25519 / SHA256 指纹"))
                .child(div().font_family(self.font_family.clone()).text_size(px(crate::style::FS_CAPTION)).text_color(gpui::rgb(crate::style::PH)).mt(px(3.)).child(SharedString::from(hk.fingerprint.clone())));
            // `.btn` 家族(06-C):普通 = L2 + h1;primary = ph 底 + ph-ink 墨字。
            let btn = |label: &'static str| {
                div()
                    .px(px(12.)).py(px(5.)).rounded(px(crate::style::R_CARD))
                    .text_size(px(crate::style::FS_CAPTION)).text_color(gpui::rgb(crate::style::T1))
                    .bg(gpui::rgb(crate::style::L2)).border_1().border_color(rgba(crate::style::H1))
                    .hover(|s| s.bg(gpui::rgb(crate::style::L4)).text_color(gpui::rgb(crate::style::T0)))
                    .child(label)
            };
            let once = btn("仅本次信任").on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    cx.stop_propagation();
                    this.ssh_hostkey_remember = false;
                    this.trust_host_key(cx);
                }),
            );
            let save = btn("写入 known_hosts")
                .bg(gpui::rgb(crate::style::PH))
                .border_color(gpui::rgb(crate::style::PH))
                .text_color(gpui::rgb(crate::style::PH_INK))
                .font_weight(gpui::FontWeight(600.))
                .hover(|s| s.bg(gpui::rgb(crate::style::PH)))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.ssh_hostkey_remember = true;
                        this.trust_host_key(cx);
                    }),
                );
            let cancel = btn("取消").on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, _e, _w, cx| {
                    cx.stop_propagation();
                    this.reject_host_key(cx);
                }),
            );
            card_chrome(
                div()
                    .child(card_header("exchange", self.ui_yellow, "首次连接", &hk.host))
                    .child(
                        div().px(px(14.)).pt(px(11.)).text_size(px(crate::style::FS_CAPTION)).text_color(col(self.ui_muted))
                            .child("无法验证主机真实性。请确认下方指纹与服务器实际指纹一致,再选择信任。"),
                    )
                    .child(fp_box)
                    .child(
                        div().px(px(14.)).pt(px(8.)).pb(px(11.)).text_size(px(crate::style::FS_MICRO))
                            .font_family(self.font_family.clone())
                            .text_color(col(self.ui_muted))
                            .child("⚠ 指纹不符时将变为 HOST KEY MISMATCH 红边错误卡"),
                    )
                    .child(div().h(px(1.)).bg(rgba(crate::style::H1)))
                    .child(
                        div().flex().flex_row().items_center().gap(px(8.)).justify_end().h(px(46.)).px(px(11.))
                            .child(once).child(save).child(cancel),
                    ),
            )
        });

        // B4: non-modal reconnect banner (pane top) while disconnected/reconnecting.
        let ssh_banner = matches!(
            self.ssh_conn,
            Some(SshConnState::Disconnected) | Some(SshConnState::Reconnecting)
        )
        .then(|| {
            let reconnecting = self.ssh_conn == Some(SshConnState::Reconnecting);
            let msg = if reconnecting {
                format!("与 {} 的连接已断开,正在重连…", self.ssh_target)
            } else {
                format!("与 {} 的连接已断开,即将自动重连…", self.ssh_target)
            };
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(8.))
                .px(px(13.))
                .py(px(7.))
                .flex_none()
                .bg(cola(self.ui_yellow, 0.12))
                .child(crate::style::icon("refresh", 13., self.ui_yellow))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(0.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(px(11.5))
                        .text_color(col(self.ui_yellow))
                        .child(SharedString::from(msg)),
                )
                .child(
                    div()
                        .px(px(9.))
                        .py(px(3.))
                        .rounded(px(7.))
                        .text_size(px(crate::style::FS_MICRO))
                        .text_color(col(self.ui_fg))
                        .bg(gpui::rgb(crate::style::L2))
                        .hover(|s| s.bg(gpui::rgb(crate::style::L4)))
                        .child("取消")
                        .on_mouse_down(
                            gpui::MouseButton::Left,
                            cx.listener(|_this, _e, _w, cx| {
                                cx.stop_propagation();
                                cx.emit(SshCloseRequested);
                            }),
                        ),
                )
        });

        // Networked telemetry sidecar awaiting confirmation (default-deny gate):
        // shows the command + which networked runtime, with 拒绝/允许. 允许 spawns
        // the `ExternalProcessAdapter`; 拒绝 hosts the agent without it.
        let sidecar_card = self.sidecar_confirm.as_ref().map(|c| {
            let cmd = c
                .descriptor
                .realtime_command
                .as_ref()
                .map(|v| v.join(" "))
                .unwrap_or_default();
            let agent_label = self
                .agent_label
                .as_ref()
                .map(|s| s.to_string())
                .unwrap_or_else(|| "外部 Agent".to_string());
            let allow = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(12.))
                .py(px(7.))
                .rounded(px(8.))
                .bg(cola(self.ui_accent, 0.16))
                .text_color(col(self.ui_accent))
                .hover(|s| s.bg(cola(self.ui_accent, 0.24)))
                .child(crate::style::icon("check", 13., self.ui_accent))
                .child(
                    div()
                        .text_size(px(crate::style::FS_CAPTION))
                        .child(SharedString::from("允许并连接")),
                )
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.confirm_sidecar(cx);
                    }),
                );
            let deny = div()
                .flex()
                .flex_row()
                .items_center()
                .gap(px(6.))
                .px(px(12.))
                .py(px(7.))
                .rounded(px(8.))
                .bg(gpui::rgb(crate::style::L2))
                .text_color(col(self.ui_fg))
                .hover(|s| s.bg(gpui::rgb(crate::style::L4)))
                .child(crate::style::icon("close", 13., self.ui_muted))
                .child(div().text_size(px(crate::style::FS_CAPTION)).child(SharedString::from("拒绝")))
                .on_mouse_down(
                    gpui::MouseButton::Left,
                    cx.listener(|this, _e, _w, cx| {
                        cx.stop_propagation();
                        this.deny_sidecar(cx);
                    }),
                );
            card_chrome(
                div()
                    .child(card_header(
                        "alert",
                        self.ui_yellow,
                        "Agent 要联网",
                        &agent_label,
                    ))
                    .child(
                        div()
                            .px(px(14.))
                            .pt(px(11.))
                            .text_size(px(12.5))
                            .text_color(col(self.ui_fg))
                            .child(SharedString::from(
                                "这个 Agent 的遥测 sidecar 要联网。默认拒绝,确认后才运行。",
                            )),
                    )
                    .child(
                        div()
                            .mx(px(14.))
                            .mt(px(9.))
                            .p(px(9.))
                            .rounded(px(crate::style::R_CARD))
                            .bg(gpui::rgb(crate::style::L0)) // 命令凹井(同 `.fp`)
                            .border_1()
                            .border_color(rgba(crate::style::H0))
                            .font_family(self.font_family.clone())
                            .text_size(px(11.5))
                            .text_color(col(self.ui_muted))
                            .child(SharedString::from(cmd)),
                    )
                    .child(div().h(px(1.)).bg(rgba(crate::style::H1)).mt(px(12.)))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap(px(8.))
                            .justify_end()
                            .p(px(11.))
                            .child(deny)
                            .child(allow),
                    ),
            )
        });

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(
                cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)),
            )
            // 从文件管理器拖文件/目录进窗格 → 路径落入输入行(等用户回车再发)。整个
            // 窗格都是落点;拖悬时整面磷光微亮提示可放下。
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                this.drop_external_paths(paths.paths(), window, cx)
            }))
            .drag_over::<ExternalPaths>(|style, _paths, _window, _cx| {
                style.bg(rgba(crate::style::PH_DIM))
            })
            // 从 Tn 自己的资源管理器左键拖文件/目录进窗格 → 路径落入输入行(等回车再发)。
            .on_drop(
                cx.listener(|this, dragged: &crate::explorer::DraggedFile, window, cx| {
                    this.drop_dragged_file(&dragged.file, window, cx)
                }),
            )
            .drag_over::<crate::explorer::DraggedFile>(|style, _d, _window, _cx| {
                style.bg(rgba(crate::style::PH_DIM))
            })
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            // Back-most, out-of-flow probe: records the pane's own width so next
            // frame's rail gate (窄面板自适应折叠) reads a measurement that the rail
            // itself can't perturb. No paint, no hit-testing — pure measurement.
            .child(
                canvas(
                    move |bounds, _window, _cx| {
                        *pane_width_cell.borrow_mut() = f32::from(bounds.size.width)
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .rounded(px(crate::style::R_PANEL - 1.)) // match the plate's inner radius
            .bg(rgba(0x00000000)) // 透明:终端默认底透出 render_node 的 L1 板面(磷光)
            .text_color(col(fg))
            .font_family(self.font_family.clone())
            .text_size(px(self.font_size))
            .line_height(px(self.line_height))
            .when_some(ssh_banner, |this, b| this.child(b))
            .when_some(header, |this, h| this.child(h))
            .child(body_region)
            .when_some(block_bar, |this, bar| this.child(bar))
            .when_some(ssh_progress_card, |this, p| this.child(p))
            .when_some(ssh_error_card, |this, p| this.child(p))
            .when_some(ssh_password_card, |this, p| this.child(p))
            .when_some(ssh_hostkey_card, |this, p| this.child(p))
            .when_some(sidecar_card, |this, p| this.child(p))
            // 额度 popover + transparent click-catcher: a click anywhere outside the
            // popover dismisses it (the popover itself stops propagation).
            .when(self.quota_open, |this| {
                this.child(
                    div()
                        .absolute()
                        .size_full()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|v, _e, _w, cx| {
                                v.quota_open = false;
                                cx.notify();
                            }),
                        )
                        .child(self.render_quota_panel()),
                )
            })
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

    /// Seed registry (Claude + Codex) for launch-spec inference in tests.
    /// The default app intentionally starts from an empty registry + config
    /// manifests; these tests exercise the optional telemetry adapters.
    fn reg() -> tn_agent::AgentRegistry {
        tn_ai::builtin_registry()
    }

    #[test]
    fn wsl_profile_launches_wsl_exe_with_distro() {
        let p =
            first_profile("[[profiles]]\nname = \"Ubuntu\"\nkind = \"wsl\"\ndistro = \"Ubuntu\"\n");
        let spec = LaunchSpec::from_profile(&p, &reg()).expect("wsl profile is launchable");
        assert_eq!(spec.program, "wsl.exe");
        assert_eq!(
            spec.args,
            vec![
                "-d".to_string(),
                "Ubuntu".to_string(),
                "--cd".to_string(),
                "~".to_string()
            ]
        );
        assert_eq!(
            spec.file_namespace,
            FileNamespace::Wsl {
                distro: Some("Ubuntu".into())
            }
        );
        assert!(spec.integrate_pwsh); // bash integration reserves rail space
        assert!(spec.agent.is_none());
    }

    #[test]
    fn wsl_profile_without_distro_runs_default() {
        let p = first_profile("[[profiles]]\nname = \"WSL\"\nkind = \"wsl\"\n");
        let spec = LaunchSpec::from_profile(&p, &reg()).expect("wsl profile is launchable");
        assert_eq!(spec.program, "wsl.exe");
        assert_eq!(spec.args, vec!["--cd".to_string(), "~".to_string()]);
    }

    // ── Launch-path coverage ────────────────────────────────────────────────
    // The SSH / native-pwsh / hosted-agent paths were previously untested; these
    // pin each one so the per-kind refactor (and future edits) stay honest.

    #[test]
    fn ssh_profile_builds_config_and_label() {
        let p = first_profile(
            "[[profiles]]\nname=\"box\"\nkind=\"ssh\"\nhost=\"example.com\"\nuser=\"alice\"\n",
        );
        let spec = LaunchSpec::from_profile(&p, &reg()).expect("ssh profile is launchable");
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
        assert!(
            LaunchSpec::from_profile(&p, &reg()).is_none(),
            "no host -> not launchable"
        );
    }

    #[test]
    fn native_pwsh_runs_directly_with_integration() {
        let p = first_profile("[[profiles]]\nname=\"PS\"\ncommand=\"powershell.exe\"\n");
        let spec = LaunchSpec::from_profile(&p, &reg()).expect("pwsh is launchable");
        assert_eq!(spec.program, "powershell.exe");
        assert!(spec.integrate_pwsh, "native pwsh gets OSC 133 integration");
        assert_eq!(spec.args, vec!["-NoLogo".to_string()]); // empty args defaulted
        assert!(spec.ssh.is_none());
        assert!(spec.agent.is_none());
    }

    #[test]
    fn launch_runtime_reflects_backend() {
        use tn_agent::AgentRuntimeKind;
        // LocalPty for a plain/native pwsh; WslPty for a WSL namespace; SshPty when
        // an SSH backend is present. Runtime is derived, distinct from FileNamespace.
        assert_eq!(LaunchSpec::pwsh().runtime(), AgentRuntimeKind::LocalPty);
        let wsl = LaunchSpec::from_profile(
            &first_profile("[[profiles]]\nname=\"U\"\nkind=\"wsl\"\ndistro=\"Ubuntu\"\n"),
            &reg(),
        )
        .unwrap();
        assert_eq!(wsl.runtime(), AgentRuntimeKind::WslPty);
        let ssh = LaunchSpec::from_profile(
            &first_profile("[[profiles]]\nname=\"b\"\nkind=\"ssh\"\nhost=\"h\"\nuser=\"u\"\n"),
            &reg(),
        )
        .unwrap();
        assert_eq!(ssh.runtime(), AgentRuntimeKind::SshPty);
    }

    #[test]
    fn agent_command_is_hosted_in_pwsh_with_noexit() {
        let p = first_profile("[[profiles]]\nname=\"Claude\"\ncommand=\"claude\"\n");
        let spec = LaunchSpec::from_profile(&p, &reg()).expect("claude is launchable");
        assert_eq!(spec.program, "powershell.exe", "hosted inside pwsh");
        assert!(!spec.integrate_pwsh);
        assert_eq!(
            spec.agent,
            Some(AgentId::new("claude")),
            "agent inferred from command"
        );
        assert!(
            spec.args.contains(&"-NoExit".to_string()),
            "persistent keeps -NoExit"
        );
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
    fn builtin_agents_launch_without_inline_defaults() {
        // Built-in Claude/Codex run as their native full-screen TUIs: no
        // inline-mode env/args are injected. (History is owned by Tn's transcript
        // surface, sourced from the session log — not terminal scrollback, which
        // these agents never fully populate.)
        let claude = first_profile("[[profiles]]\nname=\"Claude\"\ncommand=\"claude\"\n");
        let claude_spec = LaunchSpec::from_profile(&claude, &reg()).expect("claude is launchable");
        assert!(
            !claude_spec
                .env
                .iter()
                .any(|(k, _)| k == "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN"),
            "Claude must not be forced into inline mode, got {:?}",
            claude_spec.env
        );

        let codex = first_profile(
            "[[profiles]]\nname=\"Codex\"\ncommand=\"codex\"\nargs=[\"resume\", \"--last\"]\n",
        );
        let codex_spec = LaunchSpec::from_profile(&codex, &reg()).expect("codex is launchable");
        let invoke = codex_spec
            .args
            .iter()
            .find(|a| a.contains("& 'codex'"))
            .expect("hosted command");
        assert!(
            !invoke.contains("--no-alt-screen"),
            "Codex must not be forced into inline mode, got {invoke:?}"
        );
        // Profile args are still passed through untouched.
        assert!(invoke.contains("& 'codex' 'resume' '--last'"), "got {invoke:?}");
    }

    #[test]
    fn config_backed_builtin_agents_have_no_inline_defaults() {
        let cfg = tn_config::Config::from_toml_str(
            "[[agents]]\n\
             id=\"claude\"\n\
             aliases=[\"claude\"]\n\
             manages_own_cursor=true\n\
             \n\
             [[agents]]\n\
             id=\"codex\"\n\
             aliases=[\"codex\"]\n\
             \n\
             [[profiles]]\n\
             name=\"Claude\"\n\
             kind=\"agent\"\n\
             agent=\"claude\"\n\
             command=\"claude\"\n\
             \n\
             [[profiles]]\n\
             name=\"Codex\"\n\
             kind=\"agent\"\n\
             agent=\"codex\"\n\
             command=\"codex\"\n",
        )
        .expect("config parses");
        let mut reg = tn_agent::AgentRegistry::new();
        for manifest in &cfg.agents {
            match tn_ai::builtin_adapter_for_manifest(manifest) {
                Some(adapter) => reg.register(adapter),
                None => reg.register_manifest(manifest),
            }
        }

        let claude = LaunchSpec::from_profile(&cfg.profiles[0], &reg).expect("claude launchable");
        assert!(!claude
            .env
            .iter()
            .any(|(k, _)| k == "CLAUDE_CODE_DISABLE_ALTERNATE_SCREEN"));

        let codex = LaunchSpec::from_profile(&cfg.profiles[1], &reg).expect("codex launchable");
        let invoke = codex
            .args
            .iter()
            .find(|a| a.contains("& 'codex'"))
            .expect("hosted command");
        assert!(
            !invoke.contains("--no-alt-screen"),
            "config-backed Codex must not inherit an inline arg, got {invoke:?}"
        );
    }

    #[test]
    fn ephemeral_hosted_agent_omits_noexit_and_sentinel() {
        let p = first_profile("[[profiles]]\nname=\"Codex\"\ncommand=\"codex\"\n");
        let spec = LaunchSpec::from_profile_ephemeral(&p, &reg()).expect("codex is launchable");
        assert_eq!(spec.agent, Some(AgentId::new("codex")));
        assert!(
            !spec.args.contains(&"-NoExit".to_string()),
            "ephemeral drops -NoExit"
        );
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
    fn non_pty_only_agent_is_not_launched_by_pty_launcher() {
        let cfg = tn_config::Config::from_toml_str(
            "[[agents]]\n\
             id=\"bridge\"\n\
             aliases=[\"bridge\"]\n\
             runtime_support=[\"structured\", \"http\"]\n\
             allow_network=true\n\
             \n\
             [[profiles]]\n\
             name=\"Bridge\"\n\
             kind=\"agent\"\n\
             command=\"bridge\"\n\
             agent=\"bridge\"\n",
        )
        .expect("config parses");
        let mut reg = tn_agent::AgentRegistry::new();
        for manifest in &cfg.agents {
            reg.register_manifest(manifest);
        }
        let profile = cfg.profiles.first().expect("profile");

        assert!(
            LaunchSpec::from_profile(profile, &reg).is_none(),
            "the current launcher only produces PTY runtimes; structured/http agents need a dedicated runtime confirmation path"
        );
    }

    #[test]
    fn terminal_fit_ignores_tiny_hidden_bounds() {
        assert_eq!(
            fit_grid_size_from_bounds(3.0, 3.0, 8.0, 18.0),
            None,
            "tab switch/minimize can report a tiny non-zero canvas; it must not resize the PTY to 1x1"
        );
    }

    #[test]
    fn mouse_hit_test_ignores_tiny_hidden_bounds() {
        assert_eq!(
            cell_at_from_bounds(
                50.0,
                50.0,
                0.0,
                0.0,
                3.0,
                3.0,
                8.0,
                18.0,
                GridSize::new(24, 80),
            ),
            None,
            "after a tab switch, stale tiny canvas bounds must not turn the next click into row 0/col 0"
        );
    }

    #[test]
    fn mouse_hit_test_rejects_body_padding_not_grid_cells() {
        assert_eq!(
            cell_at_from_bounds(
                0.0,
                0.0,
                0.0,
                0.0,
                800.0,
                500.0,
                8.0,
                18.0,
                GridSize::new(24, 80),
            ),
            None,
            "tab restore can report a transient (0,0) mouse position; body padding must not clamp it to cell 0,0"
        );
        assert_eq!(
            cell_at_from_bounds(
                BODY_PAD_X - 1.0,
                BODY_PAD_Y + 4.0,
                0.0,
                0.0,
                800.0,
                500.0,
                8.0,
                18.0,
                GridSize::new(24, 80),
            ),
            None,
            "left body padding is visual chrome, not terminal cell 0"
        );
    }

    #[test]
    fn mouse_hit_test_rejects_space_beyond_rendered_grid() {
        assert_eq!(
            cell_at_from_bounds(
                BODY_PAD_X + 80.0 * 8.0 + 1.0,
                BODY_PAD_Y + 4.0,
                0.0,
                0.0,
                800.0,
                500.0,
                8.0,
                18.0,
                GridSize::new(24, 80),
            ),
            None,
            "right-side spare body space must not clamp to the last terminal column"
        );
        assert_eq!(
            cell_at_from_bounds(
                BODY_PAD_X + 4.0,
                BODY_PAD_Y + 24.0 * 18.0 + 1.0,
                0.0,
                0.0,
                800.0,
                500.0,
                8.0,
                18.0,
                GridSize::new(24, 80),
            ),
            None,
            "bottom spare body space must not clamp to the last terminal row"
        );
    }

    #[test]
    fn selection_drag_ends_when_mouse_up_was_lost() {
        assert_eq!(
            selection_drag_move(false, None),
            SelectionDragMove::Idle,
            "no active selection drag means mouse moves are ignored"
        );
        assert_eq!(
            selection_drag_move(true, Some(MouseButton::Left)),
            SelectionDragMove::Update,
            "an active drag with the left button still down updates the selection"
        );
        assert_eq!(
            selection_drag_move(true, None),
            SelectionDragMove::EndLostMouseUp,
            "tab switches/minimize can swallow mouse_up; the next move must clear stale drag state"
        );
        assert_eq!(
            selection_drag_move(true, Some(MouseButton::Right)),
            SelectionDragMove::EndLostMouseUp,
            "any non-left pressed button is not a live text-selection drag"
        );
    }

    #[test]
    fn input_scrolls_to_bottom_for_any_scrollback_offset() {
        assert!(
            !should_scroll_to_bottom_before_input(0, 8),
            "offset 0 is already the live bottom"
        );
        assert!(
            should_scroll_to_bottom_before_input(1, 8),
            "any non-zero offset means the viewport is reading history"
        );
        assert!(
            should_scroll_to_bottom_before_input(8, 8),
            "the top of history is still not the live bottom"
        );
    }

    #[test]
    fn cursor_block_spans_two_cells_on_a_wide_glyph() {
        fn run(text: &str, cols: usize) -> tn_core::CellRun {
            tn_core::CellRun {
                text: text.into(),
                fg: tn_core::Rgb::default(),
                bg: tn_core::Rgb::default(),
                bold: false,
                italic: false,
                underline: false,
                cols,
            }
        }
        // Row: "ab" (narrow, cols 2) + "为" (wide, cols 2) + "x" (narrow, cols 1).
        // Cell layout: a=0, b=1, 为=2..=3 (leading cell 2), x=4.
        let row = vec![run("ab", 2), run("为", 2), run("x", 1)];
        assert_eq!(cursor_cell_cols(&row, 0), 1, "ascii 'a' is one cell");
        assert_eq!(cursor_cell_cols(&row, 1), 1, "ascii 'b' is one cell");
        assert_eq!(
            cursor_cell_cols(&row, 2),
            2,
            "cursor on the leading cell of a wide CJK glyph covers two cells"
        );
        assert_eq!(cursor_cell_cols(&row, 4), 1, "ascii 'x' after the wide glyph");
        // Past the end / empty row → one cell (plain block on a blank).
        assert_eq!(cursor_cell_cols(&row, 9), 1);
        assert_eq!(cursor_cell_cols(&[], 0), 1);
    }

    #[test]
    fn agent_and_alt_screen_resize_uses_bottom_anchoring() {
        assert_eq!(
            resize_anchoring_for_pane(false, false),
            tn_core::ResizeAnchoring::Top,
            "plain shells keep the ConPTY history-preserving top anchor"
        );
        assert_eq!(
            resize_anchoring_for_pane(true, false),
            tn_core::ResizeAnchoring::Bottom,
            "agent/TUI-like panes must not push their live screen into scrollback on resize"
        );
        assert_eq!(
            resize_anchoring_for_pane(false, true),
            tn_core::ResizeAnchoring::Bottom,
            "alternate-screen programs own their viewport and should resize bottom-anchored"
        );
    }

    #[test]
    fn agent_main_screen_scroll_wheel_uses_scrollback_alt_screen_drives_app() {
        assert_eq!(
            scroll_wheel_route(tn_core::InputMode::default()),
            ScrollWheelRoute::Scrollback,
            "main-screen panes use the mouse wheel for Tn scrollback"
        );
        assert_eq!(
            scroll_wheel_route(tn_core::InputMode {
                alt_screen: true,
                ..tn_core::InputMode::default()
            }),
            ScrollWheelRoute::AppArrows,
            "alternate-screen programs own the wheel"
        );
        assert_eq!(
            scroll_wheel_route(tn_core::InputMode {
                mouse_report: true,
                alt_screen: true,
                ..tn_core::InputMode::default()
            }),
            ScrollWheelRoute::MouseReport,
            "mouse-reporting apps must receive wheel reports before the alt-screen fallback"
        );
    }

    #[test]
    fn wheel_report_sgr_encodes_button_and_one_based_cell() {
        // Wheel-up at viewport (row 4, col 2) → 1-based (5, 3), SGR button 64.
        assert_eq!(
            encode_wheel_report(true, 3, 5, true, 1),
            b"\x1b[<64;3;5M".to_vec()
        );
        // Wheel-down uses button 65.
        assert_eq!(
            encode_wheel_report(false, 3, 5, true, 1),
            b"\x1b[<65;3;5M".to_vec()
        );
        // count repeats the whole report.
        assert_eq!(
            encode_wheel_report(true, 1, 1, true, 3),
            b"\x1b[<64;1;1M\x1b[<64;1;1M\x1b[<64;1;1M".to_vec()
        );
    }

    #[test]
    fn wheel_report_legacy_packs_plus_32_bytes_and_caps_at_223() {
        // Legacy X10: ESC[M then (button+32),(col+32),(row+32).
        assert_eq!(
            encode_wheel_report(true, 1, 1, false, 1),
            vec![0x1b, b'[', b'M', 64 + 32, 1 + 32, 1 + 32]
        );
        // Coordinates beyond 223 clamp to the byte-encoding ceiling.
        let big = encode_wheel_report(false, 500, 500, false, 1);
        assert_eq!(big, vec![0x1b, b'[', b'M', 65 + 32, 223 + 32, 223 + 32]);
    }

    #[test]
    fn agent_panes_hide_shell_block_bar_even_without_alt_screen() {
        assert!(
            should_render_block_bar(false, false),
            "plain main-screen shells show command block chrome"
        );
        assert!(
            !should_render_block_bar(false, true),
            "alternate-screen programs own the whole viewport"
        );
        assert!(
            !should_render_block_bar(true, false),
            "agent panes own the whole viewport even when their TUI stays on the main screen"
        );
        assert!(
            !should_render_block_bar(true, true),
            "agent alternate-screen panes also hide shell block chrome"
        );
    }

    #[test]
    fn shell_detected_agents_overlay_the_rail_without_resizing_terminal_body() {
        assert_eq!(
            activity_rail_layout(false, false, true),
            ActivityRailLayout::None,
            "plain shells do not show an activity rail"
        );
        assert_eq!(
            activity_rail_layout(true, false, true),
            ActivityRailLayout::Flex,
            "launch-intent agents start with a side-by-side rail"
        );
        assert_eq!(
            activity_rail_layout(true, true, true),
            ActivityRailLayout::Overlay,
            "shell-detected agents must not change terminal width mid-command"
        );
        assert_eq!(
            activity_rail_layout(true, true, false),
            ActivityRailLayout::None,
            "agents without the git-diff slot do not need the activity rail"
        );
        assert_eq!(
            body_region_container_layout(ActivityRailLayout::Overlay),
            BodyRegionContainerLayout::OverlayFlexColumn,
            "overlay rail must still host the terminal body in a flex column so flex_1 does not collapse"
        );
        assert_eq!(
            body_region_container_layout(ActivityRailLayout::Flex),
            BodyRegionContainerLayout::FlexRow,
            "launch-intent agents keep the side-by-side body/rail flex row"
        );
    }

    #[test]
    fn rail_collapses_on_narrow_pane_and_restores() {
        use ActivityRailLayout::*;
        // Wide enough → keep whatever the base layout decided (并排 / overlay).
        assert_eq!(rail_layout_for_width(Flex, RAIL_MIN_PANE_W + 1.0), Flex);
        assert_eq!(
            rail_layout_for_width(Overlay, RAIL_MIN_PANE_W + 1.0),
            Overlay
        );
        // Too narrow → drop the rail so the agent body keeps the full pane width.
        assert_eq!(rail_layout_for_width(Flex, RAIL_MIN_PANE_W - 1.0), None);
        assert_eq!(rail_layout_for_width(Overlay, RAIL_MIN_PANE_W - 1.0), None);
        // Already railless stays railless regardless of width.
        assert_eq!(rail_layout_for_width(None, 10.0), None);
        // Unmeasured first frame (f32::MAX default) keeps the rail — no flash.
        assert_eq!(rail_layout_for_width(Flex, f32::MAX), Flex);
    }

    #[cfg(windows)]
    #[test]
    fn dropped_path_is_relative_under_agent_cwd_else_absolute() {
        let base = Path::new(r"D:\coder\Tn");
        // Inside the repo → relative, original casing + separators kept.
        assert_eq!(
            render_drop_path(
                Path::new(r"D:\coder\Tn\crates\tn-ui\src\mod.rs"),
                Some(base)
            ),
            r"crates\tn-ui\src\mod.rs"
        );
        // Drive-letter casing differs → still matches (Windows case-insensitive).
        assert_eq!(
            render_drop_path(Path::new(r"d:\coder\Tn\README.md"), Some(base)),
            "README.md"
        );
        // Outside the repo → absolute fallback.
        assert_eq!(
            render_drop_path(Path::new(r"E:\other\file.txt"), Some(base)),
            r"E:\other\file.txt"
        );
        // Shell pane (no base) → always absolute.
        assert_eq!(
            render_drop_path(Path::new(r"D:\coder\Tn\a.rs"), None),
            r"D:\coder\Tn\a.rs"
        );
        // Dropping the cwd itself yields no relative remainder → absolute fallback.
        assert_eq!(render_drop_path(base, Some(base)), r"D:\coder\Tn");
    }

    #[cfg(windows)]
    #[test]
    fn multi_drop_joins_with_spaces() {
        let base = Path::new(r"D:\proj");
        let paths = vec![
            PathBuf::from(r"D:\proj\a.rs"),
            PathBuf::from(r"D:\proj\sub\b.rs"),
        ];
        assert_eq!(render_dropped_paths(&paths, Some(base)), r"a.rs sub\b.rs");
    }

    #[test]
    fn inner_catch_unwind_leaves_the_lock_unpoisoned() {
        // The reader catches an alacritty panic *inside* the lock
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
        assert!(
            m.lock().is_ok(),
            "the lock must survive a caught panic un-poisoned"
        );
    }
}
