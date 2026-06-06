//! Saved pane layouts (app menu「布局」): the **active tab's split structure +
//! each pane's launcher**, persisted to a fixed set of slots so a workspace
//! arrangement can be recalled later.
//!
//! A running shell session can't be serialized — loading a layout **re-spawns**
//! each pane from its launcher (Agent / pwsh / WSL), not its session
//! content. The serializable tree ([`LayoutNode`]) mirrors `workspace::Node`, but
//! its leaves carry a [`LayoutPane`] (enough to rebuild a `LaunchSpec`) instead of
//! a live `PaneId`. Persisted as JSON at `%APPDATA%\Tn\layouts.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tn_agent::AgentId;

use crate::terminal_view::{FileNamespace, LaunchSpec};

/// Number of layout slots offered in the manager.
pub const SLOTS: usize = 7;

/// A serializable pane launcher (the re-spawnable part of a `LaunchSpec`). SSH is
/// not persisted (M2 parked) — an SSH pane saves as its hosting program.
#[derive(Clone, Serialize, Deserialize)]
pub struct LayoutPane {
    #[serde(default)]
    pub shell_integration: Option<String>,
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub integrate_pwsh: bool,
    /// The launch-intent agent id (for example `"agent"` or any registered id), or
    /// `None` for a plain shell. Stored as the raw `AgentId` string — open by design.
    #[serde(default)]
    pub agent: Option<String>,
}

impl LayoutPane {
    pub fn from_spec(s: &LaunchSpec) -> Self {
        Self {
            program: s.program.clone(),
            args: s.args.clone(),
            integrate_pwsh: s.integrate_pwsh,
            shell_integration: s.shell_integration.map(|si| match si {
                crate::terminal_view::ShellIntegration::Pwsh => "pwsh".to_string(),
                crate::terminal_view::ShellIntegration::Bash => "bash".to_string(),
            }),
            // Open: the agent id string is the wire form — no per-agent arm.
            agent: s.agent.as_ref().map(|a| a.as_str().to_string()),
        }
    }

    pub fn to_spec(&self) -> LaunchSpec {
        let shell_integration = match self.shell_integration.as_deref() {
            Some("pwsh") => Some(crate::terminal_view::ShellIntegration::Pwsh),
            Some("bash") => Some(crate::terminal_view::ShellIntegration::Bash),
            _ => {
                // Backward compat: derive from integrate_pwsh + program
                if self.integrate_pwsh {
                    if self.program.eq_ignore_ascii_case("wsl.exe") {
                        Some(crate::terminal_view::ShellIntegration::Bash)
                    } else {
                        Some(crate::terminal_view::ShellIntegration::Pwsh)
                    }
                } else {
                    None
                }
            }
        };
        let file_namespace = if self.program.eq_ignore_ascii_case("wsl.exe") {
            FileNamespace::Wsl {
                distro: wsl_distro_from_args(&self.args),
            }
        } else {
            FileNamespace::Host
        };
        LaunchSpec {
            program: self.program.clone(),
            args: self.args.clone(),
            integrate_pwsh: self.integrate_pwsh,
            shell_integration,
            agent: self.agent.as_deref().map(AgentId::new),
            ssh: None,
            cwd: None,
            file_namespace,
        }
    }
}

fn wsl_distro_from_args(args: &[String]) -> Option<String> {
    args.windows(2)
        .find_map(|w| (w[0] == "-d" || w[0] == "--distribution").then(|| w[1].clone()))
}

/// A serializable mirror of `workspace::Node` (leaves = launchers, not live panes).
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "lowercase")]
pub enum LayoutNode {
    Pane(LayoutPane),
    Split {
        /// `true` = horizontal (Row, side-by-side), `false` = vertical (Col, stacked).
        row: bool,
        kids: Vec<LayoutNode>,
        weights: Vec<f32>,
    },
}

impl LayoutNode {
    /// Number of panes (leaves) this layout would spawn.
    pub fn pane_count(&self) -> usize {
        match self {
            LayoutNode::Pane(_) => 1,
            LayoutNode::Split { kids, .. } => kids.iter().map(LayoutNode::pane_count).sum(),
        }
    }
}

/// The on-disk slot table (`layouts.json`). `slots[i] == None` = empty slot.
#[derive(Clone, Serialize, Deserialize)]
pub struct Layouts {
    pub slots: Vec<Option<LayoutNode>>,
}

impl Default for Layouts {
    fn default() -> Self {
        Self {
            slots: (0..SLOTS).map(|_| None).collect(),
        }
    }
}

impl Layouts {
    fn path() -> Option<PathBuf> {
        tn_config::config_dir().map(|d| d.join("layouts.json"))
    }

    /// Load the slot table from disk (missing / unparsable → all empty). Always
    /// returns exactly [`SLOTS`] entries.
    pub fn load() -> Self {
        let mut me = Self::path()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| serde_json::from_str::<Layouts>(&s).ok())
            .unwrap_or_default();
        me.slots.resize(SLOTS, None);
        me
    }

    /// Persist the slot table to disk (best-effort; logs on failure).
    pub fn save(&self) {
        let Some(path) = Self::path() else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match serde_json::to_string_pretty(self) {
            Ok(json) => {
                if let Err(e) = std::fs::write(&path, json) {
                    tracing::error!(path = %path.display(), error = %e, "save layouts failed");
                }
            }
            Err(e) => tracing::error!(error = %e, "serialize layouts failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_pane_roundtrips_through_spec() {
        let p = LayoutPane {
            program: "powershell.exe".into(),
            args: vec!["-NoLogo".into()],
            integrate_pwsh: true,
            shell_integration: None,
            agent: Some("claude".into()),
        };
        let spec = p.to_spec();
        assert_eq!(spec.agent, Some(AgentId::new("claude")));
        assert!(spec.ssh.is_none());
        let back = LayoutPane::from_spec(&spec);
        assert_eq!(back.program, "powershell.exe");
        assert_eq!(back.agent.as_deref(), Some("claude"));
        assert!(back.integrate_pwsh);
    }

    #[test]
    fn layout_json_roundtrips_and_counts_panes() {
        let tree = LayoutNode::Split {
            row: true,
            kids: vec![
                LayoutNode::Pane(LayoutPane {
                    program: "pwsh".into(),
                    args: vec![],
                    integrate_pwsh: true,
                    shell_integration: None,
                    agent: None,
                }),
                LayoutNode::Split {
                    row: false,
                    kids: vec![
                        LayoutNode::Pane(LayoutPane {
                            program: "a".into(),
                            args: vec![],
                            integrate_pwsh: false,
                            shell_integration: None,
                            agent: Some("codex".into()),
                        }),
                        LayoutNode::Pane(LayoutPane {
                            program: "b".into(),
                            args: vec![],
                            integrate_pwsh: false,
                            shell_integration: None,
                            agent: None,
                        }),
                    ],
                    weights: vec![1.0, 1.0],
                },
            ],
            weights: vec![2.0, 1.0],
        };
        assert_eq!(tree.pane_count(), 3);
        let json = serde_json::to_string(&tree).unwrap();
        let back: LayoutNode = serde_json::from_str(&json).unwrap();
        assert_eq!(back.pane_count(), 3);

        // Layouts default = SLOTS empty slots; resize keeps that on load.
        let d = Layouts::default();
        assert_eq!(d.slots.len(), SLOTS);
        assert!(d.slots.iter().all(Option::is_none));
    }
}
