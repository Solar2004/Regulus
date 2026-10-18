//! How the core runs a workflow (docs/03-modules.md §2): node lifecycle in wiring order,
//! calls between nodes over "uses" wires, and failures that stop what already started.

use std::sync::{Arc, Mutex};

use regulus_core::{Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore};
use regulus_proto::{method, ExecutionState, Manifest, ModuleInfo, NodeCall, Profile};
use serde_json::{json, Value};

type Log = Arc<Mutex<Vec<String>>>;

/// Serves `svc.answer` to whoever is wired to it, without declaring it in `methods`.
struct Service {
    manifest: Manifest,
    log: Log,
}

/// On start, calls whatever is wired into its `dep` input; fails if told to.
struct User {
    manifest: Manifest,
    log: Log,
    ctx: ModuleCtx,
}

fn manifest(id: &str) -> Manifest {
    let module = ModuleInfo {
        id: id.into(),
        name: id.into(),
        requires_proto: None,
        methods: vec![],
        workflow_id: None,
        origin: None,
    };
    Manifest {
        module,
        nodes: vec![],
    }
}

fn node_call(params: Value) -> Result<NodeCall, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

impl Module for Service {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError> {
        let call = node_call(params)?;
        self.log
            .lock()
            .unwrap()
            .push(format!("{method} {}", call.node.id));
        match method {
            method::NODE_START | method::NODE_STOP => Ok(Value::Null),
            "svc.answer" => Ok(json!({ "from": call.node.config["label"], "args": call.args })),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

impl Module for User {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }
    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError> {
        let call = node_call(params)?;
        self.log
            .lock()
            .unwrap()
            .push(format!("{method} {}", call.node.id));
        match method {
            method::NODE_START => {
                if call.node.config.get("fail") == Some(&json!(true)) {
                    return Err(ModuleError::Failed("refused to start".into()));
                }
                let me = NodeRef {
                    execution_id: call.execution_id,
                    node_id: call.node.id,
                };
                for endpoint in self.ctx.resolve(&me, "dep") {
                    let answer = self.ctx.call(&endpoint, "svc.answer", json!({ "q": 1 }))?;
                    let line = format!("got from={} q={}", answer["from"], answer["args"]["q"]);
                    self.log.lock().unwrap().push(line);
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

/// `user` uses `svc` over a wire; `ghost` belongs to a module that is not installed.
fn profile(user_fails: bool) -> Profile {
    serde_json::from_value(json!({
        "id": "p",
        "main_workflow_id": "main",
        "workflows": [{
            "id": "main",
            "kind": "main",
            "nodes": [
                { "id": "user", "type": "user", "module_id": "t/user@1.0.0", "config": { "fail": user_fails },
                  "ports": [{ "id": "dep", "capability": "x@1", "direction": "input", "kind": "uses" }] },
                { "id": "ghost", "type": "ghost", "module_id": "t/ghost@1.0.0" },
                { "id": "svc", "type": "svc", "module_id": "t/svc@1.0.0", "config": { "label": "S" },
                  "ports": [{ "id": "out", "capability": "x@1", "direction": "output", "kind": "uses" }] }
            ],
            "wires": [{ "id": "w", "kind": "uses",
                        "from": { "node_id": "svc", "port_id": "out" },
                        "to": { "node_id": "user", "port_id": "dep" } }]
        }]
    }))
    .unwrap()
}

fn core(user_fails: bool) -> (Arc<Core>, Log) {
    let log = Log::default();
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(User {
            manifest: manifest("t/user@1.0.0"),
            log: log.clone(),
            ctx: ModuleCtx::default(),
        }),
        Box::new(Service {
            manifest: manifest("t/svc@1.0.0"),
            log: log.clone(),
        }),
    ];
    let mut profiles = ProfileStore::default();
    profiles.insert(profile(user_fails)).unwrap();
    (Core::build(modules, &[], profiles), log)
}

fn state(core: &Core, id: &str) -> ExecutionState {
    let list = core.dispatch(method::EXECUTION_LIST, json!({})).unwrap();
    let found = list
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == id)
        .unwrap()
        .clone();
    serde_json::from_value(found["state"].clone()).unwrap()
}

#[test]
fn nodes_start_after_what_they_use_and_can_call_it_over_the_wire() {
    let (core, log) = core(false);
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    let id = execution["id"].as_str().unwrap();
    assert_eq!(state(&core, id), ExecutionState::Running);
    assert_eq!(
        *log.lock().unwrap(),
        [
            "node/start svc",
            "node/start user",
            "svc.answer svc",
            r#"got from="S" q=1"#,
        ]
    );

    log.lock().unwrap().clear();
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert_eq!(state(&core, id), ExecutionState::Stopped);
    assert_eq!(*log.lock().unwrap(), ["node/stop user", "node/stop svc"]);
}

#[test]
fn a_node_that_fails_to_start_fails_the_execution_and_stops_the_rest() {
    let (core, log) = core(true);
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    let id = execution["id"].as_str().unwrap();
    assert!(
        matches!(state(&core, id), ExecutionState::Failed { reason } if reason.contains("refused to start"))
    );
    assert_eq!(
        *log.lock().unwrap(),
        [
            "node/start svc",
            "node/start user",
            "node/stop user",
            "node/stop svc"
        ]
    );
}

#[test]
fn several_executions_of_one_workflow_live_side_by_side() {
    let (core, _) = core(false);
    let a = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    let b = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_ne!(a["id"], b["id"]);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": a["id"] }))
        .unwrap();
    assert_eq!(
        state(&core, a["id"].as_str().unwrap()),
        ExecutionState::Stopped
    );
    assert_eq!(
        state(&core, b["id"].as_str().unwrap()),
        ExecutionState::Running
    );
}

#[test]
fn unknown_profiles_workflows_and_executions_are_invalid_params() {
    let (core, _) = core(false);
    let invalid = regulus_proto::code::INVALID_PARAMS;
    let start = |params| {
        core.dispatch(method::EXECUTION_START, params)
            .unwrap_err()
            .code
    };
    assert_eq!(start(json!({ "profile_id": "nope" })), invalid);
    assert_eq!(
        start(json!({ "profile_id": "p", "workflow_id": "nope" })),
        invalid
    );
    let stop = core
        .dispatch(method::EXECUTION_STOP, json!({ "id": "nope" }))
        .unwrap_err();
    assert_eq!(stop.code, invalid);
}

#[test]
fn a_module_cannot_claim_the_lifecycle_methods() {
    let mut claims = manifest("t/sneaky@1.0.0");
    claims.module.methods = vec![method::NODE_START.into()];
    let sneaky = Service {
        manifest: claims,
        log: Log::default(),
    };
    let core = Core::build(vec![Box::new(sneaky)], &[], ProfileStore::default());
    let modules = core.dispatch(method::MODULE_LIST, json!({})).unwrap();
    assert_eq!(modules[0]["state"]["status"], "broken");
}

/// `needy` requires what `ghost` (not installed) provides; `fan` requires `needy`. Both are
/// left off; `svc` and `user` still run.
#[test]
fn a_missing_required_input_disables_the_node_and_what_requires_it() {
    let (core, log) = core(false);
    let mut profile = profile(false);
    let workflow = &mut profile.workflows[0];
    let extra: Profile = serde_json::from_value(json!({
        "id": "x", "main_workflow_id": "w",
        "workflows": [{ "id": "w", "kind": "main",
            "nodes": [
                { "id": "needy", "type": "user", "module_id": "t/user@1.0.0",
                  "ports": [{ "id": "dep", "capability": "x@1", "direction": "input", "kind": "uses", "required": true },
                            { "id": "out", "capability": "y@1", "direction": "output", "kind": "uses" }] },
                { "id": "fan", "type": "user", "module_id": "t/user@1.0.0",
                  "ports": [{ "id": "dep", "capability": "y@1", "direction": "input", "kind": "uses", "required": true }] }
            ],
            "wires": [
                { "id": "g", "kind": "uses", "from": { "node_id": "ghost", "port_id": "out" }, "to": { "node_id": "needy", "port_id": "dep" } },
                { "id": "n", "kind": "uses", "from": { "node_id": "needy", "port_id": "out" }, "to": { "node_id": "fan", "port_id": "dep" } }
            ] }]
    }))
    .unwrap();
    workflow.nodes.extend(extra.workflows[0].nodes.clone());
    workflow.wires.extend(extra.workflows[0].wires.clone());
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(User {
            manifest: manifest("t/user@1.0.0"),
            log: log.clone(),
            ctx: ModuleCtx::default(),
        }),
        Box::new(Service {
            manifest: manifest("t/svc@1.0.0"),
            log: log.clone(),
        }),
    ];
    drop(core);
    let core = Core::build(modules, &[], profiles);

    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(execution["state"]["status"], "running");
    assert_eq!(
        execution["disabled"],
        json!([{ "node_id": "needy", "missing": ["dep"] }, { "node_id": "fan", "missing": ["dep"] }])
    );
    let started: Vec<String> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.starts_with("node/start"))
        .cloned()
        .collect();
    assert_eq!(started, ["node/start svc", "node/start user"]);
}
