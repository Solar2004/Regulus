//! The module registry: loads modules, routes their methods and contains their panics.
//!
//! The core never knows a module by name. The list of modules is handed in by whoever
//! hosts the core (decision #10: one line per module, written by
//! `regulus module install`), and every lookup goes through what a manifest declares.

use std::any::Any;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Mutex, MutexGuard, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use regulus_proto::{method, satisfies, Manifest, ModuleState, ModuleSummary, PROTOCOL};
use serde_json::Value;

use crate::module::{Module, ModuleCtx, ModuleError};

#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error("method not found")]
    NotFound,
    #[error("module `{module}` is unavailable: {reason}")]
    Unavailable { module: String, reason: String },
    #[error(transparent)]
    Module(#[from] ModuleError),
    #[error("module `{0}` panicked; it is now broken")]
    Panicked(String),
}

struct Entry {
    manifest: Manifest,
    module: RwLock<Box<dyn Module>>,
    state: Mutex<ModuleState>,
}

impl Entry {
    fn state(&self) -> MutexGuard<'_, ModuleState> {
        self.state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

#[derive(Default)]
pub struct Registry {
    entries: Vec<Entry>,
    routes: HashMap<String, usize>,
}

/// `author/module@version` without the version.
pub fn base_id(id: &str) -> &str {
    id.split_once('@').map_or(id, |(base, _)| base)
}

fn excluded(id: &str, without: &[String]) -> bool {
    without.iter().any(|skip| skip == id || skip == base_id(id))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn panic_reason(payload: Box<dyn Any + Send>) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "panic without message".into())
}

fn describe(state: &ModuleState) -> String {
    match state {
        ModuleState::Disabled { missing } => {
            format!("missing required inputs: {}", missing.join(", "))
        }
        ModuleState::Broken { reason } => reason.clone(),
        ModuleState::Incompatible { requires, have } => {
            format!("requires proto {requires}, core has {have}")
        }
        ModuleState::Active { .. } | ModuleState::Idle { .. } => "available".into(),
    }
}

impl Registry {
    /// Loads every module except those listed in `without` (by id, with or without the
    /// version). A module that is incompatible, conflicts or fails in `init` stays listed
    /// with its state; it never takes the host down.
    pub fn load(modules: Vec<Box<dyn Module>>, without: &[String]) -> Self {
        Self::load_with(modules, without, &ModuleCtx::default())
    }

    /// Like [`Registry::load`], handing `ctx` to every module's `init`.
    pub fn load_with(modules: Vec<Box<dyn Module>>, without: &[String], ctx: &ModuleCtx) -> Self {
        let mut registry = Registry::default();
        for module in modules {
            let manifest = match catch_unwind(AssertUnwindSafe(|| module.manifest().clone())) {
                Ok(manifest) => manifest,
                Err(payload) => {
                    eprintln!(
                        "regulus: skipped a module whose manifest panicked: {}",
                        panic_reason(payload)
                    );
                    continue;
                }
            };
            if excluded(&manifest.module.id, without) {
                continue;
            }
            registry.admit(manifest, module, ctx);
        }
        registry
    }

    fn admit(&mut self, manifest: Manifest, mut module: Box<dyn Module>, ctx: &ModuleCtx) {
        let index = self.entries.len();
        let state = if let Some(requires) = manifest
            .module
            .requires_proto
            .as_deref()
            .filter(|r| !satisfies(r, PROTOCOL))
        {
            ModuleState::Incompatible {
                requires: requires.into(),
                have: PROTOCOL.into(),
            }
        } else if let Some(taken) = manifest.module.methods.iter().find(|m| {
            method::CORE.contains(&m.as_str())
                || [method::NODE_START, method::NODE_STOP].contains(&m.as_str())
                || self.routes.contains_key(*m)
        }) {
            ModuleState::Broken {
                reason: format!("method `{taken}` is already served by the core or another module"),
            }
        } else {
            match catch_unwind(AssertUnwindSafe(|| module.init(ctx))) {
                Ok(Ok(())) => {
                    for name in &manifest.module.methods {
                        self.routes.insert(name.clone(), index);
                    }
                    ModuleState::Idle { last_used: None }
                }
                Ok(Err(error)) => ModuleState::Broken {
                    reason: format!("init failed: {error}"),
                },
                Err(payload) => ModuleState::Broken {
                    reason: format!("init panicked: {}", panic_reason(payload)),
                },
            }
        };
        self.entries.push(Entry {
            manifest,
            module: RwLock::new(module),
            state: Mutex::new(state),
        });
    }

    /// Calls the module that declared `name`, containing any panic at this edge.
    pub fn call(&self, name: &str, params: Value) -> Result<Value, CallError> {
        let entry = &self.entries[*self.routes.get(name).ok_or(CallError::NotFound)?];
        Self::invoke(entry, name, params)
    }

    /// Calls a method on one module chosen by id (any version if the exact one is not
    /// loaded): how the core reaches the owner of a node. The method does not need to be
    /// declared in `methods`.
    pub fn call_module(
        &self,
        module_id: &str,
        name: &str,
        params: Value,
    ) -> Result<Value, CallError> {
        let entry = self
            .entries
            .iter()
            .find(|e| e.manifest.module.id == module_id)
            .or_else(|| {
                self.entries
                    .iter()
                    .find(|e| base_id(&e.manifest.module.id) == base_id(module_id))
            })
            .ok_or(CallError::NotFound)?;
        Self::invoke(entry, name, params)
    }

    fn invoke(entry: &Entry, name: &str, params: Value) -> Result<Value, CallError> {
        let id = entry.manifest.module.id.clone();
        let unavailable = |state: &ModuleState| CallError::Unavailable {
            module: id.clone(),
            reason: describe(state),
        };
        {
            let state = entry.state();
            if !state.is_usable() {
                return Err(unavailable(&state));
            }
        }
        let Ok(module) = entry.module.read() else {
            return Err(unavailable(&entry.state()));
        };
        let result = catch_unwind(AssertUnwindSafe(|| module.call(name, params)));
        drop(module);
        let mut state = entry.state();
        match result {
            Ok(result) => {
                if !matches!(*state, ModuleState::Active { .. }) {
                    *state = ModuleState::Active { since: now_ms() };
                }
                Ok(result?)
            }
            Err(payload) => {
                *state = ModuleState::Broken {
                    reason: format!("panicked in `{name}`: {}", panic_reason(payload)),
                };
                Err(CallError::Panicked(id))
            }
        }
    }

    /// Every method routed to a module.
    pub fn methods(&self) -> impl Iterator<Item = &str> {
        self.routes.keys().map(String::as_str)
    }

    /// Whether a module with this id (any version) is loaded, whatever its state.
    pub fn is_installed(&self, id: &str) -> bool {
        let wanted = base_id(id);
        self.entries
            .iter()
            .any(|entry| base_id(&entry.manifest.module.id) == wanted)
    }

    pub fn summaries(&self) -> Vec<ModuleSummary> {
        self.entries
            .iter()
            .map(|entry| ModuleSummary {
                id: entry.manifest.module.id.clone(),
                name: entry.manifest.module.name.clone(),
                node_types: entry
                    .manifest
                    .nodes
                    .iter()
                    .map(|node| node.node_type.clone())
                    .collect(),
                workflow_id: entry.manifest.module.workflow_id.clone(),
                state: entry.state().clone(),
            })
            .collect()
    }

    /// Shuts down every usable module. A failing or panicking shutdown is reported, not
    /// propagated.
    pub fn shutdown(&self) {
        for entry in &self.entries {
            if !entry.state().is_usable() {
                continue;
            }
            let Ok(mut module) = entry.module.write() else {
                continue;
            };
            match catch_unwind(AssertUnwindSafe(|| module.shutdown())) {
                Ok(Ok(())) => {}
                Ok(Err(error)) => eprintln!(
                    "regulus: {} failed to shut down: {error}",
                    entry.manifest.module.id
                ),
                Err(payload) => eprintln!(
                    "regulus: {} panicked on shutdown: {}",
                    entry.manifest.module.id,
                    panic_reason(payload)
                ),
            }
        }
    }
}
