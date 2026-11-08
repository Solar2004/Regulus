//! The Agent box in a "then" chain, against a real core and a fake engine: prompts in
//! through `workflow.send`, replies out through the workflow's own outputs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_contract_agent_engine as engine;
use regulus_core::{parse_manifest, Core, Module, ModuleCtx, ModuleError, ProfileStore, Storage};
use regulus_module_agent::Agent;
use regulus_proto::{method, BusEventKind, EventPage, Manifest, NodeCall, Profile};
use serde_json::{json, Value};

const FAKE: &str = "t/fake-engine@1.0.0";

/// What the fake engine saw: sessions opened, sessions stopped.
#[derive(Default)]
struct Seen {
    started: Vec<String>,
    stopped: Vec<String>,
}

/// An engine that answers `<session>:<turn in that session>:<text>`, fails on `boom` and
/// reports usage on `count`.
struct FakeEngine {
    manifest: Manifest,
    seen: Arc<Mutex<Seen>>,
    turns: Mutex<Vec<(String, u32)>>,
}

impl Module for FakeEngine {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params).unwrap();
        match name {
            engine::SESSION_START => {
                let mut seen = self.seen.lock().unwrap();
                let id = format!("s{}", seen.started.len() + 1);
                seen.started.push(id.clone());
                Ok(json!({ "session_id": id, "agent": { "name": "fake", "version": "1" } }))
            }
            engine::SESSION_PROMPT => {
                let args: engine::PromptArgs = serde_json::from_value(call.args).unwrap();
                if args.text == "boom" {
                    return Err(ModuleError::Failed("the engine blew up".into()));
                }
                let mut turns = self.turns.lock().unwrap();
                let n = match turns.iter_mut().find(|(s, _)| *s == args.session_id) {
                    Some((_, n)) => {
                        *n += 1;
                        *n
                    }
                    None => {
                        turns.push((args.session_id.clone(), 1));
                        1
                    }
                };
                let text = format!("{}:{n}:{}", args.session_id, args.text);
                let mut turn = json!({ "stop_reason": "end_turn", "text": text });
                if args.text == "count" {
                    turn["usage"] = json!({ "inputTokens": 12, "outputTokens": 3 });
                }
                Ok(turn)
            }
            engine::SESSION_STOP => {
                let args: engine::StopArgs = serde_json::from_value(call.args).unwrap();
                self.seen.lock().unwrap().stopped.push(args.session_id);
                Ok(Value::Null)
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn port(id: &str, capability: &str, direction: &str, kind: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
}

/// `self.go → agent.prompt`, `agent.reply|failed|usage → self.<same>`, the fake engine
/// wired into `engine`.
fn profile(fresh: &str) -> Profile {
    let outs = [
        ("reply", "text@1"),
        ("failed", "text@1"),
        ("usage", "json@1"),
    ];
    let mut own = vec![port("go", "text@1", "input", "then")];
    let mut ports = vec![
        port("engine", "agent.engine@1", "input", "uses"),
        port("prompt", "text@1", "input", "then"),
    ];
    let mut wires = vec![
        json!({ "id": "w-go", "kind": "then", "from": { "node_id": "self", "port_id": "go" },
                "to": { "node_id": "a", "port_id": "prompt" } }),
        json!({ "id": "w-engine", "kind": "uses", "from": { "node_id": "e", "port_id": "engine" },
                "to": { "node_id": "a", "port_id": "engine" } }),
    ];
    for (id, capability) in outs {
        own.push(port(id, capability, "output", "then"));
        ports.push(port(id, capability, "output", "then"));
        wires.push(json!({ "id": format!("w-{id}"), "kind": "then",
            "from": { "node_id": "a", "port_id": id }, "to": { "node_id": "self", "port_id": id } }));
    }
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "ports": own, "wires": wires, "nodes": [
            { "id": "a", "type": "agent", "module_id": "regulus/agent@0.1.0",
              "config": { "fresh_context": fresh, "workdir": "/" }, "ports": ports },
            { "id": "e", "type": "engine", "module_id": FAKE,
              "ports": [port("engine", "agent.engine@1", "output", "uses")] },
        ]}]
    }))
    .unwrap()
}

fn data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-agent-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn start(fresh: &str, data: &Path) -> (Arc<Core>, String, Arc<Mutex<Seen>>) {
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let fake = FakeEngine {
        manifest: parse_manifest(&format!(
            "[module]\nid = \"{FAKE}\"\nname = \"fake\"\n[[node]]\ntype = \"engine\"\nname = \"e\"\n"
        ))
        .unwrap(),
        seen: seen.clone(),
        turns: Mutex::default(),
    };
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Agent::new()), Box::new(fake)];
    let mut profiles = ProfileStore::default();
    profiles.insert(profile(fresh)).unwrap();
    let storage = Storage {
        data_dir: Some(data.to_path_buf()),
        ..Storage::default()
    };
    let core = Core::build_with(modules, &[], profiles, storage);
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    let id = started["id"].as_str().unwrap().to_string();
    (core, id, seen)
}

fn send(core: &Core, id: &str, text: &str) {
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": "go", "value": text }),
    )
    .unwrap();
}

/// What left the workflow's outputs, `(port, value)`, once `count` of them did.
fn outputs(core: &Core, id: &str, count: usize) -> Vec<(String, Value)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        let out: Vec<(String, Value)> = page
            .events
            .into_iter()
            .filter(|e| e.from.node_id == "self" && e.from.port_id != "go")
            .filter_map(|e| match e.kind {
                BusEventKind::Emitted { value } => Some((e.from.port_id, value)),
                _ => None,
            })
            .collect();
        if out.len() >= count {
            return out;
        }
        assert!(Instant::now() < deadline, "only {out:?} came out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn reply(text: &str) -> (String, Value) {
    ("reply".into(), json!(text))
}

#[test]
fn prompts_queue_in_order_and_keep_their_session() {
    let data = data_dir("keep");
    let (core, id, seen) = start("no", &data);
    for text in ["one", "two", "three"] {
        send(&core, &id, text);
    }
    assert_eq!(
        outputs(&core, &id, 3),
        [reply("s1:1:one"), reply("s1:2:two"), reply("s1:3:three")]
    );
    // The method still answers, in the same session.
    let turn = core
        .dispatch(
            "agent.prompt",
            json!({ "execution_id": id, "node_id": "a", "text": "rpc" }),
        )
        .unwrap();
    assert_eq!(turn["text"], "s1:4:rpc");
    assert_eq!(seen.lock().unwrap().started, ["s1"]);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert_eq!(seen.lock().unwrap().stopped, ["s1"]);
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn fresh_context_opens_a_session_per_prompt() {
    let data = data_dir("fresh");
    let (core, id, seen) = start("yes", &data);
    send(&core, &id, "one");
    send(&core, &id, "two");
    // The session opened at start serves the first prompt; each later one gets its own.
    assert_eq!(
        outputs(&core, &id, 2),
        [reply("s1:1:one"), reply("s2:1:two")]
    );
    let turn = core
        .dispatch(
            "agent.prompt",
            json!({ "execution_id": id, "node_id": "a", "text": "rpc" }),
        )
        .unwrap();
    assert_eq!(turn["text"], "s3:1:rpc");
    {
        let seen = seen.lock().unwrap();
        assert_eq!(seen.started, ["s1", "s2", "s3"]);
        assert_eq!(seen.stopped, ["s1", "s2"]);
    }
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert_eq!(seen.lock().unwrap().stopped, ["s1", "s2", "s3"]);
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn a_failed_prompt_goes_to_failed_only_and_usage_comes_before_the_reply() {
    let data = data_dir("failed");
    let (core, id, _) = start("no", &data);
    send(&core, &id, "boom");
    send(&core, &id, "count");
    let out = outputs(&core, &id, 3);
    assert_eq!(out[0].0, "failed");
    let error = out[0].1.as_str().unwrap();
    assert!(
        error.contains("agent `a`") && error.contains("blew up"),
        "{error}"
    );
    assert_eq!(
        out[1],
        (
            "usage".into(),
            json!({ "input_tokens": 12, "output_tokens": 3 })
        )
    );
    assert_eq!(out[2], reply("s1:1:count"));
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = std::fs::remove_dir_all(&data);
}
