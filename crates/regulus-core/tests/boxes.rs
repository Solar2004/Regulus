//! "Uses" wires across boxes at run time (docs/02-architecture.md §4): a service outside
//! reaches a node two boxes down, a service inside a box reaches a node outside, and an
//! unwired box input leaves the node behind it disabled.

use std::sync::{Arc, Mutex};

use regulus_core::{Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore};
use regulus_proto::{method, Execution, ExecutionState, Manifest, ModuleInfo, NodeCall, Profile};
use serde_json::{json, Value};

type Log = Arc<Mutex<Vec<String>>>;

const MODULE: &str = "t/boxes@1.0.0";
const PING: &str = "t.ping";

/// A node with `config.probe = <port>` resolves that "uses" input when it starts and pings
/// every endpoint behind it; a pinged node answers with its id.
struct Prober {
    manifest: Manifest,
    log: Log,
    ctx: ModuleCtx,
}

impl Module for Prober {
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
        match method {
            method::NODE_START => {
                if let Some(port) = call.node.config.get("probe").and_then(Value::as_str) {
                    let me = NodeRef {
                        execution_id: call.execution_id.clone(),
                        node_id: call.node.id.clone(),
                    };
                    for endpoint in self.ctx.resolve(&me, port) {
                        let answer = self.ctx.call(&endpoint, PING, Value::Null)?;
                        let line = format!("{} got {}", call.node.id, answer);
                        self.log.lock().unwrap().push(line);
                    }
                }
                Ok(Value::Null)
            }
            PING => Ok(json!(call.node.id)),
            method::NODE_STOP => Ok(Value::Null),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn prober(log: &Log) -> Box<dyn Module> {
    Box::new(Prober {
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

fn uses_port(id: &str, capability: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": "uses" })
}

fn uses_wire(id: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": "uses",
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

/// `main` has a provider and a user; the box `outer` (workflow `mid`) holds the box
/// `inner` (workflow `leaf`), whose agent uses the provider and whose `tools` node serves
/// the user outside, through both boxes.
fn profile(wired: bool) -> Profile {
    serde_json::from_value(profile_value(wired)).unwrap()
}

fn profile_value(wired: bool) -> Value {
    let main_wires = if wired {
        json!([
            uses_wire("feed", ("provider", "chat"), ("outer", "model")),
            uses_wire("out", ("outer", "toolset"), ("user", "tools"))
        ])
    } else {
        json!([uses_wire("out", ("outer", "toolset"), ("user", "tools"))])
    };
    json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main",
              "nodes": [
                { "id": "provider", "type": "t", "module_id": MODULE,
                  "ports": [uses_port("chat", "llm.endpoint@1", "output")] },
                { "id": "outer", "type": "workflow", "internal_workflow_id": "mid" },
                { "id": "user", "type": "t", "module_id": MODULE, "config": { "probe": "tools" },
                  "ports": [uses_port("tools", "tool@1", "input")] },
              ],
              "wires": main_wires },
            { "id": "mid", "kind": "extra",
              "ports": [uses_port("model", "llm.endpoint@1", "input"),
                        uses_port("toolset", "tool@1", "output")],
              "nodes": [{ "id": "inner", "type": "workflow", "internal_workflow_id": "leaf" }],
              "wires": [uses_wire("down", ("self", "model"), ("inner", "model")),
                        uses_wire("up", ("inner", "toolset"), ("self", "toolset"))] },
            { "id": "leaf", "kind": "extra",
              "ports": [uses_port("model", "llm.endpoint@1", "input"),
                        uses_port("toolset", "tool@1", "output")],
              "nodes": [
                { "id": "agent", "type": "t", "module_id": MODULE,
                  "config": { "probe": "provider" },
                  "ports": [{ "id": "provider", "capability": "llm.endpoint@1",
                              "direction": "input", "kind": "uses", "required": true }] },
                { "id": "tools", "type": "t", "module_id": MODULE,
                  "ports": [uses_port("toolset", "tool@1", "output")] },
              ],
              "wires": [uses_wire("use", ("self", "model"), ("agent", "provider")),
                        uses_wire("give", ("tools", "toolset"), ("self", "toolset"))] },
        ]
    })
}

fn start(wired: bool, workflow: &str) -> (Execution, Log) {
    start_profile(profile(wired), workflow)
}

fn start_profile(profile: Profile, workflow: &str) -> (Execution, Log) {
    let log = Log::default();
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let core = Core::build(vec![prober(&log)], &[], profiles);
    let execution = core
        .dispatch(
            method::EXECUTION_START,
            json!({ "profile_id": "p", "workflow_id": workflow }),
        )
        .unwrap();
    (serde_json::from_value(execution).unwrap(), log)
}

#[test]
fn a_provider_outside_serves_an_agent_two_boxes_down_and_a_toolset_comes_out() {
    let (execution, log) = start(true, "main");
    assert_eq!(execution.state, ExecutionState::Running);
    assert!(execution.disabled.is_empty(), "{execution:?}");
    let mut lines = log.lock().unwrap().clone();
    lines.sort();
    assert_eq!(
        lines,
        [
            r#"outer/inner/agent got "provider""#,
            r#"user got "outer/inner/tools""#,
        ]
    );
}

#[test]
fn an_unwired_box_input_disables_the_node_that_requires_it() {
    let (execution, log) = start(false, "main");
    assert_eq!(execution.state, ExecutionState::Running);
    let disabled: Vec<(&str, Vec<String>)> = execution
        .disabled
        .iter()
        .map(|d| (d.node_id.as_str(), d.missing.clone()))
        .collect();
    assert_eq!(
        disabled,
        [("outer/inner/agent", vec!["provider".to_string()])]
    );
    assert_eq!(
        log.lock().unwrap().as_slice(),
        [r#"user got "outer/inner/tools""#]
    );
}

#[test]
fn a_workflow_run_on_its_own_leaves_its_uses_input_unbound() {
    let (execution, _) = start(true, "mid");
    let disabled: Vec<&str> = execution
        .disabled
        .iter()
        .map(|d| d.node_id.as_str())
        .collect();
    assert_eq!(disabled, ["inner/agent"]);
}

#[test]
fn two_services_outside_reach_a_many_input_two_boxes_down() {
    let mut value = profile_value(true);
    let main = &mut value["workflows"][0];
    main["nodes"].as_array_mut().unwrap().push(json!({
        "id": "backup", "type": "t", "module_id": MODULE,
        "ports": [uses_port("chat", "llm.endpoint@1", "output")]
    }));
    main["wires"].as_array_mut().unwrap().push(uses_wire(
        "feed2",
        ("backup", "chat"),
        ("outer", "model"),
    ));
    for workflow in [1, 2] {
        value["workflows"][workflow]["ports"][0]["cardinality"] = json!("many");
    }
    value["workflows"][2]["nodes"][0]["ports"][0]["cardinality"] = json!("many");
    let (execution, log) = start_profile(serde_json::from_value(value).unwrap(), "main");
    assert!(execution.disabled.is_empty(), "{execution:?}");
    let mut lines = log.lock().unwrap().clone();
    lines.sort();
    assert_eq!(
        lines,
        [
            r#"outer/inner/agent got "backup""#,
            r#"outer/inner/agent got "provider""#,
            r#"user got "outer/inner/tools""#,
        ]
    );
}
