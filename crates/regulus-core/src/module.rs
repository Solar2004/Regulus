//! The internal boundary: the contract every module fulfils (docs/03-modules.md §2).

use std::path::PathBuf;
use std::sync::Weak;

use regulus_proto::Manifest;
use serde_json::Value;

use crate::service::Core;
use crate::store::ModuleStore;

/// Four methods, nothing more. `call` uses the same method names as the external socket,
/// so a module never knows whether a call came from the GUI, the CLI or another module.
pub trait Module: Send + Sync {
    fn manifest(&self) -> &Manifest;
    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError>;
    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError>;
    fn shutdown(&mut self) -> Result<(), ModuleError>;
}

/// A node inside a running execution.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NodeRef {
    pub execution_id: String,
    pub node_id: String,
}

/// The node at the other end of a "uses" wire. Opaque on purpose: a module reaches it
/// through [`ModuleCtx::call`], never by the name of the module behind it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    pub(crate) node: NodeRef,
    pub(crate) module_id: String,
}

impl Endpoint {
    pub fn node(&self) -> &NodeRef {
        &self.node
    }
}

/// The only view of the world a module gets (docs/03-modules.md §2.1). Cheap to clone:
/// a module keeps a copy from `init` to use later.
///
/// There is no `subscribe`: the "then" wires say who receives what. Growing this surface is a design conversation, not
/// a new `pub`.
#[derive(Debug, Clone, Default)]
pub struct ModuleCtx {
    core: Weak<Core>,
    /// The module this context was handed to: what scopes its directories.
    module_id: String,
}

impl ModuleCtx {
    pub(crate) fn new(core: Weak<Core>) -> Self {
        Self {
            core,
            module_id: String::new(),
        }
    }

    /// The same core, seen by one module.
    pub(crate) fn for_module(&self, module_id: &str) -> Self {
        Self {
            core: self.core.clone(),
            module_id: module_id.into(),
        }
    }

    fn core(&self) -> Result<std::sync::Arc<Core>, ModuleError> {
        self.core
            .upgrade()
            .ok_or_else(|| ModuleError::Failed("the core is not running".into()))
    }

    /// What is wired over "uses" into `port` of `node`, in that node's execution. Empty
    /// if nothing is wired or the execution is gone.
    pub fn resolve(&self, node: &NodeRef, port: &str) -> Vec<Endpoint> {
        self.core()
            .map(|core| core.resolve(node, port))
            .unwrap_or_default()
    }

    /// Calls the node at the other end of a wire. It receives a `NodeCall` with its own
    /// configuration and `args`.
    pub fn call(&self, to: &Endpoint, method: &str, args: Value) -> Result<Value, ModuleError> {
        self.core()?.call_node(to, method, args)
    }

    /// Puts `value` on the "then" output `port` of `node`, one of this module's nodes. The
    /// core delivers it, in order and without blocking the caller, as `node/event` to
    /// every input wired to that port with the same capability. An emission made while
    /// handling an event continues that chain; past `bus.max_depth` it is dropped and
    /// reported (docs/11-composition.md §4). The chain is tracked per thread: a module
    /// that hands the event to a thread of its own and emits from there starts a new
    /// chain at depth 0, out of reach of that cut. `bus.max_queue` still bounds it.
    pub fn emit(&self, node: &NodeRef, port: &str, value: Value) -> Result<(), ModuleError> {
        self.core()?.emit(&self.module_id, node, port, value)
    }

    /// A directory of this module's own for `node`, created if missing. It belongs to the
    /// node's project, workflow and id, not to the execution, so it outlives executions
    /// (docs/03-modules.md §2.1).
    pub fn node_dir(&self, node: &NodeRef) -> Result<PathBuf, ModuleError> {
        self.core()?.node_dir(&self.module_id, node)
    }

    /// This module's own key-value space in the store, keyed by its id without version: it
    /// cannot name another module's (docs/02-architecture.md §6). With a node, the space
    /// lives in that node's project database; without one, in the global database.
    pub fn store(&self, node: Option<&NodeRef>) -> Result<ModuleStore, ModuleError> {
        self.core()?.module_store(&self.module_id, node)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ModuleError {
    #[error("unknown method `{0}`")]
    UnknownMethod(String),
    #[error("invalid params: {0}")]
    InvalidParams(String),
    #[error("{0}")]
    Failed(String),
}

/// Parses a `regulus-module.toml`. Modules embed theirs with `include_str!`.
pub fn parse_manifest(source: &str) -> Result<Manifest, toml::de::Error> {
    toml::from_str(source)
}
