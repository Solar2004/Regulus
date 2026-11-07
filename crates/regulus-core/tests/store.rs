//! The store (docs/02-architecture.md §6): each module's own key-value space, and what
//! survives a restart of the core.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regulus_core::config::{Resume, StoreConfig};
use regulus_core::{Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore, Storage, Store};
use regulus_proto::{method, Manifest, ModuleInfo, NodeCall, Profile};
use serde_json::{json, Value};

type Log = Arc<Mutex<Vec<String>>>;

/// On start, writes `config.value` under `config.key` in its project space and in its
/// global space, and saves `config.checkpoint` unless it is being resumed. `<prefix>.peek`
/// lists every key it can see in its global space.
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
                if let Some(say) = call.node.config.get("say") {
                    self.ctx.emit(&me, "out", say.clone())?;
                }
                if let Some(state) = call.node.config.get("checkpoint") {
                    if call.args.is_null() {
                        self.ctx.checkpoint(&me, state.clone())?;
                    }
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

fn start(core: &Core) -> Value {
    core.dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap()
}

fn list(core: &Core) -> Vec<Value> {
    serde_json::from_value(core.dispatch(method::EXECUTION_LIST, json!({})).unwrap()).unwrap()
}

fn one_node(config: Value) -> Profile {
    profile(vec![
        json!({ "id": "x", "type": "k", "module_id": "t/a@1.0.0", "config": config }),
    ])
}

fn with_resume(resume: Resume) -> StoreConfig {
    StoreConfig {
        resume,
        ..StoreConfig::default()
    }
}

#[test]
fn an_execution_without_checkpoint_comes_back_interrupted() {
    let home = temp_home("interrupted");
    let log = Log::default();
    let core = core_on(&home, &StoreConfig::default(), one_node(json!({})), &log);
    assert_eq!(start(&core)["id"], "exec-1");
    // The core goes down without stopping anything, as on a crash.
    drop(core);

    let core = core_on(&home, &StoreConfig::default(), one_node(json!({})), &log);
    let listed = list(&core);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["state"]["status"], "running", "not recovered yet");
    let report = core.recover();
    assert_eq!(report.len(), 1, "{report:?}");
    assert!(report[0].starts_with("exec-1 interrupted"), "{report:?}");
    let listed = list(&core);
    assert_eq!(listed[0]["id"], "exec-1");
    assert_eq!(listed[0]["state"]["status"], "interrupted");
    // Numbers go on, and stopping what is not live answers it as it is.
    assert_eq!(start(&core)["id"], "exec-2");
    let stopped = core
        .dispatch(method::EXECUTION_STOP, json!({ "id": "exec-1" }))
        .unwrap();
    assert_eq!(stopped["state"]["status"], "interrupted");
    assert!(core.recover().is_empty(), "recovery happens once");
}

#[test]
fn an_execution_with_checkpoint_resumes_under_the_same_id() {
    let home = temp_home("resumed");
    let log = Log::default();
    let config = json!({ "checkpoint": { "step": 3 } });
    let core = core_on(
        &home,
        &StoreConfig::default(),
        one_node(config.clone()),
        &log,
    );
    start(&core);
    // A clean shutdown stops the nodes but leaves the execution running in the store.
    core.shutdown();
    drop(core);

    log.lock().unwrap().clear();
    let core = core_on(&home, &StoreConfig::default(), one_node(config), &log);
    let report = core.recover();
    assert_eq!(report, ["exec-1 resumed (1 checkpoints)"]);
    let listed = list(&core);
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["state"]["status"], "running");
    assert_eq!(
        log.lock().unwrap().as_slice(),
        [r#"node/start x {"checkpoint":{"step":3}}"#]
    );
}

#[test]
fn resume_never_and_always_override_the_checkpoints() {
    let log = Log::default();
    let home = temp_home("never");
    let config = json!({ "checkpoint": 1 });
    let core = core_on(
        &home,
        &with_resume(Resume::Never),
        one_node(config.clone()),
        &log,
    );
    start(&core);
    drop(core);
    let core = core_on(&home, &with_resume(Resume::Never), one_node(config), &log);
    assert!(core.recover()[0].starts_with("exec-1 interrupted"));

    let home = temp_home("always");
    let core = core_on(
        &home,
        &with_resume(Resume::Always),
        one_node(json!({})),
        &log,
    );
    start(&core);
    drop(core);
    log.lock().unwrap().clear();
    let core = core_on(
        &home,
        &with_resume(Resume::Always),
        one_node(json!({})),
        &log,
    );
    assert_eq!(core.recover(), ["exec-1 resumed (0 checkpoints)"]);
    assert_eq!(log.lock().unwrap().as_slice(), ["node/start x null"]);
}

#[test]
fn a_stopped_execution_stays_stopped_across_restarts() {
    let home = temp_home("stopped");
    let log = Log::default();
    let core = core_on(&home, &StoreConfig::default(), one_node(json!({})), &log);
    start(&core);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": "exec-1" }))
        .unwrap();
    drop(core);
    let core = core_on(&home, &StoreConfig::default(), one_node(json!({})), &log);
    assert!(core.recover().is_empty());
    assert_eq!(list(&core)[0]["state"]["status"], "stopped");
}

#[test]
fn bus_events_outlive_the_core_and_their_seq_goes_on() {
    let home = temp_home("events");
    let log = Log::default();
    let speaker = || {
        profile(vec![json!({
            "id": "x", "type": "k", "module_id": "t/a@1.0.0", "config": { "say": "hi" },
            "ports": [{ "id": "out", "capability": "text@1", "direction": "output", "kind": "then" }]
        })])
    };
    let events = |core: &Core, since: u64| -> Vec<(u64, String)> {
        let page = core
            .dispatch(method::EVENT_LIST, json!({ "since": since }))
            .unwrap();
        page["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| {
                (
                    e["seq"].as_u64().unwrap(),
                    e["execution_id"].as_str().unwrap().into(),
                )
            })
            .collect()
    };
    let core = core_on(&home, &StoreConfig::default(), speaker(), &log);
    start(&core);
    assert_eq!(events(&core, 0), [(1, "exec-1".to_string())]);
    drop(core);

    let core = core_on(&home, &StoreConfig::default(), speaker(), &log);
    assert_eq!(
        events(&core, 0),
        [(1, "exec-1".to_string())],
        "read from the store"
    );
    start(&core);
    assert_eq!(
        events(&core, 0),
        [(1, "exec-1".to_string()), (2, "exec-2".to_string())]
    );
    assert_eq!(
        events(&core, 1),
        [(2, "exec-2".to_string())],
        "read from memory"
    );
}
