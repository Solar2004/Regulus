//! The `tool` node against a real core: a workflow offered as `tool@1`, called and waited
//! for, run as a job, timed out, and run on whatever its "uses" inputs lead to.

use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{parse_manifest, Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore};
use regulus_module_flow::Flow;
use regulus_module_hello::Hello;
use regulus_proto::{method, Diagnostic, Execution, Manifest, NodeCall, Profile};
use serde_json::{json, Value};

const FLOW: &str = "regulus/flow@0.1.0";
const PROBE: &str = r#"
[module]
id   = "t/probe@1.0.0"
name = "Probe"

[[node]]
type = "prov"
name = "Provider"

[[node]]
type = "probe"
name = "Probe"
"#;

/// `probe` answers each value with the ids of the nodes its "uses" input `model` leads to.
struct Probe {
    manifest: Manifest,
    ctx: ModuleCtx,
}

impl Module for Probe {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params)
            .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
        let me = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        if name == method::NODE_EVENT {
            let found: Vec<String> = self
                .ctx
                .resolve(&me, "model")
                .iter()
                .map(|e| e.node().node_id.clone())
                .collect();
            self.ctx.emit(&me, "out", json!(found.join(",")))?;
        }
        Ok(Value::Null)
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn text_port(id: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": "text@1", "direction": direction, "kind": "then" })
}

fn uses(id: &str, capability: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": "uses" })
}

fn wire(id: &str, kind: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": kind,
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

fn tool_node(id: &str, config: Value, extra: Vec<Value>) -> Value {
    let mut ports = vec![
        uses("tools", "tool@1", "output"),
        text_port("ask", "input"),
        text_port("reply", "output"),
    ];
    ports.extend(extra);
    json!({ "id": id, "type": "tool", "module_id": FLOW, "config": config, "ports": ports })
}

/// `main` offers `greeter` as `greet` (call), `later` (job) and `stuck` (a workflow that
/// never answers, 100 ms limit), and `probe` as `which`, its `model` bound to `prov_a`.
/// `which_b` is the same tool with `prov_b` behind it.
fn profile() -> Profile {
    let model = || uses("model", "llm.endpoint@1", "input");
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main",
              "ports": [text_port("reply", "output")],
              "nodes": [
                tool_node("greet", json!({ "workflow": "greeter", "description": "Greets." }), vec![]),
                tool_node("later", json!({ "workflow": "greeter", "mode": "job", "name": "greet_later" }), vec![]),
                tool_node("stuck", json!({ "workflow": "never", "timeout_ms": 100 }), vec![]),
                tool_node("which", json!({ "workflow": "bound" }), vec![model()]),
                tool_node("which_b", json!({ "workflow": "bound" }), vec![model()]),
                { "id": "prov_a", "type": "prov", "module_id": "t/probe@1.0.0",
                  "ports": [uses("chat", "llm.endpoint@1", "output")] },
                { "id": "prov_b", "type": "prov", "module_id": "t/probe@1.0.0",
                  "ports": [uses("chat", "llm.endpoint@1", "output")] },
              ],
              "wires": [
                wire("a", "uses", ("prov_a", "chat"), ("which", "model")),
                wire("b", "uses", ("prov_b", "chat"), ("which_b", "model")),
                wire("c", "then", ("greet", "reply"), ("self", "reply")),
              ] },
            { "id": "greeter", "kind": "extra",
              "ports": [text_port("ask", "input"), text_port("reply", "output")],
              "nodes": [{ "id": "hello", "type": "greeter", "module_id": "regulus/hello@0.1.0",
                          "ports": [text_port("echo", "input"), text_port("reply", "output")] }],
              "wires": [wire("a", "then", ("self", "ask"), ("hello", "echo")),
                        wire("b", "then", ("hello", "reply"), ("self", "reply"))] },
            { "id": "never", "kind": "extra",
              "ports": [text_port("ask", "input"), text_port("reply", "output")] },
            { "id": "bound", "kind": "extra",
              "ports": [text_port("ask", "input"), text_port("reply", "output"), model()],
              "nodes": [{ "id": "probe", "type": "probe", "module_id": "t/probe@1.0.0",
                          "ports": [text_port("in", "input"), text_port("out", "output"),
                                    uses("model", "llm.endpoint@1", "input")] }],
              "wires": [wire("a", "then", ("self", "ask"), ("probe", "in")),
                        wire("b", "then", ("probe", "out"), ("self", "reply")),
                        wire("c", "uses", ("self", "model"), ("probe", "model"))] },
        ]
    }))
    .unwrap()
}

struct Running {
    core: Arc<Core>,
    id: String,
    profile: Profile,
}

impl Running {
    fn start() -> Self {
        let profile = profile();
        let mut profiles = ProfileStore::default();
        profiles.insert(profile.clone()).unwrap();
        let modules: Vec<Box<dyn Module>> = vec![
            Box::new(Flow::new()),
            Box::new(Hello::new()),
            Box::new(Probe {
                manifest: parse_manifest(PROBE).unwrap(),
                ctx: ModuleCtx::default(),
            }),
        ];
        let core = Core::build(modules, &[], profiles);
        let started = core
            .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
            .unwrap();
        let id = started["id"].as_str().unwrap().to_string();
        Self { core, id, profile }
    }

    /// Calls `tool.*` on node `node` as the Tools box does, through the module.
    fn on(&self, node: &str, name: &str, args: Value) -> Value {
        let node = self.profile.workflows[0]
            .nodes
            .iter()
            .find(|n| n.id == node)
            .unwrap();
        let call = json!({ "execution_id": self.id, "node": node, "args": args });
        self.core.registry().call_module(FLOW, name, call).unwrap()
    }

    fn call(&self, node: &str, tool: &str, args: Value) -> Value {
        let request = json!({ "name": tool, "args": args, "caller": { "name": "tester" } });
        self.on(node, "tool.call", request)
    }

    fn children(&self) -> Vec<Execution> {
        let all: Vec<Execution> = serde_json::from_value(
            self.core
                .dispatch(method::EXECUTION_LIST, json!({}))
                .unwrap(),
        )
        .unwrap();
        all.into_iter()
            .filter(|e| e.parent.as_deref() == Some(self.id.as_str()))
            .collect()
    }
}

#[test]
fn the_tool_lists_its_schema_and_answers_what_the_workflow_returns() {
    let run = Running::start();
    let specs = run.on("greet", "tool.list", Value::Null);
    assert_eq!(
        specs,
        json!([{ "name": "greet", "description": "Greets.",
                 "input": { "type": "object", "properties": { "ask": { "type": "string" } },
                            "required": [] } }])
    );
    let answer = run.call("greet", "greet", json!({ "ask": "Alexander" }));
    assert_eq!(
        answer,
        json!({ "data": { "port": "reply", "value": "Hello, Alexander!" } })
    );
    // One input: arguments named otherwise go whole.
    let answer = run.call("greet", "greet", json!("Regulus"));
    assert_eq!(answer["data"]["value"], "Hello, Regulus!");
    let answer = run.call("greet", "nope", json!({}));
    assert_eq!(answer, json!({ "data": "no tool `nope`", "error": true }));
    // The answer also leaves on the node's output of the same id.
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page = run
            .core
            .dispatch(method::EVENT_LIST, json!({ "execution_id": run.id }))
            .unwrap();
        let out = page["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["from"]["node_id"] == "self" && e["value"] == "Hello, Alexander!");
        if out {
            break;
        }
        assert!(Instant::now() < deadline, "{page}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_tool_that_does_not_answer_in_time_stops_its_child() {
    let run = Running::start();
    let started = Instant::now();
    let answer = run.call("stuck", "stuck", json!({ "ask": "x" }));
    assert_eq!(answer["error"], true, "{answer}");
    assert!(
        answer["data"].as_str().unwrap().contains("within 100 ms"),
        "{answer}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(run.children().iter().all(|c| !c.state.is_live()));
}

#[test]
fn a_job_answers_a_handle_and_its_result_later() {
    let run = Running::start();
    let names: Vec<String> = run
        .on("later", "tool.list", Value::Null)
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "greet_later",
            "greet_later_status",
            "greet_later_result",
            "greet_later_cancel"
        ]
    );
    let started = run.call("later", "greet_later", json!({ "ask": "job" }));
    let job = started["data"]["job"].as_str().unwrap().to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    let result = loop {
        let result = run.call("later", "greet_later_result", json!({ "job": job }));
        if result["data"]["pending"] != true {
            break result;
        }
        assert!(Instant::now() < deadline, "{result}");
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(
        result,
        json!({ "data": { "port": "reply", "value": "Hello, job!" } })
    );
    let status = run.call("later", "greet_later_status", json!({ "job": job }));
    assert_eq!(status["data"]["state"], "returned", "{status}");
    let unknown = run.call("later", "greet_later_status", json!({ "job": "exec-99" }));
    assert_eq!(unknown["error"], true);
}

#[test]
fn what_is_wired_into_the_tool_is_what_the_workflow_uses() {
    let run = Running::start();
    let a = run.call("which", "which", json!({ "ask": "?" }));
    assert_eq!(a["data"]["value"], "prov_a", "{a}");
    let b = run.call("which_b", "which_b", json!({ "ask": "?" }));
    assert_eq!(b["data"]["value"], "prov_b", "{b}");
    let check: Vec<Diagnostic> = serde_json::from_value(
        run.core
            .dispatch(method::PROFILE_CHECK, json!({ "id": "p" }))
            .unwrap(),
    )
    .unwrap();
    let codes: Vec<(&str, Option<&str>)> = check
        .iter()
        .map(|d| (d.code.as_str(), d.node_id.as_deref()))
        .collect();
    assert!(codes.is_empty(), "{codes:?}");
}
