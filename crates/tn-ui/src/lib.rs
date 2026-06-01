//! Tn GPUI front-end.
//!
//! Opens the main window (DirectX 11 + DirectWrite on Windows) hosting a
//! [`Workspace`] — tabs, each an n-ary tree of [`TerminalView`] panes running
//! local shells. Set `TN_AUTOQUIT=1` for the headless self-test (the first pane
//! drives a command, dumps the grid, then quits).

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
mod style;
mod terminal_view;
mod welcome;
mod usage_display;
mod workspace;

use std::sync::{Arc, Mutex};
use std::sync::atomic::Ordering;

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
    // ── Single-instance check (BEFORE GPUI) ────────────────────────────
    match platform::try_acquire_single_instance() {
        platform::InstanceCheck::AlreadyRunning => {
            platform::signal_existing_instance_to_show();
            return; // second instance exits — the first one will show its window
        }
        platform::InstanceCheck::FirstInstance => { /* continue */ }
    }

    // ── Tray listener (BEFORE GPUI, so the second instance can find it) ─
    let tray = platform::spawn_tray_listener(); // Option<(isize, UnboundedReceiver<TrayEvent>)>

    // ── Load config + start GPUI ───────────────────────────────────────
    let config = Arc::new(tn_config::load());

    let window_background = match config.theme.ui.window.backdrop {
        tn_config::Backdrop::Acrylic => WindowBackgroundAppearance::Blurred,
        _ => WindowBackgroundAppearance::Opaque,
    };

    Application::new().with_assets(assets::Assets).run(move |cx: &mut App| {
        workspace::bind_keys(cx, &config);

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

        // ── Wire up tray (if available) ───────────────────────────────
        let tray_hwnd_opt = if let Some((tray_hwnd, tray_rx)) = tray {
            cx.set_global(TrayHwnd(tray_hwnd));
            spawn_tray_events_handler(cx, tray_rx, config.clone(), tray_hwnd);
            Some(tray_hwnd)
        } else {
            tracing::warn!("tray listener unavailable; Quick Terminal will not survive main-window close");
            None
        };

        // ── Quick Terminal (always-on hidden PopUp) ────────────────────
        spawn_quick_terminal(cx, config.clone());

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
fn spawn_quick_terminal(cx: &mut App, config: Arc<tn_config::Loaded>) {
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

    let bounds = Bounds::centered(None, size(px(1000.), px(420.)), cx);
    let win_cfg = config.clone();
    let window = match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { appears_transparent: true, ..Default::default() }),
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

    // Listen for the global hotkey on a dedicated thread.
    let Some(mut rx) = platform::spawn_hotkey_listener(&spec) else {
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        while rx.next().await.is_some() {
            let _ = window.update(cx, |qt, window, cx| qt.toggle(window, cx));
        }
    })
    .detach();
}
