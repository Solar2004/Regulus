//! The Regulus base. It has no functions of its own: it does not know what an agent, a
//! model or a skill is. It loads modules, keeps the pieces and their wires, and answers
//! clients over JSON-RPC (docs/plan.md §1).

mod bus;
pub mod config;
pub mod graph;
pub mod local;
pub mod module;
pub mod process;
pub mod profiles;
pub mod projects;
pub mod registry;
pub mod secrets;
pub mod server;
pub mod service;
pub mod store;
pub mod templates;

use std::fs;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;

pub use config::{BusConfig, CallConfig, Config, HooksConfig, LocalModulesConfig};
pub use module::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
pub use profiles::ProfileStore;
pub use registry::Registry;
pub use secrets::Secrets;
pub use service::{Core, RpcError, Storage};
pub use store::{ModuleStore, Store, StoreError, StoredExecution};

/// Builds a core from its configuration and the modules its host links in. Returns the
/// warnings found while loading, which never stop the start.
///
/// A store that cannot open is a warning too: the core then keeps executions and bus events
/// in memory only, as it did before the store existed.
pub fn assemble(config: &Config, modules: Vec<Box<dyn Module>>) -> (Arc<Core>, Vec<String>) {
    let mut warnings = config.warnings.clone();
    let (mut profiles, found) = ProfileStore::load(&config.profiles.paths);
    warnings.extend(found);
    // Global profiles first: on a clash of ids, the first one stays, as between files.
    let (mut projects, found) = projects::Projects::load(&config.projects.paths);
    warnings.extend(found);
    let loaded: Vec<(String, Vec<String>)> = projects
        .iter()
        .map(|p| (p.id.clone(), profiles.load_project(&p.id, &p.profiles)))
        .collect();
    for (id, found) in loaded {
        projects.note(&id, found);
    }
    let store = match Store::open(&config.store_dir(), &config.store) {
        Ok(store) => {
            warnings.extend(store.warnings().iter().cloned());
            Some(Arc::new(store))
        }
        Err(error) => {
            warnings.push(format!(
                "store unavailable, keeping state in memory: {error}"
            ));
            None
        }
    };
    let core = Core::build_with_hooks(
        modules,
        &config.modules.without,
        profiles,
        Storage {
            secrets: config.secrets(),
            data_dir: Some(config.data_dir()),
            store,
        },
        config.bus.clone(),
        config.call.clone(),
        config.hooks.clone(),
    );
    core.set_template_places(config.template_places());
    core.set_local_modules(config.local_modules.clone());
    core.set_client(config.client.written.clone(), config.client_warnings());
    for project in projects.iter() {
        let found = project.warnings(|id| core.registry().is_installed(id));
        warnings.extend(found.iter().map(|w| format!("project {}: {w}", project.id)));
    }
    core.set_projects(
        projects,
        service::ProjectHome {
            config: config.config_path(),
            home: config.home.clone(),
        },
    );
    core.start_local_modules();
    (core, warnings)
}

/// The thread that recovers what the previous core left running.
const RECOVER_THREAD: &str = "regulus-recover";

/// Starts serving: creates the home directory, the token and the default profiles
/// directory on the first run, binds, and writes the chosen port to `<home>/port` so
/// clients can find the core. Then recovers, in the background, the executions the
/// previous core left running (`Core::recover`).
pub async fn serve(config: &Config, core: Arc<Core>) -> io::Result<server::Running> {
    fs::create_dir_all(config.default_profiles_dir())?;
    let token = server::load_or_create_token(&config.token_path())?;
    let running = server::start(core.clone(), &config.server, &token).await?;
    write_port(config, running.addr)?;
    // Resuming can start agents and take seconds: it happens behind the open socket.
    std::thread::Builder::new()
        .name(RECOVER_THREAD.into())
        .spawn(move || {
            for line in core.recover() {
                eprintln!("regulus: {line}");
            }
        })?;
    Ok(running)
}

fn write_port(config: &Config, addr: SocketAddr) -> io::Result<()> {
    fs::write(config.port_path(), addr.port().to_string())
}
