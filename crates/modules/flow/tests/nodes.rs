//! The flow nodes against a real core: values go in through `workflow.send` and what
//! leaves the workflow's own outputs is read back from the bus.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::config::StoreConfig;
use regulus_core::{Core, Module, ProfileStore, Storage, Store};
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

/// Polls until `self.<port>` has `count` values.
fn until(core: &Core, id: &str, port: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + WAIT;
    loop {
        let got = emitted(core, id, &format!("self.{port}"));
        if got.len() >= count {
            return got;
        }
        assert!(Instant::now() < deadline, "self.{port} got {got:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn no_failures(core: &Core, id: &str) {
    for event in page(core, id).events {
        if let BusEventKind::Failed { error, .. } = &event.kind {
            panic!("delivery failed: {error} ({event:?})");
        }
    }
}

const T: &str = "text@1";
const J: &str = "json@1";

#[test]
fn a_loop_turns_until_its_rule_or_stops_at_its_cap() {
    let loop_node = |config| {
        node(
            "loop",
            "loop",
            config,
            vec![
                port("in", T, "input"),
                port("back", T, "input"),
                port("body", T, "output"),
                port("done", T, "output"),
                port("exhausted", T, "output"),
            ],
        )
    };
    // The body is a template that marks each turn; `back` brings it round.
    let body = node(
        "mark",
        "convert",
        json!({ "template": "{{.}}+" }),
        vec![port("in", T, "input"), port("out", T, "output")],
    );
    let ports = vec![
        port("go", T, "input"),
        port("done", T, "output"),
        port("exhausted", T, "output"),
    ];
    let wires = vec![
        wire("self.go", "loop.in"),
        wire("loop.body", "mark.in"),
        wire("mark.out", "loop.back"),
        wire("loop.done", "self.done"),
        wire("loop.exhausted", "self.exhausted"),
    ];
    let make = |config| {
        profile(
            ports.clone(),
            vec![loop_node(config), body.clone()],
            wires.clone(),
        )
    };

    let core = core(make(json!({ "max": 3 })), Storage::default());
    let id = start(&core);
    send(&core, &id, "go", json!("x"));
    assert_eq!(settled(&core, &id, "exhausted"), [json!("x+++")]);
    assert_eq!(emitted(&core, &id, "loop.body").len(), 3, "exactly N turns");
    assert!(emitted(&core, &id, "self.done").is_empty());

    let until = json!({ "max": 10, "until": { "op": "contains", "value": "++" } });
    let core = self::core(make(until), Storage::default());
    let id = start(&core);
    send(&core, &id, "go", json!("y"));
    assert_eq!(settled(&core, &id, "done"), [json!("y++")]);
    assert_eq!(emitted(&core, &id, "loop.body").len(), 2);
    no_failures(&core, &id);
}

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

#[test]
fn a_join_barrier_waits_for_every_branch_and_fanout_splits_arrays() {
    let p = profile(
        vec![
            port("a", T, "input"),
            port("b", T, "input"),
            port("list", J, "input"),
            port("out", J, "output"),
            port("each", J, "output"),
        ],
        vec![
            node(
                "join",
                "join",
                json!({}),
                vec![
                    port("a", T, "input"),
                    port("b", T, "input"),
                    port("out", J, "output"),
                ],
            ),
            node(
                "fan",
                "fanout",
                json!({ "mode": "split" }),
                vec![port("in", J, "input"), port("out", J, "output")],
            ),
        ],
        vec![
            wire("self.a", "join.a"),
            wire("self.b", "join.b"),
            wire("join.out", "self.out"),
            wire("self.list", "fan.in"),
            wire("fan.out", "self.each"),
        ],
    );
    let core = core(p, Storage::default());
    let id = start(&core);
    send(&core, &id, "a", json!("first"));
    send(&core, &id, "a", json!("second"));
    assert!(settled(&core, &id, "out").is_empty(), "b has not come yet");
    send(&core, &id, "b", json!("B"));
    assert_eq!(settled(&core, &id, "out"), [json!(["first", "B"])]);
    send(&core, &id, "b", json!("B2"));
    assert_eq!(settled(&core, &id, "out").len(), 2, "the queued `a` met it");

    send(&core, &id, "list", json!([1, 2, 3]));
    assert_eq!(settled(&core, &id, "each"), [json!(1), json!(2), json!(3)]);
    no_failures(&core, &id);
}

#[test]
fn wait_does_not_hold_the_bus_and_a_stop_drops_what_it_holds() {
    let p = profile(
        vec![
            port("slow", T, "input"),
            port("fast", T, "input"),
            port("late", T, "output"),
            port("quick", J, "output"),
        ],
        vec![
            node(
                "wait",
                "wait",
                json!({ "ms": 400 }),
                vec![port("in", T, "input"), port("out", T, "output")],
            ),
            node(
                "conv",
                "convert",
                json!({}),
                vec![port("in", T, "input"), port("out", J, "output")],
            ),
        ],
        vec![
            wire("self.slow", "wait.in"),
            wire("wait.out", "self.late"),
            wire("self.fast", "conv.in"),
            wire("conv.out", "self.quick"),
        ],
    );
    let core = core(p, Storage::default());
    let id = start(&core);
    let sent = Instant::now();
    send(&core, &id, "slow", json!("held"));
    send(&core, &id, "fast", json!("{\"n\":1}"));
    assert_eq!(until(&core, &id, "quick", 1), [json!({ "n": 1 })]);
    assert!(
        emitted(&core, &id, "self.late").is_empty(),
        "the quick value went past the held one"
    );
    assert_eq!(until(&core, &id, "late", 1), [json!("held")]);
    assert!(sent.elapsed() >= Duration::from_millis(400));

    send(&core, &id, "slow", json!("dropped"));
    let stopped = Instant::now();
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert!(
        stopped.elapsed() < Duration::from_millis(400),
        "stop did not wait"
    );
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(
        emitted(&core, &id, "wait.out").len(),
        1,
        "nothing after the stop"
    );
}

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-flow-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

#[test]
fn a_variable_outlives_a_restart_that_resumes_its_execution() {
    let make = || {
        profile(
            vec![
                port("set", T, "input"),
                port("get", T, "input"),
                port("value", T, "output"),
            ],
            vec![
                node(
                    "keep",
                    "variable",
                    json!({ "name": "plan" }),
                    vec![port("set", T, "input"), port("value", T, "output")],
                ),
                // Another node with the same name reads the same value.
                node(
                    "read",
                    "variable",
                    json!({ "name": "plan", "initial": "none yet" }),
                    vec![port("get", T, "input"), port("value", T, "output")],
                ),
            ],
            vec![
                wire("self.set", "keep.set"),
                wire("self.get", "read.get"),
                wire("read.value", "self.value"),
            ],
        )
    };
    let home = temp_home("variable");
    let storage = || Storage {
        store: Some(Arc::new(
            Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
        )),
        ..Storage::default()
    };
    let core = core(make(), storage());
    let id = start(&core);
    send(&core, &id, "get", json!("?"));
    assert_eq!(settled(&core, &id, "value"), [json!("none yet")]);
    send(&core, &id, "set", json!("step 2"));
    settled(&core, &id, "value");
    core.shutdown();
    drop(core);

    let core = self::core(make(), storage());
    assert_eq!(core.recover(), [format!("{id} resumed (1 checkpoints)")]);
    send(&core, &id, "get", json!("?"));
    let values = settled(&core, &id, "value");
    assert_eq!(values.last(), Some(&json!("step 2")), "{values:?}");
}

/// What `execution.start` answers for `p`.
fn try_start(core: &Core) -> Value {
    core.dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap()
}

fn failures(core: &Core, id: &str) -> Vec<String> {
    page(core, id)
        .events
        .into_iter()
        .filter_map(|e| match e.kind {
            BusEventKind::Failed { error, .. } => Some(error),
            _ => None,
        })
        .collect()
}

#[test]
fn wait_refuses_an_unbounded_hold_and_drops_what_does_not_fit() {
    let make = |max_pending: u64| {
        profile(
            vec![port("in", T, "input"), port("out", T, "output")],
            vec![node(
                "wait",
                "wait",
                json!({ "ms": 300, "max_pending": max_pending }),
                vec![port("in", T, "input"), port("out", T, "output")],
            )],
            vec![wire("self.in", "wait.in"), wire("wait.out", "self.out")],
        )
    };
    let refused = try_start(&core(make(0), Storage::default()));
    assert_eq!(refused["state"]["status"], "failed", "{refused}");
    assert!(refused["state"]["reason"]
        .as_str()
        .unwrap()
        .contains("max_pending"));

    let core = core(make(1), Storage::default());
    let id = start(&core);
    send(&core, &id, "in", json!("kept"));
    send(&core, &id, "in", json!("dropped"));
    assert_eq!(until(&core, &id, "out", 1), [json!("kept")]);
    let errors = failures(&core, &id);
    assert!(
        errors.len() == 1 && errors[0].contains("dropped"),
        "{errors:?}"
    );
}

#[test]
fn a_barrier_with_nothing_to_wait_for_does_not_start() {
    let lonely = profile(
        vec![port("out", J, "output")],
        vec![node(
            "join",
            "join",
            json!({}),
            vec![port("out", J, "output")],
        )],
        vec![wire("join.out", "self.out")],
    );
    let refused = try_start(&core(lonely, Storage::default()));
    assert_eq!(refused["state"]["status"], "failed", "{refused}");
    assert!(refused["state"]["reason"]
        .as_str()
        .unwrap()
        .contains("barrier needs"));
}

#[test]
fn a_loop_has_one_run_that_a_new_start_cuts_and_a_restart_keeps() {
    let make = || {
        let ports = vec![
            port("in", T, "input"),
            port("back", T, "input"),
            port("body", T, "output"),
            port("done", T, "output"),
            port("exhausted", T, "output"),
        ];
        let wires = ["in", "back"]
            .iter()
            .map(|p| wire(&format!("self.{p}"), &format!("loop.{p}")))
            .chain(
                ["body", "done", "exhausted"]
                    .iter()
                    .map(|p| wire(&format!("loop.{p}"), &format!("self.{p}"))),
            )
            .collect();
        profile(
            ports.clone(),
            vec![node("loop", "loop", json!({ "max": 3 }), ports)],
            wires,
        )
    };
    let home = temp_home("loop");
    let storage = || Storage {
        store: Some(Arc::new(
            Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
        )),
        ..Storage::default()
    };
    let core = core(make(), storage());
    let id = start(&core);
    send(&core, &id, "in", json!("first"));
    send(&core, &id, "in", json!("second"));
    assert_eq!(settled(&core, &id, "exhausted"), [json!("first")], "cut");
    send(&core, &id, "back", json!("turn 2"));
    settled(&core, &id, "body");
    core.shutdown();
    drop(core);

    let core = self::core(make(), storage());
    assert_eq!(core.recover(), [format!("{id} resumed (1 checkpoints)")]);
    send(&core, &id, "back", json!("turn 3"));
    send(&core, &id, "back", json!("over"));
    assert_eq!(
        settled(&core, &id, "exhausted").last(),
        Some(&json!("over"))
    );
    assert!(
        failures(&core, &id).is_empty(),
        "{:?}",
        failures(&core, &id)
    );
}

#[test]
fn a_barrier_with_a_timeout_sends_what_came_and_starts_over() {
    let p = profile(
        vec![
            port("a", T, "input"),
            port("b", T, "input"),
            port("out", J, "output"),
            port("late", J, "output"),
        ],
        vec![node(
            "join",
            "join",
            json!({ "shape": "object", "timeout_ms": 150 }),
            vec![
                port("a", T, "input"),
                port("b", T, "input"),
                port("out", J, "output"),
                port("timeout", J, "output"),
            ],
        )],
        vec![
            wire("self.a", "join.a"),
            wire("self.b", "join.b"),
            wire("join.out", "self.out"),
            wire("join.timeout", "self.late"),
        ],
    );
    let core = core(p, Storage::default());
    let id = start(&core);
    // A round that opens in time sends on `out` only.
    send(&core, &id, "a", json!("1"));
    send(&core, &id, "b", json!("2"));
    assert_eq!(settled(&core, &id, "out"), [json!({ "a": "1", "b": "2" })]);
    // One that does not sends what came, `null` for the input that never did.
    send(&core, &id, "a", json!("lonely"));
    assert_eq!(
        until(&core, &id, "late", 1),
        [json!({ "a": "lonely", "b": null })]
    );
    // The barrier is empty again: the next pair opens it normally.
    send(&core, &id, "b", json!("x"));
    send(&core, &id, "a", json!("y"));
    assert_eq!(settled(&core, &id, "out").len(), 2);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        emitted(&core, &id, "self.late").len(),
        1,
        "nothing else ran out"
    );
    no_failures(&core, &id);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
}
