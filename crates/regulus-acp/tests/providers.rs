//! A provider wired into the Agent box reaches the engine (docs/12-agents.md §6): its
//! credential resolved by the core from the secret store, its protocol checked against
//! the engine's, and an isolated profile placed in the node's own directory.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regulus_acp::engine::{AcpEngine, EngineModule, Prepared, Start};
use regulus_acp::Launch;
use regulus_contract_agent_engine::{Installation, LlmEndpoint};
use regulus_core::secrets::{Secrets, Source};
use regulus_core::{parse_manifest, Core, Module, ProfileStore, Storage};
use regulus_proto::{method, Profile};
use serde_json::{json, Value};

const MANIFEST: &str = r#"
[module]
id   = "test/engine-fake@0.1.0"
name = "Fake engine"

[[node]]
type = "engine-fake"
name = "Fake"

  [[node.field]]
  id      = "profile"
  type    = "select"
  default = "user"
  options = ["user", "isolated"]
"#;

/// What the engine was handed for each session it prepared: provider, profile directory,
/// whether it is isolated, and the slots that reached it.
type Seen = Arc<Mutex<Vec<(Option<LlmEndpoint>, Option<PathBuf>, bool, Vec<String>)>>>;

struct Fake(Seen);

impl AcpEngine for Fake {
    fn detect(&self) -> Vec<Installation> {
        vec![Installation {
            path: env!("CARGO_BIN_EXE_regulus-acp-fake-agent").into(),
            version: None,
        }]
    }
    fn protocols(&self) -> &'static [&'static str] {
        &["anthropic"]
    }
    fn slots(&self) -> &'static [&'static str] {
        &["subagent"]
    }
    fn prepare(&self, start: &Start<'_>) -> Result<Prepared, String> {
        self.0.lock().unwrap().push((
            start.provider.cloned(),
            start.config_dir.clone(),
            start.isolated,
            start.slots.keys().cloned().collect(),
        ));
        Ok(Prepared {
            launch: Launch {
                program: start.detected[0].path.clone(),
                ..Launch::default()
            },
            ..Prepared::default()
        })
    }
}

fn profile(protocol: &str) -> Profile {
    profile_with(protocol, &[])
}

/// The profile with the provider also wired into each of `slots`.
fn profile_with(protocol: &str, slots: &[&str]) -> Profile {
    let mut profile = json!({
        "id": "agents",
        "project_id": "demo",
        "main_workflow_id": "main",
        "workflows": [{
            "id": "main",
            "kind": "main",
            "nodes": [
                { "id": "agent", "type": "agent", "module_id": "regulus/agent@0.1.0",
                  "config": { "workdir": "/tmp" },
                  "ports": [
                    { "id": "engine", "capability": "agent.engine@1", "direction": "input", "kind": "uses", "required": true },
                    { "id": "provider", "capability": "llm.endpoint@1", "direction": "input", "kind": "uses" }
                  ] },
                { "id": "engine", "type": "engine-fake", "module_id": "test/engine-fake@0.1.0",
                  "config": { "profile": "isolated" },
                  "ports": [{ "id": "engine", "capability": "agent.engine@1", "direction": "output", "kind": "uses" }] },
                { "id": "openrouter", "type": "provider", "module_id": "regulus/providers@0.1.0",
                  "config": { "protocol": protocol, "base_url": "https://openrouter.ai/api",
                              "credential": "secret:openrouter", "model": "m1" },
                  "ports": [{ "id": "endpoint", "capability": "llm.endpoint@1", "direction": "output", "kind": "uses" }] }
            ],
            "wires": [
                { "id": "e", "kind": "uses",
                  "from": { "node_id": "engine", "port_id": "engine" },
                  "to": { "node_id": "agent", "port_id": "engine" } },
                { "id": "p", "kind": "uses",
                  "from": { "node_id": "openrouter", "port_id": "endpoint" },
                  "to": { "node_id": "agent", "port_id": "provider" } }
            ]
        }]
    });
    let workflow = &mut profile["workflows"][0];
    for slot in slots {
        workflow["nodes"][0]["ports"]
            .as_array_mut()
            .unwrap()
            .push(json!(
                { "id": slot, "capability": "llm.endpoint@1", "direction": "input", "kind": "uses" }
            ));
        workflow["wires"].as_array_mut().unwrap().push(json!({
            "id": format!("slot-{slot}"), "kind": "uses",
            "from": { "node_id": "openrouter", "port_id": "endpoint" },
            "to": { "node_id": "agent", "port_id": slot }
        }));
    }
    serde_json::from_value(profile).unwrap()
}

fn home(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-providers-{test}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn core(home: &Path, protocol: &str, seen: &Seen) -> Arc<Core> {
    core_with(home, profile(protocol), seen)
}

fn core_with(home: &Path, profile: Profile, seen: &Seen) -> Arc<Core> {
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(regulus_module_agent::Agent::new()),
        Box::new(regulus_module_providers::Providers::new()),
        Box::new(EngineModule::new(
            parse_manifest(MANIFEST).unwrap(),
            Fake(seen.clone()),
        )),
    ];
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let storage = Storage {
        secrets: Secrets::new(vec![Source::File(home.join("secrets.toml"))]),
        data_dir: Some(home.join("data")),
    };
    Core::build_with(modules, &[], profiles, storage)
}

fn start(core: &Core) -> Value {
    core.dispatch(method::EXECUTION_START, json!({ "profile_id": "agents" }))
        .unwrap()
}

#[test]
fn the_provider_reaches_the_engine_with_its_secret_and_an_isolated_profile() {
    let home = home("wired");
    let seen = Seen::default();
    let core = core(&home, "anthropic", &seen);

    let missing = start(&core);
    assert_eq!(missing["state"]["status"], "failed");
    assert!(
        missing["state"]["reason"]
            .as_str()
            .unwrap()
            .contains("regulus secret set openrouter"),
        "a missing secret says how to set it: {missing}"
    );

    core.dispatch(
        method::SECRET_SET,
        json!({ "name": "openrouter", "value": "sk-or-1" }),
    )
    .unwrap();
    assert_eq!(
        core.dispatch(method::SECRET_LIST, Value::Null).unwrap(),
        json!(["openrouter"]),
        "listing shows names, never values"
    );
    let execution = start(&core);
    assert_eq!(execution["state"]["status"], "running", "{execution}");

    let (provider, config_dir, isolated, _) = seen.lock().unwrap().last().cloned().unwrap();
    let provider = provider.expect("the box hands the provider to the engine");
    assert_eq!(provider.header("Authorization"), Some("Bearer sk-or-1"));
    assert_eq!(provider.models, vec!["m1".to_string()]);
    assert!(isolated);
    let expected = home.join("data/projects/demo/test/engine-fake/main/engine/profile");
    assert_eq!(config_dir.as_deref(), Some(expected.as_path()));
    assert!(
        expected.is_dir(),
        "the isolated profile exists before launch"
    );

    let id = execution["id"].as_str().unwrap();
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_provider_the_engine_cannot_speak_fails_the_start_before_launching() {
    let home = home("mismatch");
    let seen = Seen::default();
    let core = core(&home, "openai", &seen);
    core.dispatch(
        method::SECRET_SET,
        json!({ "name": "openrouter", "value": "k" }),
    )
    .unwrap();
    let execution = start(&core);
    assert_eq!(execution["state"]["status"], "failed");
    let reason = execution["state"]["reason"].as_str().unwrap();
    assert!(reason.contains("translator"), "{reason}");
    assert!(seen.lock().unwrap().is_empty(), "nothing was launched");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn slots_reach_the_engine_that_knows_them_and_the_others_only_warn() {
    let home = home("slots");
    let seen = Seen::default();
    let core = core_with(
        &home,
        profile_with("anthropic", &["fast", "subagent"]),
        &seen,
    );
    core.dispatch(
        method::SECRET_SET,
        json!({ "name": "openrouter", "value": "k" }),
    )
    .unwrap();
    let execution = start(&core);
    assert_eq!(
        execution["state"]["status"], "running",
        "a slot the engine does not know never fails the start: {execution}"
    );
    let (_, _, _, slots) = seen.lock().unwrap().last().cloned().unwrap();
    assert_eq!(slots, vec!["subagent".to_string()]);
    let id = execution["id"].as_str().unwrap();
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
}
