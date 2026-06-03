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

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

// `mi_collect` 由静态链接进来的 mimalloc(我们的 #[global_allocator])提供。这里手写
// extern 声明而不引入 `libmimalloc-sys`,以免多钉一个需与 `mimalloc` 对齐版本的依赖;
// 符号在最终 binary 链接时解析(已验证可链)。
extern "C" {
    fn mi_collect(force: bool);
}

/// 空闲感知的内存回收。`mi_collect(true)` 会遍历整个堆、把空闲页归还 OS —— app 静下来时
/// 有用,但**运行中跑就是实打实的开销**。旧版每 30s **无条件**强制 collect,于是一段长的
/// Claude Code 输出里会撞上周期性 GC 卡顿(审查优化① 发现的隐患)。现在只在 app **持续空闲**
/// (一段时间无 PTY 输出)后 collect、且每个空闲段只 collect 一次 —— 繁忙的终端永不被打断,
/// 空闲的终端仍能把内存交还。空闲经 tn-ui 的 PTY 活动计数判定(廉价 relaxed atomic)。
fn spawn_mimalloc_gc() {
    std::thread::spawn(|| {
        const TICK: std::time::Duration = std::time::Duration::from_secs(5);
        const IDLE_TICKS_BEFORE_COLLECT: u32 = 2; // ~10s 无 PTY 输出才算稳定空闲
        let mut last_seq = tn_ui::pty_activity_seq();
        let mut idle_ticks = 0u32;
        let mut collected_this_idle = false;
        loop {
            std::thread::sleep(TICK);
            let seq = tn_ui::pty_activity_seq();
            if seq != last_seq {
                // 这一 tick 内有 PTY 输出 → 忙,绝不在此时 collect。
                last_seq = seq;
                idle_ticks = 0;
                collected_this_idle = false;
            } else if idle_ticks < IDLE_TICKS_BEFORE_COLLECT {
                idle_ticks += 1;
            } else if !collected_this_idle {
                // 持续空闲且本空闲段尚未回收过 → 归还一次。
                unsafe { mi_collect(true) };
                collected_this_idle = true;
            }
        }
    });
}

fn main() {
    // Keep the file-writer guard alive for the whole run (drops flush the log).
    let _guard = init_logging();
    install_panic_hook();

    // Give Tn its own taskbar identity (like Windows Terminal does) so it pins,
    // groups, and switches independently — not lumped with other GPUI/Zed apps.
    #[cfg(windows)]
    set_app_user_model_id();

    spawn_mimalloc_gc();

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
