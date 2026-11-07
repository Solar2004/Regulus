//! The flow nodes against a real core: values go in through `workflow.send` and what
//! leaves the workflow's own outputs is read back from the bus.

use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore, Storage};
use regulus_module_flow::Flow;
use regulus_proto::{method, BusEventKind, EventPage, Profile};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(5);

fn port(id: &str, capability: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": "then",
            "cardinality": if direction == "input" { "many" } else { "one" } })
}

fn node(id: &str, node_type: &str, config: Value, ports: Vec<Value>) -> Value {
    json!({ "id": id, "type": node_type, "module_id": "regulus/flow@0.1.0",
            "config": config, "ports": ports })
}

/// A wire between `a.b` and `c.d`.
fn wire(from: &str, to: &str) -> Value {
    let split = |s: &str| {
        let (n, p) = s.split_once('.').unwrap();
        json!({ "node_id": n, "port_id": p })
    };
    json!({ "id": format!("{from}>{to}"), "kind": "then", "from": split(from), "to": split(to) })
}

fn profile(ports: Vec<Value>, nodes: Vec<Value>, wires: Vec<Value>) -> Profile {
    serde_json::from_value(json!({
        "id": "p", "project_id": "proj", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "ports": ports, "nodes": nodes,
                        "wires": wires }]
    }))
    .unwrap()
}

fn core(profile: Profile, storage: Storage) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Flow::new())];
    Core::build_with(modules, &[], profiles, storage)
}

fn start(core: &Core) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    started["id"].as_str().unwrap().to_string()
}

fn send(core: &Core, id: &str, port: &str, value: Value) {
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": port, "value": value }),
    )
    .unwrap();
}

fn page(core: &Core, id: &str) -> EventPage {
    serde_json::from_value(
        core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
            .unwrap(),
    )
    .unwrap()
}

/// What left `node.port` so far.
fn emitted(core: &Core, id: &str, from: &str) -> Vec<Value> {
    let (node, port) = from.split_once('.').unwrap();
    page(core, id)
        .events
        .into_iter()
        .filter(|e| e.from.node_id == node && e.from.port_id == port)
        .filter_map(|e| match e.kind {
            BusEventKind::Emitted { value } => Some(value),
            _ => None,
        })
        .collect()
}

/// What left `self.<port>` once the bus is idle.
fn settled(core: &Core, id: &str, port: &str) -> Vec<Value> {
    let deadline = Instant::now() + WAIT;
    while page(core, id).pending > 0 {
        assert!(Instant::now() < deadline, "the bus never went idle");
        std::thread::sleep(Duration::from_millis(5));
    }
    emitted(core, id, &format!("self.{port}"))
}

fn no_failures(core: &Core, id: &str) {
    for event in page(core, id).events {
        if let BusEventKind::Failed { error, .. } = &event.kind {
            panic!("delivery failed: {error} ({event:?})");
        }
    }
}

const J: &str = "json@1";

#[test]
fn switch_routes_by_rule_and_by_enum() {
    let rules = json!({ "cases": [
        { "port": "pass", "op": "contains", "value": "PASS" },
        { "port": "high", "op": "greater", "path": "score", "value": 5 },
    ]});
    let ports = vec![
        port("in", J, "input"),
        port("pass", J, "output"),
        port("high", J, "output"),
        port("other", J, "output"),
    ];
    let wires = vec![
        wire("self.in", "sw.in"),
        wire("sw.pass", "self.pass"),
        wire("sw.high", "self.high"),
        wire("sw.other", "self.other"),
    ];
    let p = profile(
        ports.clone(),
        vec![node("sw", "switch", rules, ports.clone())],
        wires.clone(),
    );
    let core = core(p, Storage::default());
    let id = start(&core);
    send(&core, &id, "in", json!("tests PASS"));
    send(&core, &id, "in", json!({ "score": 9 }));
    send(&core, &id, "in", json!({ "score": 1 }));
    assert_eq!(settled(&core, &id, "pass"), [json!("tests PASS")]);
    assert_eq!(settled(&core, &id, "high"), [json!({ "score": 9 })]);
    assert_eq!(settled(&core, &id, "other"), [json!({ "score": 1 })]);

    let p = profile(
        ports.clone(),
        vec![node(
            "sw",
            "switch",
            json!({ "mode": "enum", "path": "/to" }),
            ports,
        )],
        wires,
    );
    let core = self::core(p, Storage::default());
    let id = start(&core);
    send(&core, &id, "in", json!({ "to": "high" }));
    send(&core, &id, "in", json!({ "to": "nowhere" }));
    assert_eq!(settled(&core, &id, "high"), [json!({ "to": "high" })]);
    assert_eq!(settled(&core, &id, "other"), [json!({ "to": "nowhere" })]);
    no_failures(&core, &id);
}
