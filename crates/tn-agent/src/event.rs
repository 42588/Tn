//! [`AgentEvent`] — the single stream of facts the UI consumes about an agent
//! session. Adapters (log observers / sidecars) and the session lifecycle
//! translate private formats into these, so the UI subscribes instead of reading
//! agent log files directly. Payloads are intentionally lean for now; the richer
//! transcript / tool-call / permission shapes are fleshed out in P4.

use crate::{AiUsage, TranscriptEntry};
use serde::{Deserialize, Serialize};

/// Coarse run state of an agent session, surfaced **honestly** — derived from
/// observable signals (shell-integration command blocks, log activity, process
/// exit), never a fabricated "thinking…" spinner.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Starting,
    Idle,
    Running,
    Exited,
    Error,
}

/// A fact about one agent session. The UI reduces a stream of these into its
/// per-pane view model (header, usage ring, status, activity rail).
#[derive(Clone, Debug, PartialEq)]
pub enum AgentEvent {
    /// The agent session became active in its pane.
    SessionStarted,
    /// The agent exited (pane reverts to a plain shell, or the launcher).
    SessionEnded,
    /// The agent reported a new working directory (OSC 7 / log). Feeds **only**
    /// the owning pane's context — never the global Explorer (runtime ≠ namespace).
    CwdChanged(String),
    /// The active model id changed.
    ModelChanged(String),
    /// Fresh usage snapshot (the common case, from the usage poller).
    UsageUpdated(AiUsage),
    /// Run-state transition.
    StatusChanged(AgentStatus),
    /// New transcript text observed — a short tail preview for the header
    /// (sidecar/realtime adapters). Distinct from [`Self::TranscriptEntries`],
    /// which carries the full structured history for Tn's scrollable surface.
    TranscriptAppended(String),
    /// A batch of structured transcript entries parsed from the agent's session
    /// log, for Tn's **own** scrollable history surface (TUI agents never put the
    /// full conversation in terminal scrollback). `replace` = true means this is a
    /// fresh full snapshot (first bind / file rewrite) that supersedes any prior
    /// entries; false means append this delta in order.
    TranscriptEntries {
        entries: Vec<TranscriptEntry>,
        replace: bool,
    },
    /// The agent's working tree changed; the pane should refresh its git diff.
    DiffChanged,
    /// The agent is asking the user to approve an action (P4).
    PermissionRequested(String),
    /// The adapter/runtime hit an error worth surfacing.
    ErrorReported(String),
}
