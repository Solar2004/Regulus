//! The hook relay (docs/13-blocks.md §4): `hook.dispatch` asks the `hook@1` nodes wired
//! into a node with `node/hook`, each within a time limit, and a hook that does not
//! answer in time counts as no answer instead of holding the agent.

use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{
    BusConfig, CallConfig, Core, HooksConfig, Module, ModuleCtx, ModuleError, ProfileStore, Storage,
};
use regulus_proto::{method, HookDispatched, HookRequest, Manifest, ModuleInfo, NodeCall, Profile};
use serde_json::{json, Value};

/// A hook node answers `config.answer`; with `config.sleep_ms` it first sleeps that long,
/// with `config.fail` it fails. It also records the request it got in its answer's
/// `seen` when `config.echo` is set.
struct Hooks {
    manifest: Manifest,
}

impl Module for Hooks {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params)
            .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
        match name {
            method::NODE_HOOK => {
                if let Some(ms) = call.node.config.get("sleep_ms").and_then(Value::as_u64) {
                    std::thread::sleep(Duration::from_millis(ms));
                }
                if call.node.config.contains_key("fail") {
                    return Err(ModuleError::Failed("told to fail".into()));
                }
                if call.node.config.contains_key("echo") {
                    let request: HookRequest = serde_json::from_value(call.args).unwrap();
                    let text = format!("{} {}", request.event, request.tool.unwrap_or_default());
                    return Ok(json!({ "decision": "context", "text": text }));
                }
                Ok(call
                    .node
                    .config
                    .get("answer")
                    .cloned()
                    .unwrap_or(Value::Null))
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

const MODULE: &str = "t/hooks@1.0.0";
/// A module whose nodes have no `node/hook`.
const DEAF: &str = "t/deaf@1.0.0";

fn module(id: &str) -> Box<dyn Module> {
    Box::new(Hooks {
        manifest: Manifest {
            module: ModuleInfo {
                id: id.into(),
                name: id.into(),
                requires_proto: None,
                methods: vec![],
                workflow_id: None,
                origin: None,
            },
            nodes: vec![],
        },
    })
}

fn hook(id: &str, module: &str, config: Value) -> Value {
    json!({ "id": id, "type": "hook", "module_id": module, "config": config, "ports": [
        { "id": "hook", "capability": "hook@1", "direction": "output", "kind": "uses" },
        { "id": "text", "capability": "text@1", "direction": "output", "kind": "uses" },
    ]})
}

fn uses(id: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": "uses",
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

/// `agent` has hooks `veto`, `context`, `slow` (never in time), `broken`, `deaf` and
/// `echo` wired into `hooks`, and `other` wired into a text input, which is no hook.
fn profile() -> Profile {
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "nodes": [
                { "id": "agent", "type": "agent", "module_id": MODULE, "ports": [
                    { "id": "hooks", "capability": "hook@1", "direction": "input",
                      "kind": "uses", "cardinality": "many" },
                    { "id": "notes", "capability": "text@1", "direction": "input", "kind": "uses" },
                ]},
                hook("veto", MODULE, json!({ "answer": { "decision": "veto", "reason": "no Bash" } })),
                hook("context", MODULE, json!({ "answer": { "decision": "context", "text": "be brief" } })),
                hook("slow", MODULE, json!({ "sleep_ms": 3000, "answer": { "decision": "veto", "reason": "late" } })),
                hook("broken", MODULE, json!({ "fail": true })),
                hook("deaf", DEAF, json!({})),
                hook("echo", MODULE, json!({ "echo": true })),
                hook("other", MODULE, json!({ "answer": { "decision": "veto", "reason": "not a hook" } })),
                hook("quiet", MODULE, json!({})),
            ],
            "wires": [
                uses("w1", ("veto", "hook"), ("agent", "hooks")),
                uses("w2", ("context", "hook"), ("agent", "hooks")),
                uses("w3", ("slow", "hook"), ("agent", "hooks")),
                uses("w4", ("broken", "hook"), ("agent", "hooks")),
                uses("w5", ("deaf", "hook"), ("agent", "hooks")),
                uses("w6", ("echo", "hook"), ("agent", "hooks")),
                uses("w7", ("other", "text"), ("agent", "notes")),
                uses("w8", ("quiet", "hook"), ("agent", "hooks")),
            ]
        }]
    }))
    .unwrap()
}

fn core(hooks: HooksConfig) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile()).unwrap();
    let deaf = Box::new(Deaf(module(DEAF)));
    Core::build_with_hooks(
        vec![module(MODULE), deaf],
        &[],
        profiles,
        Storage::default(),
        BusConfig::default(),
        CallConfig::default(),
        hooks,
    )
}

/// Answers nothing but its manifest: no lifecycle, no hooks.
struct Deaf(Box<dyn Module>);

impl Module for Deaf {
    fn manifest(&self) -> &Manifest {
        self.0.manifest()
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

fn start(core: &Core) -> String {
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(execution["state"]["status"], "running", "{execution}");
    execution["id"].as_str().unwrap().into()
}

fn dispatch(core: &Core, params: Value) -> HookDispatched {
    serde_json::from_value(core.dispatch(method::HOOK_DISPATCH, params).unwrap()).unwrap()
}

fn request() -> Value {
    json!({ "event": "pre_tool", "tool": "Bash", "raw": { "tool_name": "Bash" } })
}

#[test]
fn every_hook_wired_in_is_asked_and_one_out_of_time_counts_as_no_answer() {
    let core = core(HooksConfig::default());
    let id = start(&core);
    let began = Instant::now();
    let got = dispatch(
        &core,
        json!({ "execution_id": id, "node_id": "agent", "timeout_ms": 200, "request": request() }),
    );
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "it waited for the slow hook"
    );
    let by = |node: &str| got.replies.iter().find(|r| r.node_id == node).unwrap();
    let order: Vec<&str> = got.replies.iter().map(|r| r.node_id.as_str()).collect();
    assert_eq!(
        order,
        ["veto", "context", "slow", "broken", "deaf", "echo", "quiet"]
    );
    assert_eq!(
        by("veto").answer,
        regulus_proto::HookAnswer::Veto {
            reason: "no Bash".into()
        }
    );
    assert_eq!(
        by("echo").answer,
        regulus_proto::HookAnswer::Context {
            text: "pre_tool Bash".into()
        }
    );
    assert_eq!(by("quiet").answer, regulus_proto::HookAnswer::Nothing);
    assert_eq!(by("quiet").error, None);
    for (node, error) in [
        ("slow", "no answer within 200 ms"),
        ("broken", "told to fail"),
        ("deaf", "node/hook"),
    ] {
        let reply = by(node);
        assert_eq!(reply.answer, regulus_proto::HookAnswer::Nothing, "{node}");
        assert!(
            reply.error.as_deref().unwrap().contains(error),
            "{node}: {reply:?}"
        );
    }
    assert_eq!(
        got.combined(),
        regulus_proto::HookAnswer::Veto {
            reason: "no Bash".into()
        }
    );
}

#[test]
fn one_hook_can_be_asked_alone_and_the_default_limit_comes_from_config() {
    let core = core(HooksConfig { timeout_ms: 50 });
    let id = start(&core);
    let only = |hook: &str| {
        dispatch(
            &core,
            json!({ "execution_id": id, "node_id": "agent", "hook_id": hook, "request": request() }),
        )
    };
    let context = only("context");
    assert_eq!(context.replies.len(), 1);
    assert_eq!(
        context.combined(),
        regulus_proto::HookAnswer::Context {
            text: "be brief".into()
        }
    );
    let slow = only("slow");
    assert_eq!(
        slow.replies[0].error.as_deref(),
        Some("no answer within 50 ms")
    );
    // A node wired in, but not as a hook, cannot be reached through the relay.
    for (params, wanted) in [
        (
            json!({ "execution_id": id, "node_id": "agent", "hook_id": "other", "request": request() }),
            "no running hook `other`",
        ),
        (
            json!({ "execution_id": "exec-9", "node_id": "agent", "request": request() }),
            "Unknown or stopped execution",
        ),
        (
            json!({ "execution_id": id, "node_id": "ghost", "request": request() }),
            "not running",
        ),
    ] {
        let error = core.dispatch(method::HOOK_DISPATCH, params).unwrap_err();
        assert!(error.message.contains(wanted), "{}", error.message);
    }
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let stopped = core
        .dispatch(
            method::HOOK_DISPATCH,
            json!({ "execution_id": id, "node_id": "agent", "request": request() }),
        )
        .unwrap_err();
    assert!(stopped.message.contains("stopped"), "{}", stopped.message);
}

#[test]
fn a_node_with_no_hooks_wired_gets_no_replies() {
    let core = core(HooksConfig::default());
    let id = start(&core);
    let got = dispatch(
        &core,
        json!({ "execution_id": id, "node_id": "veto", "request": request() }),
    );
    assert!(got.replies.is_empty());
    assert_eq!(got.combined(), regulus_proto::HookAnswer::Nothing);
}

#[test]
fn the_default_limit_is_read_from_the_config_file() {
    let home = std::env::temp_dir().join(format!("regulus-hooks-config-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    assert_eq!(
        regulus_core::Config::load(&home).unwrap().hooks.timeout_ms,
        10_000
    );
    std::fs::write(home.join("config.toml"), "[hooks]\ntimeout_ms = 1234\n").unwrap();
    assert_eq!(
        regulus_core::Config::load(&home).unwrap().hooks.timeout_ms,
        1234
    );
    std::fs::remove_dir_all(&home).unwrap();
}
