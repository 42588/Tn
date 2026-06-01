//! Tn terminal application entry point.
//!
//! `windows_subsystem = "windows"` suppresses the console window in release
//! builds; debug builds keep it so logs are visible during development. Because
//! release builds have no console, logs are also written to a rolling file under
//! the config dir (`%APPDATA%\Tn\logs\tn.log`), and a panic hook records crashes
//! there before the process unwinds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

fn main() {
    // Keep the file-writer guard alive for the whole run (drops flush the log).
    let _guard = init_logging();
    install_panic_hook();

    // Give Tn its own taskbar identity (like Windows Terminal does) so it pins,
    // groups, and switches independently — not lumped with other GPUI/Zed apps.
    #[cfg(windows)]
    set_app_user_model_id();

    tn_ui::run();
}

/// Tell Windows this process is "Tn.Terminal" so the taskbar treats it as a
/// distinct application.  The matching shortcut is created by the install script.
#[cfg(windows)]
fn set_app_user_model_id() {
    use windows::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID;
    let id = windows::core::w!("Tn.Terminal");
    unsafe { SetCurrentProcessExplicitAppUserModelID(id).ok() };
}

/// Initialize logging: a stderr layer (visible in debug) plus a best-effort file
/// layer under `<config dir>/logs/tn.log`. Returns the non-blocking writer guard,
/// which must be held until exit.
fn init_logging() -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let env = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let stderr_layer = fmt::layer().with_writer(std::io::stderr);

    let (file_layer, guard) = match tn_config::config_dir().map(|d| d.join("logs")) {
        Some(dir) if std::fs::create_dir_all(&dir).is_ok() => {
            let appender = tracing_appender::rolling::never(&dir, "tn.log");
            let (nb, guard) = tracing_appender::non_blocking(appender);
            (Some(fmt::layer().with_ansi(false).with_writer(nb)), Some(guard))
        }
        _ => (None, None),
    };

    tracing_subscriber::registry()
        .with(env)
        .with(stderr_layer)
        .with(file_layer)
        .init();
    guard
}

/// Log panics (with location + backtrace-ish message) before the default hook
/// runs, so crashes are captured in the log file even with no console.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "<unknown>".to_string());
        let msg = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| s.to_string())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "<non-string panic payload>".to_string());
        tracing::error!(location = %location, "panic: {msg}");
        default(info);
    }));
}
