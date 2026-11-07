//! Child executions, "call and wait" (docs/13-blocks.md §2 #4): a node starts a workflow
//! of its profile with inputs, hears back the first value that reaches one of its outputs,
//! and the child stops with its parent.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_core::config::{Resume, StoreConfig};
use regulus_core::{
    BusConfig, CallConfig, Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore, Storage,
    Store,
};
use regulus_proto::{
    method, BusEventKind, ChildEvent, ChildEventKind, EventPage, Execution, ExecutionState,
    Manifest, ModuleInfo, NodeCall, NodeEvent, Profile,
};
use serde_json::{json, Value};

type Log = Arc<Mutex<Vec<String>>>;

/// A `caller` node (`config.workflow`) calls that workflow with what arrives on `in` as
/// its input `ask`, and emits what comes back on `out`; with `config.again = n` it calls
/// again with each answer until it has `n`. Any other node relays what arrives on `in` to
/// `out` when `config.relay` is set, and to `config.also` too. On start, a node with
/// `config.fail` fails; one with `config.hold` waits until the log says `release`, then
/// fails.
struct Tester {
    manifest: Manifest,
    log: Log,
    ctx: ModuleCtx,
}

impl Module for Tester {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }
    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params)
            .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        let log = |line: String| self.log.lock().unwrap().push(line);
        match method {
            method::NODE_EVENT => {
                let event: NodeEvent = serde_json::from_value(call.args)
                    .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
                if let Some(workflow) = call.node.config.get("workflow").and_then(Value::as_str) {
                    let child =
                        self.ctx
                            .call_workflow(&me, workflow, vec![("ask".into(), event.value)])?;
                    log(format!("{} called {child}", me.execution_id));
                    return Ok(Value::Null);
                }
                if call.node.config.get("relay") == Some(&json!(true)) {
                    self.ctx.emit(&me, "out", event.value.clone())?;
                    if let Some(port) = call.node.config.get("also").and_then(Value::as_str) {
                        self.ctx.emit(&me, port, event.value)?;
                    }
                }
                Ok(Value::Null)
            }
            method::NODE_CHILD => {
                let child: ChildEvent = serde_json::from_value(call.args)
                    .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
                match child.kind {
                    ChildEventKind::Returned { port, value, .. } => {
                        log(format!("{} returned {port} {value}", child.execution_id));
                        let again = call.node.config.get("again").and_then(Value::as_u64);
                        let returned = self
                            .log
                            .lock()
                            .unwrap()
                            .iter()
                            .filter(|l| l.contains("returned"))
                            .count() as u64;
                        if let (Some(again), Some(workflow)) = (
                            again,
                            call.node.config.get("workflow").and_then(Value::as_str),
                        ) {
                            if returned < again {
                                let next = self.ctx.call_workflow(
                                    &me,
                                    workflow,
                                    vec![("ask".into(), value.clone())],
                                )?;
                                log(format!("{} called {next}", me.execution_id));
                            }
                        }
                        self.ctx.emit(&me, "out", value)?;
                    }
                    ChildEventKind::Ended { state } => {
                        log(format!("{} ended {state:?}", child.execution_id));
                    }
                }
                Ok(Value::Null)
            }
            method::NODE_START => {
                if call.node.config.get("fail") == Some(&json!(true)) {
                    return Err(ModuleError::Failed("told to fail".into()));
                }
                if call.node.config.get("hold") == Some(&json!(true)) {
                    log(format!("{} holding", me.execution_id));
                    until("the release", || logged(&self.log, "release"));
                    log(format!("{} released", me.execution_id));
                    return Err(ModuleError::Failed("held, then failed".into()));
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

/// Another module: `other.steal` asks for a child on behalf of any node it is told about.
struct Other {
    manifest: Manifest,
    ctx: ModuleCtx,
}

impl Module for Other {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }
    fn call(&self, _: &str, params: Value) -> Result<Value, ModuleError> {
        let node = NodeRef {
            execution_id: params["execution_id"].as_str().unwrap_or_default().into(),
            node_id: params["node_id"].as_str().unwrap_or_default().into(),
        };
        let child = self.ctx.call_workflow(&node, "leaf", vec![])?;
        Ok(json!(child))
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

const MODULE: &str = "t/tester@1.0.0";

fn other() -> Box<dyn Module> {
    Box::new(Other {
        manifest: Manifest {
            module: ModuleInfo {
                id: "t/other@1.0.0".into(),
                name: "other".into(),
                requires_proto: None,
                methods: vec!["other.steal".into()],
                workflow_id: None,
                origin: None,
            },
            nodes: vec![],
        },
        ctx: ModuleCtx::default(),
    })
}

fn tester(log: &Log) -> Box<dyn Module> {
    Box::new(Tester {
        manifest: Manifest {
            module: ModuleInfo {
                id: MODULE.into(),
                name: MODULE.into(),
                requires_proto: None,
                methods: vec![],
                workflow_id: None,
                origin: None,
            },
            nodes: vec![],
        },
        log: log.clone(),
        ctx: ModuleCtx::default(),
    })
}

fn node(id: &str, config: Value) -> Value {
    json!({ "id": id, "type": "t", "module_id": MODULE, "config": config, "ports": [
        { "id": "in", "capability": "text@1", "direction": "input", "kind": "then", "cardinality": "many" },
        { "id": "out", "capability": "text@1", "direction": "output", "kind": "then" },
        { "id": "other", "capability": "text@1", "direction": "output", "kind": "then" },
    ]})
}

fn wire(id: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": "then",
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

fn ports(outputs: &[&str]) -> Value {
    let mut ports =
        vec![json!({ "id": "ask", "capability": "text@1", "direction": "input", "kind": "then" })];
    for id in outputs {
        ports.push(
            json!({ "id": id, "capability": "text@1", "direction": "output", "kind": "then" }),
        );
    }
    Value::Array(ports)
}

/// `main` takes `ask`, hands it to caller `c`, which calls `target`; what comes back goes
/// out on `reply`. `leaf` relays `ask` to `reply`; `mute` takes it and never answers;
/// `twice` answers on `reply` and on `other`.
fn profile(target: &str) -> Profile {
    profile_with(json!({ "workflow": target }))
}

/// [`profile`] with the configuration of the caller `c` of `main`.
fn profile_with(caller: Value) -> Profile {
    serde_json::from_value(json!({
        "id": "p", "project_id": "proj", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main", "ports": ports(&["reply"]),
              "nodes": [node("c", caller)],
              "wires": [wire("w1", ("self", "ask"), ("c", "in")),
                        wire("w2", ("c", "out"), ("self", "reply"))] },
            { "id": "leaf", "kind": "extra", "ports": ports(&["reply"]),
              "nodes": [node("x", json!({ "relay": true }))],
              "wires": [wire("w1", ("self", "ask"), ("x", "in")),
                        wire("w2", ("x", "out"), ("self", "reply"))] },
            { "id": "mute", "kind": "extra", "ports": ports(&["reply"]),
              "nodes": [node("x", json!({}))],
              "wires": [wire("w1", ("self", "ask"), ("x", "in")),
                        wire("w2", ("x", "out"), ("self", "reply"))] },
            { "id": "twice", "kind": "extra", "ports": ports(&["reply", "other"]),
              "nodes": [node("x", json!({ "relay": true, "also": "other" }))],
              "wires": [wire("w1", ("self", "ask"), ("x", "in")),
                        wire("w2", ("x", "out"), ("self", "reply")),
                        wire("w3", ("x", "other"), ("self", "other"))] },
            // Calls itself: only `[call] max_nesting` stops it.
            { "id": "deep", "kind": "extra", "ports": ports(&["reply"]),
              "nodes": [node("c", json!({ "workflow": "deep" }))],
              "wires": [wire("w1", ("self", "ask"), ("c", "in")),
                        wire("w2", ("c", "out"), ("self", "reply"))] },
            { "id": "closed", "kind": "extra", "ports": ports(&[]) },
            // Relays to itself and never reaches `reply`: the depth cut drops the chain.
            { "id": "spin", "kind": "extra", "ports": ports(&["reply"]),
              "nodes": [node("x", json!({ "relay": true }))],
              "wires": [wire("w1", ("self", "ask"), ("x", "in")),
                        wire("w2", ("x", "out"), ("x", "in"))] },
            { "id": "broken", "kind": "extra", "ports": ports(&["reply"]),
              "nodes": [node("x", json!({ "fail": true }))],
              "wires": [wire("w1", ("self", "ask"), ("x", "in")),
                        wire("w2", ("x", "out"), ("self", "reply"))] },
            { "id": "held", "kind": "extra", "ports": ports(&["reply"]),
              "nodes": [node("x", json!({ "hold": true }))],
              "wires": [wire("w1", ("self", "ask"), ("x", "in")),
                        wire("w2", ("x", "out"), ("self", "reply"))] },
        ]
    }))
    .unwrap()
}

fn core_with(profile: Profile, call: CallConfig, storage: Storage) -> (Arc<Core>, Log) {
    let log = Log::default();
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let core = Core::build_with_limits(
        vec![tester(&log), other()],
        &[],
        profiles,
        storage,
        BusConfig::default(),
        call,
    );
    (core, log)
}

fn core(target: &str) -> (Arc<Core>, Log) {
    core_with(profile(target), CallConfig::default(), Storage::default())
}

fn start(core: &Core, workflow: &str) -> String {
    let execution = core
        .dispatch(
            method::EXECUTION_START,
            json!({ "profile_id": "p", "workflow_id": workflow }),
        )
        .unwrap();
    assert_eq!(execution["state"]["status"], "running", "{execution}");
    execution["id"].as_str().unwrap().into()
}

fn send(core: &Core, id: &str, value: &str) {
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": "ask", "value": value }),
    )
    .unwrap();
}

fn executions(core: &Core) -> Vec<Execution> {
    serde_json::from_value(core.dispatch(method::EXECUTION_LIST, json!({})).unwrap()).unwrap()
}

fn execution(core: &Core, id: &str) -> Execution {
    executions(core).into_iter().find(|e| e.id == id).unwrap()
}

fn events(core: &Core, id: &str) -> EventPage {
    serde_json::from_value(
        core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
            .unwrap(),
    )
    .unwrap()
}

/// Waits until `done` holds.
fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn running(core: &Core, id: &str) -> bool {
    executions(core)
        .iter()
        .any(|e| e.id == id && e.state == ExecutionState::Running)
}

fn logged(log: &Log, line: &str) -> bool {
    log.lock().unwrap().iter().any(|l| l == line)
}

#[test]
fn a_child_gets_its_inputs_returns_once_and_its_answer_surfaces_in_the_parent() {
    let (core, log) = core("leaf");
    let parent = start(&core, "main");
    send(&core, &parent, "hi");
    until("the child to end", || logged(&log, "exec-2 ended Stopped"));
    assert_eq!(
        log.lock().unwrap().as_slice(),
        [
            "exec-1 called exec-2",
            r#"exec-2 returned reply "hi""#,
            "exec-2 ended Stopped"
        ]
    );

    let child = execution(&core, "exec-2");
    assert_eq!(child.parent.as_deref(), Some(parent.as_str()));
    assert_eq!(child.workflow_id, "leaf");
    assert_eq!(child.state, ExecutionState::Stopped);
    assert_eq!(execution(&core, &parent).state, ExecutionState::Running);
    assert_eq!(execution(&core, &parent).parent, None);

    // The child's own events carry its id; it answered from its `self.reply`.
    let inside = events(&core, "exec-2");
    assert!(inside.events.iter().all(|e| e.execution_id == "exec-2"));
    assert!(inside.events.iter().any(|e| e.from.node_id == "self"
        && e.from.port_id == "reply"
        && matches!(e.kind, BusEventKind::Emitted { .. })));

    // In the parent: the return is a delivery from `exec-2/self.reply` to `c`, and the
    // answer leaves the parent's own `self.reply`.
    until("the parent to answer", || {
        events(&core, &parent).pending == 0
    });
    let outside = events(&core, &parent);
    assert!(outside
        .events
        .iter()
        .any(|e| e.from.node_id == "exec-2/self"
            && matches!(&e.kind, BusEventKind::Delivered { to } if to.node_id == "c")));
    let last = outside.events.last().unwrap();
    assert_eq!(
        (last.from.node_id.as_str(), last.from.port_id.as_str()),
        ("self", "reply")
    );
    assert_eq!(last.kind, BusEventKind::Emitted { value: json!("hi") });
}

#[test]
fn n_values_start_n_children() {
    let (core, log) = core("leaf");
    let parent = start(&core, "main");
    for value in ["a", "b", "c"] {
        send(&core, &parent, value);
    }
    until("three ends", || {
        log.lock()
            .unwrap()
            .iter()
            .filter(|l| l.ends_with("ended Stopped"))
            .count()
            == 3
    });
    let children: Vec<Execution> = executions(&core)
        .into_iter()
        .filter(|e| e.parent.as_deref() == Some(parent.as_str()))
        .collect();
    assert_eq!(children.len(), 3);
}

#[test]
fn only_the_first_value_on_any_output_returns() {
    let (core, log) = core("twice");
    let parent = start(&core, "main");
    send(&core, &parent, "hi");
    until("the child to end", || logged(&log, "exec-2 ended Stopped"));
    let returned: Vec<String> = log
        .lock()
        .unwrap()
        .iter()
        .filter(|l| l.contains("returned"))
        .cloned()
        .collect();
    assert_eq!(returned, [r#"exec-2 returned reply "hi""#]);
    // The second output was in flight when the first returned: it never leaves the parent.
    until("the parent to settle", || {
        events(&core, &parent).pending == 0
    });
    let replies = events(&core, &parent)
        .events
        .iter()
        .filter(|e| e.from.node_id == "self" && matches!(e.kind, BusEventKind::Emitted { .. }))
        .filter(|e| e.from.port_id == "reply")
        .count();
    assert_eq!(replies, 1);
}

#[test]
fn a_child_stopped_from_outside_only_ends_and_stopping_the_parent_stops_children() {
    let (core, log) = core("mute");
    let parent = start(&core, "main");
    send(&core, &parent, "one");
    until("the child to run", || running(&core, "exec-2"));
    core.dispatch(method::EXECUTION_STOP, json!({ "id": "exec-2" }))
        .unwrap();
    until("the end to arrive", || logged(&log, "exec-2 ended Stopped"));
    assert!(!log.lock().unwrap().iter().any(|l| l.contains("returned")));

    send(&core, &parent, "two");
    until("the second child to run", || running(&core, "exec-3"));
    core.dispatch(method::EXECUTION_STOP, json!({ "id": parent }))
        .unwrap();
    assert_eq!(execution(&core, "exec-3").state, ExecutionState::Stopped);
    // The stop is synchronous and the parent stopped first: its queue is empty, nothing
    // waits, and nobody was left to hear the child ended.
    assert_eq!(events(&core, &parent).pending, 0);
    assert!(!logged(&log, "exec-3 ended Stopped"));
}

#[test]
fn children_nest_only_as_deep_as_the_config_says() {
    let call = CallConfig {
        max_nesting: 2,
        ..CallConfig::default()
    };
    let (core, log) = core_with(profile("deep"), call, Storage::default());
    let top = start(&core, "deep");
    send(&core, &top, "down");
    // exec-1 (0) → exec-2 (1) → exec-3 (2) → refused.
    until("the refusal", || {
        events(&core, "exec-3").events.iter().any(|e| {
            matches!(&e.kind, BusEventKind::Failed { error, .. } if error.contains("max_nesting"))
        })
    });
    assert_eq!(executions(&core).len(), 3);
    assert_eq!(execution(&core, "exec-3").parent.as_deref(), Some("exec-2"));
    assert!(logged(&log, "exec-2 called exec-3"));
}

#[test]
fn an_execution_has_at_most_max_children_live() {
    let call = CallConfig {
        max_children: 2,
        ..CallConfig::default()
    };
    let (core, _) = core_with(profile("mute"), call, Storage::default());
    let parent = start(&core, "main");
    for value in ["a", "b", "c"] {
        send(&core, &parent, value);
    }
    until("the third call to be refused", || {
        events(&core, &parent).events.iter().any(|e| {
            matches!(&e.kind, BusEventKind::Failed { error, .. } if error.contains("max_children"))
        })
    });
    let live = executions(&core)
        .iter()
        .filter(|e| e.parent.is_some() && e.state.is_live())
        .count();
    assert_eq!(live, 2);
}

#[test]
fn a_call_to_a_missing_workflow_or_one_with_no_output_fails_on_the_bus() {
    for (target, error) in [
        ("nowhere", "no workflow `nowhere`"),
        ("closed", "declares no \"then\" output"),
    ] {
        let (core, _) = core(target);
        let parent = start(&core, "main");
        send(&core, &parent, "x");
        until("the failure", || {
            events(&core, &parent).events.iter().any(
                |e| matches!(&e.kind, BusEventKind::Failed { error: e, .. } if e.contains(error)),
            )
        });
        assert_eq!(executions(&core).len(), 1, "no child for {target}");
    }
}

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-call-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn stored(home: &Path, resume: Resume) -> (Arc<Core>, Log) {
    let config = StoreConfig {
        resume,
        ..StoreConfig::default()
    };
    let store = Store::open(&home.join("store"), &config).unwrap();
    let storage = Storage {
        store: Some(Arc::new(store)),
        ..Storage::default()
    };
    core_with(profile("mute"), CallConfig::default(), storage)
}

#[test]
fn after_a_restart_a_child_keeps_its_parent_and_is_interrupted() {
    let home = temp_home("restart");
    {
        let (core, _) = stored(&home, Resume::Always);
        let parent = start(&core, "main");
        send(&core, &parent, "x");
        until("the child to run", || running(&core, "exec-2"));
        // As on Ctrl-C: the store keeps both as running.
        core.shutdown();
    }
    let (core, log) = stored(&home, Resume::Always);
    let report = core.recover();
    assert!(
        report.iter().any(|l| l.starts_with("exec-1 resumed")),
        "{report:?}"
    );
    assert!(
        report.iter().any(|l| l.starts_with("exec-2 interrupted")),
        "{report:?}"
    );
    let child = execution(&core, "exec-2");
    assert_eq!(child.parent.as_deref(), Some("exec-1"));
    assert!(
        matches!(&child.state, ExecutionState::Interrupted { reason } if reason.contains("child")),
        "{child:?}"
    );
    assert_eq!(execution(&core, "exec-1").state, ExecutionState::Running);
    // The resumed parent's caller hears that its wait is over.
    until("the end to reach the resumed parent", || {
        log.lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("exec-2 ended Interrupted"))
    });
    assert_eq!(events(&core, "exec-1").pending, 0);
    core.shutdown();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn stopping_a_parent_stops_its_grandchildren() {
    let call = CallConfig {
        max_nesting: 2,
        ..CallConfig::default()
    };
    let (core, _) = core_with(profile("deep"), call, Storage::default());
    let top = start(&core, "deep");
    send(&core, &top, "down");
    until("the grandchild to run", || running(&core, "exec-3"));
    core.dispatch(method::EXECUTION_STOP, json!({ "id": top }))
        .unwrap();
    for id in ["exec-1", "exec-2", "exec-3"] {
        assert_eq!(execution(&core, id).state, ExecutionState::Stopped, "{id}");
    }
}

#[test]
fn a_parent_is_pending_while_a_child_has_not_ended() {
    let (core, log) = core("mute");
    let parent = start(&core, "main");
    send(&core, &parent, "x");
    until("the child to run", || running(&core, "exec-2"));
    until("the parent's own queue to drain", || {
        events(&core, &parent).pending == 1
    });
    core.dispatch(method::EXECUTION_STOP, json!({ "id": "exec-2" }))
        .unwrap();
    until("the end to be delivered", || {
        events(&core, &parent).pending == 0
    });
    assert!(logged(&log, "exec-2 ended Stopped"));
}

#[test]
fn a_module_cannot_call_on_behalf_of_another_modules_node() {
    let (core, _) = core("leaf");
    let parent = start(&core, "main");
    let error = core
        .dispatch(
            "other.steal",
            json!({ "execution_id": parent, "node_id": "c" }),
        )
        .unwrap_err();
    assert!(
        error.message.contains("does not belong"),
        "{}",
        error.message
    );
    assert_eq!(executions(&core).len(), 1);
}

#[test]
fn a_child_that_fails_to_start_ends_failed_and_frees_its_slot() {
    let call = CallConfig {
        max_children: 1,
        ..CallConfig::default()
    };
    let (core, log) = core_with(profile("broken"), call, Storage::default());
    let parent = start(&core, "main");
    let failed = |n: usize| {
        log.lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains("ended Failed") && l.contains("told to fail"))
            .count()
            == n
    };
    // The second call fits only if the failed first one gave its slot back.
    send(&core, &parent, "a");
    until("the first child to fail", || failed(1));
    send(&core, &parent, "b");
    until("the second child to fail", || failed(2));
    assert!(!execution(&core, "exec-2").state.is_live());
    until("the parent to settle", || {
        events(&core, &parent).pending == 0
    });
}

#[test]
fn a_child_stopped_while_a_node_starts_stays_stopped_when_that_start_fails() {
    let (core, log) = core("held");
    let parent = start(&core, "main");
    send(&core, &parent, "x");
    until("the node to hold", || logged(&log, "exec-2 holding"));
    core.dispatch(method::EXECUTION_STOP, json!({ "id": parent }))
        .unwrap();
    assert_eq!(execution(&core, "exec-2").state, ExecutionState::Stopped);
    log.lock().unwrap().push("release".into());
    until("the start to fail", || logged(&log, "exec-2 released"));
    // The failed start is handled right after; give it time to (wrongly) overwrite.
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(execution(&core, "exec-2").state, ExecutionState::Stopped);
}

#[test]
fn a_returned_child_does_not_count_against_max_children() {
    let call = CallConfig {
        max_children: 1,
        ..CallConfig::default()
    };
    let caller = json!({ "workflow": "leaf", "again": 3 });
    let (core, log) = core_with(profile_with(caller), call, Storage::default());
    let parent = start(&core, "main");
    send(&core, &parent, "x");
    until("three answers", || {
        log.lock()
            .unwrap()
            .iter()
            .filter(|l| l.contains("returned"))
            .count()
            == 3
    });
    assert!(!events(&core, &parent).events.iter().any(|e| {
        matches!(&e.kind, BusEventKind::Failed { error, .. } if error.contains("max_children"))
    }));
}

#[test]
fn a_child_whose_chain_is_cut_fails_and_its_caller_hears_it() {
    let (core, log) = core("spin");
    let parent = start(&core, "main");
    send(&core, &parent, "x");
    until("the end", || {
        log.lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with("exec-2 ended Failed") && l.contains("max_depth"))
    });
    until("the parent to settle", || {
        events(&core, &parent).pending == 0
    });
}

#[test]
fn a_start_refuses_a_node_that_runs_a_missing_workflow() {
    // The core only knows the field from a manifest: give the tester one.
    let log = Log::default();
    let module = tester(&log);
    let manifest: Manifest = serde_json::from_value(json!({
        "module": { "id": MODULE, "name": "t" },
        "node": [{ "type": "t", "name": "t", "field": [{ "id": "workflow", "type": "workflow" }] }]
    }))
    .unwrap();
    struct WithManifest(Box<dyn Module>, Manifest);
    impl Module for WithManifest {
        fn manifest(&self) -> &Manifest {
            &self.1
        }
        fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
            self.0.init(ctx)
        }
        fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError> {
            self.0.call(method, params)
        }
        fn shutdown(&mut self) -> Result<(), ModuleError> {
            self.0.shutdown()
        }
    }
    let mut profiles = ProfileStore::default();
    profiles.insert(profile("nowhere")).unwrap();
    let core = Core::build(
        vec![Box::new(WithManifest(module, manifest))],
        &[],
        profiles,
    );
    let error = core
        .dispatch(
            method::EXECUTION_START,
            json!({ "profile_id": "p", "workflow_id": "main" }),
        )
        .unwrap_err();
    assert!(
        error.message.contains("`nowhere` does not exist"),
        "{}",
        error.message
    );
    // The rest of the profile still starts.
    start(&core, "leaf");
}
