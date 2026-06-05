//! How a pane's usage pill is displayed — cost (`$`), context (`%`), or token
//! throughput — chosen **per pane**. The live mode lives on each `TerminalView`
//! (`usage_mode`); clicking the pill cycles it in memory, independently of every
//! other pane. This module is pure + stateless: it only computes a pane's
//! *starting* mode from config and the next mode on a click — no globals, no I/O
//! beyond the one auth probe `auto` needs.

use tn_ai::AgentKind;
use tn_config::BillingMode;

/// The starting display mode for a fresh pane of `agent`: the per-agent config
/// override if set, else the global default; `Auto` is resolved to a concrete
/// mode by detecting the agent's login (subscription → `%`, metered API → `$`).
/// Never returns `Auto` — a pane always starts on something concrete.
pub(crate) fn starting_mode(
    agent: AgentKind,
    global: BillingMode,
    claude_override: Option<BillingMode>,
    codex_override: Option<BillingMode>,
) -> BillingMode {
    let configured = match agent {
        AgentKind::ClaudeCode => claude_override.unwrap_or(global),
        AgentKind::Codex => codex_override.unwrap_or(global),
    };
    match configured {
        BillingMode::Auto => {
            if tn_ai::detect_subscription(agent) {
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
    fn starting_mode_prefers_agent_override_then_global() {
        // Explicit per-agent override wins over the global default.
        assert_eq!(
            starting_mode(
                AgentKind::Codex,
                BillingMode::Api,
                None,
                Some(BillingMode::Tokens)
            ),
            BillingMode::Tokens
        );
        // The other agent's override doesn't leak across.
        assert_eq!(
            starting_mode(
                AgentKind::Codex,
                BillingMode::Subscription,
                Some(BillingMode::Tokens),
                None
            ),
            BillingMode::Subscription
        );
        // A concrete global with no overrides passes straight through.
        assert_eq!(
            starting_mode(AgentKind::ClaudeCode, BillingMode::Tokens, None, None),
            BillingMode::Tokens
        );
    }
}
