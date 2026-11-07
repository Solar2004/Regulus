//! The tasks module against a real core with a store: the example `examples/queue.toml`
//! end to end, a lease that runs out, and a queue that outlives its core.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::config::StoreConfig;
use regulus_core::{Core, Module, ProfileStore, Storage, Store};
use regulus_module_tasks::{Tasks, LEDGER, LIST};
use regulus_proto::{method, BusEventKind, Diagnostic, EventPage, Profile};
use serde_json::{json, Value};

const EXAMPLE: &str = include_str!("../examples/queue.toml");

fn home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-tasks-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn core(profile: Profile, home: &Path) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Tasks::new())];
    let storage = Storage {
        store: Some(Arc::new(
            Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
        )),
        data_dir: Some(home.join("data")),
        ..Storage::default()
    };
    Core::build_with(modules, &[], profiles, storage)
}

fn start(core: &Core, profile: &str) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": profile }))
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

/// Everything that reached the workflow's outputs so far, waiting for at least `count`.
fn out(core: &Core, id: &str, count: usize) -> Vec<(String, Value)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        let got: Vec<(String, Value)> = page
            .events
            .into_iter()
            .filter(|e| e.from.node_id == "self")
            .filter_map(|e| match e.kind {
                BusEventKind::Emitted { value } => Some((e.from.port_id, value)),
                _ => None,
            })
            .filter(|(port, _)| {
                ["task", "done", "drained", "ledger", "error"].contains(&port.as_str())
            })
            .collect();
        if got.len() >= count {
            return got;
        }
        assert!(Instant::now() < deadline, "only {got:?} came out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_example_claims_in_dependency_order_and_keeps_a_ledger() {
    let home = home("example");
    let profile: Profile = toml::from_str(EXAMPLE).unwrap();
    let core = core(profile, &home);
    let check: Vec<Diagnostic> = serde_json::from_value(
        core.dispatch(method::PROFILE_CHECK, json!({ "id": "queue" }))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(check, []);
    let id = start(&core, "queue");
    send(
        &core,
        &id,
        "add",
        json!([{ "id": "build", "title": "Build", "deps": ["setup"] },
               { "id": "setup", "title": "Set up" }, "Write the docs"]),
    );
    send(&core, &id, "plan", json!("Set up, then build"));
    let mut claimed = Vec::new();
    for (n, result) in ["ok", "built", "written"].iter().enumerate() {
        send(&core, &id, "next", json!(""));
        let got = out(&core, &id, 2 * n + 1);
        claimed.push(got[2 * n].1["id"].as_str().unwrap().to_string());
        send(&core, &id, "complete", json!(result));
        let got = out(&core, &id, 2 * n + 2);
        assert_eq!(got[2 * n + 1].0, "done");
        assert_eq!(got[2 * n + 1].1["result"], json!(result));
    }
    assert_eq!(claimed, ["setup", "build", "t3"]);
    let got = out(&core, &id, 7);
    assert_eq!(got[6].0, "drained");
    assert_eq!(got[6].1["done"], 3);
    send(&core, &id, "read", json!(""));
    let ledger = out(&core, &id, 8)[7].1.as_str().unwrap().to_string();
    assert_eq!(
        ledger,
        "## Plan\n- Set up, then build\n\n## Progress\n- Set up [done]: ok\n\
         - Build [done]: built\n- Write the docs [done]: written"
    );
    let listed = core
        .dispatch(
            LIST,
            json!({ "execution_id": id, "node_id": "claim", "queue": "example" }),
        )
        .unwrap();
    assert_eq!(listed["counts"]["done"], 3);
    let read = core
        .dispatch(LEDGER, json!({ "execution_id": id, "node_id": "ledger" }))
        .unwrap();
    assert_eq!(read["text"], ledger);
    // Nothing is left: another `next` waits, and a completion with nothing held fails.
    send(&core, &id, "next", json!(""));
    assert!(core
        .dispatch(
            method::WORKFLOW_SEND,
            json!({ "execution_id": id, "port": "complete", "value": "x" })
        )
        .is_ok());
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
}

fn claim_profile(lease_ms: u64) -> Profile {
    let port = |id: &str, capability: &str, direction: &str| json!({ "id": id, "capability": capability, "direction": direction, "kind": "then" });
    let node = |id: &str, kind: &str, config: Value, ports: Vec<Value>| {
        json!({ "id": id, "type": kind, "module_id": "regulus/tasks@0.1.0",
                "config": config, "ports": ports })
    };
    let wire = |id: &str, from: (&str, &str), to: (&str, &str)| {
        json!({ "id": id, "kind": "then",
                "from": { "node_id": from.0, "port_id": from.1 },
                "to": { "node_id": to.0, "port_id": to.1 } })
    };
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "ports": [port("add", "text@1", "input"), port("task", "text@1", "output"),
                      port("expired", "json@1", "output")],
            "nodes": [
                node("queue", "queue", json!({ "queue": "leased" }),
                     vec![port("add", "text@1", "input")]),
                node("claim", "claim",
                     json!({ "queue": "leased", "auto": "yes", "lease_ms": lease_ms,
                             "on_stop": "keep" }),
                     vec![port("task", "text@1", "output"), port("expired", "json@1", "output")]),
            ],
            "wires": [wire("a", ("self", "add"), ("queue", "add")),
                      wire("t", ("claim", "task"), ("self", "task")),
                      wire("e", ("claim", "expired"), ("self", "expired"))] }]
    }))
    .unwrap()
}

fn emitted(core: &Core, id: &str, port: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        let got: Vec<Value> = page
            .events
            .into_iter()
            .filter(|e| e.from.node_id == "self" && e.from.port_id == port)
            .filter_map(|e| match e.kind {
                BusEventKind::Emitted { value } => Some(value),
                _ => None,
            })
            .collect();
        if got.len() >= count {
            return got;
        }
        assert!(Instant::now() < deadline, "only {got:?} on {port}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_lease_runs_out_and_the_task_is_claimed_again() {
    let home = home("lease");
    let core = core(claim_profile(150), &home);
    let id = start(&core, "p");
    send(&core, &id, "add", json!("only one"));
    // `auto` claims it at once; the lease ends, it is offered again and claimed again.
    let tasks = emitted(&core, &id, "task", 2);
    assert_eq!(tasks[..2], [json!("only one"), json!("only one")]);
    let expired = emitted(&core, &id, "expired", 1);
    assert_eq!(expired[0]["id"], "t1");
    let listed = core
        .dispatch(
            LIST,
            json!({ "execution_id": id, "node_id": "claim", "queue": "leased" }),
        )
        .unwrap();
    assert!(listed["tasks"][0]["attempts"].as_u64().unwrap() >= 2);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_queue_outlives_its_core() {
    let home = home("restart");
    {
        let core = core(claim_profile(0), &home);
        let id = start(&core, "p");
        send(&core, &id, "add", json!("first\nsecond"));
        emitted(&core, &id, "task", 1);
        core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
            .unwrap();
        core.shutdown();
    }
    let core = core(claim_profile(0), &home);
    let id = start(&core, "p");
    // `first` is still held by the old execution (`on_stop = keep`, no lease): the new
    // claim takes `second`.
    assert_eq!(emitted(&core, &id, "task", 1)[0], json!("second"));
    let listed = core
        .dispatch(
            LIST,
            json!({ "execution_id": id, "node_id": "claim", "queue": "leased" }),
        )
        .unwrap();
    assert_eq!(listed["counts"]["claimed"], 2);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
}
