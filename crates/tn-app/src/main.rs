//! Tn terminal application entry point.
//!
//! `windows_subsystem = "windows"` suppresses the console window in release
//! builds; debug builds keep it so logs are visible during development.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    tracing::info!("starting Tn");
    tn_ui::run();
}
