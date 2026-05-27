//! Headless debug harness for Tn.
//!
//! Spawns a real shell through the local ConPTY backend, feeds its output into
//! the `tn-core` terminal engine on a reader thread, and prints the resulting
//! grid. This validates the full ConPTY -> alacritty parse -> snapshot pipeline
//! without any GPUI.
//!
//! Two ConPTY realities this harness handles, which the real driver must too:
//!   1. The engine emits `PtyWrite` events (e.g. the reply to ConPTY's startup
//!      `ESC[6n` cursor-position query). These MUST be written back to the PTY,
//!      or ConPTY stalls and the child never makes progress.
//!   2. ConPTY does not reliably deliver EOF on exit, so we poll `try_wait`
//!      with a hard timeout rather than blocking on read EOF.
//!
//! Usage: `cargo smoke` (or `cargo run -p tn-cli`). Pass a program + args to
//! smoke-test an arbitrary child instead of the default, e.g. (WSL, M2):
//! `cargo run -p tn-cli -- wsl.exe -d Ubuntu -- echo HELLO_TN_MARKER`.

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tn_core::{GridSize, Terminal, TermEvent};
use tn_pty::{LocalPty, PtyBackend, PtySize, SpawnSpec};

const MARKER: &str = "HELLO_TN_MARKER";
const HARD_TIMEOUT: Duration = Duration::from_secs(10);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let size = GridSize::new(30, 100);
    // Default child: simplest non-interactive command that prints the marker and
    // exits. Override with CLI args (`tn-cli <program> [args...]`) to smoke-test
    // another backend's child (e.g. a WSL distro).
    let args: Vec<String> = std::env::args().skip(1).collect();
    let spec = if let Some((program, rest)) = args.split_first() {
        let mut s = SpawnSpec::program(program);
        for a in rest {
            s = s.arg(a);
        }
        s
    } else {
        SpawnSpec::program("cmd.exe").arg("/c").arg(format!("echo {MARKER}"))
    };

    tracing::info!(
        "spawning `{}` in a {}x{} ConPTY",
        spec.program,
        size.rows,
        size.cols
    );
    let mut pty = LocalPty::spawn(&spec, PtySize::new(size.rows as u16, size.cols as u16))?;
    tracing::info!("child pid = {:?}", pty.process_id());

    let mut reader = pty.take_reader()?;
    let mut writer = pty.writer()?;

    // Reader thread: pump ConPTY output into the engine and route the engine's
    // PtyWrite replies (DSR responses, etc.) back to the PTY.
    let term = Arc::new(Mutex::new(Terminal::new(size)));
    let reader_term = Arc::clone(&term);
    let bytes_read = Arc::new(Mutex::new(0usize));
    let reader_bytes = Arc::clone(&bytes_read);
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    *reader_bytes.lock().unwrap() += n;
                    let replies: Vec<String> = {
                        let mut t = reader_term.lock().unwrap();
                        t.advance(&buf[..n]);
                        t.drain_events()
                            .into_iter()
                            .filter_map(|ev| match ev {
                                TermEvent::PtyWrite(s) => Some(s),
                                _ => None,
                            })
                            .collect()
                    };
                    for reply in replies {
                        let _ = writer.write_all(reply.as_bytes());
                    }
                    let _ = writer.flush();
                }
                Err(_) => break,
            }
        }
    });

    // Wait for the child to actually exit, with a hard timeout.
    let start = Instant::now();
    let mut code = -1;
    loop {
        match pty.try_wait() {
            Ok(Some(c)) => {
                code = c;
                break;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!("try_wait error: {e}");
                break;
            }
        }
        if start.elapsed() > HARD_TIMEOUT {
            tracing::warn!("hard timeout; killing child");
            let _ = pty.killer().and_then(|mut k| k.kill());
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }

    // Let the reader thread drain whatever ConPTY has buffered post-exit.
    thread::sleep(Duration::from_millis(300));

    let total = *bytes_read.lock().unwrap();
    let snap = term.lock().unwrap().snapshot();
    let text = snap.to_text();

    println!(
        "\n------ terminal grid {}x{} (read {total} bytes, exit {code}) ------",
        snap.rows, snap.cols
    );
    println!("{text}");
    println!("------ end grid ------");

    let ok = text.contains(MARKER);
    if ok {
        println!("\nSMOKE: PASS \u{2713}  ConPTY + alacritty parse + snapshot pipeline works.");
    } else {
        println!("\nSMOKE: FAIL \u{2717}  marker `{MARKER}` not found in grid.");
    }

    // Force-exit so the detached reader thread can't keep the process alive.
    std::process::exit(if ok { 0 } else { 1 });
}
