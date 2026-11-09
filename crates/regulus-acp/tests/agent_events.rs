//! The Agent box over the real ACP channel with the scripted agent: its session's events
//! reach the box's history, and a permission request the agent makes is answered by a
//! person through a Decider of `human`.

use std::time::{Duration, Instant};

use regulus_acp::engine::{AcpEngine, EngineModule, Prepared, Start};
use regulus_acp::Launch;
use regulus_contract_agent_engine::Installation;
use regulus_core::{parse_manifest, Core, Module, ProfileStore};
use regulus_proto::{method, Profile};
use serde_json::{json, Value};

const MANIFEST: &str = r#"
[module]
id   = "test/engine-fake@0.1.0"
name = "Fake engine"

[[node]]
type = "engine-fake"
name = "Fake"
"#;

struct Fake;

impl AcpEngine for Fake {
    fn detect(&self) -> Vec<Installation> {
        vec![Installation {
            path: env!("CARGO_BIN_EXE_regulus-acp-fake-agent").into(),
            version: None,
        }]
    }
    fn protocols(&self) -> &'static [&'static str] {
        &[]
    }
    fn prepare(&self, start: &Start<'_>) -> Result<Prepared, String> {
        Ok(Prepared {
            launch: Launch {
                program: start.detected[0].path.clone(),
                ..Launch::default()
            },
            ..Prepared::default()
        })
    }
}

fn port(id: &str, capability: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": "uses" })
}

fn profile() -> Profile {
    serde_json::from_value(json!({
        "id": "agents", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "nodes": [
            { "id": "agent", "type": "agent", "module_id": "regulus/agent@0.1.0",
              "config": { "workdir": "/tmp", "permissions": "ask", "events_poll_ms": 20,
                          "decision_poll_ms": 20 },
              "ports": [port("engine", "agent.engine@1", "input"),
                        port("decisions", "decision@1", "input")] },
            { "id": "engine", "type": "engine-fake", "module_id": "test/engine-fake@0.1.0",
              "ports": [port("engine", "agent.engine@1", "output")] },
            { "id": "person", "type": "decider", "module_id": "regulus/human@0.1.0",
              "ports": [port("decisions", "decision@1", "output")] }
        ], "wires": [
            { "id": "w1", "kind": "uses", "from": { "node_id": "engine", "port_id": "engine" },
              "to": { "node_id": "agent", "port_id": "engine" } },
            { "id": "w2", "kind": "uses",
              "from": { "node_id": "person", "port_id": "decisions" },
              "to": { "node_id": "agent", "port_id": "decisions" } }
        ] }]
    }))
    .unwrap()
}

fn history(core: &Core, id: &str) -> Value {
    core.dispatch(
        "agent.history",
        json!({ "execution_id": id, "node_id": "agent" }),
    )
    .unwrap()
}

#[test]
fn acp_events_reach_the_history_and_a_person_answers_the_agent() {
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(regulus_module_agent::Agent::new()),
        Box::new(EngineModule::new(parse_manifest(MANIFEST).unwrap(), Fake)),
        Box::new(regulus_module_human::Human::new()),
    ];
    let mut profiles = ProfileStore::default();
    profiles.insert(profile()).unwrap();
    let core = Core::build(modules, &[], profiles);
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "agents" }))
        .unwrap();
    let id = execution["id"].as_str().unwrap().to_string();

    let turn = core
        .dispatch(
            "agent.prompt",
            json!({ "execution_id": id, "node_id": "agent", "text": "tool" }),
        )
        .unwrap();
    assert_eq!(turn["text"], "tool done");
    let found = history(&core, &id);
    let kinds: Vec<&str> = found["activity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["event"]["kind"].as_str().unwrap())
        .collect();
    for kind in [
        "turn_started",
        "tool_call",
        "tool_update",
        "text",
        "turn_ended",
    ] {
        assert!(kinds.contains(&kind), "{kind} missing from {kinds:?}");
    }
    assert_eq!(found["session"]["engine_session_id"], "fake-session");

    core.dispatch(
        "agent.send",
        json!({ "execution_id": id, "node_id": "agent", "text": "ask" }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let pending = loop {
        let listed: Vec<Value> =
            serde_json::from_value(core.dispatch("human.list", json!({})).unwrap()).unwrap();
        if let Some(first) = listed.into_iter().next() {
            break first;
        }
        assert!(Instant::now() < deadline, "nobody was asked");
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(
        pending["form"]["properties"]["option"]["enum"],
        json!(["yes", "always", "no"])
    );
    core.dispatch(
        "human.answer",
        json!({ "id": pending["id"], "decision": "approve", "fields": { "option": "always" } }),
    )
    .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let found = history(&core, &id);
        let last = found["entries"].as_array().unwrap().last().cloned();
        if last.as_ref().is_some_and(|e| e["text"] == "chose always") {
            break;
        }
        assert!(Instant::now() < deadline, "{found:#}");
        std::thread::sleep(Duration::from_millis(20));
    }
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
}
