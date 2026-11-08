//! The module registry: loads modules, routes their methods and contains their panics.
//!
//! The core never knows a module by name. The list of modules is handed in by whoever
//! hosts the core (decision #10: one line per module, written by
//! `regulus module install`), and every lookup goes through what a manifest declares.

use std::any::Any;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use regulus_proto::{method, satisfies, Manifest, ModuleState, ModuleSummary, PROTOCOL};
use serde_json::Value;

use crate::module::{Module, ModuleCtx, ModuleError};
use crate::process::Health;

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
    manifest: Arc<Manifest>,
    module: RwLock<Box<dyn Module>>,
    state: Mutex<ModuleState>,
    /// For a local module: the project it belongs to and whether its process still runs.
    local: Option<(String, Option<Health>)>,
}

impl Entry {
    /// Its state. A local module whose process ended is broken, with why.
    fn state(&self) -> MutexGuard<'_, ModuleState> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if state.is_usable() {
            if let Some(why) = self
                .local
                .as_ref()
                .and_then(|(_, health)| health.as_ref()?.exited())
            {
                *state = ModuleState::Broken { reason: why };
            }
        }
        state
    }
}

/// A module of one project, run out of process (`[[modules.local]]`, docs/03-modules.md
/// §6), on its way into the registry.
pub struct Local {
    pub manifest: Manifest,
    pub module: Box<dyn Module>,
    /// Its state if it was not started (`Blocked`) or could not start (`Broken`); `None`
    /// for a running one, which gets its `init`.
    pub preset: Option<ModuleState>,
    pub health: Option<Health>,
}

#[derive(Default)]
pub struct Registry {
    entries: Vec<Arc<Entry>>,
    routes: HashMap<String, usize>,
    /// Modules of open projects: they come and go with their project.
    locals: RwLock<Vec<Arc<Entry>>>,
    /// What `init` gets, for the modules that come later.
    ctx: ModuleCtx,
    /// `--without`, for the modules that come later.
    without: Vec<String>,
}

/// `author/module@version` without the version.
pub fn base_id(id: &str) -> &str {
    id.split_once('@').map_or(id, |(base, _)| base)
}

fn excluded(id: &str, without: &[String]) -> bool {
    without.iter().any(|skip| skip == id || skip == base_id(id))
}

pub(crate) fn now_ms() -> u64 {
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
        ModuleState::Broken { reason } | ModuleState::Blocked { reason } => reason.clone(),
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
        let mut registry = Registry {
            ctx: ctx.clone(),
            without: without.to_vec(),
            ..Registry::default()
        };
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
                || method::NODE.contains(&m.as_str())
                || self.routes.contains_key(*m)
        }) {
            ModuleState::Broken {
                reason: format!("method `{taken}` is already served by the core or another module"),
            }
        } else {
            let ctx = ctx.for_module(&manifest.module.id);
            match catch_unwind(AssertUnwindSafe(|| module.init(&ctx))) {
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
        self.entries.push(Arc::new(Entry {
            manifest: Arc::new(manifest),
            module: RwLock::new(module),
            state: Mutex::new(state),
            local: None,
        }));
    }

    fn locals(&self) -> RwLockReadGuard<'_, Vec<Arc<Entry>>> {
        self.locals.read().unwrap_or_else(|p| p.into_inner())
    }

    /// Every entry: the linked modules in load order, then the local ones.
    fn all(&self) -> Vec<Arc<Entry>> {
        let mut all = self.entries.clone();
        all.extend(self.locals().iter().cloned());
        all
    }

    /// Whether `--without` (or `[modules] without`) leaves this module out. A local module
    /// is named `<project>/<id>`.
    pub fn excludes(&self, id: &str) -> bool {
        excluded(id, &self.without)
    }

    /// Adds the local modules of project `owner`. Each is checked as a linked one is
    /// (protocol, id) and gets its `init`; one that fails stays listed as broken. A local
    /// module cannot serve `methods`: the socket's methods are fixed when the core starts,
    /// so it is reached through its nodes.
    pub fn add_local(&self, owner: &str, modules: Vec<Local>) {
        for local in modules {
            let Local {
                manifest,
                mut module,
                preset,
                health,
            } = local;
            let id = manifest.module.id.clone();
            let clash = self.is_installed(&id);
            let state = if let Some(preset) = preset {
                preset
            } else if let Some(requires) = manifest
                .module
                .requires_proto
                .as_deref()
                .filter(|r| !satisfies(r, PROTOCOL))
            {
                ModuleState::Incompatible {
                    requires: requires.into(),
                    have: PROTOCOL.into(),
                }
            } else if clash {
                ModuleState::Broken {
                    reason: format!("a module `{}` is already loaded", base_id(&id)),
                }
            } else if !manifest.module.methods.is_empty() {
                ModuleState::Broken {
                    reason: "a local module cannot serve `methods`; reach it through its nodes"
                        .into(),
                }
            } else {
                let ctx = self.ctx.for_module(&id);
                match catch_unwind(AssertUnwindSafe(|| module.init(&ctx))) {
                    Ok(Ok(())) => ModuleState::Idle { last_used: None },
                    Ok(Err(error)) => ModuleState::Broken {
                        reason: format!("init failed: {error}"),
                    },
                    Err(payload) => ModuleState::Broken {
                        reason: format!("init panicked: {}", panic_reason(payload)),
                    },
                }
            };
            let entry = Arc::new(Entry {
                manifest: Arc::new(manifest),
                module: RwLock::new(module),
                state: Mutex::new(state),
                local: Some((owner.to_string(), health)),
            });
            if clash {
                // Listed, never reached: lookups find the one loaded first.
                eprintln!("regulus: local module {id} clashes with a loaded module");
            }
            self.locals
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .push(entry);
        }
    }

    /// Takes out the local modules of project `owner` and shuts each one down, whatever
    /// its state: that is what ends its process. Returns their ids.
    pub fn remove_local(&self, owner: &str) -> Vec<String> {
        let removed: Vec<Arc<Entry>> = {
            let mut locals = self.locals.write().unwrap_or_else(|p| p.into_inner());
            let (gone, kept) = locals
                .drain(..)
                .partition(|e| e.local.as_ref().is_some_and(|(o, _)| o == owner));
            *locals = kept;
            gone
        };
        for entry in &removed {
            match entry.local.as_ref().and_then(|(_, health)| health.as_ref()) {
                // Without the module's lock: a call that hangs must not hold the close.
                Some(health) => health.stop(),
                None => Self::shut(entry),
            }
        }
        removed
            .iter()
            .map(|e| e.manifest.module.id.clone())
            .collect()
    }

    /// The entry of a module by id; any version if the exact one is not loaded.
    fn find(&self, id: &str) -> Option<Arc<Entry>> {
        let all = self.all();
        all.iter()
            .find(|e| e.manifest.module.id == id)
            .or_else(|| {
                all.iter()
                    .find(|e| base_id(&e.manifest.module.id) == base_id(id))
            })
            .cloned()
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
        let entry = self.find(module_id).ok_or(CallError::NotFound)?;
        Self::invoke(&entry, name, params)
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

    /// The manifest of a loaded module, by id (any version if the exact one is not
    /// loaded), whatever its state.
    pub fn manifest(&self, id: &str) -> Option<Arc<Manifest>> {
        self.find(id).map(|e| e.manifest.clone())
    }

    /// The manifests of every loaded module, whatever its state, in load order; local
    /// modules last.
    pub fn manifests(&self) -> Vec<Arc<Manifest>> {
        self.all().iter().map(|e| e.manifest.clone()).collect()
    }

    /// Whether a module with this id (any version) is loaded, whatever its state.
    pub fn is_installed(&self, id: &str) -> bool {
        let wanted = base_id(id);
        self.all()
            .iter()
            .any(|entry| base_id(&entry.manifest.module.id) == wanted)
    }

    /// The ids (`author/module@version`) of every loaded module, whatever its state.
    pub fn ids(&self) -> Vec<String> {
        self.all()
            .iter()
            .map(|entry| entry.manifest.module.id.clone())
            .collect()
    }

    pub fn summaries(&self) -> Vec<ModuleSummary> {
        self.all()
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

    /// Shuts down every usable module, and every local one (that ends its process). A
    /// failing or panicking shutdown is reported, not propagated.
    pub fn shutdown(&self) {
        for entry in &self.entries {
            if entry.state().is_usable() {
                Self::shut(entry);
            }
        }
        let owners: Vec<String> = self
            .locals()
            .iter()
            .filter_map(|e| Some(e.local.as_ref()?.0.clone()))
            .collect();
        for owner in owners {
            self.remove_local(&owner);
        }
    }

    fn shut(entry: &Entry) {
        let Ok(mut module) = entry.module.write() else {
            return;
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
