//! [`AgentId`] — the open, string-keyed identity that replaces the closed
//! `AgentKind { ClaudeCode, Codex }` enum. A new agent needs no new variant:
//! it declares an id ("claude" / "codex" / "gemini" / …) and the whole UI keys
//! off this + the agent's [`crate::AgentDescriptor`].

use serde::{Deserialize, Serialize};

/// Stable identity for an agent provider. Lowercase, slug-like by convention
/// (matches the layout-serialization string and config `agent = "…"` field).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct AgentId(pub String);

impl AgentId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for AgentId {
    fn from(s: &str) -> Self {
        Self(s.to_string())
    }
}

impl From<String> for AgentId {
    fn from(s: String) -> Self {
        Self(s)
    }
}
