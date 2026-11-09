//! The Decider against a real core: another box asks over a "uses" wire, the question
//! waits in `human.list`, a person answers it and the box reads the choice.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_contract_decision as decision;
use regulus_core::config::StoreConfig;
use regulus_core::{parse_manifest, Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore};
use regulus_core::{Storage, Store};
use regulus_module_human::{Human, ANSWER, LIST};
use regulus_proto::{method, Manifest, Profile};
use serde_json::{json, Value};

const ASKER: &str = "t/asker@1.0.0";

/// A box that asks its wired decider: `asker.ask` { execution_id, args } and
/// `asker.poll` { execution_id, id } go to whatever is wired into `decisions`.
struct Asker {
    manifest: Manifest,
    ctx: Mutex<ModuleCtx>,
}

impl Module for Asker {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        *self.ctx.lock().unwrap() = ctx.clone();
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let (method, args) = match name {
            "asker.ask" => (decision::ASK, params["args"].clone()),
            "asker.poll" => (decision::POLL, json!({ "id": params["id"] })),
            _ => return Ok(Value::Null),
        };
        let ctx = self.ctx.lock().unwrap().clone();
        let me = NodeRef {
            execution_id: params["execution_id"].as_str().unwrap().into(),
            node_id: "a".into(),
        };
        let wired = ctx.resolve(&me, "decisions");
        ctx.call(&wired[0], method, args)
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn profile(config: Value) -> Profile {
    serde_json::from_value(json!({
        "id": "p", "project_id": "proj", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "nodes": [
            { "id": "d", "type": "decider", "module_id": "regulus/human@0.1.0", "config": config,
              "ports": [{ "id": "decisions", "capability": "decision@1",
                          "direction": "output", "kind": "uses" }] },
            { "id": "a", "type": "asker", "module_id": ASKER,
              "ports": [{ "id": "decisions", "capability": "decision@1",
                          "direction": "input", "kind": "uses" }] }
        ], "wires": [{ "id": "w", "kind": "uses",
            "from": { "node_id": "d", "port_id": "decisions" },
            "to": { "node_id": "a", "port_id": "decisions" } }] }]
    }))
    .unwrap()
}

fn home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-decider-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn start(config: Value, name: &str) -> (Arc<Core>, String) {
    let home = home(name);
    let storage = Storage {
        store: Some(Arc::new(
            Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
        )),
        data_dir: Some(home.join("data")),
        ..Storage::default()
    };
    let asker = Asker {
        manifest: parse_manifest(&format!(
            "[module]\nid = \"{ASKER}\"\nname = \"asker\"\nmethods = [\"asker.ask\", \"asker.poll\"]\n[[node]]\ntype = \"asker\"\nname = \"a\"\n"
        ))
        .unwrap(),
        ctx: Mutex::default(),
    };
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Human::new()), Box::new(asker)];
    let mut profiles = ProfileStore::default();
    profiles.insert(profile(config)).unwrap();
    let core = Core::build_with(modules, &[], profiles, storage);
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    let id = started["id"].as_str().unwrap().to_string();
    (core, id)
}

fn options(with_reject: bool) -> Value {
    let mut options = vec![
        json!({ "id": "yes", "name": "Allow", "kind": "allow_once" }),
        json!({ "id": "always", "name": "Always allow", "kind": "allow_always" }),
    ];
    if with_reject {
        options.push(json!({ "id": "no", "name": "Reject", "kind": "reject_once" }));
    }
    json!(options)
}

fn ask(core: &Core, execution: &str, with_reject: bool, timeout_ms: Option<u64>) -> String {
    let args = json!({ "prompt": "Let the agent write a file?", "options": options(with_reject),
                       "context": { "tool": "edit" }, "timeout_ms": timeout_ms });
    let asked = core
        .dispatch(
            "asker.ask",
            json!({ "execution_id": execution, "args": args }),
        )
        .unwrap();
    asked["id"].as_str().unwrap().to_string()
}

fn poll(core: &Core, execution: &str, id: &str) -> decision::Poll {
    serde_json::from_value(
        core.dispatch("asker.poll", json!({ "execution_id": execution, "id": id }))
            .unwrap(),
    )
    .unwrap()
}

fn answer(core: &Core, id: &str, decision: &str, option: Option<&str>) -> Result<Value, String> {
    let fields = option.map_or(json!({}), |o| json!({ "option": o }));
    core.dispatch(
        ANSWER,
        json!({ "id": id, "decision": decision, "fields": fields }),
    )
    .map_err(|e| e.message)
}

#[test]
fn a_question_waits_in_the_list_and_the_answer_is_read_back() {
    let (core, execution) = start(json!({}), "approve");
    let id = ask(&core, &execution, true, None);
    let listed: Vec<Value> =
        serde_json::from_value(core.dispatch(LIST, json!({})).unwrap()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0]["kind"], "decision");
    assert_eq!(listed[0]["prompt"], "Let the agent write a file?");
    assert_eq!(
        listed[0]["form"]["properties"]["option"]["enum"],
        json!(["yes", "always", "no"])
    );
    assert!(poll(&core, &execution, &id).pending);

    let wrong = answer(&core, &id, "approve", Some("maybe")).unwrap_err();
    assert!(wrong.contains("must be one of"), "{wrong}");
    let missing = answer(&core, &id, "approve", None).unwrap_err();
    assert!(missing.contains("option"), "{missing}");
    assert!(
        poll(&core, &execution, &id).pending,
        "a wrong answer leaves it waiting"
    );

    let done = answer(&core, &id, "approve", Some("always")).unwrap();
    assert_eq!(done["port"], "decisions");
    assert_eq!(
        poll(&core, &execution, &id),
        decision::Poll {
            pending: false,
            option: Some("always".into()),
            timed_out: false
        }
    );
    let listed: Vec<Value> =
        serde_json::from_value(core.dispatch(LIST, json!({})).unwrap()).unwrap();
    assert!(listed.is_empty());
    assert!(
        answer(&core, &id, "approve", Some("yes")).is_err(),
        "decided once"
    );
}

#[test]
fn a_refusal_picks_the_reject_option_or_none() {
    let (core, execution) = start(json!({}), "reject");
    let id = ask(&core, &execution, true, None);
    answer(&core, &id, "reject", None).unwrap();
    assert_eq!(poll(&core, &execution, &id).option.as_deref(), Some("no"));
    let id = ask(&core, &execution, false, None);
    answer(&core, &id, "reject", None).unwrap();
    let poll = poll(&core, &execution, &id);
    assert!(!poll.pending && poll.option.is_none() && !poll.timed_out);
}

fn wait_decided(core: &Core, execution: &str, id: &str) -> decision::Poll {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let poll = poll(core, execution, id);
        if !poll.pending {
            return poll;
        }
        assert!(Instant::now() < deadline, "never timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn nobody_deciding_in_time_gives_the_default_or_nothing() {
    let (core, execution) = start(json!({ "default_option": "yes" }), "default");
    let id = ask(&core, &execution, true, Some(100));
    assert_eq!(
        wait_decided(&core, &execution, &id),
        decision::Poll {
            pending: false,
            option: Some("yes".into()),
            timed_out: true
        }
    );
    // The field's limit, when the asker gives none; no default: no option.
    let (core, execution) = start(json!({ "timeout_ms": 100 }), "no-default");
    let id = ask(&core, &execution, true, None);
    let poll = wait_decided(&core, &execution, &id);
    assert!(poll.timed_out && poll.option.is_none());
}

#[test]
fn an_unknown_decision_is_an_error() {
    let (core, execution) = start(json!({}), "unknown");
    let error = core
        .dispatch(
            "asker.poll",
            json!({ "execution_id": execution, "id": "h-nope" }),
        )
        .unwrap_err();
    assert!(error.message.contains("no decision"), "{}", error.message);
}
