//! A pane's off-thread workers (待优化清单 §6.2), split out of `mod.rs` to keep
//! the view's render core readable: the PTY reader thread, the foreground
//! repaint pump, the cursor-blink loop, the child-exit watcher, the headless
//! self-test, and the AI-usage poller.
//!
//! These are `impl super::TerminalView` methods so they can reach the view's
//! private state via `WeakEntity::update` (a child module sees the parent's
//! private fields); they're `pub(super)` so the parent's `new()` can spawn them.

use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use futures::channel::mpsc;
use futures::StreamExt;
use gpui::{AsyncApp, Context, WeakEntity};
use tn_ai::AgentKind;
use tn_blocks::BlockModel;
use tn_core::{TermEvent, Terminal};
use tn_pty::PtyBackend;
use tn_shell::ShellParser;

use super::{ProcessExited, SharedWriter, TerminalView, UsageUpdated, CURSOR_BLINK_MS};

impl TerminalView {
    /// Reader thread: PTY bytes -> engine; route engine `PtyWrite` replies back;
    /// capture title changes; push a (coalesced) wake to the foreground.
    pub(super) fn spawn_reader(
        mut reader: Box<dyn Read + Send>,
        terminal: Arc<Mutex<Terminal>>,
        writer: SharedWriter,
        dirty: Arc<AtomicBool>,
        wake_tx: mpsc::UnboundedSender<()>,
        title: Arc<Mutex<Option<String>>>,
        blocks: Arc<Mutex<BlockModel>>,
        agent_exited: Arc<AtomicBool>,
    ) {
        thread::spawn(move || {
            // Shell-integration bypass parser + a session clock. The parser is
            // stateful (a sequence can split across reads), so it lives here.
            let mut shell = ShellParser::new();
            let start = Instant::now();
            // 64 KiB: high-throughput output (cat big files, build logs) drains in
            // far fewer read calls than the old 8 KiB, cutting lock churn on the
            // shared Terminal (待优化清单 §2.3). Heap-boxed to keep the thread stack
            // small.
            let mut buf = vec![0u8; 65536];
            // Outer guard (待优化清单 §8.1): a panic anywhere in the reader loop is
            // logged with context instead of the thread dying silently (which
            // would leave the pane frozen with no clue why).
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        // The bypass parser is independent of the terminal lock;
                        // run it first so we know whether this batch produced any
                        // block events (and thus whether the anchor line is needed).
                        let events = shell.advance(&buf[..n]);
                        // Inner guard: catch an alacritty panic *while still holding
                        // the lock* so the stack unwinds only to here and the guard
                        // drops normally — the Mutex is never poisoned, so the
                        // foreground (GPUI callbacks, non-unwinding) can't be taken
                        // down by a later `.lock().unwrap()`. On panic we stop the
                        // reader (the grid is half-mutated) but the app lives on.
                        let processed = {
                            let mut t = terminal.lock().unwrap();
                            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                                t.advance(&buf[..n]);
                                let mut replies = Vec::new();
                                for e in t.drain_events() {
                                    match e {
                                        TermEvent::PtyWrite(s) => replies.push(s),
                                        // A hosted agent emits this sentinel title
                                        // once it exits → flag it instead of
                                        // showing it as the window title.
                                        TermEvent::Title(s)
                                            if s == super::AGENT_EXIT_SENTINEL =>
                                        {
                                            agent_exited.store(true, Ordering::Relaxed);
                                        }
                                        TermEvent::Title(s) => *title.lock().unwrap() = Some(s),
                                        TermEvent::ResetTitle => *title.lock().unwrap() = None,
                                        _ => {}
                                    }
                                }
                                // The cursor anchor is only used when this batch
                                // produced block events — the common case is none,
                                // so skip the extra grid borrow (待优化清单 §2.4).
                                let abs_line =
                                    if events.is_empty() { 0 } else { t.cursor_abs_line() };
                                (replies, abs_line)
                            }))
                            // `t` drops here, normally, even on the Err path.
                        };
                        let (replies, abs_line) = match processed {
                            Ok(v) => v,
                            Err(_) => {
                                tracing::error!(
                                    "terminal reader: alacritty panicked on output; \
                                     this pane is frozen (app + other panes unaffected)"
                                );
                                break;
                            }
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
                        if !dirty.swap(true, Ordering::Relaxed)
                            && wake_tx.unbounded_send(()).is_err()
                        {
                            break; // view dropped
                        }
                    }
                    Err(_) => break,
                }
            }));
            if outcome.is_err() {
                tracing::error!("terminal reader thread panicked; this pane stopped updating");
            }
        });
    }

    /// Foreground task: await reader wakes and repaint. GPUI coalesces the
    /// `notify()` calls onto its vsync frame clock; we render the final state.
    pub(super) fn spawn_repaint_loop(
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
                let alive = this
                    .update(cx, |view, cx| {
                        // A hosted agent that just exited reverts the pane to a
                        // plain shell; emit so the workspace relabels the tab.
                        if view.clear_agent_if_exited() {
                            cx.emit(UsageUpdated);
                        }
                        cx.notify();
                    })
                    .is_ok();
                if !alive {
                    break; // view dropped
                }
            }
        })
        .detach();
    }

    /// Blink the cursor (~530ms) while the pane is focused. Toggling + notifying
    /// only when `focused` keeps an unfocused pane at zero wakes (preserving the
    /// idle-cost-nothing design); an unfocused pane shows a steady hollow cursor.
    pub(super) fn spawn_blink_loop(cx: &mut Context<Self>) {
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                exec.timer(Duration::from_millis(CURSOR_BLINK_MS)).await;
                let alive = this
                    .update(cx, |v, cx| {
                        if v.focused {
                            v.cursor_on = !v.cursor_on;
                            cx.notify();
                        } else if !v.cursor_on {
                            v.cursor_on = true; // restore steady cursor on blur
                            cx.notify();
                        }
                    })
                    .is_ok();
                if !alive {
                    break; // view dropped
                }
            }
        })
        .detach();
    }

    /// Poll the PTY child; emit [`ProcessExited`] once, when it exits. ConPTY
    /// doesn't reliably EOF the reader (see CLAUDE.md), so `try_wait` is the
    /// authoritative signal. Cheap (a brief lock every 400ms).
    pub(super) fn spawn_exit_watcher(cx: &mut Context<Self>, pty: Arc<Mutex<Box<dyn PtyBackend>>>) {
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
    pub(super) fn spawn_self_test(
        cx: &mut Context<Self>,
        terminal: Arc<Mutex<Terminal>>,
        writer: SharedWriter,
    ) {
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
    pub(super) fn spawn_usage_poller(cx: &mut Context<Self>, cwd: String, hint: Option<AgentKind>) {
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let mut last: Option<(PathBuf, SystemTime)> = None;
            loop {
                // Stop once the agent identity is gone (it exited → pane is now a
                // plain shell) or the view dropped — no point polling a dead agent.
                if this.update(cx, |v, _| v.agent().is_none()).unwrap_or(true) {
                    break;
                }
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
}
