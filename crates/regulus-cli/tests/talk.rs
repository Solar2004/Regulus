//! `regulus talk` (docs/04-client.md §7) through the real program against a real core in
//! a temporary home: an in-process engine behind a real Agent box, a Decider of `human`
//! and the `hello` greeter. `--once` with the direct agent and with a bound workflow, the
//! binding remembered, a proposal refused without `--yes`, an ambiguous workflow refused
//! with its candidates, and a permission answered with a slash command in the REPL.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::Duration;

use regulus_core::Storage;
use regulus_core::{parse_manifest, Config, Core, Module, ModuleCtx, ModuleError, ProfileStore};
use regulus_proto::{method, Manifest, NodeCall, Profile};
use serde_json::{json, Value};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const FAKE: &str = "t/talk-engine@1.0.0";

#[derive(Default)]
struct Seen {
    events: Vec<Value>,
    /// The answer to the permission request in flight; `Some(None)`: cancelled.
    decided: Option<Option<String>>,
}

#[derive(Default)]
struct Shared {
    seen: Mutex<Seen>,
    wake: Condvar,
}

/// `ask` asks permission and waits for `engine.session.decide`, then answers
/// `chose <option>`; anything else is echoed after two text chunks and a tool call.
struct Engine {
    manifest: Manifest,
    shared: Arc<Shared>,
}

fn push(seen: &mut Seen, event: Value) {
    let mut event = event;
    event["seq"] = json!(seen.events.len() as u64 + 1);
    event["at"] = json!(0);
    seen.events.push(event);
}

impl Module for Engine {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params).unwrap();
        let mut seen = self.shared.seen.lock().unwrap();
        match name {
            "engine.session.start" => Ok(json!({
                "session_id": "s1", "agent": { "name": "fake", "version": "1" },
                "capabilities": { "load_session": false, "events": true },
                "native_session_id": "native-1", "resumed": false })),
            "engine.session.prompt" => {
                let text = call.args["text"].as_str().unwrap_or_default().to_string();
                push(&mut seen, json!({ "kind": "turn_started" }));
                let answer = if text == "ask" {
                    seen.decided = None;
                    push(
                        &mut seen,
                        json!({ "kind": "permission_request", "id": "p1", "tool": "edit",
                        "title": "write a file", "options": [
                            { "id": "yes", "kind": "allow_once", "name": "Allow" },
                            { "id": "no", "kind": "reject_once", "name": "Reject" }] }),
                    );
                    while seen.decided.is_none() {
                        seen = self.shared.wake.wait(seen).unwrap();
                    }
                    let option = seen.decided.clone().unwrap();
                    push(
                        &mut seen,
                        json!({ "kind": "permission_resolved", "id": "p1", "option": option }),
                    );
                    format!("chose {}", option.unwrap_or_else(|| "cancelled".into()))
                } else {
                    let answer = format!("echo: {text}");
                    push(
                        &mut seen,
                        json!({ "kind": "tool_call", "id": "c1", "title": "look",
                                            "status": "completed" }),
                    );
                    let (head, tail) = answer.split_at(3);
                    push(&mut seen, json!({ "kind": "text", "text": head }));
                    push(&mut seen, json!({ "kind": "text", "text": tail }));
                    answer
                };
                push(
                    &mut seen,
                    json!({ "kind": "turn_ended", "stop_reason": "end_turn" }),
                );
                Ok(json!({ "stop_reason": "end_turn", "text": answer }))
            }
            "engine.session.events" => {
                let since = call.args["since"].as_u64().unwrap_or(0);
                let events: Vec<Value> = seen
                    .events
                    .iter()
                    .filter(|e| e["seq"].as_u64().unwrap() > since)
                    .cloned()
                    .collect();
                let next = events.last().map_or(since, |e| e["seq"].as_u64().unwrap());
                Ok(json!({ "events": events, "next": next }))
            }
            "engine.session.decide" => {
                seen.decided = Some(call.args["option"].as_str().map(str::to_string));
                self.shared.wake.notify_all();
                Ok(Value::Null)
            }
            "engine.session.cancel" => {
                if seen.decided.is_none() {
                    seen.decided = Some(None);
                }
                self.shared.wake.notify_all();
                Ok(Value::Null)
            }
            "engine.session.status" => Ok(json!({ "state": "idle" })),
            "engine.session.stop" => Ok(Value::Null),
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

fn wire(id: &str, kind: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": kind, "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

/// One agent with a Decider, and the workflow's `ask` → agent → `answer`.
fn chat() -> Profile {
    serde_json::from_value(json!({
        "id": "chat", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "ports": [port("ask", "text@1", "input", "then"),
                      port("answer", "text@1", "output", "then"),
                      port("activity", "agent.event@1", "output", "then")],
            "nodes": [
                { "id": "a", "type": "agent", "module_id": "regulus/agent@0.1.0",
                  "config": { "workdir": "/", "permissions": "ask", "events_poll_ms": 20,
                              "decision_poll_ms": 20 },
                  "ports": [port("engine", "agent.engine@1", "input", "uses"),
                            port("decisions", "decision@1", "input", "uses"),
                            port("prompt", "text@1", "input", "then"),
                            port("reply", "text@1", "output", "then"),
                            port("activity", "agent.event@1", "output", "then")] },
                { "id": "e", "type": "engine", "module_id": FAKE,
                  "ports": [port("engine", "agent.engine@1", "output", "uses")] },
                { "id": "d", "type": "decider", "module_id": "regulus/human@0.1.0",
                  "ports": [port("decisions", "decision@1", "output", "uses")] }
            ],
            "wires": [
                wire("w-engine", "uses", ("e", "engine"), ("a", "engine")),
                wire("w-decisions", "uses", ("d", "decisions"), ("a", "decisions")),
                wire("w-ask", "then", ("self", "ask"), ("a", "prompt")),
                wire("w-reply", "then", ("a", "reply"), ("self", "answer")),
                wire("w-activity", "then", ("a", "activity"), ("self", "activity"))
            ] }]
    }))
    .unwrap()
}

/// The greeter between `in` and `out`; with `two`, a second text input.
fn greet(id: &str, two: bool) -> Profile {
    let mut ports = vec![
        port("in", "text@1", "input", "then"),
        port("out", "text@1", "output", "then"),
    ];
    if two {
        ports.push(port("other", "text@1", "input", "then"));
    }
    serde_json::from_value(json!({
        "id": id, "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "ports": ports,
            "nodes": [{ "id": "g", "type": "greeter", "module_id": "regulus/hello@0.1.0",
                "ports": [port("echo", "text@1", "input", "then"),
                          port("reply", "text@1", "output", "then")] }],
            "wires": [wire("w-in", "then", ("self", "in"), ("g", "echo")),
                      wire("w-out", "then", ("g", "reply"), ("self", "out"))] }]
    }))
    .unwrap()
}

fn regulus(home: &Path, args: &[&str]) -> Output {
    Command::new(PROGRAM)
        .arg("--home")
        .arg(home)
        .args(["--timeout", "20"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

fn start(core: &Core, profile: &str) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": profile }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    started["id"].as_str().unwrap().to_string()
}

/// Reads the child's stdout on a thread, line by line, so the test can wait for a line.
fn lines(stdout: std::process::ChildStdout) -> mpsc::Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    rx
}

fn wait_for(rx: &mpsc::Receiver<String>, needle: &str, seen: &mut Vec<String>) {
    loop {
        match rx.recv_timeout(Duration::from_secs(15)) {
            Ok(line) => {
                let found = line.contains(needle);
                seen.push(line);
                if found {
                    return;
                }
            }
            Err(_) => panic!("never saw `{needle}` in {seen:#?}"),
        }
    }
}

fn checks(home: PathBuf, chat_exec: String, greet_exec: String, two_exec: String) {
    // The only agent, directly: the reply on stdout, the activity on stderr.
    let once = regulus(&home, &["talk", &chat_exec, "--once", "hi"]);
    assert!(once.status.success(), "{}", text(&once.stderr));
    assert_eq!(text(&once.stdout), "echo: hi\n");
    let said = text(&once.stderr);
    assert!(said.contains("talking to agent `a`"), "{said}");
    assert!(said.contains("tool c1 look"), "{said}");

    // The same workflow through its own ports: bound by the flags, and remembered.
    let args = [
        "talk", &chat_exec, "--in", "ask", "--out", "answer", "--once", "there",
    ];
    let bound = regulus(&home, &args);
    assert!(bound.status.success(), "{}", text(&bound.stderr));
    assert_eq!(text(&bound.stdout), "echo: there\n");
    assert!(
        text(&bound.stderr).contains("[a] tool c1 look"),
        "{}",
        text(&bound.stderr)
    );
    let kept: Value =
        serde_json::from_str(&std::fs::read_to_string(home.join("conversations.json")).unwrap())
            .unwrap();
    assert_eq!(
        kept["chat/main"],
        json!({ "input": "ask", "output": "answer" })
    );
    let again = regulus(&home, &["talk", &chat_exec, "--once", "again"]);
    assert!(again.status.success(), "{}", text(&again.stderr));
    assert!(
        text(&again.stderr).contains("bound earlier"),
        "{}",
        text(&again.stderr)
    );
    assert_eq!(text(&again.stdout), "echo: again\n");

    // One text pair and no agent: proposed, never bound without a yes.
    let refused = regulus(&home, &["talk", &greet_exec, "--once", "Ana"]);
    assert!(!refused.status.success());
    assert!(
        text(&refused.stderr).contains("pass --yes"),
        "{}",
        text(&refused.stderr)
    );
    let yes = regulus(&home, &["talk", &greet_exec, "--yes", "--once", "Ana"]);
    assert!(yes.status.success(), "{}", text(&yes.stderr));
    assert_eq!(text(&yes.stdout), "Hello, Ana!\n");

    // Two inputs: nothing is chosen for the person.
    let two = regulus(&home, &["talk", &two_exec, "--yes", "--once", "Ana"]);
    assert!(!two.status.success());
    let said = text(&two.stderr);
    assert!(said.contains("--in in, --in other"), "{said}");
    let missing = regulus(&home, &["talk", "exec-404", "--once", "x"]);
    assert!(text(&missing.stderr).contains("no execution `exec-404`"));

    // The REPL: a permission request answered with its option's slash command.
    let mut child = Command::new(PROGRAM)
        .arg("--home")
        .arg(&home)
        .args(["talk", &chat_exec, "--node", "a"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let rx = lines(child.stdout.take().unwrap());
    let mut seen = Vec::new();
    // The earlier turns come back as the transcript.
    wait_for(&rx, "a › echo: hi", &mut seen);
    stdin.write_all(b"/yes\nask\n").unwrap();
    wait_for(
        &rx,
        "answer: /yes (Allow) · /no (Reject) · /reject",
        &mut seen,
    );
    stdin.write_all(b"/yes\n").unwrap();
    wait_for(&rx, "a › chose yes", &mut seen);
    stdin.write_all(b"/quit\n").unwrap();
    let done = child.wait_with_output().unwrap();
    assert!(done.status.success());
    let said = text(&done.stderr);
    // `/yes` before anything asked says so instead of sending.
    assert!(said.contains("unknown command `/yes`"), "{said}");
    assert!(said.contains("answered p1 with yes"), "{said}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn talk_reaches_agents_and_bound_workflows_and_answers_permissions() {
    let home = std::env::temp_dir().join(format!("regulus-talk-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "[server]\nport = 0\n[client]\nrefresh_ms = 50\n",
    )
    .unwrap();
    let config = Config::load(&home).unwrap();
    let engine = Engine {
        manifest: parse_manifest(&format!(
            "[module]\nid = \"{FAKE}\"\nname = \"fake\"\n[[node]]\ntype = \"engine\"\nname = \"e\"\n"
        ))
        .unwrap(),
        shared: Arc::default(),
    };
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(regulus_module_agent::Agent::new()),
        Box::new(engine),
        Box::new(regulus_module_human::Human::new()),
        Box::new(regulus_module_hello::Hello::new()),
    ];
    let mut profiles = ProfileStore::default();
    for profile in [chat(), greet("greet", false), greet("two", true)] {
        profiles.insert(profile).unwrap();
    }
    let core = Core::build_with(modules, &[], profiles, Storage::default());
    let running = regulus_core::serve(&config, core.clone()).await.unwrap();
    let (chat_exec, greet_exec, two_exec) = (
        start(&core, "chat"),
        start(&core, "greet"),
        start(&core, "two"),
    );
    let result =
        tokio::task::spawn_blocking(move || checks(home, chat_exec, greet_exec, two_exec)).await;
    running.stop().await;
    core.shutdown();
    let _ = std::fs::remove_dir_all(
        std::env::temp_dir().join(format!("regulus-talk-{}", std::process::id())),
    );
    result.unwrap();
}
