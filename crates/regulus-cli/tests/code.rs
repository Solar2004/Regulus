//! `regulus code` (docs/04-client.md §12) through the real program against a real core in
//! a temporary home. The core runs the real Agent box and the Decider of `human`; the
//! engine and the coding workspace are stand-ins made here, so no model is called and the
//! test does not depend on the module that serves `code.inspect`: the workspace stand-in
//! follows the same contract (a Uses output `code.workspace@1`, the method `code.inspect`
//! with `summary`, `list`, `read`, `search` and `diff`). The template is a stand-in too,
//! with the ids and vars the real `regulus-code` template has.
//!
//! Covered: a first launch writes the folder's profile and starts it, the next attaches,
//! `--once` talks to the agent, setup flags never replace a profile without `--reset`,
//! `--new` starts another session, `inspect` asks the workspace, `ls` lists what there is,
//! and what is missing (template, module, execution) is said as what it is.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::{Arc, Condvar, Mutex};

use regulus_core::{parse_manifest, Config, Module, ModuleCtx, ModuleError};
use regulus_proto::{Manifest, NodeCall};
use serde_json::{json, Value};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");

// ── The engine: an agent that follows a script instead of a model ─────────────────────

#[derive(Default)]
struct Sitting {
    events: Vec<Value>,
    turns: u64,
    /// The answer to the permission request in flight; `Some(None)`: cancelled.
    decided: Option<Option<String>>,
    cancelled: bool,
}

#[derive(Default)]
struct Sessions {
    open: HashMap<String, Sitting>,
    started: u64,
}

#[derive(Default)]
struct Shared {
    sessions: Mutex<Sessions>,
    wake: Condvar,
}

/// `edit …` reads a file, asks permission to write it and waits for the answer; `plan …`
/// publishes a plan; `slow …` works until it is cancelled; anything else is echoed after a
/// tool call and two chunks of text.
struct Engine {
    manifest: Manifest,
    shared: Arc<Shared>,
}

fn push(sitting: &mut Sitting, event: Value) {
    let mut event = event;
    event["seq"] = json!(sitting.events.len() as u64 + 1);
    event["at"] = json!(0);
    sitting.events.push(event);
}

impl Engine {
    fn prompt(&self, session: &str, text: &str) -> Result<Value, ModuleError> {
        let mut all = self.shared.sessions.lock().unwrap();
        let gone = || ModuleError::Failed("the session is gone".into());
        let sitting = all.open.get_mut(session).ok_or_else(gone)?;
        sitting.turns += 1;
        let turn = sitting.turns;
        push(sitting, json!({ "kind": "turn_started" }));
        if text.contains("slow") {
            push(
                sitting,
                json!({ "kind": "text", "text": "thinking it over" }),
            );
            sitting.cancelled = false;
            while !all.open.get(session).ok_or_else(gone)?.cancelled {
                all = self.shared.wake.wait(all).unwrap();
            }
            let sitting = all.open.get_mut(session).ok_or_else(gone)?;
            push(
                sitting,
                json!({ "kind": "turn_ended", "stop_reason": "cancelled" }),
            );
            return Ok(json!({ "stop_reason": "cancelled", "text": "stopped on request" }));
        }
        let answer = if text.contains("edit") {
            let request = format!("p{turn}");
            push(
                sitting,
                json!({ "kind": "tool_call", "id": format!("c{turn}a"), "tool_kind": "read",
                        "title": "code_read_file src/main.rs", "status": "completed" }),
            );
            sitting.decided = None;
            push(
                sitting,
                json!({ "kind": "permission_request", "id": request, "tool": "code_write_file",
                        "title": "Write src/main.rs", "options": [
                            { "id": "yes", "kind": "allow_once", "name": "Allow once" },
                            { "id": "always", "kind": "allow_always", "name": "Always allow" },
                            { "id": "no", "kind": "reject_once", "name": "Reject" }] }),
            );
            while all.open.get(session).ok_or_else(gone)?.decided.is_none() {
                all = self.shared.wake.wait(all).unwrap();
            }
            let sitting = all.open.get_mut(session).ok_or_else(gone)?;
            let option = sitting.decided.clone().unwrap();
            push(
                sitting,
                json!({ "kind": "permission_resolved", "id": request, "option": option }),
            );
            if option.as_deref().is_some_and(|o| o != "no") {
                let id = format!("c{turn}b");
                push(
                    sitting,
                    json!({ "kind": "tool_call", "id": id, "tool_kind": "edit",
                            "title": "code_write_file src/main.rs", "status": "pending" }),
                );
                push(
                    sitting,
                    json!({ "kind": "tool_update", "id": id, "status": "completed",
                            "content": "wrote 12 bytes\nto src/main.rs" }),
                );
            }
            format!("chose {}", option.unwrap_or_else(|| "cancelled".into()))
        } else if text.contains("plan") {
            push(
                sitting,
                json!({ "kind": "plan", "entries": [
                    { "content": "read the code", "status": "completed" },
                    { "content": "change it", "status": "in_progress" },
                    { "content": "run the tests", "status": "pending" }] }),
            );
            "planned".to_string()
        } else {
            let answer = format!("echo: {text}");
            push(
                sitting,
                json!({ "kind": "tool_call", "id": format!("c{turn}"), "tool_kind": "search",
                        "title": "code_search echo", "status": "completed" }),
            );
            let (head, tail) = answer.split_at(3);
            push(sitting, json!({ "kind": "text", "text": head }));
            push(sitting, json!({ "kind": "text", "text": tail }));
            answer
        };
        let sitting = all.open.get_mut(session).ok_or_else(gone)?;
        push(
            sitting,
            json!({ "kind": "turn_ended", "stop_reason": "end_turn" }),
        );
        Ok(json!({ "stop_reason": "end_turn", "text": answer }))
    }
}

impl Module for Engine {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params)
            .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
        let session = call.args["session_id"].as_str().unwrap_or_default();
        match name {
            "engine.session.start" => {
                let mut all = self.shared.sessions.lock().unwrap();
                all.started += 1;
                let id = format!("s{}", all.started);
                all.open.insert(id.clone(), Sitting::default());
                Ok(json!({
                    "session_id": id, "agent": { "name": "script", "version": "1" },
                    "capabilities": { "load_session": false, "events": true },
                    "native_session_id": format!("native-{}", all.started), "resumed": false }))
            }
            "engine.session.prompt" => {
                self.prompt(session, call.args["text"].as_str().unwrap_or_default())
            }
            "engine.session.events" => {
                let all = self.shared.sessions.lock().unwrap();
                let since = call.args["since"].as_u64().unwrap_or(0);
                let events: Vec<Value> = all
                    .open
                    .get(session)
                    .map(|sitting| {
                        sitting
                            .events
                            .iter()
                            .filter(|e| e["seq"].as_u64().unwrap() > since)
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                let next = events.last().map_or(since, |e| e["seq"].as_u64().unwrap());
                Ok(json!({ "events": events, "next": next }))
            }
            "engine.session.decide" => {
                let mut all = self.shared.sessions.lock().unwrap();
                if let Some(sitting) = all.open.get_mut(session) {
                    sitting.decided = Some(call.args["option"].as_str().map(str::to_string));
                }
                self.shared.wake.notify_all();
                Ok(Value::Null)
            }
            "engine.session.cancel" => {
                let mut all = self.shared.sessions.lock().unwrap();
                if let Some(sitting) = all.open.get_mut(session) {
                    sitting.cancelled = true;
                    if sitting.decided.is_none() {
                        sitting.decided = Some(None);
                    }
                }
                self.shared.wake.notify_all();
                Ok(Value::Null)
            }
            "engine.session.status" => Ok(json!({ "state": "idle" })),
            "engine.session.stop" => {
                let mut all = self.shared.sessions.lock().unwrap();
                if let Some(mut sitting) = all.open.remove(session) {
                    sitting.cancelled = true;
                }
                self.shared.wake.notify_all();
                Ok(Value::Null)
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

const ENGINE: &str = r#"
[module]
id = "t/script@1.0.0"
name = "Scripted engine"

[[node]]
type = "engine"
name = "Engine"

[[node]]
type = "provider"
name = "Provider"
"#;

// ── The workspace: `code.inspect` over a real folder ──────────────────────────────────

/// A stand-in for the module that runs coding workspaces: the same capability and the same
/// read-only method, over the folder its node is set to. git is made up.
struct Workspace {
    manifest: Manifest,
    /// `(execution, node)` → folder, while the node is live.
    live: Mutex<HashMap<(String, String), PathBuf>>,
}

const WORKSPACE: &str = r#"
[module]
id = "t/code@0.1.0"
name = "Workspace stand-in"
methods = ["code.inspect"]

[[node]]
type = "workspace"
name = "Workspace"

[[node.port]]
id = "workspace"
capability = "code.workspace@1"
direction = "output"
kind = "uses"

[[node.port]]
id = "tools"
capability = "tool@1"
direction = "output"
kind = "uses"

[[node.field]]
id = "root"
type = "text"
"#;

fn invalid(message: impl Into<String>) -> ModuleError {
    ModuleError::InvalidParams(message.into())
}

impl Workspace {
    fn inspect(&self, params: &Value) -> Result<Value, ModuleError> {
        let text = |key: &str| params[key].as_str().unwrap_or_default().to_string();
        let key = (text("execution_id"), text("node_id"));
        let root = self
            .live
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .ok_or_else(|| invalid(format!("`{}` is not a live workspace node", key.1)))?;
        let shown = root.display().to_string();
        let path = text("path");
        if path.split('/').any(|part| part == "..") {
            return Err(invalid(format!("`{path}` leaves the workspace")));
        }
        let failed = |e: std::io::Error| invalid(format!("{path}: {e}"));
        match text("action").as_str() {
            "summary" => Ok(json!({ "root": shown,
                "git": { "branch": "main", "available": true,
                         "status": " M src/main.rs\n?? notes.md\n" },
                "tools": ["code_list_files", "code_read_file", "code_search"] })),
            "list" => {
                let mut entries = Vec::new();
                for entry in std::fs::read_dir(root.join(&path)).map_err(failed)? {
                    let entry = entry.map_err(failed)?;
                    let name = entry.file_name().to_string_lossy().into_owned();
                    let kind = if entry.path().is_dir() {
                        "directory"
                    } else {
                        "file"
                    };
                    let inside = if path.is_empty() {
                        name
                    } else {
                        format!("{}/{name}", path.trim_end_matches('/'))
                    };
                    entries.push(json!({ "path": inside, "kind": kind }));
                }
                entries.sort_by_key(|e| e["path"].as_str().unwrap_or_default().to_string());
                Ok(json!({ "root": shown, "path": path, "entries": entries,
                           "truncated": false }))
            }
            "read" => {
                let text = std::fs::read_to_string(root.join(&path)).map_err(failed)?;
                Ok(json!({ "root": shown, "path": path, "text": text, "truncated": false }))
            }
            "search" => {
                let query = text("query");
                if query.is_empty() {
                    return Err(invalid("search needs a query"));
                }
                let mut matches = Vec::new();
                let mut folders = vec![String::new()];
                while let Some(folder) = folders.pop() {
                    for entry in std::fs::read_dir(root.join(&folder)).map_err(failed)? {
                        let entry = entry.map_err(failed)?;
                        let name = entry.file_name().to_string_lossy().into_owned();
                        let inside = if folder.is_empty() {
                            name
                        } else {
                            format!("{folder}/{name}")
                        };
                        if entry.path().is_dir() {
                            folders.push(inside);
                        } else if let Ok(text) = std::fs::read_to_string(entry.path()) {
                            for (at, line) in text.lines().enumerate() {
                                if line.contains(&query) {
                                    matches.push(
                                        json!({ "path": inside, "line": at + 1, "text": line }),
                                    );
                                }
                            }
                        }
                    }
                }
                matches.sort_by_key(|m| m["path"].as_str().unwrap_or_default().to_string());
                Ok(json!({ "root": shown, "matches": matches, "truncated": false }))
            }
            "diff" => Ok(json!({ "root": shown, "truncated": false,
                "text": "diff --git a/src/main.rs b/src/main.rs\n--- a/src/main.rs\n\
                         +++ b/src/main.rs\n@@ -1 +1 @@\n-fn main() {}\n\
                         +fn main() { println!(\"hi\"); }\n" })),
            other => Err(invalid(format!("unknown action `{other}`"))),
        }
    }
}

impl Module for Workspace {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            "code.inspect" => self.inspect(&params),
            "node/start" | "node/stop" => {
                let call: NodeCall = serde_json::from_value(params)
                    .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
                let key = (call.execution_id, call.node.id.clone());
                let mut live = self.live.lock().unwrap();
                if name == "node/start" {
                    let root = call.node.config["root"].as_str().unwrap_or_default();
                    live.insert(key, PathBuf::from(root));
                } else {
                    live.remove(&key);
                }
                Ok(Value::Null)
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

// ── The template: the ids and vars of `regulus-code`, wired to the stand-ins ──────────

const TEMPLATE: &str = r#"
id = "regulus-code"
name = "Regulus Code"
main_workflow_id = "code"

[template]
name = "regulus-code"
description = "A coding harness (test stand-in)"
requires = [
  { module = "regulus/agent", version = ">=0.1" },
  { module = "regulus/human", version = ">=0.1" },
  { module = "t/code", version = ">=0.1" },
]

[template.vars]
root = { description = "The folder to work in, absolute" }
preset = { description = "The provider preset", default = "ollama" }
base_url = { description = "Where the provider answers", default = "" }
credential = { description = "secret:<name> of the provider's key", default = "" }
model = { description = "The model", default = "qwen3:4b" }

[[workflows]]
id = "code"
name = "Code"
kind = "main"

  [[workflows.nodes]]
  id = "agent"
  name = "Agent"
  type = "agent"
  module_id = "regulus/agent@0.1.0"
  config = { workdir = "{{root}}", permissions = "ask", events_poll_ms = 20, decision_poll_ms = 20 }
  ports = [
    { id = "engine", capability = "agent.engine@1", direction = "input", kind = "uses", required = true },
    { id = "decisions", capability = "decision@1", direction = "input", kind = "uses" },
  ]

  [[workflows.nodes]]
  id = "engine"
  name = "Engine"
  type = "engine"
  module_id = "t/script@1.0.0"
  ports = [{ id = "engine", capability = "agent.engine@1", direction = "output", kind = "uses" }]

  [[workflows.nodes]]
  id = "provider"
  name = "Provider"
  type = "provider"
  module_id = "t/script@1.0.0"
  config = { preset = "{{preset}}", base_url = "{{base_url}}", credential = "{{credential}}", model = "{{model}}" }
  ports = [{ id = "endpoint", capability = "llm.endpoint@1", direction = "output", kind = "uses" }]

  [[workflows.nodes]]
  id = "decider"
  name = "Decider"
  type = "decider"
  module_id = "regulus/human@0.1.0"
  ports = [{ id = "decisions", capability = "decision@1", direction = "output", kind = "uses" }]

  [[workflows.nodes]]
  id = "workspace"
  name = "Workspace"
  type = "workspace"
  module_id = "t/code@0.1.0"
  config = { root = "{{root}}" }
  ports = [
    { id = "workspace", capability = "code.workspace@1", direction = "output", kind = "uses" },
    { id = "tools", capability = "tool@1", direction = "output", kind = "uses" },
  ]

  [[workflows.wires]]
  id = "w-engine"
  kind = "uses"
  from = { node_id = "engine", port_id = "engine" }
  to = { node_id = "agent", port_id = "engine" }

  [[workflows.wires]]
  id = "w-decisions"
  kind = "uses"
  from = { node_id = "decider", port_id = "decisions" }
  to = { node_id = "agent", port_id = "decisions" }
"#;

// ── A core with them, in a temporary home ─────────────────────────────────────────────

struct Stage {
    home: PathBuf,
    /// The folder the sessions work in.
    work: PathBuf,
}

/// A home whose templates folder has the stand-in template, and a small folder to code in.
fn stage(name: &str, with_template: bool) -> Stage {
    let root = std::env::temp_dir().join(format!("regulus-code-{name}-{}", std::process::id()));
    stage_in(root, with_template)
}

fn stage_in(root: PathBuf, with_template: bool) -> Stage {
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let work = root.join("My App");
    std::fs::create_dir_all(home.join("templates")).unwrap();
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    std::fs::create_dir_all(work.join("src")).unwrap();
    std::fs::write(
        home.join("config.toml"),
        "[server]\nport = 0\n[client]\nrefresh_ms = 50\n",
    )
    .unwrap();
    if with_template {
        std::fs::write(home.join("templates/regulus-code.toml"), TEMPLATE).unwrap();
    }
    std::fs::write(work.join("src/main.rs"), "fn main() {}\n// needle here\n").unwrap();
    std::fs::write(work.join("notes.md"), "a needle in the notes\n").unwrap();
    Stage {
        home,
        // As the program will spell it: `/tmp` may be a link.
        work: std::fs::canonicalize(work).unwrap(),
    }
}

fn modules(with_workspace: bool) -> Vec<Box<dyn Module>> {
    let mut modules: Vec<Box<dyn Module>> = vec![
        Box::new(regulus_module_agent::Agent::new()),
        Box::new(regulus_module_human::Human::new()),
        Box::new(Engine {
            manifest: parse_manifest(ENGINE).unwrap(),
            shared: Arc::default(),
        }),
    ];
    if with_workspace {
        modules.push(Box::new(Workspace {
            manifest: parse_manifest(WORKSPACE).unwrap(),
            live: Mutex::default(),
        }));
    }
    modules
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

fn out(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn err(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn ok(home: &Path, args: &[&str]) -> Output {
    let output = regulus(home, args);
    assert!(output.status.success(), "{args:?}: {}", err(&output));
    output
}

/// The value after `name` in what `--detach` prints.
fn said(output: &Output, name: &str) -> String {
    out(output)
        .lines()
        .find_map(|line| line.strip_prefix(name).map(|rest| rest.trim().to_string()))
        .unwrap_or_else(|| panic!("no `{name}` in {:?}", out(output)))
}

fn checks(stage: Stage) {
    let home = stage.home.as_path();
    let work = stage.work.to_str().unwrap();

    let nothing = ok(home, &["code", "ls"]);
    assert_eq!(out(&nothing), "");
    assert!(
        err(&nothing).contains("no execution has an agent"),
        "{}",
        err(&nothing)
    );

    // A first launch writes the folder's own profile from the template and starts it.
    let first = ok(home, &["code", work, "--detach", "--model", "script-1"]);
    let execution = said(&first, "execution");
    let profile = said(&first, "profile");
    assert!(profile.starts_with("code-my-app-"), "{profile}");
    assert_eq!(said(&first, "agent"), "agent");
    assert_eq!(said(&first, "workspace"), "workspace");
    assert_eq!(said(&first, "root"), work);
    let notes = err(&first);
    assert!(notes.contains("wrote "), "{notes}");
    assert!(
        notes.contains("the model starts without the context"),
        "{notes}"
    );
    let written = std::fs::read_to_string(home.join(format!("profiles/{profile}.toml"))).unwrap();
    assert!(written.contains("script-1"), "{written}");
    assert!(
        written.contains("ollama"),
        "the default of a var left alone: {written}"
    );

    // The workspace, read-only, as the command prints it.
    let summary = ok(home, &["code", "inspect", &execution, "summary"]);
    assert_eq!(
        out(&summary),
        format!(
            "root    {work}\nbranch  main\nstatus   M src/main.rs\n        ?? notes.md\n\
             tools   code_list_files, code_read_file, code_search\n"
        )
    );
    let list = ok(home, &["code", "inspect", &execution, "list"]);
    assert_eq!(out(&list), "src/\nnotes.md\n");
    let inside = ok(home, &["code", "inspect", &execution, "list", "src"]);
    assert_eq!(out(&inside), "src/main.rs\n");
    let read = ok(
        home,
        &["code", "inspect", &execution, "read", "src/main.rs"],
    );
    assert_eq!(out(&read), "fn main() {}\n// needle here\n");
    let search = ok(home, &["code", "inspect", &execution, "search", "needle"]);
    assert_eq!(
        out(&search),
        "notes.md:1: a needle in the notes\nsrc/main.rs:2: // needle here\n"
    );
    let diff = ok(home, &["code", "inspect", &execution, "diff"]);
    assert!(
        out(&diff).starts_with("diff --git a/src/main.rs"),
        "{}",
        out(&diff)
    );
    let json = ok(
        home,
        &[
            "code",
            "inspect",
            &execution,
            "summary",
            "--node",
            "workspace",
            "--json",
        ],
    );
    let raw: Value = serde_json::from_str(&out(&json)).unwrap();
    assert_eq!(raw["git"]["branch"], "main");
    // What the workspace refuses is said as it said it.
    let outside = regulus(home, &["code", "inspect", &execution, "read", "../x"]);
    assert!(!outside.status.success());
    assert!(
        err(&outside).contains("leaves the workspace"),
        "{}",
        err(&outside)
    );
    let not_one = regulus(
        home,
        &["code", "inspect", &execution, "summary", "--node", "agent"],
    );
    assert!(
        err(&not_one).contains("not a live workspace node"),
        "{}",
        err(&not_one)
    );
    let unknown = regulus(home, &["code", "inspect", &execution, "write"]);
    assert!(
        err(&unknown).contains("not something to inspect"),
        "{}",
        err(&unknown)
    );

    // The same folder again: the live session, not a second one.
    let again = ok(home, &["code", work, "--detach"]);
    assert_eq!(said(&again, "execution"), execution);
    assert!(
        err(&again).contains(&format!("attached to {execution}")),
        "{}",
        err(&again)
    );

    // One prompt, its reply on stdout, what the agent did on stderr.
    let once = ok(home, &["code", work, "--once", "hi"]);
    assert_eq!(out(&once), "echo: hi\n");
    assert!(err(&once).contains("code_search echo"), "{}", err(&once));

    // Setup flags do not reach a profile that is written: nothing is replaced unasked.
    let refused = regulus(home, &["code", work, "--detach", "--model", "script-2"]);
    assert!(!refused.status.success());
    assert!(err(&refused).contains("pass --reset"), "{}", err(&refused));
    let reset = ok(
        home,
        &["code", work, "--detach", "--model", "script-2", "--reset"],
    );
    let second = said(&reset, "execution");
    assert_ne!(second, execution);
    assert_eq!(said(&reset, "profile"), profile);
    let written = std::fs::read_to_string(home.join(format!("profiles/{profile}.toml"))).unwrap();
    assert!(written.contains("script-2"), "{written}");

    // --new starts another session of the profile as it is.
    let fresh = ok(home, &["code", work, "--detach", "--new"]);
    let third = said(&fresh, "execution");
    assert!(third != execution && third != second, "{third}");
    assert!(err(&fresh).contains("started "), "{}", err(&fresh));

    // By its id, whatever the folder.
    let by_id = ok(home, &["code", "-e", &execution, "--once", "there"]);
    assert_eq!(out(&by_id), "echo: there\n");
    let missing = regulus(home, &["code", "-e", "exec-404", "--detach"]);
    assert!(!missing.status.success());
    assert!(
        err(&missing).contains("no execution `exec-404`"),
        "{}",
        err(&missing)
    );

    let listed = out(&ok(home, &["code", "ls"]));
    let lines: Vec<&str> = listed.lines().collect();
    assert_eq!(lines.len(), 3, "{listed}");
    assert!(lines[0].starts_with(&execution), "{listed}");
    assert!(lines[0].contains("running"), "{listed}");
    assert!(
        lines[0].contains(&format!(
            "{profile}/code  agent agent  workspace workspace  {work}"
        )),
        "{listed}"
    );

    // What cannot be: said before anything is written.
    let secret = regulus(
        home,
        &["code", work, "--detach", "--credential", "sk-live-123"],
    );
    assert!(!secret.status.success());
    assert!(err(&secret).contains("secret:<name>"), "{}", err(&secret));
    assert!(!err(&secret).contains("sk-live-123"), "{}", err(&secret));
    let folder = regulus(home, &["code", "/nowhere/at/all", "--detach"]);
    assert!(
        err(&folder).contains("is not a folder that can be read"),
        "{}",
        err(&folder)
    );
    let template = regulus(
        home,
        &[
            "code",
            work,
            "--detach",
            "--profile",
            "other",
            "--template",
            "nope",
        ],
    );
    assert!(!template.status.success());
    assert!(
        err(&template).contains("no template `nope` in "),
        "{}",
        err(&template)
    );
    assert!(err(&template).contains("templates"), "{}", err(&template));
}

async fn with_core(stage: Stage, with_workspace: bool, checks: fn(Stage)) {
    let config = Config::load(&stage.home).unwrap();
    let (core, warnings) = regulus_core::assemble(&config, modules(with_workspace));
    assert!(warnings.is_empty(), "{warnings:?}");
    let running = regulus_core::serve(&config, core.clone()).await.unwrap();
    let root = stage.home.parent().unwrap().to_path_buf();
    let result = tokio::task::spawn_blocking(move || checks(stage)).await;
    running.stop().await;
    core.shutdown();
    let _ = std::fs::remove_dir_all(root);
    result.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn code_launches_attaches_talks_and_inspects() {
    with_core(stage("launch", true), true, checks).await;
}

fn checks_without(stage: Stage) {
    let home = stage.home.as_path();
    let work = stage.work.to_str().unwrap();
    // No template in the templates folder: where it is looked for and what to do.
    let launch = regulus(home, &["code", work, "--detach"]);
    assert!(!launch.status.success());
    let why = err(&launch);
    assert!(why.contains("no template `regulus-code` in "), "{why}");
    assert!(why.contains("--template"), "{why}");
    // A template file by its path, on a core without the workspace module: refused by what
    // it requires, and nothing is written.
    let file = home.join("elsewhere.toml");
    std::fs::write(&file, TEMPLATE).unwrap();
    let unmet = regulus(
        home,
        &[
            "code",
            work,
            "--detach",
            "--template",
            file.to_str().unwrap(),
        ],
    );
    assert!(!unmet.status.success());
    let why = err(&unmet);
    assert!(why.contains("t/code"), "{why}");
    assert!(why.contains("regulus module ls"), "{why}");
    assert!(!home
        .join("profiles")
        .read_dir()
        .is_ok_and(|mut d| d.next().is_some()));
    // Nothing serves `code.inspect` here: not installed, not a generic error.
    let inspect = regulus(
        home,
        &[
            "code",
            "inspect",
            "exec-1",
            "summary",
            "--node",
            "workspace",
        ],
    );
    assert!(!inspect.status.success());
    assert!(
        err(&inspect).contains("is not installed"),
        "{}",
        err(&inspect)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_is_missing_is_said_as_what_it_is() {
    with_core(stage("missing", false), false, checks_without).await;
}

/// Not a check: a core with the stand-ins that serves until `<stage>/stop` exists, to try
/// the screen by hand in a terminal of your own:
///
/// ```sh
/// REGULUS_CODE_STAGE=/tmp/stage cargo test -p regulus-cli --test code -- --ignored serve
/// regulus --home /tmp/stage/home code "/tmp/stage/My App"     # in another terminal
/// touch /tmp/stage/stop
/// ```
///
/// The scripted engine answers `edit …` with a permission request, `plan …` with a plan,
/// `slow …` by working until cancelled, and echoes anything else.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "serves until <stage>/stop exists: for trying the screen by hand"]
async fn serve_the_stand_ins_to_try_the_screen_by_hand() {
    let Some(root) = std::env::var_os("REGULUS_CODE_STAGE").map(PathBuf::from) else {
        eprintln!("set REGULUS_CODE_STAGE to a folder that may be replaced");
        return;
    };
    let stage = stage_in(root.clone(), true);
    let config = Config::load(&stage.home).unwrap();
    let (core, warnings) = regulus_core::assemble(&config, modules(true));
    assert!(warnings.is_empty(), "{warnings:?}");
    let running = regulus_core::serve(&config, core.clone()).await.unwrap();
    eprintln!(
        "serving: regulus --home {} code {:?}",
        stage.home.display(),
        stage.work.display()
    );
    while !root.join("stop").exists() {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
    running.stop().await;
    core.shutdown();
    let _ = std::fs::remove_dir_all(root);
}
