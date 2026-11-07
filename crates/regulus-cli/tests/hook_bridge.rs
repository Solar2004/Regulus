//! The hook bridge (`regulus hook`, docs/13-blocks.md §4) against a real core serving
//! fake hook nodes: the event goes in on stdin, the answer comes out as the agent's CLI
//! reads it.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{
    BusConfig, CallConfig, Config, Core, HooksConfig, Module, ModuleCtx, ModuleError, ProfileStore,
    Storage,
};
use regulus_proto::{method, Manifest, ModuleInfo, NodeCall, Profile};
use serde_json::{json, Value};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const MODULE: &str = "t/hooks@1.0.0";

/// Hook nodes answer `config.answer`, after `config.sleep_ms` if set.
struct Hooks(Manifest);

impl Module for Hooks {
    fn manifest(&self) -> &Manifest {
        &self.0
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
                // The request's tool shows up in a context answer, to prove it arrived.
                let tool = call.args["tool"].as_str().unwrap_or_default().to_string();
                Ok(match call.node.config.get("answer") {
                    Some(Value::String(text)) if text == "echo" => {
                        json!({ "decision": "context", "text": format!("tool {tool}") })
                    }
                    Some(answer) => answer.clone(),
                    None => Value::Null,
                })
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn hook(id: &str, config: Value) -> Value {
    json!({ "id": id, "type": "hook", "module_id": MODULE, "config": config, "ports": [
        { "id": "hook", "capability": "hook@1", "direction": "output", "kind": "uses" },
    ]})
}

fn profile() -> Profile {
    let hooks = [
        (
            "veto",
            json!({ "answer": { "decision": "veto", "reason": "no Bash here" } }),
        ),
        (
            "context",
            json!({ "answer": { "decision": "context", "text": "be brief" } }),
        ),
        ("echo", json!({ "answer": "echo" })),
        ("quiet", json!({})),
        (
            "slow",
            json!({ "sleep_ms": 1000, "answer": { "decision": "veto", "reason": "late" } }),
        ),
    ];
    let mut nodes = vec![
        json!({ "id": "agent", "type": "agent", "module_id": MODULE, "ports": [
            { "id": "hooks", "capability": "hook@1", "direction": "input", "kind": "uses",
              "cardinality": "many" },
        ]}),
    ];
    let mut wires = Vec::new();
    for (id, config) in hooks {
        nodes.push(hook(id, config));
        wires.push(json!({ "id": format!("w-{id}"), "kind": "uses",
            "from": { "node_id": id, "port_id": "hook" },
            "to": { "node_id": "agent", "port_id": "hooks" } }));
    }
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "nodes": nodes, "wires": wires }],
    }))
    .unwrap()
}

struct Served {
    home: PathBuf,
    execution: String,
}

/// Runs `regulus --home <home> hook <args>` with `stdin`.
fn bridge(served: &Served, args: &[&str], stdin: &str) -> Output {
    let mut child = Command::new(PROGRAM)
        .arg("--home")
        .arg(&served.home)
        .args(["hook", "--execution", &served.execution, "--node", "agent"])
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

const PRE_TOOL: &str = r#"{"session_id":"s1","cwd":"/work","hook_event_name":"PreToolUse",
"tool_name":"Bash","tool_input":{"command":"ls"}}"#;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_bridge_answers_claude_code_and_plain_json() {
    let home = std::env::temp_dir().join(format!("regulus-hook-bridge-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.toml"), "[server]\nport = 0\n").unwrap();
    let config = Config::load(&home).unwrap();
    let mut profiles = ProfileStore::default();
    profiles.insert(profile()).unwrap();
    let manifest = Manifest {
        module: ModuleInfo {
            id: MODULE.into(),
            name: MODULE.into(),
            requires_proto: None,
            methods: vec![],
            workflow_id: None,
            origin: None,
        },
        nodes: vec![],
    };
    let core: Arc<Core> = Core::build_with_hooks(
        vec![Box::new(Hooks(manifest))],
        &[],
        profiles,
        Storage::default(),
        BusConfig::default(),
        CallConfig::default(),
        HooksConfig::default(),
    );
    let running = regulus_core::serve(&config, core.clone()).await.unwrap();
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let served = Served {
        home: home.clone(),
        execution,
    };

    let checks = tokio::task::spawn_blocking(move || {
        let claude = |hook: &str, event: &str, stdin: &str| {
            bridge(
                &served,
                &["--hook", hook, "--event", event, "--format", "claude"],
                stdin,
            )
        };
        // A veto: exit 2, the reason alone on stderr.
        let veto = claude("veto", "pre_tool", PRE_TOOL);
        assert_eq!(veto.status.code(), Some(2), "{}", text(&veto.stderr));
        assert_eq!(text(&veto.stderr), "no Bash here");
        assert!(veto.stdout.is_empty());
        // Context: exit 0 and Claude's JSON, with the event name Claude sent.
        let start = claude(
            "context",
            "session_start",
            r#"{"hook_event_name":"SessionStart","source":"startup"}"#,
        );
        assert_eq!(start.status.code(), Some(0));
        let out: Value = serde_json::from_slice(&start.stdout).unwrap();
        assert_eq!(out["hookSpecificOutput"]["hookEventName"], "SessionStart");
        assert_eq!(out["hookSpecificOutput"]["additionalContext"], "be brief");
        // The tool from Claude's payload reached the hook.
        let echo = claude("echo", "pre_tool", PRE_TOOL);
        let out: Value = serde_json::from_slice(&echo.stdout).unwrap();
        assert_eq!(out["hookSpecificOutput"]["additionalContext"], "tool Bash");
        // Nothing: exit 0, no output.
        let quiet = claude("quiet", "stop", r#"{"hook_event_name":"Stop"}"#);
        assert_eq!(quiet.status.code(), Some(0));
        assert!(quiet.stdout.is_empty() && quiet.stderr.is_empty());
        // A hook that never answers in time is nothing, quickly, and says why on stderr.
        let began = Instant::now();
        let slow = bridge(
            &served,
            &[
                "--hook",
                "slow",
                "--event",
                "pre_tool",
                "--timeout-ms",
                "100",
                "--format",
                "claude",
            ],
            PRE_TOOL,
        );
        assert!(began.elapsed() < Duration::from_secs(2));
        assert_eq!(slow.status.code(), Some(0));
        assert!(slow.stdout.is_empty());
        assert!(
            text(&slow.stderr).contains("no answer within 100 ms"),
            "{}",
            text(&slow.stderr)
        );
        // The neutral format: every hook of the agent, combined; the veto wins.
        let all = bridge(
            &served,
            &["--event", "pre_tool", "--timeout-ms", "100"],
            r#"{"event":"pre_tool","tool":"Bash"}"#,
        );
        assert_eq!(all.status.code(), Some(0));
        let answer: Value = serde_json::from_slice(&all.stdout).unwrap();
        assert_eq!(
            answer,
            json!({ "decision": "veto", "reason": "no Bash here" })
        );
        // No such execution: a non-blocking error, never a veto.
        let gone = Served {
            home: served.home.clone(),
            execution: "exec-99".into(),
        };
        let lost = bridge(
            &gone,
            &["--event", "pre_tool", "--format", "claude"],
            PRE_TOOL,
        );
        assert_eq!(lost.status.code(), Some(1));
        assert!(
            text(&lost.stderr).contains("regulus hook:"),
            "{}",
            text(&lost.stderr)
        );
    })
    .await;
    running.stop().await;
    core.shutdown();
    let _ = std::fs::remove_dir_all(&home);
    checks.unwrap();
}
