//! A `rule` through the core's relay (`hook.dispatch`), and the same execution without
//! the module: the hooks it wired in are gone and everything else keeps running.

use regulus_core::{Core, Module, ModuleCtx, ModuleError, ProfileStore};
use regulus_module_hooks::Hooks;
use regulus_proto::{method, HookAnswer, HookDispatched, Manifest, ModuleInfo, Profile};
use serde_json::{json, Value};

/// Stands for the Agent box: a node with a `hooks` input and no behavior of its own.
struct Holder(Manifest);

impl Module for Holder {
    fn manifest(&self) -> &Manifest {
        &self.0
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, _: Value) -> Result<Value, ModuleError> {
        Err(ModuleError::UnknownMethod(name.into()))
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn profile() -> Profile {
    let rule = |id: &str, config: Value| {
        json!({ "id": id, "type": "rule", "module_id": "regulus/hooks@0.1.0", "config": config,
                "ports": [{ "id": "hook", "capability": "hook@1", "direction": "output", "kind": "uses" }] })
    };
    let wire = |id: &str| {
        json!({ "id": format!("w-{id}"), "kind": "uses",
                "from": { "node_id": id, "port_id": "hook" },
                "to": { "node_id": "agent", "port_id": "hooks" } })
    };
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "nodes": [
                { "id": "agent", "type": "agent", "module_id": "t/holder@1.0.0",
                  "ports": [{ "id": "hooks", "capability": "hook@1", "direction": "input",
                              "kind": "uses", "cardinality": "many" }] },
                rule("no-bash", json!({ "matcher": "Bash", "deny": "yes", "reason": "no shell" })),
                rule("brief", json!({ "event": "session_start", "context": "be brief" })),
            ],
            "wires": [wire("no-bash"), wire("brief")],
        }]
    }))
    .unwrap()
}

fn core(without: &[String]) -> std::sync::Arc<Core> {
    let holder = Manifest {
        module: ModuleInfo {
            id: "t/holder@1.0.0".into(),
            name: "holder".into(),
            requires_proto: None,
            methods: vec![],
            workflow_id: None,
            origin: None,
        },
        nodes: vec![],
    };
    let mut profiles = ProfileStore::default();
    profiles.insert(profile()).unwrap();
    Core::build(
        vec![Box::new(Holder(holder)), Box::new(Hooks::new())],
        without,
        profiles,
    )
}

fn ask(core: &Core, event: &str, tool: Option<&str>) -> HookDispatched {
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(execution["state"]["status"], "running", "{execution}");
    let request = json!({ "event": event, "tool": tool, "raw": {} });
    let params = json!({ "execution_id": execution["id"], "node_id": "agent", "request": request });
    serde_json::from_value(core.dispatch(method::HOOK_DISPATCH, params).unwrap()).unwrap()
}

#[test]
fn rules_answer_through_the_relay() {
    let core = core(&[]);
    assert_eq!(
        ask(&core, "pre_tool", Some("Bash")).combined(),
        HookAnswer::Veto {
            reason: "no shell".into()
        }
    );
    assert_eq!(
        ask(&core, "session_start", None).combined(),
        HookAnswer::Context {
            text: "be brief".into()
        }
    );
    assert_eq!(
        ask(&core, "pre_tool", Some("Read")).combined(),
        HookAnswer::Nothing
    );
}

#[test]
fn without_the_module_the_agent_runs_with_no_hooks() {
    let core = core(&["regulus/hooks".to_string()]);
    let got = ask(&core, "pre_tool", Some("Bash"));
    assert!(got.replies.is_empty(), "{got:?}");
    assert_eq!(got.combined(), HookAnswer::Nothing);
}
