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
    // `TN_RESIZE_EXP=1`: instead of the smoke test, run a ConPTY resize probe
    // (does growing/shrinking the PTY lose scrollback content?). See fn below.
    if let Ok(v) = std::env::var("TN_RESIZE_EXP") {
        return match v.as_str() {
            "interactive" => resize_interactive_probe(),
            "prompt" => prompt_layout_probe(),
            _ => resize_experiment(),
        };
    }

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

    let mut pty = LocalPty::spawn(&spec, PtySize::new(size.rows as u16, size.cols as u16))?;

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
            Err(_) => break,
        }
        if start.elapsed() > HARD_TIMEOUT {
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

/// Probe: does resizing a live ConPTY lose scrollback content? Spawns pwsh,
/// prints LINE_1..LINE_40, then sleeps (so the shell is idle and ONLY ConPTY's
/// own resize-repaint can touch the grid). We snapshot the full scrollback at
/// the start size, after a GROW, and after a SHRINK, reporting which lines
/// survive. This isolates the divider-drag "content disappears" report.
fn resize_experiment() -> anyhow::Result<()> {
    let start = GridSize::new(12, 80);
    let spec = SpawnSpec::program("powershell.exe")
        .arg("-NoProfile")
        .arg("-NoLogo")
        .arg("-Command")
        .arg("1..40 | ForEach-Object { Write-Host \"LINE_$_\" }; Start-Sleep -Seconds 8");

    let mut pty = LocalPty::spawn(&spec, PtySize::new(start.rows as u16, start.cols as u16))?;
    let mut reader = pty.take_reader()?;
    let mut writer = pty.writer()?;
    let term = Arc::new(Mutex::new(Terminal::new(start)));
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
    });

    // Walk the entire scrollback into a set of trimmed non-empty lines.
    let collect = |term: &Arc<Mutex<Terminal>>| -> std::collections::BTreeSet<String> {
        let mut seen = std::collections::BTreeSet::new();
        let rows = {
            let mut t = term.lock().unwrap();
            t.scroll(1_000_000); // clamps to the top of history
            t.size().rows
        };
        loop {
            let (off, lines) = {
                let t = term.lock().unwrap();
                let snap = t.snapshot();
                (snap.scroll_offset, snap.rows_text())
            };
            for l in lines {
                let l = l.trim().to_string();
                if !l.is_empty() {
                    seen.insert(l);
                }
            }
            if off == 0 {
                break;
            }
            term.lock().unwrap().scroll(-(rows as i32)); // page toward newer
        }
        term.lock().unwrap().scroll_to_bottom();
        seen
    };
    let report = |label: &str, seen: &std::collections::BTreeSet<String>| {
        let present: Vec<u32> = (1..=40)
            .filter(|n| seen.contains(&format!("LINE_{n}")))
            .collect();
        let missing: Vec<u32> = (1..=40)
            .filter(|n| !seen.contains(&format!("LINE_{n}")))
            .collect();
        println!("[{label}] {}/40 lines survive; missing = {:?}", present.len(), missing);
    };

    // `TN_RESIZE_EXP=locked` exercises the row-lock fix: alacritty resizes
    // exactly, but ConPTY's rows are pinned to a high-water mark (never grow),
    // so its repaint can't clobber pulled-up scrollback. Default exercises the
    // naive path (resize both) that loses content.
    // Strategies: `naive` (default) resizes both exactly — alacritty bottom-
    // anchors a grow, ConPTY repaints top-anchored, clobbering pulled-up history.
    // `locked` pins ConPTY rows to a high-water mark (the reverted fix; breaks
    // normal commands via ConPTY≠alacritty mismatch). `topgrow` keeps ConPTY ==
    // alacritty but uses `resize_conpty` so alacritty top-anchors a grow (pushes
    // pulled rows back into scrollback) to match ConPTY's repaint — the candidate.
    let mode = std::env::var("TN_RESIZE_EXP").unwrap_or_default();
    let locked = mode == "locked";
    let topgrow = mode == "topgrow";
    let mut pty_hwm = start.rows as u16;
    let resize = |term: &Arc<Mutex<Terminal>>, pty: &mut LocalPty, rows: u16, cols: u16, hwm: &mut u16| {
        let size = GridSize::new(rows as usize, cols as usize);
        if topgrow {
            term.lock().unwrap().resize_conpty(size);
        } else {
            term.lock().unwrap().resize(size);
        }
        let pty_rows = if locked {
            *hwm = (*hwm).max(rows);
            *hwm
        } else {
            rows
        };
        let _ = pty.resize(PtySize::new(pty_rows, cols));
    };

    println!(
        "strategy: {}",
        match (locked, topgrow) {
            (true, _) => "row-lock (reverted fix)",
            (_, true) => "topgrow (resize_conpty: top-anchor grow)",
            _ => "naive (resize both)",
        }
    );
    thread::sleep(Duration::from_millis(2500)); // let all 40 lines land
    report("start 12x80", &collect(&term));

    resize(&term, &mut pty, 24, 80, &mut pty_hwm); // GROW (taller pane)
    thread::sleep(Duration::from_millis(1500));
    report("grow  24x80", &collect(&term));

    resize(&term, &mut pty, 8, 80, &mut pty_hwm); // SHRINK (shorter pane)
    thread::sleep(Duration::from_millis(1500));
    report("shrink 8x80", &collect(&term));

    resize(&term, &mut pty, 20, 80, &mut pty_hwm); // GROW back (within HWM when locked)
    thread::sleep(Duration::from_millis(1500));
    report("regrow 20x80", &collect(&term));

    let _ = pty.killer().and_then(|mut k| k.kill());
    std::process::exit(0);
}

/// Probe: reproduce the "blank space above the prompt" report (`TN_RESIZE_EXP=prompt`).
/// Spawns the user's real pwsh **with their profile** (so oh-my-posh / starship
/// loads), with ConPTY rows possibly larger than alacritty's grid (the row-lock).
/// Dumps the visible grid at a fresh prompt and after `ls`, so we can see whether
/// a tall ConPTY + fancy prompt pushes content down. Env: `TN_PTY_ROWS` (ConPTY,
/// default 120), `TN_GRID_ROWS` (alacritty, default 40).
fn prompt_layout_probe() -> anyhow::Result<()> {
    let pty_rows: u16 = std::env::var("TN_PTY_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(120);
    let grid_rows: usize =
        std::env::var("TN_GRID_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(40);
    let cols = 90u16;
    // Mimic the real app: spawn at the initial ROWS (34), then the first render
    // resizes ConPTY to the row-lock (120) + alacritty to the pane height. That
    // 34->120 resize is what oh-my-posh/PSReadLine react to. `TN_SPAWN_ROWS`=0
    // skips it (spawn straight at the final size).
    let spawn_rows: u16 =
        std::env::var("TN_SPAWN_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(34);
    let initial = if spawn_rows == 0 { pty_rows } else { spawn_rows };
    // `TN_PTY_NOGROW=1` = the FIX: spawn ConPTY straight at the lock height
    // (`pty_rows`) and never grow its rows — only alacritty resizes. The buggy
    // path (default) grows ConPTY `initial`->`pty_rows` on the resize step.
    let nogrow = std::env::var("TN_PTY_NOGROW").is_ok();
    let conpty_spawn = if nogrow { pty_rows } else { initial };
    // -NoLogo only (NOT -NoProfile): load the user's profile so their real prompt
    // renders, exactly like a Tn shell pane. `TN_INTEGRATE=1` also injects the
    // Tn OSC-133 shell-integration script (like the default pwsh pane), to catch
    // integration × prompt interactions.
    let mut spec = SpawnSpec::program("powershell.exe").arg("-NoLogo");
    if std::env::var("TN_INTEGRATE").is_ok() {
        spec = spec
            .arg("-NoExit")
            .arg("-EncodedCommand")
            .arg(tn_shell::Integration::new().encoded_command());
    }
    let mut pty = LocalPty::spawn(&spec, PtySize::new(conpty_spawn, cols))?;
    let mut reader = pty.take_reader()?;
    let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pty.writer()?));
    // alacritty grid starts at the spawn rows, then (below) resizes to grid_rows.
    let term = Arc::new(Mutex::new(Terminal::new(GridSize::new(initial as usize, cols as usize))));
    let reader_term = Arc::clone(&term);
    let reader_writer = Arc::clone(&writer);
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
                    .filter_map(|ev| match ev {
                        TermEvent::PtyWrite(s) => Some(s),
                        _ => None,
                    })
                    .collect()
            };
            let mut w = reader_writer.lock().unwrap();
            for reply in replies {
                let _ = w.write_all(reply.as_bytes());
            }
            let _ = w.flush();
        }
    });
    let dump = |label: &str| {
        let snap = term.lock().unwrap().snapshot();
        let rows = snap.rows_text();
        let first = rows.iter().position(|l| !l.trim().is_empty());
        let last = rows.iter().rposition(|l| !l.trim().is_empty());
        println!(
            "\n----- {label} (ConPTY {pty_rows} / grid {grid_rows}) — first non-blank row = {:?}, last = {:?} -----",
            first, last
        );
        for (i, line) in rows.iter().enumerate() {
            println!("{i:>2}|{line}");
        }
        println!("----- end -----");
    };

    thread::sleep(Duration::from_millis(2000)); // profile + prompt load at spawn size
    // First-render resize: alacritty -> grid_rows. ConPTY grows to the lock only
    // in the buggy path; the fix (`nogrow`) leaves ConPTY at its spawn height.
    if spawn_rows != 0 {
        term.lock().unwrap().resize(GridSize::new(grid_rows, cols as usize));
        if !nogrow {
            let _ = pty.resize(PtySize::new(pty_rows, cols));
        }
        thread::sleep(Duration::from_millis(800));
    }
    dump("fresh prompt");
    // Run a few commands so cumulative output scrolls alacritty; with the row-lock
    // (ConPTY rows > grid) this is where ConPTY's cursor diverges from alacritty's.
    for cmd in ["echo hi", "ls", "echo two", "echo three"] {
        {
            let mut w = writer.lock().unwrap();
            let _ = w.write_all(cmd.as_bytes());
            let _ = w.write_all(b"\r");
            let _ = w.flush();
        }
        thread::sleep(Duration::from_millis(1200));
    }
    dump("after a few commands");

    let _ = pty.killer().and_then(|mut k| k.kill());
    std::process::exit(0);
}

/// Probe: when ConPTY's rows are LOCKED larger than alacritty's grid (a plain
/// shell pane that was shrunk while row-locked), does interactive output render
/// coherently, or does ConPTY's absolute cursor positioning (in its taller
/// coordinate space) land outside alacritty's grid and corrupt the view? This
/// is the make-or-break test for the row-lock fix. We drive a real interactive
/// pwsh, shrink ONLY alacritty (ConPTY stays tall), run a 15-line command, and
/// dump the visible grid for inspection.
fn resize_interactive_probe() -> anyhow::Result<()> {
    let pty_rows: u16 = std::env::var("TN_PTY_ROWS").ok().and_then(|v| v.parse().ok()).unwrap_or(24);
    let cols = 80u16;
    let spec = SpawnSpec::program("powershell.exe").arg("-NoLogo").arg("-NoProfile");
    let mut pty = LocalPty::spawn(&spec, PtySize::new(pty_rows, cols))?;
    let mut reader = pty.take_reader()?;
    let writer: Arc<Mutex<Box<dyn Write + Send>>> = Arc::new(Mutex::new(pty.writer()?));
    // alacritty starts matched, then we shrink ONLY it to 12 (ConPTY stays 24).
    let term = Arc::new(Mutex::new(Terminal::new(GridSize::new(pty_rows as usize, cols as usize))));
    let reader_term = Arc::clone(&term);
    let reader_writer = Arc::clone(&writer);
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
                    .filter_map(|ev| match ev {
                        TermEvent::PtyWrite(s) => Some(s),
                        _ => None,
                    })
                    .collect()
            };
            let mut w = reader_writer.lock().unwrap();
            for reply in replies {
                let _ = w.write_all(reply.as_bytes());
            }
            let _ = w.flush();
        }
    });

    let send = |s: &str| {
        let mut w = writer.lock().unwrap();
        let _ = w.write_all(s.as_bytes());
        let _ = w.flush();
    };
    let dump = |label: &str| {
        let snap = term.lock().unwrap().snapshot();
        println!("\n----- visible grid after {label} ({}x{}) -----", snap.rows, snap.cols);
        for (i, line) in snap.rows_text().iter().enumerate() {
            println!("{i:>2}|{line}");
        }
        println!("----- end -----");
    };

    thread::sleep(Duration::from_millis(1800)); // initial prompt

    // Enter the row-locked-shrunk state: alacritty 12 rows, ConPTY still 24.
    term.lock().unwrap().resize(GridSize::new(12, cols as usize));
    thread::sleep(Duration::from_millis(300));

    // A 15-line burst — more than alacritty's 12 rows but fewer than ConPTY's 24,
    // so conhost won't scroll and WILL position the cursor below alacritty row 12.
    send("1..15 | ForEach-Object { \"OUT_$_\" }\r\n");
    thread::sleep(Duration::from_millis(1200));
    dump("15-line burst");

    send("Write-Host TAIL_MARKER\r\n");
    thread::sleep(Duration::from_millis(1000));
    dump("tail marker");

    let _ = pty.killer().and_then(|mut k| k.kill());
    std::process::exit(0);
}
