//! Tn GPUI front-end.
//!
//! Opens the main window (DirectX 11 + DirectWrite on Windows) hosting a
//! [`Workspace`] — tabs, each an n-ary tree of [`TerminalView`] panes running
//! local shells. Set `TN_AUTOQUIT=1` for the headless self-test (the first pane
//! drives a command, dumps the grid, then quits).

mod assets;
mod block_view;
mod explorer;
mod input;
mod perf;
mod platform;
mod quick_look;
mod quick_terminal;
mod style;
mod terminal_view;
mod welcome;
mod workspace;

use std::sync::Arc;

use futures::StreamExt;
use gpui::{
    px, size, App, AppContext, Application, AsyncApp, Bounds, TitlebarOptions,
    WindowBackgroundAppearance, WindowBounds, WindowKind, WindowOptions,
};

use quick_terminal::QuickTerminal;
use workspace::Workspace;

/// Open the main window and run the GPUI event loop (blocks until quit).
pub fn run() {
    // Load config + theme once (writes defaults on first run); shared by panes.
    let config = Arc::new(tn_config::load());
    tracing::info!(
        theme = %config.theme.name,
        font = %config.config.font.family,
        "loaded config"
    );

    // Window material. gpui 0.2.2 only exposes Opaque / Transparent / Blurred,
    // and `Blurred` on Windows = ACRYLIC (genuinely see-through blur) — NOT true
    // Mica (which is near-opaque). Acrylic lets a bright desktop bleed through
    // the edges, which reads as an unwanted transparent halo. So only an explicit
    // `acrylic` backdrop opts into see-through blur; `mica`/`solid` stay Opaque
    // (a solid dark window — the "glass" depth lives in the inner panels).
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
                        // Hide the OS caption — the workspace draws its own integrated
                        // titlebar (brand + tabs + window controls). Drag + min/max/
                        // close are wired via `window_control_area` regions.
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    window_background,
                    // Open hidden, then reveal after the first frame paints (the
                    // Workspace does this in its first `render`). Avoids the brief
                    // transparent/blank window flash before the DX swapchain
                    // presents its first frame.
                    show: false,
                    ..Default::default()
                },
                move |_window, cx| cx.new(|cx| Workspace::new(cx, main_config.clone())),
            )
            .expect("failed to open window");
        let main_id = main_window.window_id();

        // Quick Terminal (M5): a borderless, topmost drop-down window summoned by
        // a global hotkey. Opened hidden up front (its shell pre-spawns) so the
        // first summon is instant. Win32 details (topmost, slide, hotkey) live in
        // `platform.rs` — see CLAUDE.md M5.
        spawn_quick_terminal(cx, config.clone(), window_background);

        // Quit when the MAIN workspace window closes (gpui doesn't quit on its
        // own). We can't just check `windows().is_empty()`: the Quick Terminal is
        // an always-open (hidden) window, so it would keep the app alive
        // invisibly after the user closes the main window. Quit once the main
        // window is gone — that also tears down the quick window + its shell.
        cx.on_window_closed(move |cx| {
            let main_open = cx.windows().iter().any(|w| w.window_id() == main_id);
            if !main_open {
                cx.quit();
            }
        })
        .detach();

        cx.activate(true);
    });
}

/// Open the hidden Quick Terminal window and wire its global hotkey toggle.
/// No-op (with a log) when disabled or the hotkey is unparseable.
fn spawn_quick_terminal(cx: &mut App, config: Arc<tn_config::Loaded>, bg: WindowBackgroundAppearance) {
    // The headless self-test (TN_AUTOQUIT) drives the first pane and quits; a
    // second self-testing TerminalView would race it. Keep that mode focused.
    if std::env::var("TN_AUTOQUIT").is_ok() {
        return;
    }
    let qt = &config.config.quick_terminal;
    if !qt.enabled {
        return;
    }
    let Some(spec) = tn_config::parse_hotkey(&qt.hotkey) else {
        tracing::warn!(hotkey = %qt.hotkey, "invalid quick_terminal hotkey; not registered");
        return;
    };

    // Placeholder bounds; the window is repositioned (and resized) to the docking
    // edge before it is ever shown, so these never appear on screen.
    let bounds = Bounds::centered(None, size(px(1000.), px(420.)), cx);
    let win_cfg = config.clone();
    let window = match cx.open_window(
        WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { appears_transparent: true, ..Default::default() }),
            kind: WindowKind::PopUp, // borderless + off-taskbar (WS_EX_TOOLWINDOW)
            is_movable: false,
            is_resizable: false,
            is_minimizable: false,
            focus: false,
            show: false,
            window_background: bg,
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

    // Listen for the global hotkey on a dedicated thread; toggle on the main
    // thread (where the window lives) each time it fires.
    let Some(mut rx) = platform::spawn_hotkey_listener(&spec) else {
        tracing::warn!(hotkey = %qt.hotkey, "could not register quick_terminal hotkey");
        return;
    };
    cx.spawn(async move |cx: &mut AsyncApp| {
        while rx.next().await.is_some() {
            let _ = window.update(cx, |qt, window, cx| qt.toggle(window, cx));
        }
    })
    .detach();
}
