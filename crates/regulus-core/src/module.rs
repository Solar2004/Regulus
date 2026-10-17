//! The internal boundary: the contract every module fulfils (docs/03-modules.md §2).

use std::sync::Weak;

use regulus_proto::Manifest;
use serde_json::Value;

use crate::service::Core;

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
/// `emit`, `subscribe` and `store` arrive with the first module that needs them. Growing
/// this surface is a design conversation, not a new `pub`.
#[derive(Debug, Clone, Default)]
pub struct ModuleCtx {
    core: Weak<Core>,
}

impl ModuleCtx {
    pub(crate) fn new(core: Weak<Core>) -> Self {
        Self { core }
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
