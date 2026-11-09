//! Children a node owns (`ModuleCtx::start_child`, docs/02-architecture.md §6): kept
//! alive and spoken to, tagged, stopped and asked about only by the node that started
//! them, with "uses" inputs bound to what the caller uses.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_core::config::StoreConfig;
use regulus_core::{
    BusConfig, CallConfig, ChildMode, ChildOptions, Core, Module, ModuleCtx, ModuleError, NodeRef,
    ProfileStore, Storage, Store,
};
use regulus_proto::{
    method, ChildEvent, ChildEventKind, Execution, ExecutionState, ExecutionTree, Manifest,
    ModuleInfo, NodeCall, NodeEvent, Profile,
};
use serde_json::{json, Value};

type Log = Arc<Mutex<Vec<String>>>;

const MODULE: &str = "t/keeper@1.0.0";
const PING: &str = "t.ping";
/// `{execution_id, node_id, workflow, mode?, tag?, ask?, bind?}` → the child's id. `bind`
/// names a "uses" input of the child bound to what the node's own `provider` resolves to.
const START: &str = "keeper.start";
/// `{execution_id, node_id, child, value}`: sends `value` to the child's `ask`.
const SEND: &str = "keeper.send";
const STOP: &str = "keeper.stop";
const STATUS: &str = "keeper.status";

/// Starts, feeds, stops and asks about children on behalf of its nodes when a test calls
/// its methods, and logs what the children send back. A node with `config.relay` passes
/// `in` on to `out`; one with `config.probe = <port>` pings what that "uses" input leads to
/// when it starts and logs the answer; one with `config.fail` fails to start.
struct Keeper {
    manifest: Manifest,
    log: Log,
    ctx: ModuleCtx,
}

fn node_of(params: &Value) -> NodeRef {
    NodeRef {
        execution_id: params["execution_id"].as_str().unwrap_or_default().into(),
        node_id: params["node_id"].as_str().unwrap_or_default().into(),
    }
}

impl Keeper {
    fn log(&self, line: String) {
        self.log.lock().unwrap().push(line);
    }
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
        let child = || params["child"].as_str().unwrap_or_default().to_string();
        match name {
            START => {
                let me = node_of(&params);
                let mut opts = ChildOptions {
                    tag: params["tag"].as_str().map(Into::into),
                    ..ChildOptions::default()
                };
                if params["mode"] == "keep" {
                    opts.mode = ChildMode::Keep;
                }
                if let Some(port) = params["bind"].as_str() {
                    opts.bindings = vec![(port.into(), self.ctx.resolve(&me, "provider"))];
                }
                let inputs = match params.get("ask") {
                    Some(value) => vec![("ask".to_string(), value.clone())],
                    None => vec![],
                };
                let workflow = params["workflow"].as_str().unwrap_or_default();
                Ok(json!(self.ctx.start_child(&me, workflow, inputs, opts)?))
            }
            SEND => {
                let value = params["value"].clone();
                self.ctx
                    .send_child(&node_of(&params), &child(), "ask", value)?;
                Ok(Value::Null)
            }
            STOP => Ok(json!(self.ctx.stop_child(&node_of(&params), &child())?)),
            STATUS => Ok(json!(self.ctx.child_status(&node_of(&params), &child())?)),
            _ => self.node(name, params),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

impl Keeper {
    fn node(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params)
            .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        let config = &call.node.config;
        match name {
            method::NODE_START => {
                if config.get("fail") == Some(&json!(true)) {
                    return Err(ModuleError::Failed("told to fail".into()));
                }
                if let Some(port) = config.get("probe").and_then(Value::as_str) {
                    for endpoint in self.ctx.resolve(&me, port) {
                        let answer = self.ctx.call(&endpoint, PING, Value::Null)?;
                        self.log(format!("{} got {answer}", me.execution_id));
                    }
                }
                Ok(Value::Null)
            }
            method::NODE_EVENT => {
                let event: NodeEvent = serde_json::from_value(call.args)
                    .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
                if config.get("relay") == Some(&json!(true)) {
                    self.ctx.emit(&me, "out", event.value)?;
                }
                Ok(Value::Null)
            }
            method::NODE_CHILD => {
                let child: ChildEvent = serde_json::from_value(call.args)
                    .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
                let tag = child.tag.unwrap_or_default();
                let line = match child.kind {
                    ChildEventKind::Returned { port, value, .. } => {
                        format!("{} returned {port} {value} [{tag}]", child.execution_id)
                    }
                    ChildEventKind::Output { port, value, .. } => {
                        format!("{} output {port} {value} [{tag}]", child.execution_id)
                    }
                    ChildEventKind::Ended { state } => {
                        format!("{} ended {state:?} [{tag}]", child.execution_id)
                    }
                };
                self.log(line);
                Ok(Value::Null)
            }
            PING => Ok(json!(call.node.id)),
            method::NODE_STOP => Ok(Value::Null),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
}

fn keeper(log: &Log) -> Box<dyn Module> {
    Box::new(Keeper {
        manifest: Manifest {
            module: ModuleInfo {
                id: MODULE.into(),
                name: MODULE.into(),
                requires_proto: None,
                methods: [START, SEND, STOP, STATUS].map(String::from).to_vec(),
                workflow_id: None,
                origin: None,
            },
            nodes: vec![],
        },
        log: log.clone(),
        ctx: ModuleCtx::default(),
    })
}

fn port(id: &str, capability: &str, direction: &str, kind: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
}

fn wire(id: &str, kind: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": kind,
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

fn relay(id: &str, config: Value) -> Value {
    json!({ "id": id, "type": "t", "module_id": MODULE, "config": config, "ports": [
        port("in", "text@1", "input", "then"),
        port("out", "text@1", "output", "then"),
    ]})
}

/// `main` holds two keeper nodes, `boss` (which uses the provider `prov`) and `other`.
/// `echo` relays `ask` to `reply`; `agentw` does too, with an agent that pings what its
/// workflow's "uses" input `model` leads to; `broken` fails to start.
fn profile() -> Profile {
    let then_ports = || {
        vec![
            port("ask", "text@1", "input", "then"),
            port("reply", "text@1", "output", "then"),
        ]
    };
    let mut agent_ports = then_ports();
    agent_ports.push(port("model", "llm.endpoint@1", "input", "uses"));
    let mut agent = relay("agent", json!({ "relay": true, "probe": "provider" }));
    agent["ports"].as_array_mut().unwrap().push(json!({
        "id": "provider", "capability": "llm.endpoint@1", "direction": "input",
        "kind": "uses", "required": true
    }));
    let relayed = |id: &str| wire(id, "then", ("self", "ask"), ("x", "in"));
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main",
              "nodes": [
                { "id": "boss", "type": "t", "module_id": MODULE,
                  "ports": [port("provider", "llm.endpoint@1", "input", "uses")] },
                { "id": "other", "type": "t", "module_id": MODULE },
                { "id": "prov", "type": "t", "module_id": MODULE,
                  "ports": [port("chat", "llm.endpoint@1", "output", "uses")] },
              ],
              "wires": [wire("w", "uses", ("prov", "chat"), ("boss", "provider"))] },
            { "id": "echo", "kind": "extra", "ports": then_ports(),
              "nodes": [relay("x", json!({ "relay": true }))],
              "wires": [relayed("w1"), wire("w2", "then", ("x", "out"), ("self", "reply"))] },
            { "id": "agentw", "kind": "extra", "ports": agent_ports,
              "nodes": [agent],
              "wires": [wire("w0", "uses", ("self", "model"), ("agent", "provider")),
                        wire("w1", "then", ("self", "ask"), ("agent", "in")),
                        wire("w2", "then", ("agent", "out"), ("self", "reply"))] },
            { "id": "broken", "kind": "extra", "ports": then_ports(),
              "nodes": [relay("x", json!({ "fail": true }))],
              "wires": [relayed("w1"), wire("w2", "then", ("x", "out"), ("self", "reply"))] },
        ]
    }))
    .unwrap()
}

fn core(call: CallConfig) -> (Arc<Core>, Log, String) {
    core_with(call, Storage::default())
}

fn core_with(call: CallConfig, storage: Storage) -> (Arc<Core>, Log, String) {
    let log = Log::default();
    let mut profiles = ProfileStore::default();
    profiles.insert(profile()).unwrap();
    let core = Core::build_with_limits(
        vec![keeper(&log)],
        &[],
        profiles,
        storage,
        BusConfig::default(),
        call,
    );
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    let id = execution["id"].as_str().unwrap().to_string();
    (core, log, id)
}

fn start(core: &Core, parent: &str, node: &str, extra: Value) -> Result<String, String> {
    let mut params = json!({ "execution_id": parent, "node_id": node, "workflow": "echo" });
    params
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    core.dispatch(START, params)
        .map(|v| v.as_str().unwrap().to_string())
        .map_err(|e| e.message)
}

fn on_child(
    core: &Core,
    name: &str,
    parent: &str,
    node: &str,
    child: &str,
) -> Result<Value, String> {
    core.dispatch(
        name,
        json!({ "execution_id": parent, "node_id": node, "child": child, "value": "x" }),
    )
    .map_err(|e| e.message)
}

fn execution(core: &Core, id: &str) -> Execution {
    let all: Vec<Execution> =
        serde_json::from_value(core.dispatch(method::EXECUTION_LIST, json!({})).unwrap()).unwrap();
    all.into_iter().find(|e| e.id == id).unwrap()
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn logged(log: &Log, line: &str) -> bool {
    log.lock().unwrap().iter().any(|l| l == line)
}

#[test]
fn a_kept_child_takes_two_sends_and_gives_two_outputs_with_its_tag() {
    let (core, log, parent) = core(CallConfig::default());
    let child = start(
        &core,
        &parent,
        "boss",
        json!({ "mode": "keep", "tag": "w-1" }),
    )
    .unwrap();
    until("the child to run", || {
        execution(&core, &child).state == ExecutionState::Running
    });
    for value in ["one", "two"] {
        core.dispatch(
            SEND,
            json!({ "execution_id": parent, "node_id": "boss", "child": child, "value": value }),
        )
        .unwrap();
    }
    until("two outputs", || {
        logged(&log, &format!(r#"{child} output reply "two" [w-1]"#))
    });
    assert!(logged(
        &log,
        &format!(r#"{child} output reply "one" [w-1]"#)
    ));
    let kept = execution(&core, &child);
    assert_eq!(kept.state, ExecutionState::Running);
    assert_eq!(kept.caller.as_deref(), Some("boss"));
    assert_eq!(kept.tag.as_deref(), Some("w-1"));

    let stopped: Execution =
        serde_json::from_value(on_child(&core, STOP, &parent, "boss", &child).unwrap()).unwrap();
    assert_eq!(stopped.state, ExecutionState::Stopped);
    until("the end", || {
        logged(&log, &format!("{child} ended Stopped [w-1]"))
    });
}

#[test]
fn a_called_child_still_returns_once_and_stops() {
    let (core, log, parent) = core(CallConfig::default());
    let child = start(&core, &parent, "boss", json!({ "ask": "hi", "tag": "c" })).unwrap();
    until("the end", || {
        logged(&log, &format!("{child} ended Stopped [c]"))
    });
    assert!(logged(&log, &format!(r#"{child} returned reply "hi" [c]"#)));
}

#[test]
fn only_the_node_that_started_a_child_may_send_stop_or_ask() {
    let (core, _, parent) = core(CallConfig::default());
    let child = start(&core, &parent, "boss", json!({ "mode": "keep" })).unwrap();
    let refused = format!("invalid params: execution `{child}` is not a child of node `other`");
    for name in [SEND, STOP, STATUS] {
        let error = on_child(&core, name, &parent, "other", &child).unwrap_err();
        assert_eq!(error, refused, "{name}");
    }
    assert!(execution(&core, &child).state.is_live());
    let status: Execution =
        serde_json::from_value(on_child(&core, STATUS, &parent, "boss", &child).unwrap()).unwrap();
    assert_eq!(status.id, child);
}

#[test]
fn stopping_the_parent_ends_its_kept_children() {
    let (core, _, parent) = core(CallConfig::default());
    let child = start(&core, &parent, "boss", json!({ "mode": "keep" })).unwrap();
    until("the child to run", || {
        execution(&core, &child).state == ExecutionState::Running
    });
    core.dispatch(method::EXECUTION_STOP, json!({ "id": parent }))
        .unwrap();
    assert_eq!(execution(&core, &child).state, ExecutionState::Stopped);
}

#[test]
fn max_children_counts_kept_and_called_children_alike() {
    let call = CallConfig {
        max_children: 2,
        ..CallConfig::default()
    };
    let (core, _, parent) = core(call);
    start(&core, &parent, "boss", json!({ "mode": "keep" })).unwrap();
    // Called without input: it never returns, so it stays live.
    start(&core, &parent, "other", json!({})).unwrap();
    let error = start(&core, &parent, "boss", json!({ "mode": "keep" })).unwrap_err();
    assert!(error.contains("max_children"), "{error}");
}

#[test]
fn bindings_hand_the_callers_provider_to_the_child() {
    let (core, log, parent) = core(CallConfig::default());
    let child = start(
        &core,
        &parent,
        "boss",
        json!({ "workflow": "agentw", "bind": "model", "mode": "keep" }),
    )
    .unwrap();
    until("the probe", || {
        logged(&log, &format!(r#"{child} got "prov""#))
    });
    assert!(execution(&core, &child).disabled.is_empty());

    // Unbound, the agent has nothing behind `model` and stays off.
    let unbound = start(&core, &parent, "other", json!({ "workflow": "agentw" })).unwrap();
    until("the child to run", || {
        execution(&core, &unbound).state == ExecutionState::Running
    });
    let disabled: Vec<String> = execution(&core, &unbound)
        .disabled
        .iter()
        .map(|d| d.node_id.clone())
        .collect();
    assert_eq!(disabled, ["agent"]);

    // Only a "uses" input the child's workflow declares can be bound.
    let error = start(&core, &parent, "boss", json!({ "bind": "ask" })).unwrap_err();
    assert!(error.contains("is not a \"uses\" input"), "{error}");
}

#[test]
fn the_status_of_a_child_that_failed_says_so() {
    let (core, log, parent) = core(CallConfig::default());
    let child = start(
        &core,
        &parent,
        "boss",
        json!({ "workflow": "broken", "mode": "keep" }),
    )
    .unwrap();
    until("the end", || {
        log.lock()
            .unwrap()
            .iter()
            .any(|l| l.starts_with(&format!("{child} ended Failed")))
    });
    let status: Execution =
        serde_json::from_value(on_child(&core, STATUS, &parent, "boss", &child).unwrap()).unwrap();
    assert!(
        matches!(status.state, ExecutionState::Failed { .. }),
        "{status:?}"
    );
}

#[test]
fn execution_tree_shows_the_children_with_their_caller_and_tag() {
    let (core, _, parent) = core(CallConfig::default());
    let first = start(
        &core,
        &parent,
        "boss",
        json!({ "mode": "keep", "tag": "a" }),
    )
    .unwrap();
    let second = start(&core, &parent, "other", json!({ "mode": "keep" })).unwrap();
    let tree: ExecutionTree = serde_json::from_value(
        core.dispatch(method::EXECUTION_TREE, json!({ "execution_id": parent }))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(tree.execution.id, parent);
    let children: Vec<(&str, Option<&str>, Option<&str>)> = tree
        .children
        .iter()
        .map(|c| {
            (
                c.execution.id.as_str(),
                c.execution.caller.as_deref(),
                c.execution.tag.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        children,
        [
            (first.as_str(), Some("boss"), Some("a")),
            (second.as_str(), Some("other"), None)
        ]
    );
    let error = core
        .dispatch(method::EXECUTION_TREE, json!({ "execution_id": "exec-99" }))
        .unwrap_err();
    assert_eq!(error.message, "Unknown execution");
}

#[test]
fn a_kept_child_is_stored_with_its_caller_and_tag_and_interrupted_after_a_restart() {
    let home = std::env::temp_dir().join(format!("regulus-children-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let storage = || Storage {
        store: Some(Arc::new(
            Store::open(&home, &StoreConfig::default()).unwrap(),
        )),
        ..Storage::default()
    };
    let (core, _, parent) = core_with(CallConfig::default(), storage());
    let child = start(
        &core,
        &parent,
        "boss",
        json!({ "mode": "keep", "tag": "w-7" }),
    )
    .unwrap();
    until("the child to run", || {
        execution(&core, &child).state == ExecutionState::Running
    });
    core.shutdown();
    drop(core);

    let log = Log::default();
    let mut profiles = ProfileStore::default();
    profiles.insert(profile()).unwrap();
    let core = Core::build_with(vec![keeper(&log)], &[], profiles, storage());
    core.recover();
    let again = execution(&core, &child);
    assert_eq!(again.caller.as_deref(), Some("boss"));
    assert_eq!(again.tag.as_deref(), Some("w-7"));
    assert!(
        matches!(again.state, ExecutionState::Interrupted { .. }),
        "{again:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}
