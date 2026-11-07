//! The store (docs/02-architecture.md §6): each module's own key-value space, and what
//! survives a restart of the core.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regulus_core::config::{Resume, StoreConfig};
use regulus_core::{
    BusConfig, Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore, Storage, Store,
};
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
        if name.ends_with(".reach") {
            // Asks for the project space of any node it is told about.
            let node = NodeRef {
                execution_id: params["execution_id"].as_str().unwrap_or_default().into(),
                node_id: params["node_id"].as_str().unwrap_or_default().into(),
            };
            self.ctx.store(Some(&node))?.list("")?;
            return Ok(Value::Null);
        }
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
                methods: vec![format!("{prefix}.peek"), format!("{prefix}.reach")],
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
    core_with_bus(home, config, BusConfig::default(), profile, log)
}

fn core_with_bus(
    home: &Path,
    config: &StoreConfig,
    bus: BusConfig,
    profile: Profile,
    log: &Log,
) -> Arc<Core> {
    let store = Store::open(&home.join("store"), config).unwrap();
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules = vec![keeper("t/a@1.0.0", "a", log), keeper("t/b@2.0.0", "b", log)];
    let storage = Storage {
        store: Some(Arc::new(store)),
        ..Storage::default()
    };
    Core::build_with_bus(modules, &[], profiles, storage, bus)
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

#[test]
fn a_busy_database_does_not_hold_up_the_bus() {
    let home = temp_home("busy");
    let log = Log::default();
    let config = StoreConfig {
        busy_timeout_ms: 2000,
        ..StoreConfig::default()
    };
    let speaker = profile(vec![json!({
        "id": "x", "type": "k", "module_id": "t/a@1.0.0", "config": { "say": "hi" },
        "ports": [{ "id": "out", "capability": "text@1", "direction": "output", "kind": "then" }]
    })]);
    let core = core_on(&home, &config, speaker, &log);
    // Another connection holds the project database for writing.
    std::fs::create_dir_all(home.join("store/projects")).unwrap();
    let holder = rusqlite::Connection::open(home.join("store/projects/proj.db")).unwrap();
    holder.execute_batch("BEGIN EXCLUSIVE").unwrap();
    let began = std::time::Instant::now();
    assert_eq!(start(&core)["state"]["status"], "running");
    let page = core
        .dispatch(method::EVENT_LIST, json!({ "since": 0 }))
        .unwrap();
    assert_eq!(page["events"].as_array().unwrap().len(), 1, "{page}");
    assert!(
        began.elapsed() < std::time::Duration::from_millis(1000),
        "start and emit waited {:?} for the database",
        began.elapsed()
    );
    holder.execute_batch("COMMIT").unwrap();
    drop(holder);
    drop(core);
    let store = Store::open(&home.join("store"), &config).unwrap();
    assert_eq!(
        store.executions(None).unwrap().len(),
        1,
        "written once free"
    );
    assert_eq!(store.first_seq(), 2);
}

#[test]
fn pages_behind_memory_come_from_the_store_without_gaps_and_the_cursor_moves() {
    let home = temp_home("pages");
    let log = Log::default();
    let speaker = profile(vec![json!({
        "id": "x", "type": "k", "module_id": "t/a@1.0.0", "config": { "say": "hi" },
        "ports": [{ "id": "out", "capability": "text@1", "direction": "output", "kind": "then" }]
    })]);
    // Memory keeps 2 events, a page holds 2: four runs push the first two out of memory.
    let bus = BusConfig {
        history: 2,
        max_page: 2,
        ..BusConfig::default()
    };
    let core = core_with_bus(&home, &StoreConfig::default(), bus, speaker, &log);
    for _ in 0..4 {
        start(&core);
    }
    let page = |filter: Value, since: u64| -> (Vec<u64>, u64) {
        let mut params = filter;
        params["since"] = since.into();
        let page = core.dispatch(method::EVENT_LIST, params).unwrap();
        let seqs = page["events"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["seq"].as_u64().unwrap())
            .collect();
        (seqs, page["next"].as_u64().unwrap())
    };
    let follow = |filter: Value| -> Vec<u64> {
        let (mut since, mut seen) = (0, Vec::new());
        loop {
            let (seqs, next) = page(filter.clone(), since);
            assert!(next >= since, "the cursor never goes back");
            seen.extend(seqs);
            if next == since {
                return seen;
            }
            since = next;
        }
    };
    // The first page is behind memory: the store answers, clamped to max_page.
    assert_eq!(page(json!({ "limit": 100 }), 0), (vec![1, 2], 2));
    assert_eq!(follow(json!({})), [1, 2, 3, 4]);
    assert_eq!(follow(json!({ "execution_id": "exec-1" })), [1]);
    assert_eq!(follow(json!({ "execution_id": "exec-3" })), [3]);
    // Nothing matches: the cursor still moves past what was looked at.
    assert_eq!(page(json!({ "execution_id": "nope" }), 0), (vec![], 4));
    assert_eq!(page(json!({ "project_id": "other" }), 1), (vec![], 4));
}

#[test]
fn a_store_that_cannot_open_leaves_the_core_in_memory() {
    let home = temp_home("unavailable");
    drop(Store::open(&home.join("store"), &StoreConfig::default()).unwrap());
    let raw = rusqlite::Connection::open(home.join("store/global.db")).unwrap();
    raw.execute_batch("DROP TABLE executions").unwrap();
    drop(raw);
    let config = regulus_core::Config::load(&home).unwrap();
    let (core, warnings) = regulus_core::assemble(&config, Vec::new());
    assert!(
        warnings.iter().any(|w| w.starts_with("store unavailable")),
        "{warnings:?}"
    );
    assert_eq!(list(&core).len(), 0);
}

#[test]
fn a_module_cannot_open_a_project_space_through_another_modules_node() {
    let home = temp_home("foreign-node");
    let log = Log::default();
    let nodes = vec![
        json!({ "id": "x", "type": "k", "module_id": "t/a@1.0.0" }),
        json!({ "id": "y", "type": "k", "module_id": "t/b@2.0.0" }),
    ];
    let core = core_on(&home, &StoreConfig::default(), profile(nodes), &log);
    start(&core);
    let own = json!({ "execution_id": "exec-1", "node_id": "y" });
    assert!(core.dispatch("b.reach", own).is_ok());
    let foreign = json!({ "execution_id": "exec-1", "node_id": "x" });
    let error = core.dispatch("b.reach", foreign).unwrap_err();
    assert!(error.message.contains("does not belong"), "{error:?}");
}

#[test]
fn stopping_an_execution_before_recovery_reaches_it_keeps_it_stopped() {
    let home = temp_home("stop-recovering");
    let log = Log::default();
    let config = with_resume(Resume::Always);
    let core = core_on(&home, &config, one_node(json!({})), &log);
    start(&core);
    drop(core);

    log.lock().unwrap().clear();
    let core = core_on(&home, &config, one_node(json!({})), &log);
    let stopped = core
        .dispatch(method::EXECUTION_STOP, json!({ "id": "exec-1" }))
        .unwrap();
    assert_eq!(stopped["state"]["status"], "stopped");
    assert!(core.recover().is_empty(), "recover skips it");
    assert!(log.lock().unwrap().is_empty(), "no node was started");
    assert_eq!(list(&core)[0]["state"]["status"], "stopped");
    drop(core);
    let core = core_on(&home, &config, one_node(json!({})), &log);
    assert!(core.recover().is_empty());
    assert_eq!(list(&core)[0]["state"]["status"], "stopped");
}
