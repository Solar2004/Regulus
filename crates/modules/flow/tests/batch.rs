//! The `batch` node against a real core: map-collect over a child workflow with a cap on
//! live children, failures counted, cancel, a timeout that stops what is left, and the
//! node's "uses" inputs handed on to its children.

use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{parse_manifest, Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore};
use regulus_module_flow::Flow;
use regulus_proto::{
    method, BusEventKind, Diagnostic, EventPage, Execution, Manifest, NodeCall, Profile,
};
use serde_json::{json, Value};

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

/// `prov` is something to be used; `probe` answers each value with the ids of the nodes
/// its "uses" input `model` leads to.
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
            self.ctx.emit(&me, "out", json!(found))?;
        }
        Ok(Value::Null)
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn json_port(id: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": "json@1", "direction": direction, "kind": "then" })
}

fn model_port(direction: &str) -> Value {
    let id = if direction == "input" {
        "model"
    } else {
        "chat"
    };
    json!({ "id": id, "capability": "llm.endpoint@1", "direction": direction, "kind": "uses" })
}

fn wire(id: &str, kind: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": kind,
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

fn flow_node(id: &str, node_type: &str, config: Value, ports: Vec<Value>) -> Value {
    json!({ "id": id, "type": node_type, "module_id": "regulus/flow@0.1.0",
            "config": config, "ports": ports })
}

/// `main` runs a `batch` of `config` on its `items`, `stop` cancels, and the batch's
/// outputs leave as they are. `echo` answers each element after `ms` (an element
/// `boom` loops until the depth cut fails its child); `slow` takes a minute; `bound`
/// answers what its "uses" input `model` leads to.
fn profile(config: Value, expose_model: bool) -> Profile {
    let mut batch_ports = vec![
        json_port("in", "input"),
        json_port("cancel", "input"),
        json_port("item", "output"),
        json_port("done", "output"),
        json_port("partial", "output"),
        json_port("values", "output"),
    ];
    let mut main_wires = vec![
        wire("w-in", "then", ("self", "items"), ("batch", "in")),
        wire("w-cancel", "then", ("self", "stop"), ("batch", "cancel")),
        wire("w-item", "then", ("batch", "item"), ("self", "item")),
        wire("w-done", "then", ("batch", "done"), ("self", "done")),
        wire(
            "w-partial",
            "then",
            ("batch", "partial"),
            ("self", "partial"),
        ),
        wire("w-values", "then", ("batch", "values"), ("self", "values")),
    ];
    if expose_model {
        batch_ports.push(model_port("input"));
        main_wires.push(wire(
            "w-model",
            "uses",
            ("prov", "chat"),
            ("batch", "model"),
        ));
    }
    let io = || vec![json_port("in", "input"), json_port("out", "output")];
    let waits = |ms: u64| flow_node("hold", "wait", json!({ "ms": ms }), io());
    let mut bound_ports = io();
    bound_ports.push(model_port("input"));
    let mut probe_ports = io();
    probe_ports.push(model_port("input"));
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main",
              "ports": [json_port("items", "input"), json_port("stop", "input"),
                        json_port("item", "output"), json_port("done", "output"),
                        json_port("partial", "output"), json_port("values", "output")],
              "nodes": [
                flow_node("batch", "batch", config, batch_ports),
                { "id": "prov", "type": "prov", "module_id": "t/probe@1.0.0",
                  "ports": [model_port("output")] },
              ],
              "wires": main_wires },
            { "id": "echo", "kind": "extra", "ports": io(),
              "nodes": [
                flow_node("gate", "switch",
                          json!({ "cases": [{ "port": "again", "op": "equals", "value": "boom" }] }),
                          vec![json_port("in", "input"), json_port("again", "output"),
                               json_port("other", "output")]),
                waits(60),
              ],
              "wires": [
                wire("a", "then", ("self", "in"), ("gate", "in")),
                wire("b", "then", ("gate", "again"), ("gate", "in")),
                wire("c", "then", ("gate", "other"), ("hold", "in")),
                wire("d", "then", ("hold", "out"), ("self", "out")),
              ] },
            { "id": "slow", "kind": "extra", "ports": io(),
              "nodes": [waits(60_000)],
              "wires": [wire("a", "then", ("self", "in"), ("hold", "in")),
                        wire("b", "then", ("hold", "out"), ("self", "out"))] },
            { "id": "bound", "kind": "extra", "ports": bound_ports,
              "nodes": [{ "id": "probe", "type": "probe", "module_id": "t/probe@1.0.0",
                          "ports": probe_ports }],
              "wires": [wire("a", "then", ("self", "in"), ("probe", "in")),
                        wire("b", "then", ("probe", "out"), ("self", "out")),
                        wire("c", "uses", ("self", "model"), ("probe", "model"))] },
        ]
    }))
    .unwrap()
}

fn running(config: Value, expose_model: bool) -> (Arc<Core>, String) {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile(config, expose_model)).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(Flow::new()),
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
    (core, id)
}

fn send(core: &Core, id: &str, port: &str, value: Value) {
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": port, "value": value }),
    )
    .unwrap();
}

/// What left `self.<port>` of `id` so far.
fn emitted(core: &Core, id: &str, port: &str) -> Vec<Value> {
    let page: EventPage = serde_json::from_value(
        core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
            .unwrap(),
    )
    .unwrap();
    page.events
        .into_iter()
        .filter(|e| e.execution_id == id && e.from.node_id == "self" && e.from.port_id == port)
        .filter_map(|e| match e.kind {
            BusEventKind::Emitted { value } => Some(value),
            _ => None,
        })
        .collect()
}

/// Live children of `id`.
fn live_children(core: &Core, id: &str) -> Vec<Execution> {
    let all: Vec<Execution> =
        serde_json::from_value(core.dispatch(method::EXECUTION_LIST, json!({})).unwrap()).unwrap();
    all.into_iter()
        .filter(|e| e.parent.as_deref() == Some(id) && e.state.is_live())
        .collect()
}

/// The first value on `self.<port>`, watching how many children are live meanwhile.
fn first(core: &Core, id: &str, port: &str) -> (Value, usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut most = 0;
    loop {
        most = most.max(live_children(core, id).len());
        if let Some(value) = emitted(core, id, port).into_iter().next() {
            return (value, most);
        }
        assert!(Instant::now() < deadline, "nothing on `{port}`");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn five_elements_two_at_a_time_one_failing() {
    let (core, id) = running(json!({ "workflow": "echo", "max_in_flight": 2 }), false);
    send(&core, &id, "items", json!(["a", "b", "boom", "d", "e"]));
    let (done, most) = first(&core, &id, "done");
    assert!(most <= 2, "{most} children were live at once");
    assert!(most >= 1);
    assert_eq!(done["total"], 5, "{done}");
    assert_eq!(done["returned"], 4, "{done}");
    assert_eq!(done["failed"], 1, "{done}");
    assert_eq!(done["cancelled"], 0, "{done}");
    let results = done["results"].as_array().unwrap();
    let indices: Vec<u64> = results
        .iter()
        .map(|r| r["index"].as_u64().unwrap())
        .collect();
    assert_eq!(indices, [0, 1, 2, 3, 4]);
    assert_eq!(results[0]["value"], "a");
    assert_eq!(results[0]["port"], "out");
    assert_eq!(results[4]["value"], "e");
    assert!(
        results[2]["error"].as_str().unwrap().contains("max_depth"),
        "{done}"
    );
    let mut items: Vec<String> = emitted(&core, &id, "item")
        .iter()
        .map(|i| i["value"].as_str().unwrap().to_string())
        .collect();
    items.sort();
    assert_eq!(items, ["a", "b", "d", "e"]);
    // The answers alone, in element order, without the failed one.
    assert_eq!(emitted(&core, &id, "values"), [json!(["a", "b", "d", "e"])]);
    // Children carry `<batch>/<index>`.
    let all: Vec<Execution> =
        serde_json::from_value(core.dispatch(method::EXECUTION_LIST, json!({})).unwrap()).unwrap();
    let mut tags: Vec<String> = all
        .iter()
        .filter(|e| e.parent.as_deref() == Some(id.as_str()))
        .filter_map(|e| e.tag.clone())
        .collect();
    tags.sort();
    assert_eq!(tags, ["b1/0", "b1/1", "b1/2", "b1/3", "b1/4"]);
}

#[test]
fn on_failure_cancel_stops_the_rest() {
    let (core, id) = running(
        json!({ "workflow": "echo", "max_in_flight": 1, "on_failure": "cancel" }),
        false,
    );
    send(&core, &id, "items", json!(["boom", "a", "b"]));
    let (done, _) = first(&core, &id, "done");
    assert_eq!(
        (&done["returned"], &done["failed"], &done["cancelled"]),
        (&json!(0), &json!(1), &json!(2)),
        "{done}"
    );
}

#[test]
fn cancel_mid_way_stops_the_live_children() {
    let (core, id) = running(json!({ "workflow": "slow", "max_in_flight": 2 }), false);
    send(&core, &id, "items", json!([1, 2, 3, 4, 5]));
    let deadline = Instant::now() + Duration::from_secs(5);
    while live_children(&core, &id).len() < 2 {
        assert!(Instant::now() < deadline, "the children never started");
        std::thread::sleep(Duration::from_millis(2));
    }
    send(&core, &id, "stop", json!(true));
    let (done, _) = first(&core, &id, "done");
    assert_eq!(done["cancelled"], 5, "{done}");
    assert_eq!(done["returned"], 0, "{done}");
    assert!(live_children(&core, &id).is_empty());
    assert!(emitted(&core, &id, "partial").is_empty());
}

#[test]
fn a_timeout_gives_partial_and_stops_what_is_left() {
    let (core, id) = running(
        json!({ "workflow": "slow", "max_in_flight": 0, "timeout_ms": 150 }),
        false,
    );
    send(&core, &id, "items", json!(["x", "y", "z"]));
    let (partial, most) = first(&core, &id, "partial");
    assert_eq!(most, 3, "max_in_flight 0 starts them all");
    assert_eq!(partial["total"], 3, "{partial}");
    assert_eq!(partial["cancelled"], 3, "{partial}");
    assert_eq!(partial["results"][1]["error"], "timed out");
    assert!(live_children(&core, &id).is_empty());
    std::thread::sleep(Duration::from_millis(50));
    assert!(emitted(&core, &id, "done").is_empty());
}

#[test]
fn an_empty_array_is_done_at_once() {
    let (core, id) = running(json!({ "workflow": "echo" }), false);
    send(&core, &id, "items", json!([]));
    let (done, _) = first(&core, &id, "done");
    assert_eq!(done["total"], 0, "{done}");
}

#[test]
fn the_nodes_uses_inputs_reach_the_children() {
    let (core, id) = running(json!({ "workflow": "bound" }), true);
    let check: Vec<Diagnostic> = serde_json::from_value(
        core.dispatch(method::PROFILE_CHECK, json!({ "id": "p" }))
            .unwrap(),
    )
    .unwrap();
    assert!(
        check.iter().all(|d| d.node_id.as_deref() != Some("batch")),
        "{check:?}"
    );
    send(&core, &id, "items", json!(["one", "two"]));
    let (done, _) = first(&core, &id, "done");
    assert_eq!(done["returned"], 2, "{done}");
    assert_eq!(done["results"][0]["value"], json!(["prov"]), "{done}");

    // Without the input on the node, the child's `model` stays empty and the check says so.
    let (core, id) = running(json!({ "workflow": "bound" }), false);
    let check: Vec<Diagnostic> = serde_json::from_value(
        core.dispatch(method::PROFILE_CHECK, json!({ "id": "p" }))
            .unwrap(),
    )
    .unwrap();
    assert!(
        check
            .iter()
            .any(|d| d.code == "field.workflow_uses_unbound"
                && d.node_id.as_deref() == Some("batch")),
        "{check:?}"
    );
    send(&core, &id, "items", json!(["one"]));
    let (done, _) = first(&core, &id, "done");
    assert_eq!(done["results"][0]["value"], json!([]), "{done}");
}
