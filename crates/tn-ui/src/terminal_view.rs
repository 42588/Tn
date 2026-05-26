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
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use futures::channel::mpsc;
use futures::StreamExt;
use gpui::{
    canvas, div, prelude::*, px, rgba, AsyncApp, Bounds, ClipboardItem, Context, Div, FocusHandle,
    FontWeight, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels,
    Point, Rgba, ScrollDelta, ScrollWheelEvent, SharedString, WeakEntity, Window,
};
use tn_blocks::BlockModel;
use tn_config::Loaded;
use tn_core::{GridSize, Palette, Rgb, TermEvent, Terminal};
use tn_pty::{LocalPty, PtyBackend, PtySize, SpawnSpec};
use tn_shell::{Integration, ShellParser};

use crate::block_view;

/// Convert a tn-core RGB color to a GPUI color.
fn col(c: Rgb) -> Rgba {
    gpui::rgb(((c.r as u32) << 16) | ((c.g as u32) << 8) | c.b as u32)
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
    // Latest OSC window title (OSC 0/2), captured off the reader thread.
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
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>, config: Arc<Loaded>) -> Self {
        let size = GridSize::new(ROWS, COLS);
        // Inject the pwsh shell-integration script (OSC 133 FTCS) via
        // -EncodedCommand so command blocks light up — no temp file, no echoed
        // input. Bypassable with TN_NO_SHELL_INTEGRATION for safety.
        let spec = if std::env::var("TN_NO_SHELL_INTEGRATION").is_ok() {
            SpawnSpec::program("powershell.exe").arg("-NoLogo")
        } else {
            SpawnSpec::program("powershell.exe")
                .arg("-NoLogo")
                .arg("-NoExit")
                .arg("-EncodedCommand")
                .arg(Integration::new().encoded_command())
        };
        let mut pty = LocalPty::spawn(&spec, PtySize::new(size.rows as u16, size.cols as u16))
            .expect("failed to spawn shell");
        let reader = pty.take_reader().expect("pty reader");
        let writer: SharedWriter = Arc::new(Mutex::new(pty.writer().expect("pty writer")));

        // Build the engine with the configured scrollback + theme palette.
        let palette = palette_from(&config.theme);
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
            pty: Arc::new(Mutex::new(pty)),
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

    /// The focus handle for this pane, so the workspace can route focus.
    pub fn focus_handle(&self) -> FocusHandle {
        self.focus_handle.clone()
    }

    /// The latest OSC window title for this session, if the program set one.
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
            let btn = |label: &'static str, color: Rgba| {
                div()
                    .px_2()
                    .py_1()
                    .rounded_md()
                    .bg(rgba(0xffffff10))
                    .text_color(color)
                    .child(label)
            };
            bar = bar
                .child(btn("复制", pal.dim).on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _e: &MouseDownEvent, _w, cx| {
                        this.copy_command(&copy_cmd, cx)
                    }),
                ))
                .child(btn("重跑", pal.blue).on_mouse_down(
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
            );

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .overflow_hidden()
            .bg(col(snapshot.bg))
            .text_color(col(snapshot.fg))
            .font_family(self.font_family.clone())
            .text_size(px(self.font_size))
            .line_height(px(self.line_height))
            .child(term_area)
            .when_some(block_bar, |this, bar| this.child(bar))
    }
}

// Key → byte encoding now lives in `crate::input` (see `input.rs`).
