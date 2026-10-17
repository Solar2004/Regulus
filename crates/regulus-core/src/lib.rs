//! The Regulus base. It has no functions of its own: it does not know what an agent, a
//! model or a skill is. It loads modules, keeps the pieces and their wires, and answers
//! clients over JSON-RPC (docs/plan.md §1).

pub mod config;
pub mod graph;
pub mod module;
pub mod profiles;
pub mod registry;
pub mod server;
pub mod service;

use std::fs;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

pub use config::Config;
pub use module::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
pub use profiles::ProfileStore;
pub use registry::Registry;
pub use service::{Core, RpcError};

/// Builds a core from its configuration and the modules its host links in. Returns the
/// warnings found while loading, which never stop the start.
pub fn assemble(config: &Config, modules: Vec<Box<dyn Module>>) -> (Arc<Core>, Vec<String>) {
    let (profiles, warnings) = ProfileStore::load(&config.profiles.paths);
    (
        Core::build(modules, &config.modules.without, profiles),
        warnings,
    )
}

/// Starts serving: creates the home directory, the token and the default profiles
/// directory on the first run, binds, and writes the chosen port to `<home>/port` so
/// clients can find the core.
pub async fn serve(config: &Config, core: Arc<Core>) -> io::Result<server::Running> {
    fs::create_dir_all(config.default_profiles_dir())?;
    let token = server::load_or_create_token(&config.token_path())?;
    let running = server::start(core, &config.server, &token).await?;
    write_port(config, running.addr)?;
    Ok(running)
}

fn write_port(config: &Config, addr: SocketAddr) -> io::Result<()> {
    fs::write(config.port_path(), addr.port().to_string())
}
