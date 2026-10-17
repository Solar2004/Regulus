//! The Regulus base. It has no functions of its own: it does not know what an agent, a
//! model or a skill is. It loads modules, keeps the pieces and their wires, and answers
//! clients over JSON-RPC (docs/plan.md §1).

pub mod config;
pub mod graph;
pub mod module;
pub mod profiles;
pub mod registry;
pub mod service;

pub use config::Config;
pub use module::{parse_manifest, Module, ModuleCtx, ModuleError};
pub use profiles::ProfileStore;
pub use registry::Registry;
pub use service::{Core, RpcError};

/// Builds a core from its configuration and the modules its host links in. Returns the
/// warnings found while loading, which never stop the start.
pub fn assemble(config: &Config, modules: Vec<Box<dyn Module>>) -> (Core, Vec<String>) {
    let registry = Registry::load(modules, &config.modules.without);
    let (profiles, warnings) = ProfileStore::load(&config.profiles.paths);
    (Core::new(registry, profiles), warnings)
}
