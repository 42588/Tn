//! Tn GPUI front-end.
//!
//! Opens the main window (DirectX 11 + DirectWrite on Windows) hosting a live
//! [`TerminalView`] running a local shell. Set `TN_AUTOQUIT=1` to run the
//! built-in headless self-test (drives a command, dumps the grid, then quits).

mod terminal_view;

use gpui::{
    px, size, App, AppContext, Application, Bounds, TitlebarOptions, WindowBounds, WindowOptions,
};

use terminal_view::TerminalView;

/// Open the main window and run the GPUI event loop (blocks until quit).
pub fn run() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(960.), px(640.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("Tn".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
            |_window, cx| cx.new(|cx| TerminalView::new(cx)),
        )
        .expect("failed to open window");
        cx.activate(true);
    });
}
