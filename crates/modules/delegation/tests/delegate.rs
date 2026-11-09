//! The `delegate` node against a real core, with the hello greeter as a worker: kept
//! workers with stable ids, results buffered per worker, follow-ups, closing one, a group
//! in a channel, and the parent's stop ending the rest.

use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore};
use regulus_module_delegation::Delegation;
use regulus_module_hello::Hello;
use regulus_proto::{method, Execution, ExecutionTree, Profile};
use serde_json::{json, Value};

const MODULE: &str = "regulus/delegation@0.1.0";

fn port(id: &str, capability: &str, direction: &str, kind: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
}

fn wire(id: &str, kind: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": kind,
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

/// `main` holds `team`, a delegate whose workers greet (`greeter`); `self.briefs` creates
/// workers and `created`, `result` and `closed` leave as they are.
fn profile(config: Value) -> Profile {
    let json_port = |id: &str, direction: &str| port(id, "json@1", direction, "then");
    let text = |id: &str, direction: &str| port(id, "text@1", direction, "then");
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main",
              "ports": [json_port("briefs", "input"), json_port("created", "output"),
                        json_port("result", "output"), json_port("closed", "output")],
              "nodes": [{ "id": "team", "type": "delegate", "module_id": MODULE,
                          "config": config,
                          "ports": [port("tools", "tool@1", "output", "uses"),
                                    port("participant", "participant@1", "output", "uses"),
                                    json_port("create", "input"), json_port("created", "output"),
                                    json_port("result", "output"), json_port("closed", "output")] }],
              "wires": [wire("a", "then", ("self", "briefs"), ("team", "create")),
                        wire("b", "then", ("team", "created"), ("self", "created")),
                        wire("c", "then", ("team", "result"), ("self", "result")),
                        wire("d", "then", ("team", "closed"), ("self", "closed"))] },
            { "id": "greeter", "kind": "extra",
              "ports": [text("prompt", "input"), text("reply", "output")],
              "nodes": [{ "id": "hello", "type": "greeter", "module_id": "regulus/hello@0.1.0",
                          "ports": [text("echo", "input"), text("reply", "output")] }],
              "wires": [wire("a", "then", ("self", "prompt"), ("hello", "echo")),
                        wire("b", "then", ("hello", "reply"), ("self", "reply"))] },
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
    fn start(config: Value) -> Self {
        let profile = profile(config);
        let mut profiles = ProfileStore::default();
        profiles.insert(profile.clone()).unwrap();
        let modules: Vec<Box<dyn Module>> =
            vec![Box::new(Delegation::new()), Box::new(Hello::new())];
        let core = Core::build(modules, &[], profiles);
        let started = core
            .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
            .unwrap();
        let id = started["id"].as_str().unwrap().to_string();
        Self { core, id, profile }
    }

    fn method(&self, name: &str, args: Value) -> Result<Value, String> {
        let mut params = json!({ "execution_id": self.id, "node_id": "team" });
        params
            .as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        self.core.dispatch(name, params).map_err(|e| e.message)
    }

    /// A call to the node itself, as the Tools box or a channel makes it.
    fn on_node(&self, name: &str, args: Value) -> Value {
        let node = &self.profile.workflows[0].nodes[0];
        let call = json!({ "execution_id": self.id, "node": node, "args": args });
        self.core
            .registry()
            .call_module(MODULE, name, call)
            .unwrap()
    }

    fn tool(&self, name: &str, args: Value) -> Value {
        self.on_node(
            "tool.call",
            json!({ "name": name, "args": args, "caller": { "name": "boss" } }),
        )
    }

    fn live_children(&self) -> Vec<Execution> {
        let tree: ExecutionTree = serde_json::from_value(
            self.core
                .dispatch(method::EXECUTION_TREE, json!({ "execution_id": self.id }))
                .unwrap(),
        )
        .unwrap();
        tree.children
            .into_iter()
            .map(|c| c.execution)
            .filter(|e| e.state.is_live())
            .collect()
    }

    fn emitted(&self, port: &str) -> Vec<Value> {
        let page = self
            .core
            .dispatch(method::EVENT_LIST, json!({ "execution_id": self.id }))
            .unwrap();
        page["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e["kind"] == "emitted"
                    && e["from"]["node_id"] == "self"
                    && e["from"]["port_id"] == port
            })
            .map(|e| e["value"].clone())
            .collect()
    }
}

fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn workers_are_created_followed_up_and_closed_one_by_one() {
    let run = Running::start(json!({ "workers": { "greeter": "greeter" }, "max_workers": 2 }));
    let one = run
        .method("delegation.create", json!({ "brief": "Ana" }))
        .unwrap();
    let two = run
        .method("delegation.create", json!({ "brief": "Bo", "name": "bo" }))
        .unwrap();
    assert_eq!(
        (&one["worker"], &two["worker"]),
        (&json!("w-1"), &json!("w-2"))
    );
    assert_eq!(one["kind"], "greeter");
    let full = run
        .method("delegation.create", json!({ "brief": "Cy" }))
        .unwrap_err();
    assert!(full.contains("max_workers"), "{full}");

    let first = run
        .method(
            "delegation.result",
            json!({ "worker": "w-1", "wait_ms": 5000 }),
        )
        .unwrap();
    assert_eq!(
        first,
        json!({ "worker": "w-1", "port": "reply", "value": "Hello, Ana!" })
    );
    // By name too.
    let second = run
        .method(
            "delegation.result",
            json!({ "worker": "bo", "wait_ms": 5000 }),
        )
        .unwrap();
    assert_eq!(second["value"], "Hello, Bo!");
    let none = run
        .method("delegation.result", json!({ "worker": "w-1" }))
        .unwrap();
    assert_eq!(none["pending"], true, "{none}");

    let closed = run
        .method("delegation.close", json!({ "worker": "w-1" }))
        .unwrap();
    assert_eq!(closed["state"], "closed", "{closed}");
    until("one live child", || run.live_children().len() == 1);
    assert_eq!(run.live_children()[0].tag.as_deref(), Some("w-2"));

    run.method(
        "delegation.send",
        json!({ "worker": "w-2", "text": "again" }),
    )
    .unwrap();
    let follow_up = run
        .method(
            "delegation.result",
            json!({ "worker": "w-2", "wait_ms": 5000 }),
        )
        .unwrap();
    assert_eq!(follow_up["value"], "Hello, again!");
    let refused = run
        .method("delegation.send", json!({ "worker": "w-1", "text": "x" }))
        .unwrap_err();
    assert!(refused.contains("not running"), "{refused}");

    // Room again after a close.
    run.method("delegation.create", json!({ "brief": "Cy" }))
        .unwrap();
    let list = run.method("delegation.workers", json!({})).unwrap();
    let states: Vec<(&str, &str)> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["worker"].as_str().unwrap(), w["state"].as_str().unwrap()))
        .collect();
    assert_eq!(
        states,
        [("w-1", "closed"), ("w-2", "running"), ("w-3", "running")]
    );
    until("the outputs", || run.emitted("result").len() == 4);
    assert_eq!(run.emitted("created").len(), 3);
    until("closed", || run.emitted("closed").len() == 1);

    run.core
        .dispatch(method::EXECUTION_STOP, json!({ "id": run.id }))
        .unwrap();
    assert!(run.live_children().is_empty());
}

#[test]
fn an_agent_drives_the_workers_through_tools() {
    let run = Running::start(json!({ "workers": { "greeter": "greeter" } }));
    let names: Vec<String> = run
        .on_node("tool.list", Value::Null)
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        names,
        [
            "worker_create",
            "worker_status",
            "worker_result",
            "worker_send",
            "worker_close",
            "worker_list"
        ]
    );
    let created = run.tool("worker_create", json!({ "brief": "Ana" }));
    assert_eq!(created["data"]["worker"], "w-1", "{created}");
    let result = run.tool("worker_result", json!({ "worker": "w-1", "wait_ms": 5000 }));
    assert_eq!(result["data"]["value"], "Hello, Ana!", "{result}");
    let status = run.tool("worker_status", json!({ "worker": "w-1" }));
    assert_eq!(status["data"]["state"], "running");
    assert_eq!(status["data"]["pending_results"], 0);
    let wrong = run.tool("worker_create", json!({ "brief": "x", "kind": "nope" }));
    assert_eq!(wrong["error"], true, "{wrong}");
    let closed = run.tool("worker_close", json!({ "worker": "w-1" }));
    assert_eq!(closed["data"]["state"], "closed", "{closed}");
}

#[test]
fn a_channel_reaches_the_mentioned_worker_or_all() {
    let run = Running::start(json!({ "workers": { "greeter": "greeter" }, "name": "crew" }));
    let identity = run.on_node("participant.describe", Value::Null);
    assert_eq!(
        identity,
        json!({ "name": "crew", "kind": "group", "state": "idle" })
    );
    for brief in ["Ana", "Bo"] {
        run.method("delegation.create", json!({ "brief": brief }))
            .unwrap();
    }
    for worker in ["w-1", "w-2"] {
        run.method(
            "delegation.result",
            json!({ "worker": worker, "wait_ms": 5000 }),
        )
        .unwrap();
    }
    let reply = run.on_node(
        "participant.deliver",
        json!({ "channel": "c", "from": "boss", "text": "@w-2, hi" }),
    );
    assert_eq!(reply, json!({ "text": "delivered to w-2" }));
    let got = run
        .method(
            "delegation.result",
            json!({ "worker": "w-2", "wait_ms": 5000 }),
        )
        .unwrap();
    assert!(
        got["value"]
            .as_str()
            .unwrap()
            .contains("[message from @boss in channel `c`]\n@w-2, hi"),
        "{got}"
    );
    let all = run.on_node(
        "participant.deliver",
        json!({ "channel": "c", "from": "boss", "text": "everyone" }),
    );
    assert_eq!(all, json!({ "text": "delivered to w-1, w-2" }));
}

#[test]
fn briefs_on_create_start_one_worker_each() {
    let run = Running::start(json!({ "workers": { "greeter": "greeter" } }));
    run.core
        .dispatch(
            method::WORKFLOW_SEND,
            json!({ "execution_id": run.id, "port": "briefs",
                    "value": ["Ana", { "brief": "Bo", "name": "bo" }] }),
        )
        .unwrap();
    until("two results", || run.emitted("result").len() == 2);
    let created = run.emitted("created");
    assert_eq!(created.len(), 2);
    assert_eq!(created[1]["name"], "bo");
}

#[test]
fn a_checkpoint_brings_the_workers_back_as_interrupted() {
    let delegation = Delegation::new();
    let node = json!({ "id": "team", "type": "delegate", "module_id": MODULE });
    let saved = json!({ "next": 2, "workers": [
        { "worker": "w-1", "kind": "k", "name": "w-1", "execution": "exec-2", "state": "closed" },
        { "worker": "w-2", "kind": "k", "name": "w-2", "execution": "exec-3", "state": "running" },
    ] });
    delegation
        .call(
            method::NODE_START,
            json!({ "execution_id": "exec-1", "node": node, "args": { "checkpoint": saved } }),
        )
        .unwrap();
    let list = delegation
        .call(
            "delegation.workers",
            json!({ "execution_id": "exec-1", "node_id": "team" }),
        )
        .unwrap();
    assert_eq!(list[0]["state"], "closed");
    assert_eq!(list[1]["state"], "interrupted");
    assert_eq!(list[1]["execution"], "exec-3");
}

#[test]
fn the_example_is_a_profile_with_a_coordinator_and_a_worker() {
    let example: Profile = toml::from_str(include_str!("../examples/delegate.toml")).unwrap();
    let ids: Vec<&str> = example.workflows.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(ids, ["main", "worker"]);
    let team = example.workflows[0]
        .nodes
        .iter()
        .find(|n| n.id == "team")
        .unwrap();
    assert_eq!(team.config["workers"], json!({ "worker": "worker" }));
}
