//! Resolve the configured starting mode for a pane's usage readout. The header now
//! keeps the right chip token-only, but the config still accepts the existing
//! billing-mode fields so older user config remains harmless.

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

#[cfg(test)]
mod tests {
    use super::*;

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
