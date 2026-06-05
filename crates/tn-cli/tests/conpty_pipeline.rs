//! Integration test (待优化清单 §7.1): the full ConPTY → tn-core pipeline.
//!
//! The workspace's unit tests are all headless-pure; nothing exercised a real
//! child process end to end. This spawns one through the local ConPTY backend,
//! pumps its output into the `tn-core` engine on a reader thread (routing the
//! engine's `PtyWrite` replies back to the PTY — the DSR answer ConPTY needs at
//! startup, without which the child stalls), polls `try_wait` with a hard
//! timeout (ConPTY has no reliable EOF), then asserts the rendered grid carries
//! the child's output. Windows-only (ConPTY).
#![cfg(windows)]

use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use tn_core::{GridSize, TermEvent, Terminal};
use tn_pty::{LocalPty, PtyBackend, PtySize, SpawnSpec};

#[test]
fn conpty_echo_reaches_the_grid() {
    const MARKER: &str = "HELLO_TN_PIPELINE";
    let size = GridSize::new(24, 80);
    let spec = SpawnSpec::program("cmd.exe")
        .arg("/c")
        .arg(format!("echo {MARKER}"));
    let mut pty = LocalPty::spawn(&spec, PtySize::new(size.rows as u16, size.cols as u16))
        .expect("spawn cmd.exe in a ConPTY");
    let mut reader = pty.take_reader().expect("pty reader");
    let mut writer = pty.writer().expect("pty writer");

    // Reader thread: ConPTY bytes -> engine; route the engine's PtyWrite replies
    // (the startup DSR answer, etc.) back to the PTY or the child never advances.
    let term = Arc::new(Mutex::new(Terminal::new(size)));
    let reader_term = Arc::clone(&term);
    thread::spawn(move || {
        let mut buf = [0u8; 8192];
        while let Ok(n) = reader.read(&mut buf) {
            if n == 0 {
                break;
            }
            let replies: Vec<String> = {
                let mut t = reader_term.lock().unwrap();
                t.advance(&buf[..n]);
                t.drain_events()
                    .into_iter()
                    .filter_map(|e| match e {
                        TermEvent::PtyWrite(s) => Some(s),
                        _ => None,
                    })
                    .collect()
            };
            for r in replies {
                let _ = writer.write_all(r.as_bytes());
            }
            let _ = writer.flush();
        }
    });

    // Wait for the child to exit, with a hard cap so a hang fails fast.
    let start = Instant::now();
    loop {
        if matches!(pty.try_wait(), Ok(Some(_))) {
            break;
        }
        if start.elapsed() > Duration::from_secs(10) {
            let _ = pty.killer().and_then(|mut k| k.kill());
            panic!("child did not exit within 10s");
        }
        thread::sleep(Duration::from_millis(25));
    }
    // Let the reader drain whatever ConPTY buffered after exit.
    thread::sleep(Duration::from_millis(300));

    let text = term.lock().unwrap().snapshot().to_text();
    assert!(
        text.contains(MARKER),
        "marker {MARKER:?} not found in grid:\n{text}"
    );
}
