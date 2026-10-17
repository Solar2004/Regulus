//! The internal boundary: the contract every module fulfils (docs/03-modules.md §2).

use regulus_proto::Manifest;
use serde_json::Value;

/// Four methods, nothing more. `call` uses the same method names as the external socket,
/// so a module never knows whether a call came from the GUI, the CLI or another module.
pub trait Module: Send + Sync {
    fn manifest(&self) -> &Manifest;
    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError>;
    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError>;
    fn shutdown(&mut self) -> Result<(), ModuleError>;
}

/// The only view of the world a module gets.
///
/// Deliberately empty for now: `resolve`, `emit`, `subscribe` and `store`
/// (docs/03-modules.md §2.1) arrive with the first module that needs them. Growing this
/// surface is a design conversation, not a new `pub`.
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct ModuleCtx {}

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
