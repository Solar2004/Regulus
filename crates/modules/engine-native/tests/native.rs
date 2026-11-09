//! The native engine against a real core, a fake OpenAI-style model server and in-process
//! `tool@1` and `hook@1` nodes: tools run through `tool.call`, events are kept, the `ask`
//! policy waits for `decide`, `deny` refuses, `max_steps` ends the loop, hooks steer it
//! (prompt context, a `pre_tool` veto, a `stop` that keeps it going), the history is
//! trimmed, and the Agent box drives it like any other engine.
//!
//! The Agent box's tool mounts (`StartArgs.tools`) run against a fake MCP server: this
//! same test binary started again with `FAKE_MCP` set, serving tools-mcp's own server
//! over a fake backend. A mounted tool goes through the same policy and events, the
//! mount's environment (the caller's binding) reaches it, a cancel ends a call that
//! hangs, a stop kills the process, and mounts that fail are left out with a warning.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_contract_agent_engine as engine;
use regulus_core::{parse_manifest, Core, Module, ModuleCtx, ModuleError, ProfileStore};
use regulus_module_engine_native::NativeEngine;
use regulus_proto::{method, Manifest, NodeCall, Profile};
use serde_json::{json, Value};

const FIXTURE: &str = "t/fixture@1.0.0";

// --- the fake model server ------------------------------------------------------------

type Seen = Arc<Mutex<Vec<Value>>>;

/// Answers the n-th request (from 1) with `answer(n, body)`, keeps every body.
fn serve(answer: impl Fn(usize, &Value) -> Value + Send + 'static) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let mut length = 0;
            loop {
                let mut header = String::new();
                reader.read_line(&mut header).unwrap();
                let header = header.trim_end();
                if header.is_empty() {
                    break;
                }
                if let Some((name, value)) = header.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let count = {
                let mut log = log.lock().unwrap();
                log.push(body.clone());
                log.len()
            };
            let reply = answer(count, &body).to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    (url, seen)
}

fn text(content: &str) -> Value {
    json!({ "choices": [{ "message": { "role": "assistant", "content": content },
                          "finish_reason": "stop" }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 2 } })
}

fn tool_call(id: &str, name: &str, args: Value) -> Value {
    json!({ "choices": [{ "message": { "role": "assistant", "content": null,
        "tool_calls": [{ "id": id, "type": "function",
                         "function": { "name": name, "arguments": args.to_string() } }] },
        "finish_reason": "tool_calls" }],
        "usage": { "prompt_tokens": 7, "completion_tokens": 3 } })
}

// --- the fixture module: a tool node and a hook node ----------------------------------

#[derive(Default)]
struct Shared {
    /// Every `tool.call`: (name, args, caller).
    calls: Mutex<Vec<(String, Value, String)>>,
    /// How many times each hook node answered.
    answered: Mutex<HashMap<String, usize>>,
}

/// `tools`: one tool, `echo`, that answers `{echo: args}`. `hook`: listens to its
/// `event` and answers `context` with `text` (`mode = context`), or a veto with `text`
/// only the first time (`mode = veto_once`).
struct Fixture {
    manifest: Manifest,
    shared: Arc<Shared>,
}

impl Module for Fixture {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params).unwrap();
        let config = &call.node.config;
        match name {
            "tool.list" => Ok(
                json!([{ "name": "echo", "description": "Echoes its arguments",
                "input": { "type": "object", "properties": { "x": { "type": "number" } } } }]),
            ),
            "tool.call" => {
                let args: regulus_contract_tool::Call = serde_json::from_value(call.args).unwrap();
                self.shared.calls.lock().unwrap().push((
                    args.name.clone(),
                    args.args.clone(),
                    args.caller.name,
                ));
                Ok(json!({ "data": { "echo": args.args } }))
            }
            "hook" => Ok(json!({ "event": config["event"] })),
            // The Agent box asks for its tools in its own name.
            "toolset.mount" => {
                let args: regulus_contract_toolset::MountArgs =
                    serde_json::from_value(call.args).unwrap();
                let pid_file = PathBuf::from(config["pid_file"].as_str().unwrap());
                Ok(serde_json::to_value(mount("box", "tools", &args.caller, &pid_file)).unwrap())
            }
            "node/hook" => {
                let request: regulus_proto::HookRequest =
                    serde_json::from_value(call.args).unwrap();
                if request.event != config["event"].as_str().unwrap() {
                    return Ok(Value::Null);
                }
                let n = {
                    let mut answered = self.shared.answered.lock().unwrap();
                    let n = answered.entry(call.node.id.clone()).or_default();
                    *n += 1;
                    *n
                };
                let text = config["text"].clone();
                Ok(match config["mode"].as_str().unwrap_or("context") {
                    "veto_once" if n == 1 => json!({ "decision": "veto", "reason": text }),
                    "veto_once" => Value::Null,
                    _ => json!({ "decision": "context", "text": text }),
                })
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

// --- the core ---------------------------------------------------------------------------

fn port(id: &str, capability: &str, direction: &str, kind: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
}

fn uses(from: &str, from_port: &str, to: &str, to_port: &str) -> Value {
    json!({ "id": format!("{from}-{to}"), "kind": "uses",
            "from": { "node_id": from, "port_id": from_port },
            "to": { "node_id": to, "port_id": to_port } })
}

fn native_node(config: Value) -> Value {
    json!({ "id": "n", "type": "native", "module_id": "regulus/engine-native@0.1.0",
            "config": config,
            "ports": [port("engine", "agent.engine@1", "output", "uses"),
                      port("model", "llm.endpoint@1", "input", "uses"),
                      port("tools", "tool@1", "input", "uses"),
                      port("context", "hook@1", "input", "uses")] })
}

/// A hook node: from the fixture (`config` has `mode`), or a `rule` of `hooks`.
struct HookNode {
    id: &'static str,
    config: Value,
    rule: bool,
}

fn fixture_hook(id: &'static str, event: &str, mode: &str, text: &str) -> HookNode {
    HookNode {
        id,
        config: json!({ "event": event, "mode": mode, "text": text }),
        rule: false,
    }
}

fn rule(id: &'static str, config: Value) -> HookNode {
    HookNode {
        id,
        config,
        rule: true,
    }
}

fn profile(
    url: &str,
    protocol: &str,
    config: Value,
    hooks: &[HookNode],
    with_box: Option<Value>,
    toolset: Option<Value>,
) -> Profile {
    let mut nodes = vec![
        json!({ "id": "model", "type": "provider", "module_id": "regulus/providers@0.1.0",
                "config": { "protocol": protocol, "base_url": url, "auth": "none",
                            "model": "fake-1" },
                "ports": [port("endpoint", "llm.endpoint@1", "output", "uses")] }),
        native_node(config),
        json!({ "id": "t", "type": "tools", "module_id": FIXTURE,
                "ports": [port("tools", "tool@1", "output", "uses")] }),
    ];
    let mut wires = vec![
        uses("model", "endpoint", "n", "model"),
        uses("t", "tools", "n", "tools"),
    ];
    for hook in hooks {
        let (kind, module) = if hook.rule {
            ("rule", "regulus/hooks@0.1.0")
        } else {
            ("hook", FIXTURE)
        };
        nodes.push(json!({ "id": hook.id, "type": kind, "module_id": module,
                           "config": hook.config,
                           "ports": [port("hook", "hook@1", "output", "uses")] }));
        wires.push(uses(hook.id, "hook", "n", "context"));
    }
    if let Some(config) = with_box {
        nodes.push(
            json!({ "id": "a", "type": "agent", "module_id": "regulus/agent@0.1.0",
                    "config": config,
                    "ports": [port("engine", "agent.engine@1", "input", "uses"),
                              port("tools", "toolset@1", "input", "uses")] }),
        );
        wires.push(uses("n", "engine", "a", "engine"));
    }
    if let Some(config) = toolset {
        nodes.push(
            json!({ "id": "ts", "type": "toolset", "module_id": FIXTURE, "config": config,
                    "ports": [port("toolset", "toolset@1", "output", "uses")] }),
        );
        wires.push(uses("ts", "toolset", "a", "tools"));
    }
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "nodes": nodes, "wires": wires }]
    }))
    .unwrap()
}

struct Rig {
    core: Arc<Core>,
    engine: NativeEngine,
    shared: Arc<Shared>,
    execution: String,
    node: Value,
}

fn rig(url: &str, config: Value, hooks: &[HookNode], with_box: Option<Value>) -> Rig {
    rig_on(url, "openai", config, hooks, with_box, None)
}

fn rig_on(
    url: &str,
    protocol: &str,
    config: Value,
    hooks: &[HookNode],
    with_box: Option<Value>,
    toolset: Option<Value>,
) -> Rig {
    let shared: Arc<Shared> = Arc::default();
    let fixture = Fixture {
        manifest: parse_manifest(&format!(
            "[module]\nid = \"{FIXTURE}\"\nname = \"fixture\"\n\
             [[node]]\ntype = \"tools\"\nname = \"tools\"\n\
             [[node]]\ntype = \"hook\"\nname = \"hook\"\n\
             [[node]]\ntype = \"toolset\"\nname = \"toolset\"\n"
        ))
        .unwrap(),
        shared: shared.clone(),
    };
    let engine = NativeEngine::new();
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(engine.clone()),
        Box::new(fixture),
        Box::new(regulus_module_providers::Providers::new()),
        Box::new(regulus_module_hooks::Hooks::new()),
        Box::new(regulus_module_agent::Agent::new()),
    ];
    let mut profiles = ProfileStore::default();
    profiles
        .insert(profile(
            url,
            protocol,
            config.clone(),
            hooks,
            with_box,
            toolset,
        ))
        .unwrap();
    let core = Core::build(modules, &[], profiles);
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    Rig {
        core,
        engine,
        shared,
        execution: started["id"].as_str().unwrap().to_string(),
        node: native_node(config),
    }
}

impl Rig {
    fn call(&self, name: &str, args: Value) -> Result<Value, ModuleError> {
        self.engine.call(
            name,
            json!({ "execution_id": self.execution, "node": self.node, "args": args }),
        )
    }

    fn start(&self, permissions: &str) -> (String, engine::Started) {
        self.start_with(permissions, &[])
    }

    fn start_with(
        &self,
        permissions: &str,
        tools: &[regulus_contract_toolset::Mount],
    ) -> (String, engine::Started) {
        let started: engine::Started = serde_json::from_value(
            self.call(
                engine::SESSION_START,
                json!({ "cwd": "/", "permissions": permissions, "tools": tools }),
            )
            .unwrap(),
        )
        .unwrap();
        (started.session_id.clone(), started)
    }

    fn prompt(&self, session: &str, text: &str) -> engine::Turn {
        serde_json::from_value(
            self.call(
                engine::SESSION_PROMPT,
                json!({ "session_id": session, "text": text }),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn events(&self, session: &str) -> Vec<engine::SessionEvent> {
        let events: engine::Events = serde_json::from_value(
            self.call(
                engine::SESSION_EVENTS,
                json!({ "session_id": session, "since": 0 }),
            )
            .unwrap(),
        )
        .unwrap();
        events.events
    }

    fn calls(&self) -> Vec<(String, Value, String)> {
        self.shared.calls.lock().unwrap().clone()
    }
}

fn kinds(events: &[engine::SessionEvent]) -> Vec<String> {
    events
        .iter()
        .map(|e| {
            serde_json::to_value(e).unwrap()["kind"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect()
}

/// The messages of the n-th request (from 0) with `role`, their content as text.
fn contents(seen: &Seen, n: usize, role: &str) -> Vec<String> {
    seen.lock().unwrap()[n]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] == role)
        .map(|m| m["content"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn tool_then_text(n: usize, _: &Value) -> Value {
    match n {
        1 => tool_call("c1", "echo", json!({ "x": 1 })),
        _ => text("done"),
    }
}

// --- the tests --------------------------------------------------------------------------

#[test]
fn a_tool_call_runs_through_tool_call_and_the_events_say_so() {
    let (url, seen) = serve(tool_then_text);
    let rig = rig(
        &url,
        json!({ "tool_policy": "allow", "name": "scout", "system": "Be brief." }),
        &[],
        None,
    );
    let (session, started) = rig.start("reject");
    assert!(started.capabilities.events && !started.capabilities.load_session);
    let turn = rig.prompt(&session, "use the tool");
    assert_eq!(turn.text, "done");
    assert_eq!(turn.stop_reason, "end_turn");
    let usage = turn.usage.unwrap();
    assert_eq!((usage.input_tokens, usage.output_tokens), (17, 5));
    assert_eq!(
        rig.calls(),
        vec![("echo".to_string(), json!({ "x": 1 }), "scout".to_string())]
    );

    let first = seen.lock().unwrap()[0].clone();
    assert_eq!(first["model"], "fake-1");
    assert_eq!(first["tools"][0]["function"]["name"], "echo");
    assert_eq!(contents(&seen, 0, "system"), vec!["Be brief."]);
    let observation = contents(&seen, 1, "tool");
    assert_eq!(observation, vec![r#"{"echo":{"x":1}}"#]);
    assert_eq!(
        seen.lock().unwrap()[1]["messages"][2]["tool_calls"][0]["id"],
        "c1"
    );

    let events = rig.events(&session);
    assert_eq!(
        kinds(&events),
        [
            "state",
            "turn_started",
            "state",
            "tool_call",
            "tool_update",
            "tool_update",
            "text",
            "turn_ended",
            "state"
        ]
    );
    assert!(events.windows(2).all(|w| w[0].seq < w[1].seq));
    assert!(matches!(
        &events[5].kind,
        engine::SessionEventKind::ToolUpdate { status, .. } if status == "completed"
    ));
    assert_eq!(events[3].turn, Some(1));
}

#[test]
fn ask_waits_for_decide_and_runs_the_tool_once_allowed() {
    let (url, _seen) = serve(tool_then_text);
    let rig = Arc::new(rig(&url, json!({ "tool_policy": "ask" }), &[], None));
    let (session, started) = rig.start("ask");
    assert!(started.warnings.is_empty(), "{:?}", started.warnings);
    let turn = {
        let (rig, session) = (rig.clone(), session.clone());
        std::thread::spawn(move || rig.prompt(&session, "go"))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    let request = loop {
        let found = rig.events(&session).into_iter().find_map(|e| match e.kind {
            engine::SessionEventKind::PermissionRequest {
                id, tool, options, ..
            } => Some((id, tool, options)),
            _ => None,
        });
        if let Some(found) = found {
            break found;
        }
        assert!(Instant::now() < deadline, "no permission request");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(request.1.as_deref(), Some("echo"));
    std::thread::sleep(Duration::from_millis(100));
    assert!(rig.calls().is_empty(), "ran before the decision");
    assert!(!turn.is_finished());
    let wrong = rig.call(
        engine::SESSION_DECIDE,
        json!({ "session_id": session, "request_id": request.0, "option": "maybe" }),
    );
    assert!(wrong.is_err());
    rig.call(
        engine::SESSION_DECIDE,
        json!({ "session_id": session, "request_id": request.0, "option": "allow_once" }),
    )
    .unwrap();
    let turn = turn.join().unwrap();
    assert_eq!(turn.text, "done");
    assert_eq!(rig.calls().len(), 1);
    assert!(rig.events(&session).iter().any(|e| matches!(
        &e.kind,
        engine::SessionEventKind::PermissionResolved { option: Some(o), .. } if o == "allow_once"
    )));
}

#[test]
fn a_cancel_ends_a_turn_waiting_for_a_decision() {
    let (url, _seen) = serve(tool_then_text);
    let rig = Arc::new(rig(&url, json!({}), &[], None));
    let (session, _) = rig.start("ask");
    let turn = {
        let (rig, session) = (rig.clone(), session.clone());
        std::thread::spawn(move || rig.prompt(&session, "go"))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !kinds(&rig.events(&session)).contains(&"permission_request".to_string()) {
        assert!(Instant::now() < deadline, "no permission request");
        std::thread::sleep(Duration::from_millis(10));
    }
    rig.call(engine::SESSION_CANCEL, json!({ "session_id": session }))
        .unwrap();
    let turn = turn.join().unwrap();
    assert_eq!(turn.stop_reason, "cancelled");
    assert!(rig.calls().is_empty());
}

#[test]
fn deny_answers_the_model_with_a_refusal_and_runs_nothing() {
    let (url, seen) = serve(tool_then_text);
    let rig = rig(&url, json!({ "tool_policy": "deny" }), &[], None);
    let (session, _) = rig.start("allow");
    assert_eq!(rig.prompt(&session, "go").text, "done");
    assert!(rig.calls().is_empty());
    let observation = contents(&seen, 1, "tool");
    assert!(observation[0].starts_with("refused:"), "{observation:?}");
}

#[test]
fn ask_without_anyone_to_ask_takes_the_box_answer_and_says_so() {
    let (url, _seen) = serve(tool_then_text);
    let rig = rig(&url, json!({}), &[], None);
    let (session, started) = rig.start("reject");
    assert!(
        started.warnings.iter().any(|w| w.contains("refused")),
        "{:?}",
        started.warnings
    );
    rig.prompt(&session, "go");
    assert!(rig.calls().is_empty());
}

#[test]
fn max_steps_ends_a_loop_that_never_stops_calling_tools() {
    let (url, seen) = serve(|n, _| tool_call(&format!("c{n}"), "echo", json!({ "x": n })));
    let rig = rig(
        &url,
        json!({ "tool_policy": "allow", "max_steps": 3 }),
        &[],
        None,
    );
    let (session, _) = rig.start("reject");
    let turn = rig.prompt(&session, "loop");
    assert_eq!(turn.stop_reason, "max_steps");
    assert_eq!(seen.lock().unwrap().len(), 3);
    assert_eq!(rig.calls().len(), 3);
}

#[test]
fn max_tool_calls_answers_further_calls_with_an_error() {
    let (url, seen) = serve(|n, _| match n {
        1..=2 => tool_call(&format!("c{n}"), "echo", json!({ "x": n })),
        _ => text("ok"),
    });
    let rig = rig(
        &url,
        json!({ "tool_policy": "allow", "max_tool_calls": 1 }),
        &[],
        None,
    );
    let (session, _) = rig.start("reject");
    assert_eq!(rig.prompt(&session, "go").text, "ok");
    assert_eq!(rig.calls().len(), 1);
    let observations = contents(&seen, 2, "tool");
    assert!(observations[1].contains("spent"), "{observations:?}");
}

#[test]
fn a_stop_hook_keeps_the_turn_going_with_its_reason() {
    let (url, seen) = serve(|n, _| text(&format!("answer {n}")));
    let rig = rig(
        &url,
        json!({}),
        &[fixture_hook(
            "h",
            "stop",
            "veto_once",
            "Not done: check again.",
        )],
        None,
    );
    let (session, _) = rig.start("reject");
    let turn = rig.prompt(&session, "go");
    assert_eq!(turn.text, "answer 2");
    assert_eq!(turn.stop_reason, "end_turn");
    assert_eq!(seen.lock().unwrap().len(), 2);
    assert_eq!(
        contents(&seen, 1, "user"),
        vec!["go", "Not done: check again."]
    );
}

#[test]
fn prompt_and_session_start_hooks_inject_context() {
    let (url, seen) = serve(|_, _| text("ok"));
    let rig = rig(
        &url,
        json!({ "system": "Base." }),
        &[
            rule(
                "rules",
                json!({ "event": "prompt", "context": "House rule: be kind." }),
            ),
            fixture_hook("boot", "session_start", "context", "Project: Regulus."),
        ],
        None,
    );
    let (session, _) = rig.start("reject");
    rig.prompt(&session, "hi");
    assert_eq!(
        contents(&seen, 0, "system"),
        vec!["Base.\n\nProject: Regulus.\n\nHouse rule: be kind."]
    );
    assert_eq!(contents(&seen, 0, "user"), vec!["hi"]);
}

#[test]
fn a_pre_tool_veto_wins_over_an_allow_policy() {
    let (url, seen) = serve(tool_then_text);
    let rig = rig(
        &url,
        json!({ "tool_policy": "allow" }),
        &[rule(
            "guard",
            json!({ "event": "pre_tool", "matcher": "echo", "deny": "yes",
                    "reason": "echo is off today" }),
        )],
        None,
    );
    let (session, _) = rig.start("reject");
    rig.prompt(&session, "go");
    assert!(rig.calls().is_empty());
    assert_eq!(
        contents(&seen, 1, "tool"),
        vec!["refused: echo is off today"]
    );
}

#[test]
fn a_pre_tool_context_can_allow_what_the_policy_denies() {
    let (url, seen) = serve(tool_then_text);
    let rig = rig(
        &url,
        json!({ "tool_policy": "deny", "pre_tool_context": "allow" }),
        &[rule(
            "ok",
            json!({ "event": "pre_tool", "matcher": "echo", "context": "Echo is safe." }),
        )],
        None,
    );
    let (session, _) = rig.start("reject");
    rig.prompt(&session, "go");
    assert_eq!(rig.calls().len(), 1);
    assert_eq!(
        contents(&seen, 1, "tool"),
        vec!["{\"echo\":{\"x\":1}}\n\nEcho is safe."]
    );
}

#[test]
fn history_turns_keeps_only_the_latest_turns() {
    let (url, seen) = serve(|n, _| text(&format!("reply {n}")));
    let rig = rig(&url, json!({ "history_turns": 1 }), &[], None);
    let (session, _) = rig.start("reject");
    for prompt in ["one", "two", "three"] {
        rig.prompt(&session, prompt);
    }
    assert_eq!(contents(&seen, 1, "user"), vec!["one", "two"]);
    assert_eq!(contents(&seen, 2, "user"), vec!["two", "three"]);
    assert_eq!(contents(&seen, 2, "assistant"), vec!["reply 2"]);
}

#[test]
fn the_agent_box_drives_the_native_engine() {
    let (url, _seen) = serve(tool_then_text);
    let rig = rig(
        &url,
        json!({ "tool_policy": "ask" }),
        &[],
        Some(json!({ "workdir": "/", "permissions": "allow" })),
    );
    let answer = rig
        .core
        .dispatch(
            "agent.prompt",
            json!({ "execution_id": rig.execution, "node_id": "a", "text": "go" }),
        )
        .unwrap();
    assert_eq!(answer["text"], "done", "{answer}");
    assert_eq!(rig.calls().len(), 1);
    let stopped = rig
        .core
        .dispatch(method::EXECUTION_STOP, json!({ "id": rig.execution }))
        .unwrap();
    assert_eq!(stopped["state"]["status"], "stopped", "{stopped}");
}

#[test]
fn an_anthropic_provider_gets_tool_use_and_tool_result_blocks() {
    let (url, seen) = serve(|n, _| match n {
        1 => json!({ "content": [{ "type": "text", "text": "Let me look." },
                                 { "type": "tool_use", "id": "tu1", "name": "echo",
                                   "input": { "x": 2 } }],
                     "stop_reason": "tool_use",
                     "usage": { "input_tokens": 5, "output_tokens": 1 } }),
        _ => json!({ "content": [{ "type": "text", "text": "two" }], "stop_reason": "end_turn",
                     "usage": { "input_tokens": 6, "output_tokens": 1 } }),
    });
    let base = url.trim_end_matches("/v1").to_string();
    let rig = rig_on(
        &base,
        "anthropic",
        json!({ "tool_policy": "allow", "system": "Sys." }),
        &[],
        None,
        None,
    );
    let (session, _) = rig.start("reject");
    let turn = rig.prompt(&session, "go");
    assert_eq!(turn.text, "two");
    assert_eq!(rig.calls().len(), 1);
    let second = seen.lock().unwrap()[1].clone();
    assert_eq!(second["system"], "Sys.");
    assert_eq!(second["tools"][0]["input_schema"]["type"], "object");
    let messages = second["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "{second:#}");
    assert_eq!(messages[1]["content"][1]["type"], "tool_use");
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"][0]["type"], "tool_result");
    assert_eq!(messages[2]["content"][0]["tool_use_id"], "tu1");
}

// --- the fake MCP server ----------------------------------------------------------------

/// Set in a mount's environment: what the fake server does (`tools`, `exit`, `bad_list`).
const FAKE_MCP: &str = "REGULUS_TEST_FAKE_MCP";
/// Where the fake server writes its pid.
const PID_FILE: &str = "REGULUS_TEST_PID_FILE";
/// The binding a tools box would put in a mount's environment: a secret.
const SECRET: &str = "binding-5f0c2a9e7d41b3c8";

struct FakeBackend {
    mode: String,
}

impl regulus_module_tools_mcp::Backend for FakeBackend {
    fn list(&self) -> Result<Vec<regulus_contract_tool::Spec>, String> {
        let binding = std::env::var(regulus_module_tools_mcp::BINDING_ENV).unwrap_or_default();
        if self.mode == "bad_list" {
            return Err(format!("binding {binding} is not known"));
        }
        let spec = |name: &str, description: &str| regulus_contract_tool::Spec {
            name: name.into(),
            description: description.into(),
            input: json!({ "type": "object" }),
        };
        Ok(vec![
            spec("whoami", "Says the binding it was started with"),
            spec("echo", "Echoes its arguments, from the mount"),
            spec("wait", "Never answers in time"),
        ])
    }

    fn call(&self, name: &str, args: Value) -> Result<(String, bool), String> {
        match name {
            "whoami" => Ok((
                std::env::var(regulus_module_tools_mcp::BINDING_ENV).unwrap_or_default(),
                false,
            )),
            "echo" => Ok((json!({ "mounted": args }).to_string(), false)),
            "wait" => {
                std::thread::sleep(Duration::from_secs(60));
                Ok(("late".into(), false))
            }
            other => Err(format!("no tool `{other}`")),
        }
    }
}

/// Not a test: the fake MCP server, when this binary runs again with `FAKE_MCP` set.
#[test]
fn fake_mcp_server() {
    let Ok(mode) = std::env::var(FAKE_MCP) else {
        return;
    };
    if let Ok(file) = std::env::var(PID_FILE) {
        std::fs::write(file, std::process::id().to_string()).unwrap();
    }
    if mode == "exit" {
        std::process::exit(3);
    }
    regulus_module_tools_mcp::serve(&FakeBackend { mode }, "fake").unwrap();
    std::process::exit(0);
}

/// A mount that starts the fake server in `mode`, with `binding` in its environment.
fn mount(
    name: &str,
    mode: &str,
    binding: &str,
    pid_file: &std::path::Path,
) -> regulus_contract_toolset::Mount {
    regulus_contract_toolset::Mount {
        protocol: "mcp-stdio".into(),
        name: name.into(),
        command: std::env::current_exe().unwrap(),
        args: [
            "fake_mcp_server",
            "--exact",
            "--nocapture",
            "-q",
            "--test-threads=1",
        ]
        .map(String::from)
        .to_vec(),
        env: [
            (FAKE_MCP.to_string(), mode.to_string()),
            (
                regulus_module_tools_mcp::BINDING_ENV.to_string(),
                binding.to_string(),
            ),
            (PID_FILE.to_string(), pid_file.display().to_string()),
        ]
        .into(),
    }
}

fn pid_file(name: &str) -> PathBuf {
    let file = std::env::temp_dir().join(format!(
        "regulus-native-mcp-{name}-{}.pid",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&file);
    file
}

/// The pid the fake server wrote, once it did.
fn pid(file: &std::path::Path) -> u32 {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(pid) = std::fs::read_to_string(file)
            .ok()
            .and_then(|p| p.parse().ok())
        {
            return pid;
        }
        assert!(Instant::now() < deadline, "the fake server never started");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

fn until_gone(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while alive(pid) {
        assert!(
            Instant::now() < deadline,
            "the mount's process {pid} outlived its session"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn tool_names(seen: &Seen, n: usize) -> Vec<String> {
    seen.lock().unwrap()[n]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

// --- the mount tests ----------------------------------------------------------------------

#[test]
fn a_mounted_tool_is_listed_called_and_answered_like_a_wired_one() {
    let (url, seen) = serve(|n, _| match n {
        1 => tool_call("c1", "whoami", json!({})),
        2 => tool_call("c2", "box__echo", json!({ "y": 2 })),
        _ => text("done"),
    });
    let rig = rig(&url, json!({ "tool_policy": "allow" }), &[], None);
    let file = pid_file("listed");
    let (session, started) = rig.start_with("reject", &[mount("box", "tools", SECRET, &file)]);
    // The fixture's `echo` was there first: the mount's goes under its mount's name.
    assert!(
        started.warnings.iter().any(|w| w.contains("`box__echo`")),
        "{:?}",
        started.warnings
    );
    assert!(
        !started.warnings.iter().any(|w| w.contains("left out")),
        "{:?}",
        started.warnings
    );
    let turn = rig.prompt(&session, "who am I?");
    assert_eq!(turn.text, "done");
    assert_eq!(
        tool_names(&seen, 0),
        ["echo", "whoami", "box__echo", "wait"]
    );
    // The mount's environment reached the server: the caller's binding answers.
    assert_eq!(contents(&seen, 1, "tool"), vec![SECRET]);
    assert_eq!(
        contents(&seen, 2, "tool")[1],
        json!({ "mounted": { "y": 2 } }).to_string()
    );
    assert!(rig.calls().is_empty(), "the fixture's echo ran");
    let statuses: Vec<String> = rig
        .events(&session)
        .into_iter()
        .filter_map(|e| match e.kind {
            engine::SessionEventKind::ToolUpdate { status, .. } => Some(status),
            _ => None,
        })
        .collect();
    assert_eq!(
        statuses,
        ["in_progress", "completed", "in_progress", "completed"]
    );
    let pid = pid(&file);
    assert!(alive(pid));
    rig.call(engine::SESSION_STOP, json!({ "session_id": session }))
        .unwrap();
    until_gone(pid);
}

#[test]
fn the_policy_and_hooks_apply_to_mounted_tools() {
    let (url, seen) = serve(|n, _| match n {
        1 => tool_call("c1", "whoami", json!({})),
        _ => text("done"),
    });
    let denied = rig(
        &url,
        json!({ "tool_policy": "allow", "deny_tools": ["whoami"] }),
        &[],
        None,
    );
    let file = pid_file("policy");
    let (session, _) = denied.start_with("reject", &[mount("box", "tools", SECRET, &file)]);
    denied.prompt(&session, "go");
    let observation = contents(&seen, 1, "tool");
    assert!(observation[0].starts_with("refused:"), "{observation:?}");

    let (url, seen) = serve(|n, _| match n {
        1 => tool_call("c1", "whoami", json!({})),
        _ => text("done"),
    });
    let rig = rig(
        &url,
        json!({ "tool_policy": "allow" }),
        &[rule(
            "guard",
            json!({ "event": "pre_tool", "matcher": "whoami", "deny": "yes",
                    "reason": "not today" }),
        )],
        None,
    );
    let (session, _) = rig.start_with("reject", &[mount("box", "tools", SECRET, &file)]);
    rig.prompt(&session, "go");
    assert_eq!(contents(&seen, 1, "tool"), vec!["refused: not today"]);
}

#[test]
fn a_cancel_ends_a_mounted_call_and_a_stop_kills_the_mount() {
    let (url, _seen) = serve(|n, _| match n {
        1 => tool_call("c1", "wait", json!({})),
        _ => text("done"),
    });
    let rig = Arc::new(rig(&url, json!({ "tool_policy": "allow" }), &[], None));
    let file = pid_file("cancel");
    let (session, _) = rig.start_with("reject", &[mount("box", "tools", SECRET, &file)]);
    let turn = {
        let (rig, session) = (rig.clone(), session.clone());
        std::thread::spawn(move || rig.prompt(&session, "go"))
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    while !rig.events(&session).iter().any(|e| {
        matches!(&e.kind, engine::SessionEventKind::ToolUpdate { status, .. } if status == "in_progress")
    }) {
        assert!(Instant::now() < deadline, "the mounted call never started");
        std::thread::sleep(Duration::from_millis(10));
    }
    let cancelled = Instant::now();
    rig.call(engine::SESSION_CANCEL, json!({ "session_id": session }))
        .unwrap();
    let turn = turn.join().unwrap();
    assert_eq!(turn.stop_reason, "cancelled");
    assert!(cancelled.elapsed() < Duration::from_secs(5));
    let pid = pid(&file);
    assert!(alive(pid), "a cancel ends the turn, not the mount");
    rig.call(engine::SESSION_STOP, json!({ "session_id": session }))
        .unwrap();
    until_gone(pid);
}

#[test]
fn a_mounted_call_past_its_timeout_is_an_error_for_the_model() {
    let (url, seen) = serve(|n, _| match n {
        1 => tool_call("c1", "wait", json!({})),
        _ => text("done"),
    });
    let rig = rig(
        &url,
        json!({ "tool_policy": "allow", "mount_call_timeout_ms": 300 }),
        &[],
        None,
    );
    let file = pid_file("timeout");
    let (session, _) = rig.start_with("reject", &[mount("box", "tools", SECRET, &file)]);
    assert_eq!(rig.prompt(&session, "go").text, "done");
    let observation = contents(&seen, 1, "tool");
    assert!(
        observation[0].contains("did not answer in 300 ms"),
        "{observation:?}"
    );
    let pid = pid(&file);
    rig.call(engine::SESSION_STOP, json!({ "session_id": session }))
        .unwrap();
    until_gone(pid);
}

#[test]
fn mounts_that_fail_are_left_out_with_a_warning_that_hides_the_binding() {
    let (url, seen) = serve(tool_then_text);
    let rig = rig(
        &url,
        json!({ "tool_policy": "allow", "mount_start_timeout_ms": 5000 }),
        &[],
        None,
    );
    let mut sse = mount("remote", "tools", SECRET, &pid_file("sse"));
    sse.protocol = "mcp-sse".into();
    let mut missing = mount("missing", "tools", SECRET, &pid_file("missing"));
    missing.command = PathBuf::from("/nonexistent/regulus-mcp");
    let exits = pid_file("exits");
    let mounts = [
        mount("exits", "exit", SECRET, &exits),
        mount("liar", "bad_list", SECRET, &pid_file("liar")),
        sse,
        missing,
    ];
    let (session, started) = rig.start_with("reject", &mounts);
    let warnings = started.warnings.join("\n");
    for name in ["exits", "liar", "remote", "missing"] {
        assert!(warnings.contains(&format!("`{name}`")), "{warnings}");
    }
    assert!(
        warnings.contains("speaks mcp-stdio, not `mcp-sse`"),
        "{warnings}"
    );
    assert!(
        warnings.contains("binding [hidden] is not known"),
        "{warnings}"
    );
    assert!(!warnings.contains(SECRET), "{warnings}");
    until_gone(pid(&exits));
    // What did start still works: the wired tool runs and the turn ends.
    assert_eq!(rig.prompt(&session, "go").text, "done");
    assert_eq!(rig.calls().len(), 1);
    assert_eq!(tool_names(&seen, 0), ["echo"]);
}

#[test]
fn the_agent_box_mounts_its_tools_for_the_native_engine() {
    let (url, seen) = serve(|n, _| match n {
        1 => tool_call("c1", "whoami", json!({})),
        _ => text("done"),
    });
    let file = pid_file("agent");
    let rig = rig_on(
        &url,
        "openai",
        json!({ "tool_policy": "ask" }),
        &[],
        Some(json!({ "workdir": "/", "permissions": "allow", "name": "scout" })),
        Some(json!({ "pid_file": file.display().to_string() })),
    );
    let answer = rig
        .core
        .dispatch(
            "agent.prompt",
            json!({ "execution_id": rig.execution, "node_id": "a", "text": "go" }),
        )
        .unwrap();
    assert_eq!(answer["text"], "done", "{answer}");
    assert!(tool_names(&seen, 0).contains(&"whoami".to_string()));
    // The fixture's toolset put the caller's name in the binding.
    assert_eq!(contents(&seen, 1, "tool"), vec!["scout"]);
    let pid = pid(&file);
    rig.core
        .dispatch(method::EXECUTION_STOP, json!({ "id": rig.execution }))
        .unwrap();
    until_gone(pid);
}
