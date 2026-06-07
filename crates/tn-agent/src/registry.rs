//! [`AgentRegistry`] — the set of agents this Tn instance knows about, built by
//! the app at startup (tn-app wiring). The UI resolves identity / presentation /
//! usage through it and never names a concrete agent. An **empty** registry is a
//! pure shell host (the P6 end state, before agents are registered back).

use std::sync::Arc;

use crate::{AgentAdapter, AgentDescriptor, AgentId, GenericAdapter};

/// Holds the registered agent adapters and resolves agents by id or by command.
#[derive(Clone, Default)]
pub struct AgentRegistry {
    adapters: Vec<Arc<dyn AgentAdapter>>,
}

impl AgentRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register an agent adapter (built-in Claude/Codex, or an external one).
    pub fn register(&mut self, adapter: Arc<dyn AgentAdapter>) {
        self.adapters.push(adapter);
    }

    /// Builder form of [`register`](Self::register).
    pub fn with(mut self, adapter: Arc<dyn AgentAdapter>) -> Self {
        self.register(adapter);
        self
    }

    /// Register a config-declared agent (`[[agents]]`) as a no-telemetry
    /// [`GenericAdapter`]. A **no-op if the id is already registered** — a built-in
    /// (telemetry-carrying) adapter wins over a config manifest of the same id, so
    /// a user manifest can't downgrade Claude/Codex to "no usage".
    pub fn register_manifest(&mut self, manifest: &tn_config::AgentManifest) {
        let id = AgentId::new(manifest.id.clone());
        if self.get(&id).is_some() {
            return;
        }
        self.register(Arc::new(GenericAdapter::new(
            AgentDescriptor::from_manifest(manifest),
        )));
    }

    /// The descriptor for `id`, if registered.
    pub fn get(&self, id: &AgentId) -> Option<&AgentDescriptor> {
        self.adapter(id).map(|a| a.descriptor())
    }

    /// The adapter for `id`, if registered (for usage parsing / session lookup).
    pub fn adapter(&self, id: &AgentId) -> Option<&Arc<dyn AgentAdapter>> {
        self.adapters.iter().find(|a| &a.descriptor().id == id)
    }

    /// Classify a launch command into a registered agent id (the launch-intent
    /// signal). First registered adapter whose alias matches wins; `None` for a
    /// plain shell / an unregistered command.
    pub fn match_command(&self, command: &str) -> Option<AgentId> {
        self.adapters
            .iter()
            .find(|a| a.descriptor().matches_command(command))
            .map(|a| a.descriptor().id.clone())
    }

    /// All registered descriptors (launch surfaces enumerate these).
    pub fn descriptors(&self) -> impl Iterator<Item = &AgentDescriptor> {
        self.adapters.iter().map(|a| a.descriptor())
    }

    /// The descriptor for `id` if registered, else a synthesized
    /// [`AgentDescriptor::generic`] using `fallback_label` — so a config-declared
    /// agent with no adapter still has presentation to render.
    pub fn descriptor_or_generic(&self, id: &AgentId, fallback_label: &str) -> AgentDescriptor {
        self.get(id)
            .cloned()
            .unwrap_or_else(|| AgentDescriptor::generic(id.clone(), fallback_label))
    }

    pub fn is_empty(&self) -> bool {
        self.adapters.is_empty()
    }
}
