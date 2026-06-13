//! [`AgentAdapter`] — translates one agent's private logs/usage into the unified
//! [`AiUsage`] / session model. Built-in Claude/Codex adapters live in `tn-ai`;
//! external adapters (P5) speak stdio/JSON-RPC. An agent with **no** adapter is
//! config-level only — it launches and gets the generic terminal surface, but
//! reports no usage (every method below defaults to "nothing observed").

use std::path::PathBuf;
use std::time::SystemTime;

use crate::{AgentDescriptor, AgentEvent, AiUsage};

/// A resolved session log: where it is and when last touched. The owning agent
/// is known from the adapter that produced it. Pane ownership (pane id + runtime
/// + file namespace) is paired on at the UI layer — kept out of this headless
/// type so `tn-agent` needs no UI dependency (runtime ≠ namespace boundary).
#[derive(Clone, Debug)]
pub struct SessionRef {
    pub path: PathBuf,
    pub mtime: SystemTime,
}

/// Observe + translate a single agent's private format. The default impls make a
/// "launch-only, no telemetry" adapter trivial: override just what the agent
/// actually exposes.
pub trait AgentAdapter: Send + Sync {
    /// This adapter's static identity/presentation/capabilities.
    fn descriptor(&self) -> &AgentDescriptor;

    /// Every known session log for this agent, as `(path, mtime)`. Keyed on
    /// **mtime** by the pane-binding logic — a resumed session reuses an old file,
    /// so creation time can't identify "this pane's session"; activity can.
    fn sessions_with_mtime(&self) -> Vec<(PathBuf, SystemTime)> {
        Vec::new()
    }

    /// Newest session log whose recorded cwd matches `cwd` (adapters fall back to
    /// the agent's newest session overall when the cwd differs, e.g. Codex in `~`).
    fn latest_session_file(&self, cwd: &str) -> Option<PathBuf> {
        let _ = cwd;
        None
    }

    /// Parse a full session log into usage. `None` if it carries no usage yet.
    fn parse_usage(&self, text: &str) -> Option<AiUsage> {
        let _ = text;
        None
    }

    /// Fold newly-appended log text into prior usage (incremental poll step).
    fn update_usage(&self, text: &str, prev: AiUsage) -> AiUsage {
        let _ = text;
        prev
    }

    /// Is the user on a subscription (Claude Pro/Max, ChatGPT) vs a metered API
    /// key? Drives the usage-display default (context % vs $). Unknown → false.
    fn is_subscription(&self) -> bool {
        false
    }

    /// Whether this adapter exposes a live event stream (sidecar / JSON-RPC /
    /// Tn Agent Protocol). The UI only starts the lightweight event poller for
    /// adapters that opt in here, so built-in log-only adapters do not gain a new
    /// hot path.
    fn has_realtime_events(&self) -> bool {
        false
    }

    /// Drain any live events observed since the previous call. Implementations
    /// must return quickly and never block on IO; process/stdout/socket clients
    /// should push into an internal queue from their reader thread.
    fn drain_events(&self) -> Vec<AgentEvent> {
        Vec::new()
    }
}

/// An adapter with identity only — no telemetry. Used for config-declared agents
/// (`[[agents]]`, the config-level tier) and as the base an external (sidecar /
/// JSON-RPC) adapter would extend. Every observation method uses the trait
/// default, so the agent hosts as a terminal but reports no usage.
pub struct GenericAdapter {
    descriptor: AgentDescriptor,
}

impl GenericAdapter {
    pub fn new(descriptor: AgentDescriptor) -> Self {
        Self { descriptor }
    }
}

impl AgentAdapter for GenericAdapter {
    fn descriptor(&self) -> &AgentDescriptor {
        &self.descriptor
    }
}
