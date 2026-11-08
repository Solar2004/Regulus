//! The Agent box in a "then" chain, against a real core and a fake engine: prompts in
//! through `workflow.send`, replies out through the workflow's own outputs.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use regulus_contract_agent_engine as engine;
use regulus_core::{parse_manifest, Core, Module, ModuleCtx, ModuleError, ProfileStore, Storage};
use regulus_module_agent::Agent;
use regulus_proto::{method, BusEventKind, EventPage, Manifest, NodeCall, Profile};
use serde_json::{json, Value};

const FAKE: &str = "t/fake-engine@1.0.0";

/// What the fake engine saw: sessions opened, sessions stopped, and `slow` turns that
/// began and that returned.
#[derive(Default)]
struct Seen {
    started: Vec<String>,
    /// The directory each session opened in, in the order they opened.
    cwds: Vec<PathBuf>,
    stopped: Vec<String>,
    slow_began: usize,
    slow_returned: usize,
    /// Lets every `slow` turn finish.
    open: bool,
}

/// The fake engine's state, with a condvar to wake `slow` turns.
#[derive(Default)]
struct Shared {
    seen: Mutex<Seen>,
    wake: Condvar,
}

impl Shared {
    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen.lock().unwrap()
    }

    fn open(&self) {
        self.seen().open = true;
        self.wake.notify_all();
    }

    /// Waits until `check` holds, or fails after a while.
    fn wait(&self, check: impl Fn(&Seen) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !check(&self.seen()) {
            assert!(Instant::now() < deadline, "the engine never got there");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// An engine that answers `<session>:<turn in that session>:<text>`, fails on `boom`,
/// reports usage on `count`, and holds `slow` until opened or its session stops (then it
/// fails, like a closed ACP session).
struct FakeEngine {
    manifest: Manifest,
    shared: Arc<Shared>,
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
                let args: engine::StartArgs = serde_json::from_value(call.args).unwrap();
                let mut seen = self.shared.seen();
                seen.cwds.push(args.cwd);
                let id = format!("s{}", seen.started.len() + 1);
                seen.started.push(id.clone());
                Ok(json!({ "session_id": id, "agent": { "name": "fake", "version": "1" } }))
            }
            engine::SESSION_PROMPT => {
                let args: engine::PromptArgs = serde_json::from_value(call.args).unwrap();
                if args.text == "panic" {
                    panic!("the fake engine panicked");
                }
                if args.text == "boom" {
                    return Err(ModuleError::Failed("the engine blew up".into()));
                }
                if args.text == "slow" {
                    let mut seen = self.shared.seen();
                    seen.slow_began += 1;
                    while !seen.open && !seen.stopped.contains(&args.session_id) {
                        seen = self.shared.wake.wait(seen).unwrap();
                    }
                    seen.slow_returned += 1;
                    if !seen.open {
                        return Err(ModuleError::Failed("the agent session is closed".into()));
                    }
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
                self.shared.seen().stopped.push(args.session_id);
                self.shared.wake.notify_all();
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
fn profile(config: Value) -> Profile {
    let outs = [
        ("reply", "text@1"),
        ("failed", "text@1"),
        ("usage", "json@1"),
    ];
    let mut own = vec![
        port("go", "text@1", "input", "then"),
        port("dir", "path@1", "input", "then"),
    ];
    let mut ports = vec![
        port("engine", "agent.engine@1", "input", "uses"),
        port("prompt", "text@1", "input", "then"),
        port("workdir", "path@1", "input", "then"),
    ];
    let mut wires = vec![
        json!({ "id": "w-go", "kind": "then", "from": { "node_id": "self", "port_id": "go" },
                "to": { "node_id": "a", "port_id": "prompt" } }),
        json!({ "id": "w-dir", "kind": "then", "from": { "node_id": "self", "port_id": "dir" },
                "to": { "node_id": "a", "port_id": "workdir" } }),
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
              "config": config, "ports": ports },
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

fn start(fresh: &str, data: &Path) -> (Arc<Core>, String, Arc<Shared>) {
    start_with(json!({ "fresh_context": fresh, "workdir": "/" }), data)
}

fn start_with(config: Value, data: &Path) -> (Arc<Core>, String, Arc<Shared>) {
    let shared: Arc<Shared> = Arc::default();
    let fake = FakeEngine {
        manifest: parse_manifest(&format!(
            "[module]\nid = \"{FAKE}\"\nname = \"fake\"\n[[node]]\ntype = \"engine\"\nname = \"e\"\n"
        ))
        .unwrap(),
        shared: shared.clone(),
        turns: Mutex::default(),
    };
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Agent::new()), Box::new(fake)];
    let mut profiles = ProfileStore::default();
    profiles.insert(profile(config)).unwrap();
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
    (core, id, shared)
}

fn send(core: &Core, id: &str, text: &str) {
    send_to(core, id, "go", text);
}

fn send_to(core: &Core, id: &str, port: &str, text: &str) {
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": port, "value": text }),
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
            .filter(|e| {
                e.from.node_id == "self" && !["go", "dir"].contains(&e.from.port_id.as_str())
            })
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
    assert_eq!(seen.seen().started, ["s1"]);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert_eq!(seen.seen().stopped, ["s1"]);
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
        let seen = seen.seen();
        assert_eq!(seen.started, ["s1", "s2", "s3"]);
        assert_eq!(seen.stopped, ["s1", "s2"]);
    }
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert_eq!(seen.seen().stopped, ["s1", "s2", "s3"]);
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
    // A turn that panics fails alone: the box still answers the next value. The core
    // takes a module that panicked out of service, so that answer is a failure too.
    send(&core, &id, "panic");
    send(&core, &id, "after");
    let out = outputs(&core, &id, 5);
    assert_eq!(out[3].0, "failed");
    assert!(
        out[3].1.as_str().unwrap().contains("panicked"),
        "{:?}",
        out[3]
    );
    assert_eq!(out[4].0, "failed");
    assert!(
        out[4].1.as_str().unwrap().contains("unavailable"),
        "{:?}",
        out[4]
    );
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn a_value_past_max_queue_goes_to_failed_with_its_text_and_nothing_else() {
    let data = data_dir("full");
    // `1.0`, not `1`: a whole number written as a float still counts.
    let config = json!({ "workdir": "/", "max_queue": 1.0, "failed_echo_chars": 5 });
    let (core, id, engine) = start_with(config, &data);
    send(&core, &id, "slow");
    engine.wait(|seen| seen.slow_began == 1);
    send(&core, &id, "two");
    send(&core, &id, "three and more");
    let out = outputs(&core, &id, 1);
    assert_eq!(out[0].0, "failed");
    let error = out[0].1.as_str().unwrap();
    assert!(error.contains("dropped: three…"), "{error}");
    engine.open();
    let out = outputs(&core, &id, 3);
    assert_eq!(out[1..], [reply("s1:1:slow"), reply("s1:2:two")]);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(outputs(&core, &id, 3).len(), 3);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn stopping_with_a_prompt_in_flight_emits_nothing_and_does_not_hang() {
    let data = data_dir("stop");
    let (core, id, engine) = start("no", &data);
    send(&core, &id, "slow");
    send(&core, &id, "queued");
    engine.wait(|seen| seen.slow_began == 1);
    let began = Instant::now();
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    // The turn in flight returned (its session closed), so its thread is free to end.
    engine.wait(|seen| seen.slow_returned == 1);
    std::thread::sleep(Duration::from_millis(100));
    let page: EventPage = serde_json::from_value(
        core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
            .unwrap(),
    )
    .unwrap();
    let emitted: Vec<_> = page
        .events
        .iter()
        .filter(|e| e.from.node_id == "a")
        .collect();
    assert!(emitted.is_empty(), "{emitted:?}");
    assert_eq!(engine.seen().stopped, ["s1"]);
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn a_workdir_value_moves_the_following_turns_to_a_session_opened_there() {
    let data = data_dir("workdir");
    let elsewhere = data_dir("workdir-target");
    std::fs::create_dir_all(&elsewhere).unwrap();
    let (core, id, engine) = start("no", &data);
    send(&core, &id, "one");
    assert_eq!(outputs(&core, &id, 1), [reply("s1:1:one")]);
    // Turns after the value run in a session opened there; the next ones stay in it.
    send_to(&core, &id, "dir", elsewhere.to_str().unwrap());
    send(&core, &id, "two");
    send(&core, &id, "three");
    assert_eq!(
        outputs(&core, &id, 3)[1..],
        [reply("s2:1:two"), reply("s2:2:three")]
    );
    // A path that is no directory fails the turns instead of running them elsewhere.
    send_to(&core, &id, "dir", "/no/such/regulus/dir");
    send(&core, &id, "four");
    let out = outputs(&core, &id, 4);
    assert_eq!(out[3].0, "failed");
    let error = out[3].1.as_str().unwrap();
    assert!(
        error.contains("not an existing absolute directory"),
        "{error}"
    );
    // Empty text: back to the field.
    send_to(&core, &id, "dir", "");
    send(&core, &id, "five");
    assert_eq!(outputs(&core, &id, 5)[4], reply("s3:1:five"));
    {
        let seen = engine.seen();
        assert_eq!(seen.started, ["s1", "s2", "s3"]);
        assert_eq!(
            seen.cwds,
            [PathBuf::from("/"), elsewhere.clone(), PathBuf::from("/")]
        );
        assert_eq!(seen.stopped, ["s1", "s2"]);
    }
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = std::fs::remove_dir_all(&data);
    let _ = std::fs::remove_dir_all(&elsewhere);
}
