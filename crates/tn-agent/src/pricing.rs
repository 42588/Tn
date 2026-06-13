//! Model pricing + context windows (USD per million tokens).
//!
//! Two tiers, prefer-table-then-fallback:
//!   1. **Loaded table** — a [`PricingTable`] parsed from LiteLLM's public
//!      `model_prices_and_context_window.json` and installed via
//!      [`set_pricing_table`] by the IO layer (tn-ui fetches + caches it). Covers
//!      every provider and auto-updates as new models ship.
//!   2. **Built-in fallback** ([`pricing_builtin`]) — a small hand-kept table by
//!      family + generation, used offline / on a table miss. Unknown models fall
//!      back to a zero-cost entry so the UI still shows tokens / context but never a
//!      wrong dollar figure.
//!
//! This crate is pure (no IO): it parses a table someone else fetched and holds it
//! in an in-memory slot. Subscription plans (Claude Max / Codex) should read these
//! as "equivalent API cost", not a literal bill — see
//! docs/产品体验/智能体用量与诚实原则.md.

use std::collections::HashMap;
use std::sync::RwLock;

use serde::{Deserialize, Serialize};

/// Per-million-token rates + context window for one model.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Pricing {
    pub input: f64,
    pub output: f64,
    pub cache_write: f64,
    pub cache_read: f64,
    pub context_window: u32,
}

impl Pricing {
    /// Estimated cost (USD) for the given token counts.
    /// Anthropic reports `input_tokens`, `cache_creation_input_tokens`, and
    /// `cache_read_input_tokens` as three **separate, additive** buckets — input
    /// does NOT include either cache count (a real turn reads 47K cached tokens
    /// with `input_tokens=2`). Charge each at its own rate; never subtract cache
    /// from input (that underflows u64 → astronomical cost / debug panic).
    pub fn cost(&self, input: u64, output: u64, cache_create: u64, cache_read: u64) -> f64 {
        const M: f64 = 1_000_000.0;
        input as f64 / M * self.input
            + output as f64 / M * self.output
            + cache_create as f64 / M * self.cache_write
            + cache_read as f64 / M * self.cache_read
    }
}

/// A parsed pricing table keyed by lowercased model id. Serializable so the IO
/// layer can cache it to disk and reload it offline.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct PricingTable {
    map: HashMap<String, Pricing>,
}

/// One entry from LiteLLM's `model_prices_and_context_window.json`. Costs are
/// **per token**; everything is optional so non-model rows (e.g. `sample_spec`)
/// and unknown fields parse without error.
#[derive(Deserialize)]
struct LiteLlmEntry {
    #[serde(default)]
    input_cost_per_token: Option<f64>,
    #[serde(default)]
    output_cost_per_token: Option<f64>,
    #[serde(default)]
    cache_creation_input_token_cost: Option<f64>,
    #[serde(default)]
    cache_read_input_token_cost: Option<f64>,
    #[serde(default)]
    max_input_tokens: Option<u32>,
}

impl PricingTable {
    /// Parse the LiteLLM JSON (`{ "<model-id>": { input_cost_per_token, … }, … }`)
    /// into per-MTok rates. Rows without an input cost are skipped (not chat models).
    /// `None` if the JSON doesn't parse or yields no priced models.
    pub fn from_litellm_json(json: &str) -> Option<Self> {
        const M: f64 = 1_000_000.0;
        // Parse to Values first, then each entry leniently: one malformed row (a
        // field with an unexpected type, the `sample_spec` placeholder) is skipped
        // instead of sinking the whole table.
        let raw: HashMap<String, serde_json::Value> = serde_json::from_str(json).ok()?;
        let mut map = HashMap::new();
        for (id, v) in raw {
            let Ok(e) = serde_json::from_value::<LiteLlmEntry>(v) else {
                continue;
            };
            let Some(inp) = e.input_cost_per_token else {
                continue;
            };
            let input = inp * M;
            let output = e.output_cost_per_token.unwrap_or(0.0) * M;
            // LiteLLM omits cache costs for many models — derive the standard
            // 1.25× write / 0.1× read of input (matches the built-in tiers).
            let cache_write = e.cache_creation_input_token_cost.map_or(input * 1.25, |c| c * M);
            let cache_read = e.cache_read_input_token_cost.map_or(input * 0.10, |c| c * M);
            let context_window = e
                .max_input_tokens
                .filter(|w| *w > 0)
                .unwrap_or_else(|| context_window_for(&id));
            map.insert(
                id.to_ascii_lowercase(),
                Pricing {
                    input,
                    output,
                    cache_write,
                    cache_read,
                    context_window,
                },
            );
        }
        (!map.is_empty()).then_some(Self { map })
    }

    /// Number of priced models (for logging / cache sanity).
    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    /// Look up a model id: exact, then with a trailing `-YYYYMMDD` date snapshot
    /// stripped, then the longest table key that is a prefix of the id (so a dated
    /// or suffixed id still resolves). `None` → caller uses the built-in fallback.
    pub fn get(&self, model: &str) -> Option<Pricing> {
        let id = model.to_ascii_lowercase();
        if let Some(p) = self.map.get(&id) {
            return Some(*p);
        }
        if let Some(base) = strip_date_suffix(&id) {
            if let Some(p) = self.map.get(base) {
                return Some(*p);
            }
        }
        self.map
            .iter()
            .filter(|(k, _)| id.starts_with(k.as_str()))
            .max_by_key(|(k, _)| k.len())
            .map(|(_, p)| *p)
    }
}

/// `Some(base)` when `id` ends in a `-YYYYMMDD` snapshot suffix (e.g.
/// `claude-haiku-4-5-20251001` → `claude-haiku-4-5`).
fn strip_date_suffix(id: &str) -> Option<&str> {
    let (base, tail) = id.rsplit_once('-')?;
    (tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit())).then_some(base)
}

/// Process-wide slot for the loaded table. `None` until the IO layer installs one;
/// [`pricing_for`] then prefers it and falls back to [`pricing_builtin`].
static LOADED: RwLock<Option<PricingTable>> = RwLock::new(None);

/// Install (or clear with `None`) the fetched/cached pricing table. Called once at
/// startup from the on-disk cache and again after a successful refresh.
pub fn set_pricing_table(table: Option<PricingTable>) {
    if let Ok(mut g) = LOADED.write() {
        *g = table;
    }
}

/// Whether a table is currently loaded (for UI "数据来源" honesty).
pub fn has_loaded_table() -> bool {
    LOADED.read().map(|g| g.is_some()).unwrap_or(false)
}

/// Context-window size (tokens) inferred from a model id (built-in fallback).
///
/// Claude's session logs don't record the window (unlike Codex, which logs
/// `model_context_window`), so we infer it from the id. **Current-generation
/// Claude models ship a 1M window by default** — Opus 4.6 / 4.7 / 4.8, Sonnet 4.6,
/// and Fable 5 / Mythos 5 — while Haiku and older families stay at 200K. The
/// legacy `…-1m` marker still wins for an explicit long-context variant
/// (e.g. `claude-sonnet-4-6-1m`). Unknown / non-Claude ids fall back to 200K;
/// [`parse_claude_session`](crate::claude::parse_claude_session) then widens to 1M
/// if an observed turn already exceeds that, so the ring never reads >100%.
///
/// Substring + generation checks (not an exact-id table) so dated snapshots like
/// `claude-haiku-4-5-20251001` still match. Catalog source: claude-api model
/// reference (cached 2026-06) — extend the 1M list as new families land.
pub fn context_window_for(model: &str) -> u32 {
    let id = model.to_ascii_lowercase();
    let one_m = id.contains("1m")
        || id.contains("fable")
        || id.contains("mythos")
        || id.contains("opus-4-6")
        || id.contains("opus-4-7")
        || id.contains("opus-4-8")
        || id.contains("sonnet-4-6");
    if one_m {
        1_000_000
    } else {
        200_000
    }
}

/// Look up pricing for a model id: the loaded LiteLLM table first, else the
/// built-in fallback. The public entry point — call sites stay a pure lookup.
pub fn pricing_for(model: &str) -> Pricing {
    if let Ok(g) = LOADED.read() {
        if let Some(p) = g.as_ref().and_then(|t| t.get(model)) {
            return p;
        }
    }
    pricing_builtin(model)
}

/// Hand-kept fallback pricing by family + generation (per MTok). Used offline or
/// when the loaded table lacks the model. Cache rates follow the standard
/// 1.25× write / 0.1× read of input. Catalog source: claude-api reference (2026-06).
pub fn pricing_builtin(model: &str) -> Pricing {
    let id = model.to_ascii_lowercase();
    let context_window = context_window_for(&id);
    let (input, output, cache_write, cache_read) = if id.contains("fable") || id.contains("mythos")
    {
        // Fable 5 / Mythos 5.
        (10.0, 50.0, 12.50, 1.00)
    } else if id.contains("opus") {
        if id.contains("opus-4-6") || id.contains("opus-4-7") || id.contains("opus-4-8") {
            // Current Opus dropped to $5/$25 (was $15/$75 on Opus 3/4.0/4.1/4.5).
            (5.0, 25.0, 6.25, 0.50)
        } else {
            (15.0, 75.0, 18.75, 1.50)
        }
    } else if id.contains("haiku") {
        (1.0, 5.0, 1.25, 0.10)
    } else if id.contains("sonnet") {
        (3.0, 15.0, 3.75, 0.30)
    } else if id.contains("gpt") || id.contains("codex") || id.contains("o3") || id.contains("o4") {
        // Codex / OpenAI families — coarse placeholder, refined when Codex lands.
        (2.5, 10.0, 0.0, 0.25)
    } else {
        (0.0, 0.0, 0.0, 0.0) // unknown: report tokens/context, no cost guess
    };
    Pricing {
        input,
        output,
        cache_write,
        cache_read,
        context_window,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtin_families_and_generations() {
        // Current Opus → $5/$25 (the stale-pricing bug was $15/$75 here).
        assert_eq!(pricing_builtin("claude-opus-4-8").input, 5.0);
        assert_eq!(pricing_builtin("claude-opus-4-7").output, 25.0);
        assert_eq!(pricing_builtin("claude-opus-4-6").input, 5.0);
        // Legacy Opus stays $15/$75.
        assert_eq!(pricing_builtin("claude-opus-4-5").input, 15.0);
        assert_eq!(pricing_builtin("claude-opus-4-1").output, 75.0);
        // Fable / Mythos → $10/$50.
        assert_eq!(pricing_builtin("claude-fable-5").input, 10.0);
        assert_eq!(pricing_builtin("claude-mythos-5").output, 50.0);
        // Sonnet / Haiku unchanged.
        assert_eq!(pricing_builtin("claude-sonnet-4-6").output, 15.0);
        assert_eq!(pricing_builtin("claude-haiku-4-5").input, 1.0);
        // Unknown → zero (no wrong dollar guess).
        assert_eq!(pricing_builtin("totally-unknown").input, 0.0);
        // Cache rates follow 1.25× / 0.1× of input.
        let o = pricing_builtin("claude-opus-4-8");
        assert!((o.cache_write - 6.25).abs() < 1e-9);
        assert!((o.cache_read - 0.50).abs() < 1e-9);
    }

    #[test]
    fn context_window_matches_current_catalog() {
        for m in [
            "claude-opus-4-8",
            "claude-opus-4-7",
            "claude-opus-4-6",
            "claude-sonnet-4-6",
            "claude-fable-5",
            "claude-mythos-5",
            "claude-sonnet-4-6-1m",
        ] {
            assert_eq!(context_window_for(m), 1_000_000, "{m} should be 1M");
        }
        for m in [
            "claude-haiku-4-5-20251001",
            "claude-opus-4-5",
            "claude-opus-4-1",
            "claude-sonnet-4-5",
            "deepseek-v4-pro",
            "totally-unknown",
        ] {
            assert_eq!(context_window_for(m), 200_000, "{m} should be 200K");
        }
    }

    #[test]
    fn cost_math() {
        let p = pricing_builtin("claude-sonnet-4-6"); // 3 / 15 / 3.75 / 0.30 per MTok
                                                       // 1M input + 1M output = $3 + $15 = $18.
        assert!((p.cost(1_000_000, 1_000_000, 0, 0) - 18.0).abs() < 1e-9);
    }

    #[test]
    fn litellm_parse_and_lookup() {
        // Per-token costs → per-MTok; max_input_tokens → context window; date-suffix
        // and prefix fallback resolve dated ids; non-model rows skipped.
        let json = r#"{
            "sample_spec": {"litellm_provider": "x"},
            "claude-opus-4-8": {
                "max_input_tokens": 1000000,
                "input_cost_per_token": 0.000005,
                "output_cost_per_token": 0.000025,
                "cache_creation_input_token_cost": 0.00000625,
                "cache_read_input_token_cost": 0.0000005
            },
            "claude-haiku-4-5": {
                "max_input_tokens": 200000,
                "input_cost_per_token": 0.000001,
                "output_cost_per_token": 0.000005
            },
            "embed-only": {"max_input_tokens": 8192}
        }"#;
        let t = PricingTable::from_litellm_json(json).expect("table");
        assert_eq!(t.len(), 2, "priced rows only (sample_spec + embed-only skipped)");

        let opus = t.get("claude-opus-4-8").expect("exact");
        assert!((opus.input - 5.0).abs() < 1e-9);
        assert!((opus.output - 25.0).abs() < 1e-9);
        assert_eq!(opus.context_window, 1_000_000);

        // Dated snapshot resolves via date-suffix strip.
        let h = t.get("claude-haiku-4-5-20251001").expect("date-stripped");
        assert!((h.input - 1.0).abs() < 1e-9);
        // Missing cache costs → derived 1.25× / 0.1×.
        assert!((h.cache_write - 1.25).abs() < 1e-9);
        assert!((h.cache_read - 0.10).abs() < 1e-9);

        assert!(t.get("gpt-5").is_none(), "unknown id → miss → builtin");
    }

    #[test]
    fn loaded_table_overrides_builtin_then_clears() {
        // Pin a deliberately-wrong price so the override is unmistakable.
        let json = r#"{"claude-opus-4-8":{"input_cost_per_token":0.999,"output_cost_per_token":0.0,"max_input_tokens":1000000}}"#;
        set_pricing_table(PricingTable::from_litellm_json(json));
        assert!((pricing_for("claude-opus-4-8").input - 999_000.0).abs() < 1e-3);
        // A model absent from the table falls through to the built-in.
        assert_eq!(pricing_for("claude-sonnet-4-6").output, 15.0);
        // Clear → back to built-in everywhere (keep the global clean for other tests).
        set_pricing_table(None);
        assert_eq!(pricing_for("claude-opus-4-8").input, 5.0);
    }

    #[test]
    fn strip_date_suffix_only_on_8_digits() {
        assert_eq!(strip_date_suffix("claude-haiku-4-5-20251001"), Some("claude-haiku-4-5"));
        assert_eq!(strip_date_suffix("claude-opus-4-8"), None); // "8" is not 8 digits
        assert_eq!(strip_date_suffix("gpt-5"), None);
    }
}
