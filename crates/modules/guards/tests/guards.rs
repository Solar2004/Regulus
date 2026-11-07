//! The guards against a real core: one guard between the workflow's own ports, values in
//! through `workflow.send`, what it gives out through the workflow's outputs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore};
use regulus_module_guards::Guards;
use regulus_proto::{method, BusEventKind, EventPage, Profile};
use serde_json::{json, Value};

const MODULE: &str = "regulus/guards@0.1.0";

fn port(id: &str, capability: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": "then",
            "cardinality": if direction == "input" { "many" } else { "one" } })
}

/// One guard `g` with each input and output wired to the workflow's port of the same id.
fn profile(node_type: &str, config: Value, inputs: &[&str], outputs: &[(&str, &str)]) -> Profile {
    let (mut ports, mut wires) = (Vec::new(), Vec::new());
    for id in inputs {
        ports.push(port(id, "text@1", "input"));
        wires.push(json!({ "id": format!("in-{id}"), "kind": "then",
            "from": { "node_id": "self", "port_id": id }, "to": { "node_id": "g", "port_id": id } }));
    }
    for (id, capability) in outputs {
        ports.push(port(id, capability, "output"));
        wires.push(json!({ "id": format!("out-{id}"), "kind": "then",
            "from": { "node_id": "g", "port_id": id }, "to": { "node_id": "self", "port_id": id } }));
    }
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "ports": ports, "wires": wires, "nodes": [
            { "id": "g", "type": node_type, "module_id": MODULE, "config": config, "ports": ports }
        ]}]
    }))
    .unwrap()
}

fn start(profile: Profile) -> (Arc<Core>, String) {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Guards::new())];
    let core = Core::build(modules, &[], profiles);
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
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

/// Every value that left one of the workflow's outputs, `(port, value)`.
fn emitted(core: &Core, id: &str, outputs: &[&str]) -> Vec<(String, Value)> {
    let page: EventPage = serde_json::from_value(
        core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
            .unwrap(),
    )
    .unwrap();
    page.events
        .into_iter()
        .filter(|e| e.from.node_id == "self" && outputs.contains(&e.from.port_id.as_str()))
        .filter_map(|e| match e.kind {
            BusEventKind::Emitted { value } => Some((e.from.port_id, value)),
            _ => None,
        })
        .collect()
}

/// Waits until `count` values left those outputs.
fn wait_for(core: &Core, id: &str, outputs: &[&str], count: usize) -> Vec<(String, Value)> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let got = emitted(core, id, outputs);
        if got.len() >= count {
            return got;
        }
        assert!(Instant::now() < deadline, "only {got:?} came out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn pair(port: &str, value: &str) -> (String, Value) {
    (port.into(), json!(value))
}

#[test]
fn a_budget_forwards_until_it_is_spent_and_reset_starts_over() {
    let config = json!({ "max_items": 2 });
    let outs = [("out", "text@1"), ("exceeded", "text@1")];
    let (core, id) = start(profile("budget", config, &["in", "reset"], &outs));
    for value in ["a", "b", "c"] {
        send(&core, &id, "in", json!(value));
    }
    let got = wait_for(&core, &id, &["out", "exceeded"], 3);
    assert_eq!(
        got,
        [pair("out", "a"), pair("out", "b"), pair("exceeded", "c")]
    );
    send(&core, &id, "reset", json!(""));
    send(&core, &id, "in", json!("d"));
    assert_eq!(
        wait_for(&core, &id, &["out", "exceeded"], 4)[3],
        pair("out", "d")
    );
}

#[test]
fn a_retry_sends_the_value_again_then_gives_up() {
    let config = json!({ "max_retries": 2, "backoff_ms": 20, "backoff_factor": 2 });
    let outs = [("out", "text@1"), ("gave_up", "text@1")];
    let (core, id) = start(profile("retry", config, &["in", "fail"], &outs));
    send(&core, &id, "in", json!("x"));
    wait_for(&core, &id, &["out", "gave_up"], 1);
    let started = Instant::now();
    for n in 2..=3 {
        send(&core, &id, "fail", json!("broken"));
        wait_for(&core, &id, &["out", "gave_up"], n);
    }
    // Two backoffs: 20 ms, then 40.
    assert!(started.elapsed() >= Duration::from_millis(60));
    send(&core, &id, "fail", json!("broken"));
    let got = wait_for(&core, &id, &["out", "gave_up"], 4);
    assert_eq!(
        got,
        [
            pair("out", "x"),
            pair("out", "x"),
            pair("out", "x"),
            pair("gave_up", "x")
        ]
    );
}

#[test]
fn a_timeout_fires_without_holding_anything_up() {
    let config = json!({ "timeout_ms": 300 });
    let outs = [("out", "text@1"), ("timeout", "text@1")];
    let (core, id) = start(profile("timeout", config, &["in", "ack"], &outs));
    let sent = Instant::now();
    send(&core, &id, "in", json!("late"));
    send(&core, &id, "in", json!("answered"));
    // Both are forwarded at once, long before any clock runs out.
    wait_for(&core, &id, &["out"], 2);
    assert!(sent.elapsed() < Duration::from_millis(300));
    // The ack stops the oldest clock: `late`'s. `answered` is the one that times out.
    send(&core, &id, "ack", json!("ok"));
    let got = wait_for(&core, &id, &["out", "timeout"], 3);
    assert_eq!(got[2], pair("timeout", "answered"));
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(emitted(&core, &id, &["timeout"]).len(), 1);
}

#[test]
fn a_watchdog_reports_a_stall_restarts_once_and_progress_keeps_it_quiet() {
    let config = json!({ "stall_ms": 150, "restart": "yes", "max_restarts": 1 });
    let outs = [
        ("out", "text@1"),
        ("stuck", "text@1"),
        ("restart", "text@1"),
    ];
    let (core, id) = start(profile(
        "watchdog",
        config,
        &["in", "progress", "done"],
        &outs,
    ));
    send(&core, &id, "in", json!("step"));
    let got = wait_for(&core, &id, &["out", "stuck", "restart"], 4);
    assert_eq!(
        got,
        [
            pair("out", "step"),
            pair("stuck", "step"),
            pair("restart", "step"),
            pair("stuck", "step")
        ]
    );
    // Busy with progress every 30 ms: never stuck; done, and it stays quiet.
    send(&core, &id, "in", json!("alive"));
    for _ in 0..8 {
        std::thread::sleep(Duration::from_millis(30));
        send(&core, &id, "progress", json!("."));
    }
    send(&core, &id, "done", json!("."));
    std::thread::sleep(Duration::from_millis(300));
    let stuck = emitted(&core, &id, &["stuck"]);
    assert_eq!(stuck.len(), 2, "{stuck:?}");
}

#[test]
fn the_error_node_normalizes_what_reaches_the_fail_branch() {
    let (core, id) = start(profile(
        "error",
        json!({}),
        &["fail"],
        &[("error", "error@1")],
    ));
    send(&core, &id, "fail", json!("disk full"));
    let (_, failure) = wait_for(&core, &id, &["error"], 1).remove(0);
    assert_eq!(failure["message"], "disk full");
    assert_eq!(
        failure["from"],
        json!({ "node_id": "self", "port_id": "fail" })
    );
    assert_eq!(failure["value"], "disk full");
}
