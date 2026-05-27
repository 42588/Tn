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
mod terminal_view;
mod viewer;
mod workspace;

use std::sync::Arc;

use gpui::{
    px, size, App, AppContext, Application, Bounds, TitlebarOptions, WindowBackgroundAppearance,
    WindowBounds, WindowOptions,
};

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

    // Calm Glass: a Mica/Acrylic theme asks the OS to blur the desktop behind
    // the window (Windows acrylic), so the translucent chrome reads as frosted
    // glass over a real material. A `solid` theme stays opaque.
    let window_background = match config.theme.ui.window.backdrop {
        tn_config::Backdrop::Solid => WindowBackgroundAppearance::Opaque,
        _ => WindowBackgroundAppearance::Blurred,
    };

    Application::new().with_assets(assets::Assets).run(move |cx: &mut App| {
        workspace::bind_keys(cx, &config);

        // Quit when the last window is closed (gpui doesn't do this by default),
        // so closing the window exits cleanly instead of leaving the process up.
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        let bounds = Bounds::centered(None, size(px(1100.), px(720.)), cx);
        cx.open_window(
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
                ..Default::default()
            },
            move |_window, cx| cx.new(|cx| Workspace::new(cx, config.clone())),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
