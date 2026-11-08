//! Processes (docs/13-blocks.md §3.2): `command` runs a program per value, `gate` turns
//! its exit code into a verdict on the value it forwards, `job` keeps a long-running
//! program with a log and a timeout, and `terminal` runs one in a pseudo-terminal.
//!
//! Every program runs on threads of this module, never on the bus thread: `node/event`
//! spawns and returns. Each child leads its own process group. `node/stop` signals the
//! group and returns; a reaper thread kills what is left after `grace_ms`. A node runs
//! programs only between its `node/start` and `node/stop`.

mod exit;
mod pty;
mod run;

use std::collections::HashMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Direction, Manifest, Node, NodeCall, NodeEvent, WireKind};
use serde_json::{json, Value};

pub use exit::{Exit, Started, EXIT_CAPABILITY, JOB_CAPABILITY};
use run::{Limits, Refused, Runs, Spec, THREAD};

const MANIFEST: &str = include_str!("../regulus-module.toml");

const COMMAND: &str = "command";
const GATE: &str = "gate";
const JOB: &str = "job";
const TERMINAL: &str = "terminal";

/// Replaced, in every argument, by the value received.
const INPUT_MARK: &str = "{{input}}";
/// The `select` value that switches a yes/no field on.
const YES: &str = "yes";
/// The variable a shell and many tools take their working directory from.
const PWD: &str = "PWD";
/// What `enter` types after each value in a terminal.
const ENTER: &[u8] = b"\r";

/// Ports, by the role they play.
mod port {
    pub const STDOUT: &str = "stdout";
    pub const EXIT: &str = "exit";
    pub const FAIL: &str = "fail";
    pub const PASS: &str = "pass";
    pub const STARTED: &str = "started";
    pub const DONE: &str = "done";
    pub const TIMEOUT: &str = "timeout";
    pub const OUT: &str = "out";
}

pub struct Process {
    manifest: Manifest,
    ctx: ModuleCtx,
    runs: Arc<Runs>,
    terminals: Arc<Mutex<HashMap<NodeRef, pty::Terminal>>>,
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

/// `text@1` is a JSON string; anything else is passed as its JSON.
fn text_of(value: &Value) -> String {
    value
        .as_str()
        .map_or_else(|| value.to_string(), str::to_string)
}

fn to_value(value: impl serde::Serialize) -> Value {
    serde_json::to_value(value).unwrap_or(Value::Null)
}

/// A node of this module in one execution, and the "then" outputs it declares: it only
/// emits on those, so a node may leave out what it does not need.
#[derive(Clone)]
struct At {
    node: NodeRef,
    outputs: Vec<String>,
}

impl At {
    fn of(call: &NodeCall) -> Self {
        Self {
            node: NodeRef {
                execution_id: call.execution_id.clone(),
                node_id: call.node.id.clone(),
            },
            outputs: call
                .node
                .ports
                .iter()
                .filter(|p| p.direction == Direction::Output && p.kind == WireKind::Then)
                .map(|p| p.id.clone())
                .collect(),
        }
    }

    fn has(&self, port: &str) -> bool {
        self.outputs.iter().any(|p| p == port)
    }

    /// Emits if the node has `port`. An execution that stopped meanwhile takes nothing,
    /// and that is fine: whoever stopped it no longer wants the answer.
    fn emit(&self, ctx: &ModuleCtx, port: &str, value: Value) {
        if self.has(port) {
            let _ = ctx.emit(&self.node, port, value);
        }
    }
}

/// A node's fields, defaults from the manifest.
struct Fields<'a> {
    manifest: &'a Manifest,
    node: &'a Node,
}

impl Fields<'_> {
    fn get(&self, id: &str) -> Option<&Value> {
        self.manifest.setting(self.node, id)
    }

    fn text(&self, id: &str) -> String {
        self.get(id)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    }

    fn number(&self, id: &str) -> u64 {
        self.get(id).and_then(Value::as_u64).unwrap_or_default()
    }

    fn yes(&self, id: &str) -> bool {
        self.text(id) == YES
    }

    fn millis(&self, id: &str) -> Option<Duration> {
        Some(self.number(id))
            .filter(|ms| *ms > 0)
            .map(Duration::from_millis)
    }

    fn list(&self, id: &str) -> Vec<String> {
        match self.get(id) {
            Some(Value::Array(items)) => items.iter().map(text_of).collect(),
            Some(Value::String(one)) if !one.is_empty() => vec![one.clone()],
            _ => Vec::new(),
        }
    }

    fn map(&self, id: &str) -> Vec<(String, String)> {
        match self.get(id) {
            Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), text_of(v))).collect(),
            _ => Vec::new(),
        }
    }

    fn usize(&self, id: &str) -> usize {
        usize::try_from(self.number(id)).unwrap_or(usize::MAX)
    }
}

/// `env` with `PWD` set to `cwd`, unless `env` sets it itself. The child would otherwise
/// inherit the core's `PWD`, and tools that take their project from it (opencode) would
/// work in the wrong directory.
fn with_pwd(mut env: Vec<(String, String)>, cwd: &Path) -> Vec<(String, String)> {
    if !env.iter().any(|(name, _)| name == PWD) {
        env.push((PWD.into(), cwd.to_string_lossy().into_owned()));
    }
    env
}

impl Process {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("process manifest is valid"),
            ctx: ModuleCtx::default(),
            runs: Arc::default(),
            terminals: Arc::default(),
        }
    }

    fn fields<'a>(&'a self, node: &'a Node) -> Fields<'a> {
        Fields {
            manifest: &self.manifest,
            node,
        }
    }

    /// What to run for `input`: `{{input}}` filled in, the directory resolved against the
    /// node's own.
    fn spec(&self, call: &NodeCall, at: &At, input: &str) -> Result<Spec, ModuleError> {
        let fields = self.fields(&call.node);
        let program = fields.text("command");
        if program.is_empty() {
            return Err(ModuleError::InvalidParams(format!(
                "node `{}` names no `command`",
                call.node.id
            )));
        }
        let cwd = self.dir(at, &fields.text("cwd"))?;
        Ok(Spec {
            program,
            args: fields
                .list("args")
                .into_iter()
                .map(|a| a.replace(INPUT_MARK, input))
                .collect(),
            env: with_pwd(fields.map("env"), &cwd),
            cwd,
        })
    }

    /// `path` inside the node's directory, or as is when absolute.
    fn dir(&self, at: &At, path: &str) -> Result<PathBuf, ModuleError> {
        let path = PathBuf::from(path);
        if path.is_absolute() {
            return Ok(path);
        }
        let dir = self.ctx.node_dir(&at.node)?.join(path);
        std::fs::create_dir_all(&dir)
            .map_err(|e| ModuleError::Failed(format!("cannot create {}: {e}", dir.display())))?;
        Ok(dir)
    }

    fn grace(&self, node: &Node) -> Duration {
        Duration::from_millis(self.fields(node).number("grace_ms"))
    }

    /// A child is spawned and its waiter thread may not be: then nobody would reap it, so
    /// it is killed and reaped here.
    fn waiting(
        &self,
        waited: std::io::Result<thread::JoinHandle<()>>,
        node: &NodeRef,
        pid: u32,
    ) -> Result<(), ModuleError> {
        let Err(error) = waited else {
            return Ok(());
        };
        run::abandon(pid);
        self.runs.ended(node, pid);
        Err(ModuleError::Failed(format!(
            "no thread to wait on it: {error}"
        )))
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        self.runs.start(&At::of(&call).node);
        match call.node.node_type.as_str() {
            GATE => same_capability(&call.node, &[port::PASS, port::FAIL])?,
            JOB if self.fields(&call.node).yes("autostart") => {
                self.job(&call, &At::of(&call), "")?;
            }
            TERMINAL => self.terminal(&call)?,
            _ => {}
        }
        Ok(Value::Null)
    }

    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let at = At::of(&call);
        self.terminals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&at.node);
        self.runs.stop(&at.node, self.grace(&call.node));
        Ok(Value::Null)
    }

    fn event(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(call.args.clone())?;
        let at = At::of(&call);
        let input = text_of(&event.value);
        match call.node.node_type.as_str() {
            COMMAND | GATE => self.command(&call, at, &input, event.value),
            JOB => self.job(&call, &at, &input),
            TERMINAL => self.type_in(&call, &at, input),
            other => Err(ModuleError::InvalidParams(format!(
                "`{other}` is not a node of this module"
            ))),
        }
    }

    /// One run of a command or a gate. Its end goes out from the waiting thread.
    fn command(
        &self,
        call: &NodeCall,
        at: At,
        input: &str,
        value: Value,
    ) -> Result<Value, ModuleError> {
        let fields = self.fields(&call.node);
        let spec = self.spec(call, &at, input)?;
        let stdin = fields.yes("stdin").then(|| input.to_string());
        let limits = Limits {
            timeout: fields.millis("timeout_ms"),
            max_output: fields.usize("max_output"),
            pipe_grace: Duration::from_millis(fields.number("pipe_grace_ms")),
        };
        let trim = fields.yes("trim");
        let gate = call.node.node_type == GATE;
        let node = at.node.clone();
        let started = Instant::now();
        let spawned = self.runs.spawn(&node, fields.usize("max_running"), || {
            let mut command = spec.command();
            command
                .stdin(if stdin.is_some() {
                    Stdio::piped()
                } else {
                    Stdio::null()
                })
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            let child = command.spawn()?;
            Ok((child.id(), child))
        });
        let finish = move |ctx: &ModuleCtx, mut exit: Exit| {
            if trim {
                exit.stdout.truncate(exit.stdout.trim_end().len());
                exit.stderr.truncate(exit.stderr.trim_end().len());
            }
            if gate {
                at.emit(ctx, port::EXIT, to_value(&exit));
                let verdict = if exit.ok() { port::PASS } else { port::FAIL };
                at.emit(ctx, verdict, value);
            } else {
                at.emit(ctx, port::EXIT, to_value(&exit));
                if exit.ok() {
                    at.emit(ctx, port::STDOUT, Value::String(exit.stdout));
                } else {
                    at.emit(ctx, port::FAIL, to_value(&exit));
                }
            }
        };
        let child = match spawned {
            Ok(child) => child,
            Err(Refused::Node(why)) => return Err(ModuleError::Failed(why)),
            Err(Refused::Spawn(error)) => {
                let error = format!("cannot run `{}`: {error}", spec.program);
                finish(&self.ctx, Exit::not_started(&error));
                return Ok(json!({ "error": error }));
            }
        };
        let pid = child.id();
        let (ctx, runs) = (self.ctx.clone(), self.runs.clone());
        let waiter = node.clone();
        let waited = thread::Builder::new().name(THREAD.into()).spawn(move || {
            let exit = run::collect(child, stdin, limits, started, || {
                runs.ended(&waiter, pid);
            });
            finish(&ctx, exit);
        });
        self.waiting(waited, &node, pid)?;
        Ok(json!({ "pid": pid }))
    }

    /// Starts a job: its output goes to a numbered log in the node's directory.
    fn job(&self, call: &NodeCall, at: &At, input: &str) -> Result<Value, ModuleError> {
        let fields = self.fields(&call.node);
        let spec = self.spec(call, at, input)?;
        let logs = self.dir(at, &fields.text("log_dir"))?;
        let timeout = fields.millis("timeout_ms");
        let lines = fields.usize("tail_lines");
        let started = Instant::now();
        let spawned = self.runs.spawn(&at.node, fields.usize("max_running"), || {
            // Under the module's lock: two jobs never take the same number.
            let number = run::next_log(&logs);
            let path = logs.join(format!("{number}.log"));
            let log = File::options().write(true).create_new(true).open(&path)?;
            let child = spec
                .command()
                .stdin(Stdio::null())
                .stdout(log.try_clone()?)
                .stderr(log)
                .spawn()?;
            Ok((child.id(), (child, number, path)))
        });
        let (child, number, path) = match spawned {
            Ok(kept) => kept,
            Err(Refused::Node(why)) => return Err(ModuleError::Failed(why)),
            Err(Refused::Spawn(error)) => {
                let error = format!("cannot run `{}`: {error}", spec.program);
                at.emit(&self.ctx, port::FAIL, to_value(Exit::not_started(&error)));
                return Ok(json!({ "error": error }));
            }
        };
        let pid = child.id();
        let log = path.display().to_string();
        let news = Started {
            job: number,
            pid,
            log: log.clone(),
        };
        at.emit(&self.ctx, port::STARTED, to_value(&news));
        let (ctx, runs, at) = (self.ctx.clone(), self.runs.clone(), at.clone());
        let node = at.node.clone();
        let waited = thread::Builder::new().name(THREAD.into()).spawn(move || {
            let mut exit = run::wait(child, timeout, started, || {
                let stopped = !runs.is_running(&at.node);
                runs.ended(&at.node, pid);
                stopped
            });
            exit.job = Some(number);
            exit.tail = Some(run::tail(&path, lines));
            exit.log = Some(log);
            // A node without `timeout` hears a timed-out job on `fail`, never nothing.
            let port = match (exit.timed_out, exit.ok()) {
                (true, _) if at.has(port::TIMEOUT) => port::TIMEOUT,
                (false, true) => port::DONE,
                _ => port::FAIL,
            };
            at.emit(&ctx, port, to_value(&exit));
        });
        self.waiting(waited, &node, pid)?;
        Ok(to_value(news))
    }

    /// Opens the node's terminal: one thread pumps its output to `out`, one waits for it.
    fn terminal(&self, call: &NodeCall) -> Result<(), ModuleError> {
        let fields = self.fields(&call.node);
        let at = At::of(call);
        let spec = self.spec(call, &at, "")?;
        let size = |id| u16::try_from(fields.number(id)).unwrap_or(u16::MAX);
        let (rows, cols, chunk) = (size("rows"), size("cols"), fields.usize("chunk"));
        let started = Instant::now();
        let opened = self
            .runs
            .spawn(&at.node, 0, || pty::open(&spec, rows, cols))
            .map_err(|refused| match refused {
                Refused::Node(why) => ModuleError::Failed(why),
                Refused::Spawn(e) => {
                    ModuleError::Failed(format!("cannot run `{}` in a terminal: {e}", spec.program))
                }
            })?;
        let pid = opened.child.process_id().unwrap_or_default();
        self.terminals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(at.node.clone(), opened.terminal);
        let (ctx, out) = (self.ctx.clone(), at.clone());
        let reader = opened.reader;
        let (pumped, pump_done) = std::sync::mpsc::channel::<()>();
        let pump = thread::Builder::new().name(THREAD.into()).spawn(move || {
            pty::pump(reader, chunk, |text| {
                out.emit(&ctx, port::OUT, Value::String(text))
            });
            drop(pumped);
        });
        let grace = Duration::from_millis(fields.number("pipe_grace_ms"));
        let (ctx, runs, terminals) = (self.ctx.clone(), self.runs.clone(), self.terminals.clone());
        let child = opened.child;
        let node = at.node.clone();
        let waited = thread::Builder::new().name(THREAD.into()).spawn(move || {
            run::wait_exited(pid);
            runs.ended(&at.node, pid);
            let exit = pty::wait(child, started);
            terminals
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&at.node);
            // What it printed last goes out before `exit`, unless something it left keeps
            // the terminal open past the grace.
            let _ = pump_done.recv_timeout(grace);
            at.emit(&ctx, port::EXIT, to_value(&exit));
        });
        if let Err(error) = pump {
            run::abandon(pid);
            self.runs.ended(&node, pid);
            return Err(ModuleError::Failed(format!(
                "no thread to read the terminal: {error}"
            )));
        }
        self.waiting(waited, &node, pid)?;
        Ok(())
    }

    /// Types a value into the node's terminal; the write happens on its own thread.
    fn type_in(&self, call: &NodeCall, at: &At, text: String) -> Result<Value, ModuleError> {
        let mut bytes = text.into_bytes();
        if self.fields(&call.node).yes("enter") {
            bytes.extend_from_slice(ENTER);
        }
        let terminals = self.terminals.lock().unwrap_or_else(|e| e.into_inner());
        let terminal = terminals.get(&at.node).ok_or_else(|| {
            ModuleError::Failed(format!("the terminal of `{}` is closed", at.node.node_id))
        })?;
        terminal
            .input
            .send(bytes)
            .map_err(|_| ModuleError::Failed("the terminal is closed".into()))?;
        Ok(Value::Null)
    }
}

/// A wrapper takes and gives one capability: every port in `outputs` the node declares
/// must have the capability of its `in`.
fn same_capability(node: &Node, outputs: &[&str]) -> Result<(), ModuleError> {
    let Some(input) = node.port("in") else {
        return Ok(());
    };
    for id in outputs {
        if let Some(port) = node.port(id).filter(|p| p.capability != input.capability) {
            return Err(ModuleError::InvalidParams(format!(
                "node `{}`: `{id}` is {} but `in` is {}; a gate forwards what it gets",
                node.id, port.capability, input.capability
            )));
        }
    }
    Ok(())
}

impl Default for Process {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Process {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            method::NODE_START => self.start(parse(params)?),
            method::NODE_EVENT => self.event(parse(params)?),
            method::NODE_STOP => self.stop(parse(params)?),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.terminals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.runs.stop_all();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_manifest_declares_the_four_nodes_with_their_defaults() {
        let process = Process::new();
        let manifest = process.manifest();
        for node in [COMMAND, GATE, JOB, TERMINAL] {
            let node = manifest.node_type(node).unwrap();
            assert!(node.field("command").is_some(), "{}", node.node_type);
            assert!(node.field("grace_ms").is_some(), "{}", node.node_type);
        }
        assert!(manifest.module.methods.is_empty());
        let job = manifest.node_type(JOB).unwrap();
        let started = job.ports.iter().find(|p| p.id == port::STARTED).unwrap();
        assert_eq!(started.capability, JOB_CAPABILITY);
        let exit = job.ports.iter().find(|p| p.id == port::DONE).unwrap();
        assert_eq!(exit.capability, EXIT_CAPABILITY);
    }

    #[test]
    fn a_gate_must_forward_what_it_gets() {
        let node: Node = serde_json::from_value(json!({ "id": "g", "type": "gate", "ports": [
            { "id": "in", "capability": "image@1", "direction": "input", "kind": "then" },
            { "id": "pass", "capability": "image@1", "direction": "output", "kind": "then" },
            { "id": "fail", "capability": "text@1", "direction": "output", "kind": "then" },
        ]}))
        .unwrap();
        let error = same_capability(&node, &[port::PASS, port::FAIL]).unwrap_err();
        assert!(error.to_string().contains("`fail` is text@1"), "{error}");
    }

    #[test]
    fn fields_read_lists_maps_and_defaults() {
        let process = Process::new();
        let node: Node = serde_json::from_value(json!({ "id": "c", "type": "command",
            "config": { "args": ["-c", "{{input}}", 3], "env": { "A": "1", "B": 2 } } }))
        .unwrap();
        let fields = process.fields(&node);
        assert_eq!(fields.list("args"), ["-c", "{{input}}", "3"]);
        assert_eq!(
            fields.map("env"),
            [("A".into(), "1".into()), ("B".into(), "2".into())]
        );
        assert!(fields.yes("trim"));
        assert!(!fields.yes("stdin"));
        assert_eq!(
            fields.millis("timeout_ms"),
            Some(Duration::from_millis(600_000))
        );
        assert_eq!(text_of(&json!({ "a": 1 })), r#"{"a":1}"#);
    }
}
