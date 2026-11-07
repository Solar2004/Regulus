//! The `hooks` slot from the box to the profile (docs/13-blocks.md §4): the Agent box
//! asks each hook wired in what it listens to, builds the bridge command in its own name,
//! and the engine writes it into an isolated profile, never into the user's.

use std::path::{Path, PathBuf};

use regulus_acp::engine::{AcpEngine, EngineModule, Prepared, Start};
use regulus_acp::Launch;
use regulus_contract_agent_engine::{HookMount, Installation};
use regulus_core::{parse_manifest, Core, Module, ModuleCtx, ModuleError, ProfileStore};
use regulus_module_agent::{Agent, HookBridge};
use regulus_proto::{method, Manifest, NodeCall, Profile};
use serde_json::{json, Value};

const ENGINE: &str = r#"
[module]
id      = "test/engine-fake@0.1.0"
name    = "Fake engine"

[[node]]
type = "engine-fake"
name = "Fake"
"#;

const HOOKS: &str = r#"
[module]
id   = "test/hooks@0.1.0"
name = "Fake hooks"

[[node]]
type = "hook"
name = "Hook"
"#;

/// Writes hooks the way Claude Code reads them; runs the fake ACP agent.
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
    fn place_hooks(&self, dir: &Path, hooks: &[HookMount]) -> Option<Result<Vec<String>, String>> {
        Some(regulus_module_engine_claude::hook::place(dir, hooks))
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

/// A hook node describes itself from its own config.
struct Hooks(Manifest);

impl Module for Hooks {
    fn manifest(&self) -> &Manifest {
        &self.0
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params).unwrap();
        match name {
            regulus_proto::HOOK_DESCRIBE => Ok(Value::Object(call.node.config)),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-acp-hooks-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn profile(engine: Value) -> Profile {
    let hook = |id: &str, config: Value| {
        json!({ "id": id, "type": "hook", "module_id": "test/hooks@0.1.0", "config": config,
                "ports": [{ "id": "hook", "capability": "hook@1", "direction": "output", "kind": "uses" }] })
    };
    let uses = |id: &str, from: (&str, &str), to: &str| {
        json!({ "id": id, "kind": "uses", "from": { "node_id": from.0, "port_id": from.1 },
                "to": { "node_id": "agent", "port_id": to } })
    };
    serde_json::from_value(json!({
        "id": "agents", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "nodes": [
                { "id": "agent", "type": "agent", "module_id": "regulus/agent@0.1.0",
                  "config": { "workdir": "/tmp" } },
                { "id": "engine", "type": "engine-fake", "module_id": "test/engine-fake@0.1.0",
                  "config": engine,
                  "ports": [{ "id": "engine", "capability": "agent.engine@1", "direction": "output", "kind": "uses" }] },
                hook("no-bash", json!({ "event": "pre_tool", "matcher": "Bash", "timeout_ms": 3000, "returns": ["veto"] })),
                hook("brief", json!({ "event": "session_start", "returns": ["context"] })),
            ],
            "wires": [
                uses("w1", ("engine", "engine"), "engine"),
                uses("w2", ("no-bash", "hook"), "hooks"),
                uses("w3", ("brief", "hook"), "hooks"),
            ]
        }]
    }))
    .unwrap()
}

fn core(agent: Agent, engine: Value) -> std::sync::Arc<Core> {
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(agent),
        Box::new(EngineModule::new(parse_manifest(ENGINE).unwrap(), Fake)),
        Box::new(Hooks(parse_manifest(HOOKS).unwrap())),
    ];
    let mut profiles = ProfileStore::default();
    profiles.insert(profile(engine)).unwrap();
    Core::build(modules, &[], profiles)
}

fn bridge() -> HookBridge {
    HookBridge {
        program: "/usr/bin/regulus".into(),
        args: vec!["--home".into(), "/h".into(), "hook".into()],
    }
}

#[test]
fn hooks_wired_into_the_box_land_in_the_isolated_profile() {
    let profile_dir = dir("isolated");
    let engine = json!({ "profile": "isolated", "config_dir": profile_dir });
    let core = core(Agent::with_bridge(bridge()), engine);
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "agents" }))
        .unwrap();
    assert_eq!(execution["state"]["status"], "running", "{execution}");
    let id = execution["id"].as_str().unwrap();
    let settings: Value =
        serde_json::from_str(&std::fs::read_to_string(profile_dir.join("settings.json")).unwrap())
            .unwrap();
    let pre = &settings["hooks"]["PreToolUse"][0];
    assert_eq!(pre["matcher"], "Bash");
    assert_eq!(pre["hooks"][0]["timeout"], 4);
    assert_eq!(
        pre["hooks"][0]["command"],
        format!(
            "'/usr/bin/regulus' '--home' '/h' 'hook' '--execution' '{id}' '--node' 'agent' \
             '--hook' 'no-bash' '--event' 'pre_tool' '--timeout-ms' '3000' '--format' 'claude'"
        )
    );
    let start = &settings["hooks"]["SessionStart"][0]["hooks"][0]["command"];
    assert!(
        start
            .as_str()
            .unwrap()
            .contains("'--hook' 'brief' '--event' 'session_start'"),
        "{start}"
    );
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    std::fs::remove_dir_all(&profile_dir).unwrap();
}

#[test]
fn a_host_without_the_bridge_cannot_start_a_box_with_hooks() {
    let profile_dir = dir("nobridge");
    let engine = json!({ "profile": "isolated", "config_dir": profile_dir });
    let core = core(Agent::new(), engine);
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "agents" }))
        .unwrap();
    let reason = execution["state"]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("cannot start the hook bridge"),
        "{execution}"
    );
    std::fs::remove_dir_all(&profile_dir).unwrap();
}

#[test]
fn the_users_profile_gets_no_hooks_and_a_warning() {
    let profile_dir = dir("user");
    let engine = EngineModule::new(parse_manifest(ENGINE).unwrap(), Fake);
    let node = json!({ "id": "engine", "type": "engine-fake", "module_id": "test/engine-fake@0.1.0",
                       "config": { "profile": "user", "config_dir": profile_dir } });
    let hook = json!({ "node": "no-bash", "event": "pre_tool", "command": "/usr/bin/regulus" });
    let started = engine
        .call(
            "engine.session.start",
            json!({ "execution_id": "exec-1", "node": node,
                    "args": { "cwd": "/tmp", "hooks": [hook] } }),
        )
        .unwrap();
    let warnings: Vec<String> = serde_json::from_value(started["warnings"].clone()).unwrap();
    assert_eq!(warnings, ["hooks are placed only in an isolated profile"]);
    assert!(!profile_dir.join("settings.json").exists());
    let session = started["session_id"].clone();
    engine
        .call(
            "engine.session.stop",
            json!({ "execution_id": "exec-1", "node": node, "args": { "session_id": session } }),
        )
        .unwrap();
    std::fs::remove_dir_all(&profile_dir).unwrap();
}
