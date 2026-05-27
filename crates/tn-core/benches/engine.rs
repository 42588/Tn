//! Hot-path benchmarks for the terminal engine (待优化清单 §7.3).
//!
//! Two paths dominate runtime cost: parsing PTY bytes ([`Terminal::advance`])
//! and building per-frame render data ([`TerminalSnapshot::row_runs`], which the
//! UI calls every paint — §2.1 flags it as "全量重建"). Run with
//! `cargo bench -p tn-core`; these give a baseline before any perf work.

use criterion::{black_box, criterion_group, criterion_main, BatchSize, Criterion};
use tn_core::{GridSize, Terminal};

/// A realistic chunk of colored shell output (`ls`-like lines with SGR runs),
/// so the parser and run-batcher both see mixed styles rather than plain text.
fn sample_output(lines: usize) -> Vec<u8> {
    let mut s = String::new();
    for i in 0..lines {
        s.push_str(&format!(
            "\x1b[34mdrwxr-xr-x\x1b[0m  \x1b[32m{:>10}\x1b[0m  file_name_{}.rs\r\n",
            i * 4096,
            i
        ));
    }
    s.into_bytes()
}

fn bench_advance(c: &mut Criterion) {
    let data = sample_output(200);
    // Fresh terminal per iteration (excluded from timing) so we measure parsing,
    // not accumulated grid state.
    c.bench_function("advance_200_colored_lines", |b| {
        b.iter_batched(
            || Terminal::new(GridSize::new(40, 120)),
            |mut t| t.advance(black_box(&data)),
            BatchSize::SmallInput,
        );
    });
}

fn bench_render_data(c: &mut Criterion) {
    let mut t = Terminal::new(GridSize::new(40, 120));
    t.advance(&sample_output(200));
    // The full per-frame extraction the renderer runs: snapshot + run batching.
    c.bench_function("snapshot_and_row_runs_40x120", |b| {
        b.iter(|| {
            let snap = t.snapshot();
            black_box(snap.row_runs());
        });
    });
}

criterion_group!(benches, bench_advance, bench_render_data);
criterion_main!(benches);
