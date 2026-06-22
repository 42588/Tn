//! Opt-in render instrumentation. See docs/质量保障/性能分析报告.md.
//!
//! A view wraps one [`PerfStats`] and calls [`record`](PerfStats::record) once
//! per render, saying whether it reused cached render data and (on a rebuild)
//! how long the rebuild took. When the `TN_PERF` env var is set, it emits a
//! `tracing::info!(target: "tn::perf", …)` summary about once a second — render
//! rate, cache hit-rate, and rebuild avg/max — so the effect of the caching is
//! observable in `%APPDATA%\Tn\logs\tn.log` (or the console in a debug build).
//!
//! Zero cost when disabled: `record` early-returns on the cached `enabled` flag,
//! so a release run with `TN_PERF` unset does no timing math or formatting.

use std::time::{Duration, Instant};

pub(crate) struct PerfStats {
    label: &'static str,
    enabled: bool,
    window_start: Instant,
    renders: u64,
    cache_hits: u64,
    rebuilds: u64,
    rebuild_ns_sum: u128,
    rebuild_ns_max: u128,
}

impl PerfStats {
    pub fn new(label: &'static str) -> Self {
        Self {
            label,
            enabled: std::env::var_os("TN_PERF").is_some(),
            window_start: Instant::now(),
            renders: 0,
            cache_hits: 0,
            rebuilds: 0,
            rebuild_ns_sum: 0,
            rebuild_ns_max: 0,
        }
    }

    /// Whether instrumentation is on — callers can skip taking timestamps when
    /// it isn't (an `Instant::now()` pair per render is otherwise wasted).
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    /// Record one render. `cache_hit` = reused cached render data; `rebuild` =
    /// `Some(duration)` of the data rebuild when it was a miss. Emits a summary
    /// roughly once a second.
    pub fn record(&mut self, cache_hit: bool, rebuild: Option<Duration>) {
        if !self.enabled {
            return;
        }
        self.renders += 1;
        if cache_hit {
            self.cache_hits += 1;
        }
        if let Some(d) = rebuild {
            let ns = d.as_nanos();
            self.rebuilds += 1;
            self.rebuild_ns_sum += ns;
            self.rebuild_ns_max = self.rebuild_ns_max.max(ns);
        }
        if self.window_start.elapsed() >= Duration::from_secs(1) {
            self.flush();
        }
    }

    fn flush(&mut self) {
        let secs = self.window_start.elapsed().as_secs_f64().max(1e-6);
        let hit_pct = if self.renders > 0 {
            100.0 * self.cache_hits as f64 / self.renders as f64
        } else {
            0.0
        };
        let avg_us = if self.rebuilds > 0 {
            self.rebuild_ns_sum as f64 / self.rebuilds as f64 / 1000.0
        } else {
            0.0
        };
        let max_us = self.rebuild_ns_max as f64 / 1000.0;
        tracing::info!(
            target: "tn::perf",
            "{}: {} renders ({:.0}/s) · cache {:.0}% hit · rebuild avg {:.1}µs max {:.1}µs over {} builds",
            self.label,
            self.renders,
            self.renders as f64 / secs,
            hit_pct,
            avg_us,
            max_us,
            self.rebuilds,
        );
        self.window_start = Instant::now();
        self.renders = 0;
        self.cache_hits = 0;
        self.rebuilds = 0;
        self.rebuild_ns_sum = 0;
        self.rebuild_ns_max = 0;
    }
}
