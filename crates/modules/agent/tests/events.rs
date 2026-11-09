//! The box's view of its engine's session, against a real core and an in-process engine
//! that serves `engine.session.events` and `engine.session.decide`: the recorded
//! history, the `activity` output, permission requests answered through a Decider of
//! `human`, the new methods, the native resume and which conversation each node keeps.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use regulus_contract_agent_engine as engine;
use regulus_core::config::{Resume, StoreConfig};
use regulus_core::{parse_manifest, Core, Module, ModuleCtx, ModuleError, ProfileStore};
use regulus_core::{Storage, Store};
use regulus_module_agent::Agent;
use regulus_module_human::Human;
use regulus_proto::{method, BusEventKind, EventPage, Manifest, NodeCall, Profile};
use serde_json::{json, Value};

const FAKE: &str = "t/event-engine@1.0.0";

#[derive(Default)]
struct Seen {
    starts: Vec<engine::StartArgs>,
    /// Each session's events, by its id.
    events: HashMap<String, Vec<engine::SessionEvent>>,
    /// The answer to the permission request in flight; `Some(None)`: cancelled.
    decided: Option<Option<String>>,
    cancelled: usize,
    /// The engine says it can load sessions.
    loads: bool,
}

#[derive(Default)]
struct Shared {
    seen: Mutex<Seen>,
    wake: Condvar,
}

impl Shared {
    fn seen(&self) -> std::sync::MutexGuard<'_, Seen> {
        self.seen
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// `tool` reports a tool call and answers `done`; `ask` asks permission and waits for
/// `engine.session.decide` (or a cancel), then answers `chose <option>`; anything else
/// is echoed, streamed as a `text` event after a pause in which other sessions run.
struct EventEngine {
    manifest: Manifest,
    shared: Arc<Shared>,
}

impl EventEngine {
    fn push(seen: &mut Seen, session: &str, kind: engine::SessionEventKind) {
        let events = seen.events.entry(session.to_string()).or_default();
        let seq = events.len() as u64 + 1;
        events.push(engine::SessionEvent {
            seq,
            at: 0,
            turn: Some(1),
            kind,
        });
    }
}

impl Module for EventEngine {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params).unwrap();
        let mut seen = self.shared.seen();
        match name {
            engine::SESSION_START => {
                let args: engine::StartArgs = serde_json::from_value(call.args).unwrap();
                let n = seen.starts.len() + 1;
                let resumed = seen.loads && args.resume.is_some();
                let native = match (&args.resume, resumed) {
                    (Some(id), true) => id.clone(),
                    _ => format!("native-{n}"),
                };
                seen.starts.push(args);
                Ok(json!({ "session_id": format!("s{n}"),
                           "agent": { "name": "fake", "version": "1" },
                           "capabilities": { "load_session": seen.loads, "events": true },
                           "native_session_id": native, "resumed": resumed }))
            }
            engine::SESSION_PROMPT => {
                let args: engine::PromptArgs = serde_json::from_value(call.args).unwrap();
                let session = args.session_id.as_str();
                Self::push(&mut seen, session, engine::SessionEventKind::TurnStarted);
                let text = match args.text.as_str() {
                    "tool" => {
                        Self::push(
                            &mut seen,
                            session,
                            engine::SessionEventKind::ToolCall {
                                id: "c1".into(),
                                title: "read a file".into(),
                                kind: Some("read".into()),
                                status: "pending".into(),
                            },
                        );
                        "done".to_string()
                    }
                    "ask" => {
                        seen.decided = None;
                        let options = vec![
                            engine::PermissionOption {
                                id: "yes".into(),
                                kind: "allow_once".into(),
                                name: "Allow".into(),
                            },
                            engine::PermissionOption {
                                id: "no".into(),
                                kind: "reject_once".into(),
                                name: "Reject".into(),
                            },
                        ];
                        Self::push(
                            &mut seen,
                            session,
                            engine::SessionEventKind::PermissionRequest {
                                id: "p1".into(),
                                tool: Some("edit".into()),
                                title: "write a file".into(),
                                options,
                            },
                        );
                        while seen.decided.is_none() {
                            seen = self.shared.wake.wait(seen).unwrap();
                        }
                        let option = seen.decided.clone().unwrap();
                        Self::push(
                            &mut seen,
                            session,
                            engine::SessionEventKind::PermissionResolved {
                                id: "p1".into(),
                                option: option.clone(),
                            },
                        );
                        format!("chose {}", option.unwrap_or_else(|| "cancelled".into()))
                    }
                    other => {
                        drop(seen);
                        std::thread::sleep(Duration::from_millis(15));
                        seen = self.shared.seen();
                        let text = format!("echo: {other}");
                        let streamed = engine::SessionEventKind::Text { text: text.clone() };
                        Self::push(&mut seen, session, streamed);
                        text
                    }
                };
                Self::push(
                    &mut seen,
                    session,
                    engine::SessionEventKind::TurnEnded {
                        stop_reason: "end_turn".into(),
                    },
                );
                Ok(json!({ "stop_reason": "end_turn", "text": text }))
            }
            engine::SESSION_EVENTS => {
                let args: engine::EventsArgs = serde_json::from_value(call.args).unwrap();
                let events: Vec<_> = seen
                    .events
                    .get(&args.session_id)
                    .into_iter()
                    .flatten()
                    .filter(|e| e.seq > args.since)
                    .cloned()
                    .collect();
                let next = events.last().map_or(args.since, |e| e.seq);
                Ok(json!({ "events": events, "next": next }))
            }
            engine::SESSION_DECIDE => {
                let args: engine::DecideArgs = serde_json::from_value(call.args).unwrap();
                assert_eq!(args.request_id, "p1");
                seen.decided = Some(args.option);
                self.shared.wake.notify_all();
                Ok(Value::Null)
            }
            engine::SESSION_CANCEL => {
                seen.cancelled += 1;
                if seen.decided.is_none() {
                    seen.decided = Some(None);
                }
                self.shared.wake.notify_all();
                Ok(Value::Null)
            }
            engine::SESSION_STATUS => Ok(json!({ "state": "idle" })),
            engine::SESSION_STOP => Ok(Value::Null),
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

fn uses(id: &str, from: &str, to: &str, port: &str) -> Value {
    json!({ "id": id, "kind": "uses", "from": { "node_id": from, "port_id": port },
            "to": { "node_id": to, "port_id": port } })
}

/// Agent `a` with engine `e`, `reply` and `activity` leaving the workflow, and with
/// `decider` a Decider `d` wired into `decisions`.
fn profile(config: Value, decider: bool) -> Profile {
    let mut nodes = vec![
        json!({ "id": "a", "type": "agent", "module_id": "regulus/agent@0.1.0", "config": config,
                "ports": [port("engine", "agent.engine@1", "input", "uses"),
                          port("decisions", "decision@1", "input", "uses"),
                          port("reply", "text@1", "output", "then"),
                          port("activity", "agent.event@1", "output", "then")] }),
        json!({ "id": "e", "type": "engine", "module_id": FAKE,
                "ports": [port("engine", "agent.engine@1", "output", "uses")] }),
    ];
    let mut wires = vec![
        uses("w-engine", "e", "a", "engine"),
        json!({ "id": "w-reply", "kind": "then", "from": { "node_id": "a", "port_id": "reply" },
                "to": { "node_id": "self", "port_id": "reply" } }),
        json!({ "id": "w-activity", "kind": "then",
                "from": { "node_id": "a", "port_id": "activity" },
                "to": { "node_id": "self", "port_id": "activity" } }),
    ];
    if decider {
        nodes.push(
            json!({ "id": "d", "type": "decider", "module_id": "regulus/human@0.1.0",
            "ports": [port("decisions", "decision@1", "output", "uses")] }),
        );
        wires.push(uses("w-decisions", "d", "a", "decisions"));
    }
    serde_json::from_value(json!({
        "id": "p", "project_id": "proj", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "nodes": nodes, "wires": wires,
            "ports": [port("reply", "text@1", "output", "then"),
                      port("activity", "agent.event@1", "output", "then")] }]
    }))
    .unwrap()
}

fn home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-agent-ev-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn core(config: Value, decider: bool, home: &Path, loads: bool) -> (Arc<Core>, Arc<Shared>) {
    core_with(config, decider, home, loads, &StoreConfig::default())
}

fn core_with(
    config: Value,
    decider: bool,
    home: &Path,
    loads: bool,
    store: &StoreConfig,
) -> (Arc<Core>, Arc<Shared>) {
    let shared: Arc<Shared> = Arc::default();
    shared.seen().loads = loads;
    let fake = EventEngine {
        manifest: parse_manifest(&format!(
            "[module]\nid = \"{FAKE}\"\nname = \"fake\"\n[[node]]\ntype = \"engine\"\nname = \"e\"\n"
        ))
        .unwrap(),
        shared: shared.clone(),
    };
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(Agent::new()),
        Box::new(fake),
        Box::new(Human::new()),
    ];
    let mut profiles = ProfileStore::default();
    profiles.insert(profile(config, decider)).unwrap();
    let storage = Storage {
        store: Some(Arc::new(Store::open(&home.join("store"), store).unwrap())),
        data_dir: Some(home.join("data")),
        ..Storage::default()
    };
    (Core::build_with(modules, &[], profiles, storage), shared)
}

fn start(core: &Core) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    started["id"].as_str().unwrap().to_string()
}

fn history(core: &Core, execution: &str, since: u64) -> Value {
    core.dispatch(
        "agent.history",
        json!({ "execution_id": execution, "node_id": "a", "since": since }),
    )
    .unwrap()
}

/// The history once `check` holds.
fn history_when(core: &Core, execution: &str, check: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let found = history(core, execution, 0);
        if check(&found) {
            return found;
        }
        assert!(Instant::now() < deadline, "never: {found:#}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn entries(history: &Value) -> Vec<(String, String, String)> {
    history["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| {
            (
                e["role"].as_str().unwrap().to_string(),
                e["text"].as_str().unwrap().to_string(),
                e["turn"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

fn fast() -> Value {
    json!({ "workdir": "/", "events_poll_ms": 20, "decision_poll_ms": 20 })
}

fn emitted(core: &Core, execution: &str, port: &str) -> Vec<Value> {
    let page: EventPage = serde_json::from_value(
        core.dispatch(method::EVENT_LIST, json!({ "execution_id": execution }))
            .unwrap(),
    )
    .unwrap();
    page.events
        .into_iter()
        .filter(|e| e.from.node_id == "self" && e.from.port_id == port)
        .filter_map(|e| match e.kind {
            BusEventKind::Emitted { value } => Some(value),
            _ => None,
        })
        .collect()
}

#[test]
fn the_history_records_turns_and_activity_and_the_events_leave_on_activity() {
    let home = home("record");
    let (core, _) = core(fast(), false, &home, false);
    let id = start(&core);
    let turn = core
        .dispatch(
            "agent.prompt",
            json!({ "execution_id": id, "node_id": "a", "text": "tool" }),
        )
        .unwrap();
    assert_eq!(turn["text"], "done");
    let found = history(&core, &id, 0);
    assert_eq!(
        entries(&found),
        [
            ("user".into(), "tool".into(), "t-1".into()),
            ("agent".into(), "done".into(), "t-1".into())
        ]
    );
    let kinds: Vec<&str> = found["activity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["event"]["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["turn_started", "tool_call", "turn_ended"]);
    assert_eq!(found["activity"][1]["turn"], "t-1");
    assert_eq!(found["session"]["engine_session_id"], "native-1");
    assert_eq!(found["session"]["native_resume"], false);
    assert_eq!(found["session"]["state"], "idle");

    // `agent.send` answers the turn at once; the cursor gives only what is new.
    let next = found["next"].as_u64().unwrap();
    let sent = core
        .dispatch(
            "agent.send",
            json!({ "execution_id": id, "node_id": "a", "text": "hello" }),
        )
        .unwrap();
    assert_eq!(sent, json!({ "turn": "t-2" }));
    let after = history_when(&core, &id, |h| h["entries"].as_array().unwrap().len() == 4);
    assert_eq!(
        entries(&after)[3],
        ("agent".into(), "echo: hello".into(), "t-2".into())
    );
    let newer = history(&core, &id, next);
    assert_eq!(entries(&newer).len(), 2);
    assert!(newer["activity"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["seq"].as_u64() > Some(next)));

    let activity = emitted(&core, &id, "activity");
    assert!(activity
        .iter()
        .any(|e| e["kind"] == "tool_call" && e["tool_kind"] == "read"));
    assert_eq!(emitted(&core, &id, "reply"), [json!("echo: hello")]);

    // A stopped node still answers; the next execution starts a conversation of its own.
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert_eq!(history(&core, &id, 0)["session"]["state"], "stopped");
    let again = start(&core);
    core.dispatch(
        "agent.prompt",
        json!({ "execution_id": again, "node_id": "a", "text": "more" }),
    )
    .unwrap();
    let own = entries(&history(&core, &again, 0));
    assert_eq!(
        own,
        [
            ("user".into(), "more".into(), "t-1".into()),
            ("agent".into(), "echo: more".into(), "t-1".into())
        ]
    );
    assert_eq!(entries(&history(&core, &id, 0)).len(), 4);
    let _ = std::fs::remove_dir_all(&home);
}

fn prompt(core: &Core, execution: &str, text: &str) -> Value {
    core.dispatch(
        "agent.prompt",
        json!({ "execution_id": execution, "node_id": "a", "text": text }),
    )
    .unwrap()
}

fn texts(history: &Value, list: &str) -> Vec<String> {
    history[list]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|item| match list {
            "entries" => item["text"].as_str(),
            _ => item["event"]["text"].as_str(),
        })
        .map(String::from)
        .collect()
}

#[test]
fn two_executions_of_one_workflow_keep_their_own_conversations() {
    let home = home("isolated");
    let mut config = fast();
    config["resume"] = json!("native");
    let (core, shared) = core(config, false, &home, true);
    let (one, two) = (start(&core), start(&core));
    // Both ask at once: their turns interleave in the engine.
    std::thread::scope(|scope| {
        for (id, who) in [(&one, "one"), (&two, "two")] {
            let core = &core;
            scope.spawn(move || {
                for n in 1..=2 {
                    prompt(core, id, &format!("{who}-{n}"));
                }
            });
        }
    });
    // Nobody resumed anybody: two new conversations, two engine sessions.
    assert!(shared.seen().starts.iter().all(|s| s.resume.is_none()));
    let mut native = Vec::new();
    for (id, who) in [(&one, "one"), (&two, "two")] {
        let found = history(&core, id, 0);
        let turns: Vec<String> = entries(&found).into_iter().map(|e| e.2).collect();
        assert_eq!(turns, ["t-1", "t-1", "t-2", "t-2"], "{who}");
        assert_eq!(
            texts(&found, "entries"),
            [
                format!("{who}-1"),
                format!("echo: {who}-1"),
                format!("{who}-2"),
                format!("echo: {who}-2")
            ]
        );
        assert!(found["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["execution_id"] == json!(id)));
        assert_eq!(
            texts(&found, "activity"),
            [format!("echo: {who}-1"), format!("echo: {who}-2")]
        );
        assert_eq!(found["session"]["native_resume"], false);
        native.push(found["session"]["engine_session_id"].clone());
    }
    assert_ne!(native[0], native[1]);

    // Each one resumes its own session after a stop, never the other's.
    core.dispatch(method::EXECUTION_STOP, json!({ "id": one }))
        .unwrap();
    let three = start(&core);
    assert_eq!(shared.seen().starts[2].resume, None);
    assert!(history(&core, &three, 0)["entries"]
        .as_array()
        .unwrap()
        .is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_same_execution_finds_its_conversation_after_a_core_restart() {
    let home = home("restart");
    let mut config = fast();
    config["resume"] = json!("native");
    let store = StoreConfig {
        resume: Resume::Always,
        ..StoreConfig::default()
    };
    let (core, _) = core_with(config.clone(), false, &home, true, &store);
    let id = start(&core);
    prompt(&core, &id, "before");
    let before = history(&core, &id, 0);
    // The core goes down with the execution running.
    drop(core);

    let (core, shared) = core_with(config, false, &home, true, &store);
    let report = core.recover();
    assert!(
        report.iter().any(|line| line.contains("resumed")),
        "{report:?}"
    );
    assert_eq!(shared.seen().starts[0].resume.as_deref(), Some("native-1"));
    let after = history(&core, &id, 0);
    assert_eq!(after["entries"], before["entries"]);
    assert_eq!(after["session"]["native_resume"], true);
    prompt(&core, &id, "after");
    let turns: Vec<String> = entries(&history(&core, &id, 0))
        .into_iter()
        .map(|e| e.2)
        .collect();
    assert_eq!(turns, ["t-1", "t-1", "t-2", "t-2"]);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_named_conversation_goes_on_in_the_next_execution_and_is_held_by_one() {
    let home = home("named");
    let mut config = fast();
    config["resume"] = json!("native");
    config["conversation"] = json!("named");
    config["conversation_id"] = json!("review");
    let (core, shared) = core(config, false, &home, true);
    let first = start(&core);
    prompt(&core, &first, "hello");

    // While the first holds it, another execution that names it fails to start, and
    // says who holds it.
    let refused = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(refused["state"]["status"], "failed", "{refused}");
    let reason = refused["state"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("chat/id/review") && reason.contains(&format!("`{first}`")),
        "{reason}"
    );
    assert_eq!(shared.seen().starts.len(), 1, "the engine was never asked");

    // Stopped, the next execution continues it: transcript, turns and engine session.
    core.dispatch(method::EXECUTION_STOP, json!({ "id": first }))
        .unwrap();
    let second = start(&core);
    assert_eq!(shared.seen().starts[1].resume.as_deref(), Some("native-1"));
    prompt(&core, &second, "again");
    let found = history(&core, &second, 0);
    let all = entries(&found);
    assert_eq!(all.len(), 4, "{all:?}");
    assert_eq!(all[2], ("user".into(), "again".into(), "t-2".into()));
    assert_eq!(found["entries"][0]["execution_id"], json!(first));
    assert_eq!(found["session"]["native_resume"], true);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_named_conversation_without_an_id_does_not_start() {
    let home = home("unnamed");
    let mut config = fast();
    config["conversation"] = json!("named");
    config["conversation_id"] = json!("a/b");
    let (core, shared) = core(config, false, &home, false);
    let refused = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(refused["state"]["status"], "failed", "{refused}");
    assert!(refused["state"]["reason"]
        .as_str()
        .unwrap()
        .contains("conversation_id"));
    assert!(shared.seen().starts.is_empty());
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn the_workflow_layout_is_read_only_when_asked_for() {
    let home = home("legacy");
    let mut legacy = fast();
    legacy["conversation"] = json!("workflow");
    // Recorded under `conv/<workflow>/<node>`, as before the field existed.
    let (core, _) = core(legacy.clone(), false, &home, false);
    let old = start(&core);
    prompt(&core, &old, "old");
    core.dispatch(method::EXECUTION_STOP, json!({ "id": old }))
        .unwrap();
    drop(core);

    // A new worker with the default sees none of it.
    let (core, _) = self::core(fast(), false, &home, false);
    let fresh = start(&core);
    assert!(history(&core, &fresh, 0)["entries"]
        .as_array()
        .unwrap()
        .is_empty());
    drop(core);

    // One that asks for the workflow's conversation goes on with it.
    let (core, _) = self::core(legacy, false, &home, false);
    let again = start(&core);
    prompt(&core, &again, "new");
    assert_eq!(
        texts(&history(&core, &again, 0), "entries"),
        ["old", "echo: old", "new", "echo: new"]
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn without_a_store_a_named_conversation_goes_on_in_memory() {
    let shared: Arc<Shared> = Arc::default();
    let fake = EventEngine {
        manifest: parse_manifest(&format!(
            "[module]\nid = \"{FAKE}\"\nname = \"fake\"\n[[node]]\ntype = \"engine\"\nname = \"e\"\n"
        ))
        .unwrap(),
        shared,
    };
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Agent::new()), Box::new(fake)];
    let mut config = fast();
    config["conversation"] = json!("named");
    config["conversation_id"] = json!("scratch");
    let mut profiles = ProfileStore::default();
    profiles.insert(profile(config, false)).unwrap();
    let core = Core::build_with(modules, &[], profiles, Storage::default());
    let first = start(&core);
    prompt(&core, &first, "kept");
    core.dispatch(method::EXECUTION_STOP, json!({ "id": first }))
        .unwrap();
    let second = start(&core);
    assert_eq!(
        texts(&history(&core, &second, 0), "entries"),
        ["kept", "echo: kept"]
    );
    // The stopped execution still answers with the same conversation.
    let stopped = history(&core, &first, 0);
    assert_eq!(stopped["session"]["state"], "stopped");
    assert_eq!(texts(&stopped, "entries").len(), 2);
}

#[test]
fn a_permission_request_is_answered_through_the_decider() {
    let home = home("ask");
    let mut config = fast();
    config["permissions"] = json!("ask");
    let (core, shared) = core(config, true, &home, false);
    let id = start(&core);
    assert_eq!(
        shared.seen().starts[0].permissions,
        engine::Permissions::Ask
    );
    core.dispatch(
        "agent.send",
        json!({ "execution_id": id, "node_id": "a", "text": "ask" }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let pending = loop {
        let listed: Vec<Value> =
            serde_json::from_value(core.dispatch("human.list", json!({})).unwrap()).unwrap();
        if let Some(found) = listed.into_iter().next() {
            break found;
        }
        assert!(Instant::now() < deadline, "the decider was never asked");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(pending["kind"], "decision");
    assert!(pending["prompt"].as_str().unwrap().contains("write a file"));
    assert_eq!(pending["value"]["context"]["tool"], "edit");
    core.dispatch(
        "human.answer",
        json!({ "id": pending["id"], "decision": "approve", "fields": { "option": "yes" } }),
    )
    .unwrap();
    let found = history_when(&core, &id, |h| h["entries"].as_array().unwrap().len() == 2);
    assert_eq!(entries(&found)[1].1, "chose yes");
    assert!(found["activity"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["event"]["kind"] == "permission_resolved" && a["event"]["option"] == "yes"));
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn ask_without_a_decider_rejects_and_says_so() {
    let home = home("downgrade");
    let mut config = fast();
    config["permissions"] = json!("ask");
    let (core, shared) = core(config, false, &home, false);
    let id = start(&core);
    assert_eq!(
        shared.seen().starts[0].permissions,
        engine::Permissions::Reject
    );
    let found = history(&core, &id, 0);
    let notes = entries(&found);
    assert_eq!(notes[0].0, "system");
    assert!(
        notes[0].1.contains("nothing is wired into `decisions`"),
        "{notes:?}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn agent_cancel_reaches_the_engine() {
    let home = home("cancel");
    let mut config = fast();
    config["permissions"] = json!("allow");
    let (core, shared) = core(config, false, &home, false);
    let id = start(&core);
    core.dispatch(
        "agent.send",
        json!({ "execution_id": id, "node_id": "a", "text": "ask" }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !shared
        .seen()
        .events
        .values()
        .flatten()
        .any(|e| matches!(e.kind, engine::SessionEventKind::PermissionRequest { .. }))
    {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(10));
    }
    core.dispatch(
        "agent.cancel",
        json!({ "execution_id": id, "node_id": "a" }),
    )
    .unwrap();
    let found = history_when(&core, &id, |h| h["entries"].as_array().unwrap().len() == 2);
    assert_eq!(entries(&found)[1].1, "chose cancelled");
    assert_eq!(shared.seen().cancelled, 1);
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn resume_native_loads_the_last_session_only_when_the_engine_can() {
    let home = home("resume");
    let mut config = fast();
    config["resume"] = json!("native");
    // Only a conversation the next execution continues has a session to resume.
    config["conversation"] = json!("named");
    config["conversation_id"] = json!("worker");
    let (core, shared) = core(config.clone(), false, &home, true);
    let first = start(&core);
    assert_eq!(shared.seen().starts[0].resume, None);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": first }))
        .unwrap();
    let second = start(&core);
    assert_eq!(shared.seen().starts[1].resume.as_deref(), Some("native-1"));
    let found = history(&core, &second, 0);
    assert_eq!(found["session"]["native_resume"], true);
    assert_eq!(found["session"]["engine_session_id"], "native-1");

    // An engine that cannot load is never asked to, and nothing pretends it resumed.
    let home = self::home("no-resume");
    let (core, shared) = self::core(config, false, &home, false);
    let first = start(&core);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": first }))
        .unwrap();
    let second = start(&core);
    assert_eq!(shared.seen().starts[1].resume, None);
    assert_eq!(
        history(&core, &second, 0)["session"]["native_resume"],
        false
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn paging_with_the_cursor_gives_every_entry_and_event_exactly_once() {
    let home = home("pages");
    let (core, _) = core(fast(), false, &home, false);
    let id = start(&core);
    for text in ["tool", "tool", "hello"] {
        core.dispatch(
            "agent.prompt",
            json!({ "execution_id": id, "node_id": "a", "text": text }),
        )
        .unwrap();
    }
    let all = history(&core, &id, 0);
    let seqs = |page: &Value| -> Vec<u64> {
        let mut seqs: Vec<u64> = ["entries", "activity"]
            .iter()
            .flat_map(|list| page[*list].as_array().unwrap().clone())
            .map(|item| item["seq"].as_u64().unwrap())
            .collect();
        seqs.sort_unstable();
        seqs
    };
    let full = seqs(&all);
    assert!(full.len() > 6, "{full:?}");
    // Entries and activity share one sequence: no seq is used twice.
    assert!(full.windows(2).all(|w| w[0] < w[1]), "{full:?}");
    let page = |since: u64, limit: usize| {
        core.dispatch(
            "agent.history",
            json!({ "execution_id": id, "node_id": "a", "since": since, "limit": limit }),
        )
        .unwrap()
    };
    let half = full.len() / 2;
    let first = page(0, half);
    let next = first["next"].as_u64().unwrap();
    let second = page(next, full.len());
    let mut read = seqs(&first);
    assert_eq!(read.len(), half);
    read.extend(seqs(&second));
    assert_eq!(read, full, "two pages give the whole set, once");
    let last = second["next"].as_u64().unwrap();
    assert_eq!(last, *full.last().unwrap());
    let empty = page(last, full.len());
    assert!(seqs(&empty).is_empty() && empty["next"] == json!(last));
    let _ = std::fs::remove_dir_all(&home);
}
