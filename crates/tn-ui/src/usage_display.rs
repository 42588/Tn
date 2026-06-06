//! How a pane's usage pill is displayed — cost (`$`), context (`%`), or token
//! throughput — chosen **per pane**. The live mode lives on each `TerminalView`
//! (`usage_mode`); clicking the pill cycles it in memory, independently of every
//! other pane. This module is pure + stateless: it only computes a pane's
//! *starting* mode from config and the next mode on a click — no globals, no I/O
//! beyond the one auth probe `auto` needs.

use tn_config::BillingMode;

/// The starting display mode for a fresh agent pane: the per-agent config
/// `override_mode` if set, else the global default; `Auto` is resolved to a
/// concrete mode from `is_subscription` (subscription → `%`, metered API → `$`).
/// Agent-agnostic — the caller resolves the override (via `General::billing_for`)
/// and the subscription flag (via the agent's adapter), so no agent is named here.
/// Never returns `Auto` — a pane always starts on something concrete.
pub(crate) fn starting_mode(
    global: BillingMode,
    override_mode: Option<BillingMode>,
    is_subscription: bool,
) -> BillingMode {
    match override_mode.unwrap_or(global) {
        BillingMode::Auto => {
            if is_subscription {
                BillingMode::Subscription
            } else {
                BillingMode::Api
            }
        }
        concrete => concrete,
    }
}

/// Next mode when the pill is clicked: `$` → `%` → tokens → `$` (wraps). `Auto`
/// shouldn't reach here (panes store a concrete mode), but maps to `$` so a click
/// always lands somewhere concrete.
pub(crate) fn cycle(mode: BillingMode) -> BillingMode {
    match mode {
        BillingMode::Api => BillingMode::Subscription,
        BillingMode::Subscription => BillingMode::Tokens,
        BillingMode::Tokens | BillingMode::Auto => BillingMode::Api,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cycle_wraps_through_three_modes() {
        assert_eq!(cycle(BillingMode::Api), BillingMode::Subscription);
        assert_eq!(cycle(BillingMode::Subscription), BillingMode::Tokens);
        assert_eq!(cycle(BillingMode::Tokens), BillingMode::Api);
        assert_eq!(cycle(BillingMode::Auto), BillingMode::Api); // safety landing
    }

    #[test]
    fn starting_mode_prefers_override_then_global() {
        // Explicit per-agent override wins over the global default.
        assert_eq!(
            starting_mode(BillingMode::Api, Some(BillingMode::Tokens), false),
            BillingMode::Tokens
        );
        // No override → the global passes straight through.
        assert_eq!(
            starting_mode(BillingMode::Subscription, None, false),
            BillingMode::Subscription
        );
        // A concrete global with no override passes straight through.
        assert_eq!(
            starting_mode(BillingMode::Tokens, None, true),
            BillingMode::Tokens
        );
        // `Auto` resolves from the subscription flag.
        assert_eq!(
            starting_mode(BillingMode::Auto, None, true),
            BillingMode::Subscription
        );
        assert_eq!(
            starting_mode(BillingMode::Auto, None, false),
            BillingMode::Api
        );
    }
}
