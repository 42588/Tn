//! Live terminal view: renders a `tn-core` [`Terminal`] driven by a `tn-pty`
//! ConPTY backend, with keyboard input routed back to the shell.
//!
//! Threading model (the M0 cut):
//!   - A dedicated reader thread pumps PTY bytes into the shared [`Terminal`]
//!     and writes the engine's `PtyWrite` replies (DSR responses, etc.) back to
//!     the PTY — without this ConPTY stalls on startup.
//!   - A GPUI foreground task watches a `dirty` flag and calls `notify()` so the
//!     view repaints when new output arrives. (This poll will become a push
//!     channel with the 4ms coalescing window once we build the real element.)

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use gpui::{
    div, prelude::*, px, rgb, AsyncApp, Context, FocusHandle, Keystroke, KeyDownEvent, SharedString,
    WeakEntity, Window,
};
use tn_core::{GridSize, TermEvent, Terminal};
use tn_pty::{LocalPty, PtyBackend, PtySize, SpawnSpec};

const FONT: &str = "Consolas";
const FONT_SIZE: f32 = 14.0;
const LINE_HEIGHT: f32 = 18.0;
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
    focused_once: bool,
}

impl TerminalView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let size = GridSize::new(ROWS, COLS);
        let spec = SpawnSpec::program("powershell.exe").arg("-NoLogo");
        let mut pty = LocalPty::spawn(&spec, PtySize::new(size.rows as u16, size.cols as u16))
            .expect("failed to spawn shell");
        let reader = pty.take_reader().expect("pty reader");
        let writer: SharedWriter = Arc::new(Mutex::new(pty.writer().expect("pty writer")));

        let terminal = Arc::new(Mutex::new(Terminal::new(size)));
        let dirty = Arc::new(AtomicBool::new(true));

        Self::spawn_reader(reader, terminal.clone(), writer.clone(), dirty.clone());
        Self::spawn_repaint_loop(cx, dirty.clone());

        if std::env::var("TN_AUTOQUIT").is_ok() {
            Self::spawn_self_test(cx, terminal.clone(), writer.clone());
        }

        // Measure the monospace cell width once so we can fit the grid to the
        // window. Falls back to a ratio estimate if the glyph can't be measured.
        let font_id = cx.text_system().resolve_font(&gpui::font(FONT));
        let cell_width = cx
            .text_system()
            .advance(font_id, px(FONT_SIZE), 'm')
            .map(|s| f32::from(s.width))
            .unwrap_or(FONT_SIZE * 0.6);

        Self {
            terminal,
            writer,
            pty: Arc::new(Mutex::new(pty)),
            focus_handle: cx.focus_handle(),
            size,
            cell_width,
            focused_once: false,
        }
    }

    /// Reader thread: PTY bytes -> engine; route engine `PtyWrite` replies back.
    fn spawn_reader(
        mut reader: Box<dyn Read + Send>,
        terminal: Arc<Mutex<Terminal>>,
        writer: SharedWriter,
        dirty: Arc<AtomicBool>,
    ) {
        thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        let replies: Vec<String> = {
                            let mut t = terminal.lock().unwrap();
                            t.advance(&buf[..n]);
                            t.drain_events()
                                .into_iter()
                                .filter_map(|e| match e {
                                    TermEvent::PtyWrite(s) => Some(s),
                                    _ => None,
                                })
                                .collect()
                        };
                        if !replies.is_empty() {
                            let mut w = writer.lock().unwrap();
                            for r in replies {
                                let _ = w.write_all(r.as_bytes());
                            }
                            let _ = w.flush();
                        }
                        dirty.store(true, Ordering::Relaxed);
                    }
                    Err(_) => break,
                }
            }
        });
    }

    /// Foreground task: repaint the view whenever the engine has new content.
    fn spawn_repaint_loop(cx: &mut Context<Self>, dirty: Arc<AtomicBool>) {
        let executor = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
            executor.timer(Duration::from_millis(8)).await;
            if dirty.swap(false, Ordering::Relaxed)
                && this.update(cx, |_view, cx| cx.notify()).is_err()
            {
                break;
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

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, _cx: &mut Context<Self>) {
        if let Some(bytes) = keystroke_to_bytes(&event.keystroke) {
            let mut w = self.writer.lock().unwrap();
            let _ = w.write_all(&bytes);
            let _ = w.flush();
        }
    }
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focused_once {
            self.focus_handle.focus(window);
            self.focused_once = true;
        }

        // Fit the grid to the current window size (padding = p_2 = 8px a side).
        const PAD: f32 = 16.0;
        let vp = window.viewport_size();
        let cols = (((f32::from(vp.width) - PAD) / self.cell_width).floor() as usize).max(1);
        let rows_n = (((f32::from(vp.height) - PAD) / LINE_HEIGHT).floor() as usize).max(1);
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

        let rows = self.terminal.lock().unwrap().snapshot().rows_text();

        div()
            .track_focus(&self.focus_handle)
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, window, cx| this.on_key(ev, window, cx)))
            .size_full()
            .bg(rgb(0x1e1e2e))
            .text_color(rgb(0xcdd6f4))
            .font_family(FONT)
            .text_size(px(FONT_SIZE))
            .line_height(px(LINE_HEIGHT))
            .p_2()
            .flex()
            .flex_col()
            .children(rows.into_iter().map(|line| {
                let content: SharedString = if line.is_empty() {
                    " ".into()
                } else {
                    line.into()
                };
                div().h(px(LINE_HEIGHT)).child(content)
            }))
    }
}

/// Map a GPUI keystroke to the bytes a terminal should send to the PTY.
/// A minimal keymap for M0; the full `to_esc_str` (app-cursor mode, etc.) lands
/// with the proper input layer.
fn keystroke_to_bytes(ks: &Keystroke) -> Option<Vec<u8>> {
    let m = &ks.modifiers;
    let key = ks.key.as_str();

    // Ctrl + key -> C0 control bytes (ctrl-c => 0x03, etc.).
    if m.control && !m.alt && !m.platform && key.chars().count() == 1 {
        let c = key.chars().next().unwrap();
        if c.is_ascii_alphabetic() {
            return Some(vec![(c.to_ascii_lowercase() as u8 - b'a') + 1]);
        }
        match c {
            ' ' => return Some(vec![0]),
            '[' => return Some(vec![0x1b]),
            '\\' => return Some(vec![0x1c]),
            ']' => return Some(vec![0x1d]),
            '^' => return Some(vec![0x1e]),
            '_' => return Some(vec![0x1f]),
            _ => {}
        }
    }

    let named: Option<&[u8]> = match key {
        "enter" => Some(b"\r"),
        "tab" => Some(b"\t"),
        "backspace" => Some(b"\x7f"),
        "escape" => Some(b"\x1b"),
        "space" => Some(b" "),
        "up" => Some(b"\x1b[A"),
        "down" => Some(b"\x1b[B"),
        "right" => Some(b"\x1b[C"),
        "left" => Some(b"\x1b[D"),
        "home" => Some(b"\x1b[H"),
        "end" => Some(b"\x1b[F"),
        "pageup" => Some(b"\x1b[5~"),
        "pagedown" => Some(b"\x1b[6~"),
        "delete" => Some(b"\x1b[3~"),
        "insert" => Some(b"\x1b[2~"),
        _ => None,
    };
    if let Some(bytes) = named {
        return Some(bytes.to_vec());
    }

    // Printable character (honors shift/layout via key_char).
    if !m.control && !m.platform {
        if let Some(kc) = &ks.key_char {
            if !kc.is_empty() {
                return Some(kc.clone().into_bytes());
            }
        }
        if key.chars().count() == 1 {
            return Some(key.as_bytes().to_vec());
        }
    }

    None
}
