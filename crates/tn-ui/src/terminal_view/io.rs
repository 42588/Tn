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
use futures::future::{select, Either};
use futures::StreamExt;
use gpui::{AsyncApp, Context, WeakEntity};
use tn_ai::AgentKind;
use tn_blocks::BlockModel;
use tn_core::{TermEvent, Terminal};
use tn_pty::PtyBackend;
use tn_shell::ShellParser;

use super::{
    CwdChanged, ProcessExited, SharedWriter, TerminalView, UsageUpdated, BELL_FLASH_MS,
    CURSOR_BLINK_MS, CURSOR_GLIDE_MS, RAIL_WATCH_DEBOUNCE_MS,
};

impl TerminalView {
    /// Reader thread: PTY bytes -> engine; route engine `PtyWrite` replies back;
    /// capture title changes; push a (coalesced) wake to the foreground.
    pub(super) fn spawn_reader(
        mut reader: Box<dyn Read + Send>,
        terminal: Arc<Mutex<Terminal>>,
        writer_tx: std::sync::mpsc::Sender<Vec<u8>>,
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
            // 16 KiB: balances throughput with lock-hold latency. Larger buffers
            // would hold the terminal lock longer during `advance()` and block the
            // UI thread on keystrokes (input stutter). Heap-boxed to keep the
            // thread stack small.
            let mut buf = vec![0u8; 16384];
            let mut replies = Vec::new();
            // Outer guard (待优化清单 §8.1): a panic anywhere in the reader loop is
            // logged with context instead of the thread dying silently (which
            // would leave the pane frozen with no clue why).
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        // Mark PTY activity for the idle-aware mimalloc GC (tn-app): any
                        // output ⇒ not idle ⇒ don't run a forced collect now (优化①).
                        crate::note_pty_activity();
                        // The bypass parser is independent of the terminal lock;
                        // run it first so we know whether this batch produced any
                        let events = shell.advance(&buf[..n]);
                        if replies.capacity() > 1024 {
                            replies.shrink_to_fit();
                        }
                        replies.clear();
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
                                for e in t.drain_events() {
                                    match e {
                                        TermEvent::PtyWrite(s) => replies.push(s),
                                        // A hosted agent emits this sentinel title
                                        // once it exits → flag it instead of
                                        // showing it as the window title.
                                        TermEvent::Title(s) if s == super::AGENT_EXIT_SENTINEL => {
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
                                if events.is_empty() {
                                    0
                                } else {
                                    t.cursor_abs_line()
                                }
                            }))
                            // `t` drops here, normally, even on the Err path.
                        };
                        let abs_line = match processed {
                            Ok(line) => line,
                            Err(_) => {
                                tracing::error!(
                                    "terminal reader: alacritty panicked on output; \
                                     this pane is frozen (app + other panes unaffected)"
                                );
                                break;
                            }
                        };
                        if !replies.is_empty() {
                            let mut combined = Vec::new();
                            for r in &replies {
                                combined.extend_from_slice(r.as_bytes());
                            }
                            let _ = writer_tx.send(combined);
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
                        // (待优化清单 §8.1 / §2.4) Terminal-lock 争用缓解。reader 持
                        // `terminal` 锁跑 `advance()`,前台 render/on_key 要同一把锁;
                        // Claude Code(Ink)每秒数十次整屏重绘 → reader 连续抢锁会饿死
                        // 前台(输入卡顿)。刚释放锁、也唤醒了前台,这里主动让出一次调度:
                        //  • yield_now:近零成本(无其他就绪线程时立即返回),给正在等锁的
                        //    UI 线程一个被调度+抢锁的窗口 —— 覆盖 Claude 这类高频小批量;
                        //  • 缓冲过半(≥8KiB)= 大吞吐(cat 大文件),额外硬让 1ms,确保前台
                        //    能插进去刷帧、不被刷屏饿死。
                        std::thread::yield_now();
                        if n >= buf.len() / 2 {
                            std::thread::sleep(Duration::from_millis(1));
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
                        let mut events = Vec::new();
                        if let Ok(mut pty) = view.pty.lock() {
                            while let Some(ev) = pty.try_recv_event() {
                                events.push(ev);
                            }
                        }
                        for ev in events {
                            view.handle_pty_event(ev, cx);
                        }
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

                        // Check if the current working directory has changed and emit CwdChanged
                        let current_cwd = view.cwd();
                        if current_cwd != view.last_cwd {
                            view.last_cwd = current_cwd;
                            cx.emit(CwdChanged);
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
                        .cursor_anim_start
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

    /// Poll this pane's agent usage off the main thread. Binds to the session
    /// **this pane activated** — created fresh OR **resumed** from an old file —
    /// by watching for one that goes stale→fresh after `launched_at`, ignoring a
    /// session already active at launch (a concurrent dev Claude editing this very
    /// repo); see [`tn_ai::resolve_session_for_pane`]. Once found it's **pinned**,
    /// so a later session can't hijack the readout. Re-parses only when the pinned
    /// file's mtime changes (idle agent = one cheap `stat`, idle-zero-wakeup).
    /// Until the agent writes nothing is shown — honest, not a guess. Emits
    /// [`UsageUpdated`] on change so the workspace status bar repaints.
    pub(super) fn spawn_usage_poller(
        cx: &mut Context<Self>,
        kind: AgentKind,
        launched_at: SystemTime,
    ) {
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // Baseline mtimes at launch: any session already fresh now is a
            // concurrent (someone else's) one; ours flips stale→fresh later.
            let baseline = std::sync::Arc::new(
                exec.spawn(async move {
                    let (tx, rx) = futures::channel::oneshot::channel();
                    std::thread::spawn(move || {
                        let _ = tx.send(tn_ai::session_mtimes(kind));
                    });
                    rx.await.unwrap_or_default()
                })
                .await,
            );
            let mut pinned: Option<PathBuf> = None; // this pane's session, once found
            let mut last_mtime: Option<SystemTime> = None;
            let mut file_offset = 0u64;
            let mut current_usage: Option<tn_ai::AiUsage> = None;
            loop {
                // Stop once the agent identity is gone (it exited → pane is now a
                // plain shell) or the view dropped — no point polling a dead agent.
                if this.update(cx, |v, _| v.agent().is_none()).unwrap_or(true) {
                    break;
                }
                let pinned2 = pinned.clone();
                let prev = last_mtime;
                let baseline2 = baseline.clone();
                let prev_offset = file_offset;
                let prev_usage_clone = current_usage.clone();
                let res = exec
                    .spawn(async move {
                        let (tx, rx) = futures::channel::oneshot::channel();
                        std::thread::spawn(move || {
                            let result = (|| {
                                // Lock onto this pane's session once; afterward just follow
                                // that exact file (a later session can't steal it).
                                let path = match pinned2 {
                                    Some(p) => p,
                                    None => {
                                        tn_ai::resolve_session_for_pane(
                                            kind,
                                            launched_at,
                                            &baseline2,
                                        )?
                                        .path
                                    }
                                };
                                let mtime = std::fs::metadata(&path).ok()?.modified().ok()?;
                                if prev == Some(mtime) {
                                    return Some((path, mtime, None, prev_offset));
                                    // pinned, unchanged
                                }
                                let mut f = std::fs::File::open(&path).ok()?;
                                let len = f.metadata().ok()?.len();
                                use std::io::{Read, Seek, SeekFrom};
                                let (next_offset, usage) = if prev_offset > 0
                                    && prev_offset <= len
                                    && prev_usage_clone.is_some()
                                {
                                    f.seek(SeekFrom::Start(prev_offset)).ok()?;
                                    let mut delta = String::new();
                                    f.read_to_string(&mut delta).ok()?;
                                    let valid_bytes = delta.rfind('\n').map(|i| i + 1).unwrap_or(0);
                                    let valid_delta = &delta[..valid_bytes];
                                    let new_offset = prev_offset + valid_bytes as u64;
                                    let new_usage = if valid_bytes > 0 {
                                        tn_ai::update_session(
                                            kind,
                                            valid_delta,
                                            prev_usage_clone.unwrap(),
                                        )
                                    } else {
                                        prev_usage_clone.unwrap()
                                    };
                                    (new_offset, new_usage)
                                } else {
                                    let mut text = String::new();
                                    f.read_to_string(&mut text).ok()?;
                                    let valid_bytes = text.rfind('\n').map(|i| i + 1).unwrap_or(0);
                                    let valid_text = &text[..valid_bytes];
                                    let new_offset = valid_bytes as u64;
                                    let u = tn_ai::parse_session(kind, valid_text)?;
                                    (new_offset, u)
                                };
                                Some((path, mtime, Some(usage), next_offset))
                            })();
                            let _ = tx.send(result);
                        });
                        rx.await.unwrap_or(None)
                    })
                    .await;
                if let Some((path, mtime, usage_opt, next_offset)) = res {
                    pinned = Some(path); // bound from now on
                    last_mtime = Some(mtime);
                    file_offset = next_offset;
                    // `agent` is fixed from launch intent; the poller only updates
                    // the usage snapshot (never relabels the pane). The activity-rail
                    // git data is refreshed separately by the change watcher
                    // (`spawn_change_watcher` → `refresh_changes`, 变化即刷新).
                    if let Some(usage) = usage_opt {
                        current_usage = Some(usage.clone());
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
        root: PathBuf,
    ) -> Option<notify::RecommendedWatcher> {
        use notify::Watcher;
        if !root.is_dir() {
            return None;
        }
        // notify callback (own thread) → unbounded channel → debounce task.
        let (tx, mut rx) = mpsc::unbounded::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if let Ok(ev) = res {
                if ev.paths.iter().any(|p| crate::gitutil::is_noise_path(p)) {
                    return;
                }
                let _ = tx.unbounded_send(());
            }
        })
        .ok()?;
        if watcher
            .watch(&root, notify::RecursiveMode::Recursive)
            .is_err()
        {
            return None;
        }
        let exec = cx.background_executor().clone();
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // Initial populate: pre-existing changes when the agent starts.
            if this.update(cx, |v, cx| v.refresh_changes(cx)).is_err() {
                return;
            }
            while rx.next().await.is_some() {
                // Trailing-edge debounce(优化③):收到一个事件后持续吸收后续事件,直到静默
                // RAIL_WATCH_DEBOUNCE_MS 才刷一次。单次保存 ~该窗口后即刷(响应快);长构建
                // 产生的持续事件流被每个新事件不断推后 → 构建期间不刷、只在真正停下后刷一次
                // (旧的固定窗口会每窗口都刷)。配合 is_noise_path 已过滤 target/node_modules,
                // 源码区不会有"持续不断"的事件流,故无需 max-wait 上限。
                loop {
                    match select(
                        rx.next(),
                        std::pin::pin!(exec.timer(Duration::from_millis(RAIL_WATCH_DEBOUNCE_MS))),
                    )
                    .await
                    {
                        Either::Left((Some(_), _)) => continue, // 又来事件 → 重置静默窗口
                        Either::Left((None, _)) => return,      // 通道关闭(view dropped)
                        Either::Right(((), _)) => break,        // 静默期满 → 去刷新
                    }
                }
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

// is_noise_path 已移至 crate::gitutil(与 explorer 的树监听共用,审查⑨ 去重)。
