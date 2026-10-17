//! The whole path of docs/12-agents.md through the real core: an execution starts the
//! Agent box, the box reaches its engine over the wire, the engine opens an ACP session,
//! a prompt goes through and stopping the execution ends the agent process.

use regulus_acp::engine::{AcpEngine, EngineModule, Prepared, Start};
use regulus_acp::Launch;
use regulus_contract_agent_engine::Installation;
use regulus_core::{parse_manifest, Core, Module, ProfileStore};
use regulus_proto::{method, ExecutionState, Profile};
use serde_json::{json, Value};

const MANIFEST: &str = r#"
[module]
id      = "test/engine-fake@0.1.0"
name    = "Fake engine"
methods = ["fake.detect"]

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

fn profile(wired: bool) -> Profile {
    let wires = if wired {
        json!([{ "id": "w", "kind": "uses",
                 "from": { "node_id": "engine", "port_id": "engine" },
                 "to": { "node_id": "agent", "port_id": "engine" } }])
    } else {
        json!([])
    };
    serde_json::from_value(json!({
        "id": "agents",
        "main_workflow_id": "main",
        "workflows": [{
            "id": "main",
            "kind": "main",
            "nodes": [
                { "id": "agent", "type": "agent", "module_id": "regulus/agent@0.1.0",
                  "config": { "workdir": "/tmp", "permissions": "allow" },
                  "ports": [{ "id": "engine", "capability": "agent.engine@1", "direction": "input", "kind": "uses", "required": true }] },
                { "id": "engine", "type": "engine-fake", "module_id": "test/engine-fake@0.1.0",
                  "ports": [{ "id": "engine", "capability": "agent.engine@1", "direction": "output", "kind": "uses" }] }
            ],
            "wires": wires
        }]
    }))
    .unwrap()
}

fn core(wired: bool) -> std::sync::Arc<Core> {
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(regulus_module_agent::Agent::new()),
        Box::new(EngineModule::new(parse_manifest(MANIFEST).unwrap(), Fake)),
    ];
    let mut profiles = ProfileStore::default();
    profiles.insert(profile(wired)).unwrap();
    Core::build(modules, &[], profiles)
}

fn prompt(core: &Core, execution: &str, text: &str) -> Result<Value, regulus_core::RpcError> {
    core.dispatch(
        "agent.prompt",
        json!({ "execution_id": execution, "node_id": "agent", "text": text }),
    )
}

#[test]
fn the_box_runs_its_wired_engine_end_to_end() {
    let core = core(true);
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "agents" }))
        .unwrap();
    assert_eq!(execution["state"]["status"], "running", "{execution}");
    let id = execution["id"].as_str().unwrap();

    let turn = prompt(&core, id, "hello").unwrap();
    assert_eq!(
        turn,
        json!({ "stop_reason": "end_turn", "text": "echo: hello" })
    );
    assert_eq!(
        prompt(&core, id, "cwd").unwrap()["text"],
        "/tmp",
        "the box's workdir reaches the agent"
    );
    assert_eq!(
        prompt(&core, id, "permission").unwrap()["text"],
        "yes",
        "the box's policy reaches the session"
    );

    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert!(
        prompt(&core, id, "hello").is_err(),
        "a stopped agent takes no prompts"
    );
}

#[test]
fn a_box_without_an_engine_is_disabled_and_says_why() {
    let core = core(false);
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "agents" }))
        .unwrap();
    let state: ExecutionState = serde_json::from_value(execution["state"].clone()).unwrap();
    assert_eq!(state, ExecutionState::Running);
    assert_eq!(
        execution["disabled"],
        json!([{ "node_id": "agent", "missing": ["engine"] }])
    );
    let id = execution["id"].as_str().unwrap();
    assert!(
        prompt(&core, id, "hello").is_err(),
        "a disabled agent takes no prompts"
    );
}

#[test]
fn engines_report_what_they_find_without_an_execution() {
    let core = core(true);
    let detected = core.dispatch("fake.detect", json!({})).unwrap();
    assert_eq!(detected["installations"].as_array().unwrap().len(), 1);
}
