//! Usage-ring pricing sync (auto-detect 定价).
//!
//! `$` rates aren't in anything Tn observes — session logs carry only token
//! counts, and Tn has no API credentials — so pricing can only come from a table.
//! This module keeps that table fresh: load the on-disk cache at startup (offline,
//! instant), then, when enabled and the cache is stale, fetch LiteLLM's public
//! price list in the background and re-cache it. The parsed table is installed into
//! [`tn_agent::pricing`]'s loaded slot; its built-in fallback covers offline / miss,
//! so a failed or disabled fetch is never wrong, just less current.
//!
//! Network is opt-out (`[general] pricing_auto_refresh`). The fetch is a single
//! bounded GET on a detached thread (like `gitutil::capture_bounded`) — no async
//! runtime, never blocks the UI.

use std::path::PathBuf;
use std::time::Duration;

use tn_agent::pricing::{set_pricing_table, PricingTable};

/// Reuse a cached table without a network hit while it's younger than this.
const REFRESH_TTL: Duration = Duration::from_secs(7 * 24 * 3600);
/// Hard bound on the background GET so a slow/hung host can't leak a thread.
const FETCH_TIMEOUT: Duration = Duration::from_secs(8);

fn cache_path() -> Option<PathBuf> {
    tn_config::config_dir().map(|d| d.join("pricing_cache.json"))
}

/// Install the cached table into the pricing slot now (sync, offline, ~ms). Returns
/// the cache's age so the caller can decide whether a refresh is due. `None` when
/// there is no usable cache (→ refresh should run, built-in fallback meanwhile).
pub fn install_cached() -> Option<Duration> {
    let path = cache_path()?;
    let json = std::fs::read_to_string(&path).ok()?;
    if let Some(table) = PricingTable::from_litellm_json(&json) {
        set_pricing_table(Some(table));
    }
    std::fs::metadata(&path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
}

/// Spawn a bounded background refresh when enabled and the cache is stale/missing.
/// On success: install the new table and rewrite the cache. All best-effort — any
/// failure silently leaves the cached/built-in pricing in place.
pub fn spawn_refresh(enabled: bool, url: String, cache_age: Option<Duration>) {
    if !enabled {
        return;
    }
    if cache_age.is_some_and(|age| age < REFRESH_TTL) {
        return; // cache still fresh → no network this launch
    }
    std::thread::spawn(move || {
        let Ok(client) = reqwest::blocking::Client::builder()
            .timeout(FETCH_TIMEOUT)
            .user_agent("tn-usage-ring")
            .build()
        else {
            return;
        };
        let body = match client
            .get(url.as_str())
            .send()
            .and_then(|r| r.error_for_status())
        {
            Ok(resp) => match resp.text() {
                Ok(b) => b,
                Err(e) => {
                    tracing::warn!(target: "tn::pricing", error = %e, "read pricing body failed");
                    return;
                }
            },
            Err(e) => {
                tracing::warn!(target: "tn::pricing", error = %e, "fetch pricing failed");
                return;
            }
        };
        let Some(table) = PricingTable::from_litellm_json(&body) else {
            tracing::warn!(target: "tn::pricing", "pricing body parsed to an empty table");
            return;
        };
        let n = table.len();
        set_pricing_table(Some(table));
        if let Some(path) = cache_path() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, body.as_bytes());
        }
        tracing::info!(target: "tn::pricing", models = n, "pricing table refreshed from network");
    });
}
