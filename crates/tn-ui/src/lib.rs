//! Tn GPUI front-end.
//!
//! Opens the main window (DirectX 11 + DirectWrite on Windows) hosting a
//! [`Workspace`] — tabs, each an n-ary tree of [`TerminalView`] panes running
//! local shells. Set `TN_AUTOQUIT=1` for the headless self-test (the first pane
//! drives a command, dumps the grid, then quits).

mod agent_host;
mod assets;
mod block_view;
mod explorer;
mod gitutil;
mod input;
mod layout;
mod perf;
mod platform;
mod quick_look;
mod quick_terminal;
mod ssh_recents;
mod style;
mod terminal_view;
mod usage_display;
mod welcome;
mod workspace;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use futures::StreamExt;
use gpui::{
    px, size, App, AppContext, Application, AsyncApp, Bounds, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};

use quick_terminal::QuickTerminal;
use workspace::Workspace;

// ── Globals (set once in `run()`, read by workspace) ──────────────────────

/// Stored as a GPUI global so the Quit action handler can remove the tray icon
/// before calling `cx.quit()`.
pub(crate) struct TrayHwnd(pub(crate) isize);

impl gpui::Global for TrayHwnd {}

// ── PTY 活动信号(供 tn-app 的 mimalloc GC 判空闲)──────────────────────────
// 进程级单调计数:任一 pane 的 reader 收到 PTY 输出就 +1。tn-app 的内存回收线程
// (mi_collect)据此判空闲 —— 繁忙(计数在变)时绝不 collect,只在持续无输出时归还内存
// 一次,避免 Claude 狂输出时被周期性强制 GC 微卡(审查优化① 发现的隐患)。纯 relaxed
// atomic add,对 reader 热路径几乎零成本(远轻于读时钟)。
static PTY_ACTIVITY_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Mark PTY activity — called by each pane's reader on output. Cheap: one relaxed add.
pub fn note_pty_activity() {
    PTY_ACTIVITY_SEQ.fetch_add(1, Ordering::Relaxed);
}

/// Current PTY-activity counter; tn-app's GC compares it across intervals to detect idle
/// (unchanged across a window ⇒ no PTY output happened, so it's safe to reclaim memory).
pub fn pty_activity_seq() -> u64 {
    PTY_ACTIVITY_SEQ.load(Ordering::Relaxed)
}

// ── App state (shared between `on_window_closed` and the tray event handler) ─

struct AppState {
    /// The main workspace window ID, if it is currently open.
    main_window_id: Option<gpui::WindowId>,
    /// The message-only tray window HWND (IPC target + icon host).
    tray_hwnd: Option<isize>,
    /// Whether the tray icon is currently visible.
    tray_icon_visible: bool,
}

// ── run() ──────────────────────────────────────────────────────────────────

/// Open the main window and run the GPUI event loop (blocks until quit).
pub fn run() {
    // ── Load config ────────────────────────────────────────────────────
    let config = Arc::new(tn_config::load());

    // ── Determine primary vs secondary instance ────────────────────────
    // "Primary" = we can register the global Quick Terminal hotkey. Only
    // the primary instance gets the QT window + tray icon + hide-to-tray
    // behaviour. Secondary instances are plain main-window-only processes
    // that quit when their window closes.
    let qt_cfg = &config.config.quick_terminal;
    let is_primary = qt_cfg.enabled
        && std::env::var("TN_AUTOQUIT").is_err()
        && tn_config::parse_hotkey(&qt_cfg.hotkey)
            .map(|spec| platform::probe_hotkey(&spec))
            .unwrap_or(false);

    // ── Tray listener (primary only, BEFORE GPUI) ──────────────────────
    let tray = if is_primary {
        platform::spawn_tray_listener()
    } else {
        None
    };

    let window_background = match config.theme.ui.window.backdrop {
        tn_config::Backdrop::Acrylic => WindowBackgroundAppearance::Blurred,
        _ => WindowBackgroundAppearance::Opaque,
    };

    Application::new()
        .with_assets(assets::Assets)
        .run(move |cx: &mut App| {
            workspace::bind_keys(cx, &config);

            // ── 嵌入 CaskaydiaCove Nerd Font ──────────────────────────────────
            // include_bytes! 在编译期将字体硬编码进二进制，运行时不再依赖系统安装。
            // GPUI 解析 .ttf 提取 Family Name → 与 config.toml 的 family 字段匹配。
            let font_regular =
                include_bytes!("../assets/fonts/CaskaydiaCoveNerdFont-Regular.ttf").to_vec();
            let font_bold =
                include_bytes!("../assets/fonts/CaskaydiaCoveNerdFont-Bold.ttf").to_vec();
            let font_italic =
                include_bytes!("../assets/fonts/CaskaydiaCoveNerdFont-Italic.ttf").to_vec();
            let font_bold_italic =
                include_bytes!("../assets/fonts/CaskaydiaCoveNerdFont-BoldItalic.ttf").to_vec();
            cx.text_system()
                .add_fonts(vec![
                    std::borrow::Cow::Owned(font_regular),
                    std::borrow::Cow::Owned(font_bold),
                    std::borrow::Cow::Owned(font_italic),
                    std::borrow::Cow::Owned(font_bold_italic),
                ])
                .expect("Failed to load embedded CaskaydiaCove Nerd Font");
            // ──────────────────────────────────────────────────────────────────

            // Install the app-wide agent registry before any pane is built — the
            // UI resolves all agent identity through it. Built-in Claude/Codex
            // first, then user `[[agents]]` manifests (config-level agents; a
            // manifest can't override a built-in id — see `register_manifest`).
            let mut registry = tn_ai::builtin_registry();
            for manifest in &config.config.agents {
                registry.register_manifest(manifest);
            }
            cx.set_global(agent_host::AgentHost(registry));

            let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
            let main_config = config.clone();
            let main_window = cx
                .open_window(
                    WindowOptions {
                        window_bounds: Some(WindowBounds::Windowed(bounds)),
                        titlebar: Some(TitlebarOptions {
                            title: Some("Tn".into()),
                            appears_transparent: true,
                            ..Default::default()
                        }),
                        window_background,
                        show: false,
                        ..Default::default()
                    },
                    move |_window, cx| cx.new(|cx| Workspace::new(cx, main_config.clone())),
                )
                .expect("failed to open window");
            let main_id = main_window.window_id();

            // ── Wire up tray (primary only) ──────────────────────────────
            let tray_hwnd_opt = if let Some((tray_hwnd, tray_rx)) = tray {
                cx.set_global(TrayHwnd(tray_hwnd));
                spawn_tray_events_handler(cx, tray_rx, config.clone(), tray_hwnd);
                Some(tray_hwnd)
            } else {
                None
            };

            // ── Quick Terminal (primary only — hotkey probe passed) ───────
            if is_primary {
                spawn_quick_terminal(cx, config.clone());
            }

            // ── Shared state for window-close handling ─────────────────────
            let state = Arc::new(Mutex::new(AppState {
                main_window_id: Some(main_id),
                tray_hwnd: tray_hwnd_opt,
                tray_icon_visible: false,
            }));

            // ── on_window_closed: hide-to-tray or quit ─────────────────────
            cx.on_window_closed(move |cx| {
                // Genuine quit in progress — let everything tear down.
                if platform::QUITTING.load(Ordering::Acquire) {
                    return;
                }
                // All windows gone (shouldn't normally happen while Quick Terminal
                // is alive, but guard against edge cases).
                if cx.windows().is_empty() {
                    cx.quit();
                    return;
                }
                let mut s = state.lock().unwrap();
                let main_gone = s
                    .main_window_id
                    .map(|id| !cx.windows().iter().any(|w| w.window_id() == id))
                    .unwrap_or(true);
                if main_gone {
                    s.main_window_id = None;
                    if let Some(h) = s.tray_hwnd {
                        if !s.tray_icon_visible {
                            s.tray_icon_visible = platform::create_tray_icon(h);
                        }
                        // Process stays alive — Quick Terminal's hidden PopUp
                        // window keeps the GPUI event loop running, and the
                        // global hotkey thread continues to listen.
                    } else {
                        // No tray = old behavior: quit when the main window closes.
                        cx.quit();
                    }
                }
            })
            .detach();

            cx.activate(true);
        });
}

// ── Tray event handler (GPUI side) ─────────────────────────────────────────

/// Receive tray icon selections and dispatch to the appropriate action.
fn spawn_tray_events_handler(
    cx: &mut App,
    mut tray_rx: futures::channel::mpsc::UnboundedReceiver<platform::TrayEvent>,
    config: Arc<tn_config::Loaded>,
    tray_hwnd: isize,
) {
    cx.spawn(async move |cx: &mut AsyncApp| {
        while let Some(event) = tray_rx.next().await {
            match event {
                platform::TrayEvent::Show | platform::TrayEvent::ShowFromIpc => {
                    // Re-create the main workspace window if it isn't already open.
                    let _ = recreate_main_window(cx, config.clone());
                }
                platform::TrayEvent::Quit => {
                    platform::QUITTING.store(true, Ordering::Release);
                    platform::remove_tray_icon(tray_hwnd);
                    let _ = cx.update(|cx| cx.quit());
                    break;
                }
            }
        }
    })
    .detach();
}

// ── Window recreation ──────────────────────────────────────────────────────

/// Open a fresh main workspace window (called when the user clicks "Show Tn"
/// from the tray icon context menu). Returns the new window's ID, or logs an
/// error if creation fails.
fn recreate_main_window(
    cx: &mut AsyncApp,
    config: Arc<tn_config::Loaded>,
) -> Option<gpui::WindowId> {
    let result = cx.update(|cx| {
        let window_background = match config.theme.ui.window.backdrop {
            tn_config::Backdrop::Acrylic => WindowBackgroundAppearance::Blurred,
            _ => WindowBackgroundAppearance::Opaque,
        };
        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        let cfg = config.clone();
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Tn".into()),
                    appears_transparent: true,
                    ..Default::default()
                }),
                window_background,
                show: false, // revealed on first paint by Workspace::render
                ..Default::default()
            },
            move |_window, cx| cx.new(|cx| Workspace::new(cx, cfg.clone())),
        )
    });

    match result {
        Ok(Ok(window)) => {
            let id = window.window_id();
            tracing::info!("recreated main workspace window (id={id:?})");
            Some(id)
        }
        Ok(Err(e)) => {
            tracing::error!("failed to create main window entity: {e}");
            None
        }
        Err(_) => {
            // cx.update() failed — the app is likely shutting down.
            None
        }
    }
}

// ── Quick Terminal ─────────────────────────────────────────────────────────

/// Open the hidden Quick Terminal window and wire its global hotkey toggle.
/// Only called for the **primary** instance (the one that passed the hotkey probe).
fn spawn_quick_terminal(cx: &mut App, config: Arc<tn_config::Loaded>) {
    // The `is_primary` check in `run()` already guards this, but keep the early
    // returns so the function is self-contained.
    if std::env::var("TN_AUTOQUIT").is_ok() {
        return;
    }
    let qt = &config.config.quick_terminal;
    if !qt.enabled {
        return;
    }
    let Some(spec) = tn_config::parse_hotkey(&qt.hotkey) else {
        return;
    };

    // Register the global hotkey *before* creating the window, so a failure
    // (another instance grabbed it between the probe and now) doesn't leave an
    // orphan QT window that can never be summoned.
    let Some(mut hotkey_rx) = platform::spawn_hotkey_listener(&spec) else {
        tracing::info!(
            "quick terminal hotkey lost between probe and permanent registration; skipping QT"
        );
        return;
    };

    let bounds = Bounds::centered(None, size(px(1000.), px(420.)), cx);
    let win_cfg = config.clone();
    let window = match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                appears_transparent: true,
                ..Default::default()
            }),
            kind: WindowKind::PopUp,
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            focus: false,
            show: false,
            window_background: WindowBackgroundAppearance::Transparent,
            ..Default::default()
        },
        move |_window, cx| cx.new(|cx| QuickTerminal::new(cx, win_cfg.clone())),
    ) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!("failed to open quick terminal window: {e}");
            return;
        }
    };

    cx.spawn(async move |cx: &mut AsyncApp| {
        while hotkey_rx.next().await.is_some() {
            let _ = window.update(cx, |qt, window, cx| qt.toggle(window, cx));
        }
    })
    .detach();
}
