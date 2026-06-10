//! The app-wide [`AgentRegistry`] as a GPUI global. Built once at startup
//! (default = config-declared agents; optional telemetry adapters can be registered) and read
//! wherever the UI resolves an agent's identity, presentation, capabilities, or
//! usage adapter — so no UI code names a concrete agent (the Agent Host model).
//!
//! The P6 end state installs an **empty** registry here (pure shell host) plus
//! whatever agents the user registers; nothing else in the UI changes.

use gpui::App;
use tn_agent::AgentRegistry;

/// GPUI global wrapper for the active agent registry.
pub(crate) struct AgentHost(pub(crate) AgentRegistry);

impl gpui::Global for AgentHost {}

/// Build the agent registry from a loaded config — the single place both startup
/// (`lib::run`) and live reload (`Workspace::reload_agents`) construct it, so they
/// can't drift. Each `[[agents]]` manifest becomes either a **built-in telemetry
/// adapter** (when its command names Claude/Codex → real usage ring, keeping the
/// user's color/label) or a generic no-telemetry agent. Empty config → empty
/// registry = pure shell host.
pub(crate) fn build_registry(config: &tn_config::Loaded) -> AgentRegistry {
    let mut registry = AgentRegistry::new();
    for manifest in &config.config.agents {
        match tn_ai::builtin_adapter_for_manifest(manifest) {
            Some(builtin) => registry.register(builtin),
            None => registry.register_manifest(manifest),
        }
    }
    registry
}

/// The active registry — a cheap (Arc-backed `Vec`) clone. Returns an **empty**
/// registry when none is installed (e.g. a headless unit test that never called
/// `set_global`), so agent-dependent UI degrades to "no agent" instead of
/// panicking. Cloning sidesteps holding an `&App` borrow across view mutations.
pub(crate) fn agent_registry(cx: &App) -> AgentRegistry {
    cx.try_global::<AgentHost>()
        .map(|g| g.0.clone())
        .unwrap_or_default()
}

#[cfg(test)]
mod guard {
    use std::fs;
    use std::path::{Path, PathBuf};

    fn rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for e in entries.flatten() {
            let p = e.path();
            if p.is_dir() {
                rs_files(&p, out);
            } else if p.extension().is_some_and(|x| x == "rs") {
                out.push(p);
            }
        }
    }

    /// Guard: the UI must never reintroduce the closed agent enum (`Agent`+`Kind`).
    /// All agent identity flows through `AgentId` + the registry descriptor, so a
    /// third agent needs no UI change. The needle is assembled at runtime so this
    /// guard file doesn't match itself.
    #[test]
    fn ui_has_no_closed_agent_enum() {
        let needle = format!("Agent{}", "Kind");
        let mut files = Vec::new();
        rs_files(Path::new("src"), &mut files);
        assert!(!files.is_empty(), "no source files scanned (cwd wrong?)");
        let offenders: Vec<String> = files
            .into_iter()
            .filter(|f| {
                fs::read_to_string(f)
                    .unwrap_or_default()
                    .contains(needle.as_str())
            })
            .map(|f| f.display().to_string())
            .collect();
        assert!(
            offenders.is_empty(),
            "closed agent enum reintroduced in tn-ui ({offenders:?}); \
             resolve identity via AgentId + the registry descriptor instead"
        );
    }
}
