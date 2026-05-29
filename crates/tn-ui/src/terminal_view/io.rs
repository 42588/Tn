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

use super::{
    ProcessExited, SharedWriter, TerminalView, UsageUpdated, BELL_FLASH_MS, CHAR_FADE_MS,
    CURSOR_BLINK_MS, CURSOR_GLIDE_MS, RAIL_WATCH_DEBOUNCE_MS,
};

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
        bell: Arc<AtomicBool>,
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
                                        // BEL (\x07): flag it; the foreground turns
                                        // this into a brief flash / optional beep on
                                        // the next wake (待优化清单 §3.8).
                                        TermEvent::Bell => bell.store(true, Ordering::Relaxed),
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
                        // Typed `claude`/`codex` at a plain-shell prompt → flip to
                        // agent state (and back when it finishes) via shell integration.
                        view.sync_shell_agent(cx);
                        // BEL on this batch → start the flash / beep (待优化清单 §3.8).
                        view.handle_bell_if_rung(cx);
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

    /// React to a bell flagged by the reader since the last repaint (待优化清单
    /// §3.8): optionally beep, and (if `visual_bell`) start a brief flash. Called
    /// from the foreground repaint, so it's safe to touch view state + spawn.
    pub(super) fn handle_bell_if_rung(&mut self, cx: &mut Context<Self>) {
        if !self.bell.swap(false, Ordering::Relaxed) {
            return;
        }
        if self.audio_bell {
            crate::platform::system_beep();
        }
        if self.visual_bell {
            self.bell_flash_at = Some(Instant::now());
            self.spawn_bell_fade(cx);
        }
    }

    /// Drive the visual-bell fade: notify every frame until `BELL_FLASH_MS` after
    /// the last bell, then clear the flash. `bell_fading` ensures only one such
    /// task runs — repeated bells just push `bell_flash_at` forward, extending
    /// (not stacking) the fade.
    fn spawn_bell_fade(&mut self, cx: &mut Context<Self>) {
        if self.bell_fading {
            return;
        }
        self.bell_fading = true;
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                exec.timer(Duration::from_millis(16)).await;
                let again = this.update(cx, |v, cx| {
                    let done = v
                        .bell_flash_at
                        .map(|t| t.elapsed() >= Duration::from_millis(BELL_FLASH_MS))
                        .unwrap_or(true);
                    if done {
                        v.bell_flash_at = None;
                        v.bell_fading = false;
                    }
                    cx.notify();
                    !done
                });
                if !matches!(again, Ok(true)) {
                    break; // done, or view dropped
                }
            }
        })
        .detach();
    }

    /// Drive the smooth cursor glide (待优化清单 §3.1): notify every frame until the
    /// ease window elapses, then stop. `cursor_gliding` ensures a single task — a new
    /// move mid-glide just refreshes `cursor_glide_start` (extends, not stacks). Mirror
    /// of `spawn_bell_fade`; render reads the elapsed time to interpolate the position.
    pub(super) fn spawn_cursor_glide(&mut self, cx: &mut Context<Self>) {
        if self.cursor_gliding {
            return;
        }
        self.cursor_gliding = true;
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                exec.timer(Duration::from_millis(16)).await;
                let again = this.update(cx, |v, cx| {
                    let active = v
                        .cursor_glide_start
                        .map(|t| t.elapsed() < Duration::from_millis(CURSOR_GLIDE_MS))
                        .unwrap_or(false);
                    if !active {
                        v.cursor_gliding = false;
                    }
                    cx.notify();
                    active
                });
                if !matches!(again, Ok(true)) {
                    break; // done, or view dropped
                }
            }
        })
        .detach();
    }

    /// Drive the character fade (待优化清单 §3.1): notify every frame, pruning expired
    /// fades, until none remain. `cell_fading` guards against spawning more than one.
    pub(super) fn spawn_cell_fade(&mut self, cx: &mut Context<Self>) {
        if self.cell_fading {
            return;
        }
        self.cell_fading = true;
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            loop {
                exec.timer(Duration::from_millis(16)).await;
                let again = this.update(cx, |v, cx| {
                    v.cell_fades
                        .retain(|f| f.start.elapsed() < Duration::from_millis(CHAR_FADE_MS));
                    let active = !v.cell_fades.is_empty();
                    if !active {
                        v.cell_fading = false;
                    }
                    cx.notify();
                    active
                });
                if !matches!(again, Ok(true)) {
                    break; // done, or view dropped
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
                    // the usage snapshot (never relabels the pane). The activity-rail
                    // git data is refreshed separately by the change watcher
                    // (`spawn_change_watcher` → `refresh_changes`, 变化即刷新).
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

    /// Watch the agent pane's working tree and refresh the activity-rail `git diff`
    /// on each (debounced) change — 「变化即刷新」. Returns the live watcher (store it;
    /// **dropping it stops watching**). Also does the initial populate. Noise dirs
    /// (`.git` churns on every git op incl. our own diff; build/dep dirs are huge +
    /// irrelevant) are filtered so a `cargo build` / git op doesn't spam refreshes.
    /// Bounded git runs off-thread in `refresh_changes`. `None` if unwatchable.
    pub(super) fn spawn_change_watcher(
        cx: &mut Context<Self>,
        cwd: String,
    ) -> Option<notify::RecommendedWatcher> {
        use notify::Watcher;
        let root = PathBuf::from(&cwd);
        if !root.is_dir() {
            return None;
        }
        // notify callback (own thread) → unbounded channel → debounce task.
        let (tx, mut rx) = mpsc::unbounded::<()>();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    if ev.paths.iter().any(|p| is_noise_path(p)) {
                        return;
                    }
                    let _ = tx.unbounded_send(());
                }
            })
            .ok()?;
        if watcher.watch(&root, notify::RecursiveMode::Recursive).is_err() {
            return None;
        }
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // Initial populate: pre-existing changes when the agent starts.
            if this.update(cx, |v, cx| v.refresh_changes(cx)).is_err() {
                return;
            }
            while rx.next().await.is_some() {
                // Debounce: a save / build touches many files — coalesce to one diff.
                exec.timer(Duration::from_millis(RAIL_WATCH_DEBOUNCE_MS)).await;
                while rx.try_recv().is_ok() {} // drain the burst
                // Stop once the pane is no longer an agent (rail gone) / view dropped.
                let go_on = this
                    .update(cx, |v, cx| {
                        if v.agent().is_none() {
                            return false;
                        }
                        v.refresh_changes(cx);
                        true
                    })
                    .unwrap_or(false);
                if !go_on {
                    break;
                }
            }
        })
        .detach();
        Some(watcher)
    }
}

/// Working-tree change-watcher noise filter: paths under these dirs don't affect
/// `git diff HEAD` (or churn constantly — `.git` ticks on every git op, including
/// our own diff), so a change there must not trigger a rail refresh.
fn is_noise_path(p: &std::path::Path) -> bool {
    p.components().any(|c| {
        matches!(
            c.as_os_str().to_str(),
            Some(".git" | "target" | "node_modules" | ".cargo" | "dist" | ".next")
        )
    })
}
