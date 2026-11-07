//! Approval and Review against a real core: values wait under an id, a decision by
//! `human.answer` or by `decide` sends them on, a deadline sends them to `timeout`, and a
//! restart of the core keeps them waiting.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::config::StoreConfig;
use regulus_core::{Core, Module, ProfileStore, Storage, Store};
use regulus_module_human::{Human, ANSWER, GET, LIST};
use regulus_proto::{method, BusEventKind, EventPage, Profile};
use serde_json::{json, Value};

const J: &str = "json@1";
const T: &str = "text@1";

fn port(id: &str, capability: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": "then",
            "cardinality": if direction == "input" { "many" } else { "one" } })
}

fn wire(from: &str, to: &str) -> Value {
    let split = |s: &str| {
        let (n, p) = s.split_once('.').unwrap();
        json!({ "node_id": n, "port_id": p })
    };
    json!({ "id": format!("{from}>{to}"), "kind": "then", "from": split(from), "to": split(to) })
}

/// One `kind` node `h` with `config`, its ports all leaving the workflow by the same id.
fn profile(kind: &str, cap: &str, config: Value, outs: &[&str]) -> Profile {
    let mut ports = vec![port("in", cap, "input"), port("decide", J, "input")];
    let mut node_ports = ports.clone();
    let mut wires = vec![wire("self.in", "h.in"), wire("self.decide", "h.decide")];
    for out in outs {
        let capability = if matches!(*out, "pending" | "decision") {
            J
        } else {
            cap
        };
        ports.push(port(out, capability, "output"));
        node_ports.push(port(out, capability, "output"));
        wires.push(wire(&format!("h.{out}"), &format!("self.{out}")));
    }
    serde_json::from_value(json!({
        "id": "p", "project_id": "proj", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "ports": ports, "wires": wires,
            "nodes": [{ "id": "h", "type": kind, "module_id": "regulus/human@0.1.0",
                        "config": config, "ports": node_ports }] }]
    }))
    .unwrap()
}

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-human-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn storage(home: &std::path::Path) -> Storage {
    Storage {
        store: Some(Arc::new(
            Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
        )),
        data_dir: Some(home.join("data")),
        ..Storage::default()
    }
}

fn core(profile: Profile, storage: Storage) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Human::new())];
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

/// What left the workflow on `self.<port>`, once at least `count` did.
fn out(core: &Core, id: &str, port: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        let values: Vec<Value> = page
            .events
            .into_iter()
            .filter(|e| e.from.node_id == "self" && e.from.port_id == port)
            .filter_map(|e| match e.kind {
                BusEventKind::Emitted { value } => Some(value),
                _ => None,
            })
            .collect();
        if values.len() >= count {
            return values;
        }
        assert!(Instant::now() < deadline, "self.{port}: {values:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn list(core: &Core) -> Vec<Value> {
    serde_json::from_value(core.dispatch(LIST, json!({})).unwrap()).unwrap()
}

#[test]
fn an_approval_waits_and_goes_on_with_the_form_merged() {
    let form = json!({ "type": "object", "properties": { "replicas": { "type": "integer" } },
                       "required": ["replicas"] })
    .to_string();
    let config = json!({ "prompt": "Deploy?", "form": form, "merge_fields": "yes" });
    let core = core(
        profile("approval", J, config, &["pending", "approved", "rejected"]),
        storage(&temp_home("approve")),
    );
    let id = start(&core);
    send(&core, &id, "in", json!({ "tag": "v2" }));
    let notice = &out(&core, &id, "pending", 1)[0];
    let pending = notice["id"].as_str().unwrap();
    assert_eq!(notice["prompt"], "Deploy?");
    assert_eq!(list(&core).len(), 1);
    assert_eq!(
        core.dispatch(GET, json!({ "id": pending })).unwrap()["value"],
        json!({ "tag": "v2" })
    );

    // The form is not filled: it stays pending.
    let error = core
        .dispatch(ANSWER, json!({ "id": pending, "decision": "approve" }))
        .unwrap_err();
    assert!(error.message.contains("replicas"), "{}", error.message);
    assert_eq!(list(&core).len(), 1);

    let answered = core
        .dispatch(
            ANSWER,
            json!({ "id": pending, "decision": "approve", "fields": { "replicas": "3" },
                    "text": "go" }),
        )
        .unwrap();
    assert_eq!(answered["port"], "approved");
    assert_eq!(
        out(&core, &id, "approved", 1),
        [json!({ "tag": "v2", "replicas": 3, "comment": "go" })]
    );
    assert!(list(&core).is_empty());
    assert!(core
        .dispatch(ANSWER, json!({ "id": pending, "decision": "reject" }))
        .is_err());
}

#[test]
fn decide_rejects_from_the_workflow_and_a_deadline_times_out() {
    let core = core(
        profile(
            "approval",
            T,
            json!({ "timeout_ms": 200 }),
            &["pending", "rejected", "timeout"],
        ),
        storage(&temp_home("decide")),
    );
    let id = start(&core);
    send(&core, &id, "in", json!("first"));
    let first = out(&core, &id, "pending", 1)[0]["id"].clone();
    send(
        &core,
        &id,
        "decide",
        json!({ "id": first, "decision": "reject" }),
    );
    assert_eq!(out(&core, &id, "rejected", 1), [json!("first")]);

    send(&core, &id, "in", json!("second"));
    assert_eq!(out(&core, &id, "timeout", 1), [json!("second")]);
    assert!(list(&core).is_empty());
}

#[test]
fn a_review_sends_the_edited_text() {
    let core = core(
        profile(
            "review",
            T,
            json!({ "prompt": "Check the spec" }),
            &["pending", "out", "rejected"],
        ),
        storage(&temp_home("review")),
    );
    let id = start(&core);
    send(&core, &id, "in", json!("draft"));
    let pending = out(&core, &id, "pending", 1)[0]["id"].clone();
    core.dispatch(
        ANSWER,
        json!({ "id": pending, "decision": "approve", "text": "edited" }),
    )
    .unwrap();
    assert_eq!(out(&core, &id, "out", 1), [json!("edited")]);
}

#[test]
fn a_pending_decision_outlives_a_restart_of_the_core() {
    let home = temp_home("restart");
    let make = || {
        profile(
            "approval",
            J,
            json!({ "timeout_ms": 60_000 }),
            &["pending", "approved", "timeout"],
        )
    };
    let first = core(make(), storage(&home));
    let id = start(&first);
    send(&first, &id, "in", json!({ "n": 1 }));
    let pending = out(&first, &id, "pending", 1)[0]["id"].clone();
    first.shutdown();
    drop(first);

    let second = core(make(), storage(&home));
    assert_eq!(second.recover(), [format!("{id} resumed (1 checkpoints)")]);
    let waiting = list(&second);
    assert_eq!(waiting.len(), 1);
    assert_eq!(waiting[0]["id"], pending);
    second
        .dispatch(ANSWER, json!({ "id": pending, "decision": "approve" }))
        .unwrap();
    assert_eq!(out(&second, &id, "approved", 1), [json!({ "n": 1 })]);
}
