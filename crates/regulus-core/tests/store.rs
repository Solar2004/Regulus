//! The store (docs/02-architecture.md §6): each module's own key-value space, and what
//! survives a restart of the core.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regulus_core::config::StoreConfig;
use regulus_core::{Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore, Storage, Store};
use regulus_proto::{method, Manifest, ModuleInfo, NodeCall, Profile};
use serde_json::{json, Value};

type Log = Arc<Mutex<Vec<String>>>;

/// On start, writes `config.value` under `config.key` in its project space and in its
/// global space. `<prefix>.peek` lists every key it can see in both.
struct Keeper {
    manifest: Manifest,
    ctx: ModuleCtx,
    log: Log,
}

impl Module for Keeper {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        if name.ends_with(".peek") {
            let keys = |space: Vec<(String, Vec<u8>)>| -> Vec<String> {
                space.into_iter().map(|(k, _)| k).collect()
            };
            let global = keys(self.ctx.store(None)?.list("")?);
            return Ok(json!({ "global": global }));
        }
        let call: NodeCall = serde_json::from_value(params)
            .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        self.log
            .lock()
            .unwrap()
            .push(format!("{name} {} {}", call.node.id, call.args));
        match name {
            method::NODE_START => {
                if let Some(key) = call.node.config.get("key").and_then(Value::as_str) {
                    let value = &call.node.config["value"];
                    self.ctx.store(Some(&me))?.set_json(key, value)?;
                    self.ctx.store(None)?.set_json(key, value)?;
                }
                Ok(Value::Null)
            }
            method::NODE_STOP => Ok(Value::Null),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn keeper(id: &str, prefix: &str, log: &Log) -> Box<dyn Module> {
    Box::new(Keeper {
        manifest: Manifest {
            module: ModuleInfo {
                id: id.into(),
                name: id.into(),
                requires_proto: None,
                methods: vec![format!("{prefix}.peek")],
                workflow_id: None,
                origin: None,
            },
            nodes: vec![],
        },
        ctx: ModuleCtx::default(),
        log: log.clone(),
    })
}

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-store-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn profile(nodes: Vec<Value>) -> Profile {
    serde_json::from_value(json!({
        "id": "p", "project_id": "proj", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "nodes": nodes, "wires": [] }]
    }))
    .unwrap()
}

fn core_on(home: &Path, config: &StoreConfig, profile: Profile, log: &Log) -> Arc<Core> {
    let store = Store::open(&home.join("store"), config).unwrap();
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules = vec![keeper("t/a@1.0.0", "a", log), keeper("t/b@2.0.0", "b", log)];
    let storage = Storage {
        store: Some(Arc::new(store)),
        ..Storage::default()
    };
    Core::build_with(modules, &[], profiles, storage)
}

#[test]
fn each_module_sees_only_its_own_space() {
    let home = temp_home("spaces");
    let log = Log::default();
    let nodes = vec![
        json!({ "id": "x", "type": "k", "module_id": "t/a@1.0.0",
                "config": { "key": "shared", "value": "from a" } }),
        json!({ "id": "y", "type": "k", "module_id": "t/b@2.0.0",
                "config": { "key": "only-b", "value": "from b" } }),
    ];
    let core = core_on(&home, &StoreConfig::default(), profile(nodes), &log);
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    let a = core.dispatch("a.peek", Value::Null).unwrap();
    let b = core.dispatch("b.peek", Value::Null).unwrap();
    assert_eq!(a["global"], json!(["shared"]));
    assert_eq!(b["global"], json!(["only-b"]));
    // The project space is the project's database, not the global one.
    assert!(home.join("store/projects/proj.db").is_file());
}
