//! Regulus custom nodes in Rust: one file, `serde_json` and the standard library only.
//!
//! A project's local module (`[[modules.local]]` in `regulus.toml`) is a process that speaks
//! JSON-RPC 2.0 with the core over stdin and stdout, one object per line
//! (docs/03-modules.md §6). This file hides that protocol. It sits beside `src/main.rs`
//! (`regulus node init --lang rust` copies it there), which declares the node types it
//! serves:
//!
//! ```ignore
//! mod regulus;
//!
//! use regulus::{Context, Error, Module};
//! use serde_json::Value;
//!
//! fn main() {
//!     Module::new(include_str!("../regulus-module.toml"))
//!         .node("caps", caps)
//!         .run();
//! }
//!
//! fn caps(ctx: &Context, _port: &str, value: Value) -> Result<(), Error> {
//!     let text = value.as_str().ok_or_else(|| Error::invalid("text@1 must be a string"))?;
//!     ctx.emit("out", text.to_uppercase())
//! }
//! ```
//!
//! What it does:
//!
//! - answers `manifest` with the text of `regulus-module.toml`, and `start` and `stop`;
//! - turns `call` → `node/event` into `handler(ctx, port, value)` for the node's type;
//!   [`Context::emit`] writes the `emit` notification at once, so the core has it before the
//!   call's answer, which is written when the handler returns;
//! - runs events on a pool of worker threads ([`Module::workers`], `REGULUS_MAX_WORKERS`, 4
//!   by default), so a slow event of one execution does not hold the events of the others.
//!   The core hands the events of one execution over one at a time, so those stay in order;
//! - answers errors with the codes the core maps: [`Error::invalid`] → `-32602`
//!   (`InvalidParams`); [`Error::failed`], any other error turned by `?`, and a panic →
//!   `-32000` (`Failed`). A panic fails its call only: its message goes to stderr, the
//!   worker and the module go on;
//! - never writes a line the core would not read (`REGULUS_MAX_LINE_BYTES`, 16 MiB by
//!   default, as `[local_modules] max_line_bytes`): `emit` returns an error instead.
//!
//! What it does not do, on purpose:
//!
//! - **Stdout and stdin belong to the protocol.** Log with `eprintln!`: the core logs
//!   stderr and shows its last lines when the module breaks. Never `print!` or `println!`:
//!   a whole line is noise the core skips, a partial one breaks the next message. A child
//!   process you start must get `Stdio::null()` or `Stdio::piped()` for its stdin and
//!   stdout, or it reads and writes the protocol.
//! - Only `emit` of the module context: no `node/start`, `node/stop`, `node/child` or
//!   `node/hook` (they are answered `-32601`), no store, no secrets beyond the node's
//!   config (the core resolves `secret:<name>` there before the call).
//! - The core does not apply the defaults of `[[node.field]]`: read a field with its
//!   fallback (`ctx.config("prefix").as_str().unwrap_or("")`).
//! - One process serves every node of its types, in every execution of the project. A
//!   handler gets a fresh [`Context`] per value; state it keeps elsewhere (a `static`, an
//!   `Arc<Mutex<_>>` it captured) is shared by all of them and by the workers at once: key
//!   it by `(ctx.execution_id, ctx.node_id)` when it belongs to one node of one execution.
//! - With `panic = "abort"` in the crate's profile a panic cannot be caught: the process
//!   ends and the module shows as broken.
//! - `stop` is answered at once and nothing more is read; the core then closes stdin and
//!   sends `SIGTERM`, so an event still running ends with the process. When the input just
//!   ends, the events already read run to the end and are answered.
//! - It is not a sandbox: it runs with the user's permissions.

#![allow(dead_code)] // A node uses only part of this file.

use std::any::Any;
use std::collections::HashMap;
use std::fmt;
use std::io::{self, BufRead, Write};
use std::panic::{self, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Mutex, MutexGuard};
use std::thread;

use serde_json::{json, Value};

/// The core does not know the method (`ModuleError::UnknownMethod`).
pub const METHOD_NOT_FOUND: i64 = -32601;
/// The value or the config is not acceptable (`ModuleError::InvalidParams`).
pub const INVALID_PARAMS: i64 = -32602;
/// The call failed (`ModuleError::Failed`).
pub const FAILED: i64 = -32000;
/// Events handled at once when neither [`Module::workers`] nor the environment says.
pub const DEFAULT_WORKERS: usize = 4;
/// Sets the number of workers (`env` of the entry in `regulus.toml`).
pub const WORKERS_ENV: &str = "REGULUS_MAX_WORKERS";
/// The longest line the core reads from a module: the default of `[local_modules]
/// max_line_bytes`. A longer one gets the process killed, so it is never written.
pub const DEFAULT_MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
/// Set it when the core's `max_line_bytes` is not the default.
pub const MAX_LINE_BYTES_ENV: &str = "REGULUS_MAX_LINE_BYTES";

/// The error of one call: the core marks that call failed with its message, the module
/// goes on. `?` turns any error into [`Error::failed`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    code: i64,
    message: String,
}

impl Error {
    /// The value or the node's config is not acceptable: `InvalidParams` in the core.
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }

    /// The node could not do its work: `Failed` in the core.
    pub fn failed(message: impl Into<String>) -> Self {
        Self::new(FAILED, message)
    }

    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn not_found(method: &str) -> Self {
        Self::new(METHOD_NOT_FOUND, format!("unknown method `{method}`"))
    }

    /// The JSON-RPC error code the core gets.
    pub fn code(&self) -> i64 {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl<E: std::error::Error> From<E> for Error {
    fn from(error: E) -> Self {
        Self::failed(error.to_string())
    }
}

/// What a handler sees of one node in one execution, for one value.
pub struct Context<'a> {
    /// The execution the value belongs to.
    pub execution_id: String,
    /// The node in the workflow.
    pub node_id: String,
    pub node_type: String,
    /// The node as the core sent it: `config` (secrets resolved), `ports`, `name`…
    pub node: Value,
    /// The `node/event` arguments without `value`: `port`, `capability`, `from`, `event`.
    pub event: Value,
    /// The project and its folder, from `start`; empty until the core sends it.
    pub project_id: String,
    pub root: String,
    /// The node's "then" outputs; `None` when the core sent no ports (anything goes).
    outputs: Option<Vec<String>>,
    wire: &'a dyn Sink,
}

impl Context<'_> {
    /// The value of the node's field `id` (`Null` when it is not set).
    pub fn config(&self, id: &str) -> &Value {
        &self.node["config"][id]
    }

    /// Puts `value` on the "then" output `port` of this node, now. Refused (nothing is
    /// written) if the node has no such output, or if the message would be longer than the
    /// core reads; a handler that passes the error on with `?` fails its call.
    pub fn emit(&self, port: &str, value: impl Into<Value>) -> Result<(), Error> {
        if let Some(outputs) = &self.outputs {
            if !outputs.iter().any(|id| id == port) {
                return Err(Error::failed(format!(
                    "`{}` has no output `{port}` (outputs: {})",
                    self.node_type,
                    if outputs.is_empty() {
                        "none".into()
                    } else {
                        outputs.join(", ")
                    }
                )));
            }
        }
        let message = json!({
            "jsonrpc": "2.0",
            "method": "emit",
            "params": {
                "execution_id": self.execution_id,
                "node_id": self.node_id,
                "port": port,
                "value": value.into(),
            },
        });
        self.wire.send(&message).map_err(Error::failed)
    }
}

/// Where messages to the core go: whole lines, one writer at a time.
trait Sink: Sync {
    fn send(&self, message: &Value) -> Result<(), String>;
}

struct Wire<W> {
    out: Mutex<W>,
    max_line_bytes: usize,
    /// The core is gone: nothing more can be written.
    broken: AtomicBool,
}

impl<W: Write + Send> Sink for Wire<W> {
    fn send(&self, message: &Value) -> Result<(), String> {
        let mut line = message.to_string().into_bytes();
        if line.len() > self.max_line_bytes {
            return Err(format!(
                "the message is {} bytes; the core reads lines of at most {} \
                 ({MAX_LINE_BYTES_ENV}, [local_modules] max_line_bytes)",
                line.len(),
                self.max_line_bytes
            ));
        }
        line.push(b'\n');
        let mut out = lock(&self.out);
        out.write_all(&line)
            .and_then(|()| out.flush())
            .map_err(|e| {
                self.broken.store(true, Ordering::Relaxed);
                format!("cannot write to the core: {e}")
            })
    }
}

impl<W: Write + Send> Wire<W> {
    /// Answers request `id`. An answer too long to send becomes an error that fits.
    fn answer(&self, id: Value, result: Result<Value, Error>) {
        let message = match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(error) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": error.code, "message": error.message },
            }),
        };
        if let Err(why) = self.send(&message) {
            if !self.broken.load(Ordering::Relaxed) {
                let error = json!({ "code": FAILED, "message": why });
                let _ =
                    self.send(&json!({ "jsonrpc": "2.0", "id": message["id"], "error": error }));
            }
        }
    }
}

type Handler = Box<dyn Fn(&Context<'_>, &str, Value) -> Result<(), Error> + Send + Sync>;

/// A local module: its manifest and the handler of each node type it serves.
pub struct Module {
    manifest: String,
    handlers: HashMap<String, Handler>,
    workers: Option<usize>,
    max_line_bytes: Option<usize>,
}

impl Module {
    /// A module whose `manifest` answer is this text: the module's `regulus-module.toml`,
    /// which declares the node types, their ports and fields.
    pub fn new(manifest: impl Into<String>) -> Self {
        Self {
            manifest: manifest.into(),
            handlers: HashMap::new(),
            workers: None,
            max_line_bytes: None,
        }
    }

    /// Serves the node type `node_type` with `handler`, called as `handler(ctx, port,
    /// value)` for each value that arrives on one of its inputs.
    ///
    /// # Panics
    ///
    /// If `node_type` already has a handler.
    pub fn node<F>(mut self, node_type: &str, handler: F) -> Self
    where
        F: Fn(&Context<'_>, &str, Value) -> Result<(), Error> + Send + Sync + 'static,
    {
        let taken = self
            .handlers
            .insert(node_type.to_owned(), Box::new(handler));
        assert!(
            taken.is_none(),
            "node type `{node_type}` is registered twice"
        );
        self
    }

    /// Events handled at once (1: one at a time, in arrival order). It wins over
    /// `REGULUS_MAX_WORKERS`.
    ///
    /// # Panics
    ///
    /// If `workers` is 0.
    pub fn workers(mut self, workers: usize) -> Self {
        assert!(workers > 0, "a module needs at least one worker");
        self.workers = Some(workers);
        self
    }

    /// The longest line written to the core. It wins over `REGULUS_MAX_LINE_BYTES`.
    pub fn max_line_bytes(mut self, bytes: usize) -> Self {
        self.max_line_bytes = Some(bytes);
        self
    }

    /// Serves the core on stdin and stdout until it says `stop` or closes stdin. A setting
    /// in the environment that is not a number above 0 ends the process at once (status 2),
    /// saying why on stderr.
    pub fn run(mut self) {
        let from_env = |name: &str| match std::env::var(name) {
            Err(_) => Ok(None),
            Ok(text) => match text.trim().parse::<usize>() {
                Ok(number) if number > 0 => Ok(Some(number)),
                _ => Err(format!(
                    "{name} must be a whole number above 0, not {text:?}"
                )),
            },
        };
        match (from_env(WORKERS_ENV), from_env(MAX_LINE_BYTES_ENV)) {
            (Ok(workers), Ok(max_line_bytes)) => {
                self.workers = self.workers.or(workers);
                self.max_line_bytes = self.max_line_bytes.or(max_line_bytes);
            }
            (Err(error), _) | (_, Err(error)) => {
                eprintln!("regulus: {error}");
                std::process::exit(2);
            }
        }
        // `io::stdout()`, not a handle of its own: a stray `println!` takes the same lock,
        // so it can never land inside one of these lines.
        self.serve(io::stdin().lock(), io::stdout());
    }

    /// [`Module::run`] over any input and output, for tests: returns once `input` says
    /// `stop` or ends and every event read has been answered.
    ///
    /// # Panics
    ///
    /// If no node type has a handler, or a worker thread cannot be started.
    pub fn serve(self, mut input: impl BufRead, output: impl Write + Send) {
        assert!(
            !self.handlers.is_empty(),
            "no node types: register one with Module::node before serving"
        );
        let wire = Wire {
            out: Mutex::new(output),
            max_line_bytes: self.max_line_bytes.unwrap_or(DEFAULT_MAX_LINE_BYTES),
            broken: AtomicBool::new(false),
        };
        let started = Mutex::new(Value::Null);
        let (jobs, queue) = mpsc::channel::<(Value, Value)>();
        let queue = Mutex::new(queue);
        thread::scope(|scope| {
            for index in 0..self.workers.unwrap_or(DEFAULT_WORKERS) {
                thread::Builder::new()
                    .name(format!("regulus-worker-{index}"))
                    .spawn_scoped(scope, || self.work(&queue, &wire, &started))
                    .expect("cannot start a worker thread");
            }
            let mut line = Vec::new();
            loop {
                line.clear();
                match input.read_until(b'\n', &mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        eprintln!("regulus: cannot read stdin: {e}");
                        break;
                    }
                }
                let stop = self.request(&String::from_utf8_lossy(&line), &jobs, &wire, &started);
                if stop || wire.broken.load(Ordering::Relaxed) {
                    break;
                }
            }
            drop(jobs); // the workers end once the events already read are answered
        });
    }

    /// One line from the core; `true` once it said `stop`.
    fn request<W: Write + Send>(
        &self,
        line: &str,
        jobs: &mpsc::Sender<(Value, Value)>,
        wire: &Wire<W>,
        started: &Mutex<Value>,
    ) -> bool {
        let line = line.trim();
        if line.is_empty() {
            return false;
        }
        let mut request = match serde_json::from_str::<Value>(line) {
            Ok(request) => request,
            Err(error) => {
                let start: String = line.chars().take(200).collect();
                eprintln!("regulus: ignored a line that is not JSON ({error}): {start}");
                return false;
            }
        };
        // A notification, or an answer: nothing to answer.
        let Some(id) = request.get("id").filter(|id| !id.is_null()).cloned() else {
            return false;
        };
        let Some(method) = request
            .get("method")
            .and_then(Value::as_str)
            .map(str::to_owned)
        else {
            return false;
        };
        let mut params = take(&mut request, "params");
        match method.as_str() {
            "manifest" => wire.answer(id, Ok(Value::String(self.manifest.clone()))),
            "start" => {
                *lock(started) = params;
                wire.answer(id, Ok(Value::Null));
            }
            "stop" => {
                wire.answer(id, Ok(Value::Null));
                return true;
            }
            "call" if params["method"] == "node/event" => {
                // Only fails when no worker is left, which `work` never allows.
                let _ = jobs.send((id, take(&mut params, "params")));
            }
            // `node/start`, `node/stop`, `node/child`, `node/hook`: no lifecycle per node.
            "call" => {
                let called = params["method"].as_str().unwrap_or_default();
                wire.answer(id, Err(Error::not_found(called)));
            }
            other => wire.answer(id, Err(Error::not_found(other))),
        }
        false
    }

    /// A worker: answers events until there are no more. A panic fails its event only.
    fn work<W: Write + Send>(
        &self,
        queue: &Mutex<mpsc::Receiver<(Value, Value)>>,
        wire: &Wire<W>,
        started: &Mutex<Value>,
    ) {
        loop {
            let job = lock(queue).recv();
            let Ok((id, call)) = job else {
                return;
            };
            let result = panic::catch_unwind(AssertUnwindSafe(|| self.event(call, wire, started)))
                .unwrap_or_else(|panic| Err(Error::failed(panicked(&*panic))));
            wire.answer(id, result);
        }
    }

    /// `node/event`: hands the value to the handler of the node's type.
    fn event(
        &self,
        mut call: Value,
        wire: &dyn Sink,
        started: &Mutex<Value>,
    ) -> Result<Value, Error> {
        let node = take(&mut call, "node");
        let node_type = node["type"].as_str().unwrap_or_default().to_owned();
        let handler = self
            .handlers
            .get(&node_type)
            .ok_or_else(|| Error::invalid(format!("no node type `{node_type}` here")))?;
        let mut event = take(&mut call, "args");
        let port = event["port"].as_str().unwrap_or_default().to_owned();
        if let Some(inputs) = ports(&node, "input") {
            if !inputs.contains(&port) {
                return Err(Error::invalid(format!(
                    "`{node_type}` takes nothing on `{port}`"
                )));
            }
        }
        let value = take(&mut event, "value");
        let (project_id, root) = {
            let started = lock(started);
            let text = |key: &str| started[key].as_str().unwrap_or_default().to_owned();
            (text("project_id"), text("root"))
        };
        let ctx = Context {
            execution_id: call["execution_id"].as_str().unwrap_or_default().to_owned(),
            node_id: node["id"].as_str().unwrap_or_default().to_owned(),
            outputs: ports(&node, "output"),
            node_type,
            node,
            event,
            project_id,
            root,
            wire,
        };
        handler(&ctx, &port, value)?;
        Ok(Value::Null)
    }
}

/// Removes `key` from an object and returns it (`Null` if it is not there, or `value` is
/// no object).
fn take(value: &mut Value, key: &str) -> Value {
    value
        .as_object_mut()
        .and_then(|object| object.remove(key))
        .unwrap_or_default()
}

/// The ids of the node's "then" ports in `direction`, as the core sent them; `None` if it
/// sent none.
fn ports(node: &Value, direction: &str) -> Option<Vec<String>> {
    let ports = node
        .get("ports")
        .and_then(Value::as_array)
        .filter(|p| !p.is_empty())?;
    let then = |port: &Value| port.get("kind").is_none() || port["kind"] == "then";
    Some(
        ports
            .iter()
            .filter(|port| port["direction"] == direction && then(port))
            .filter_map(|port| port["id"].as_str().map(str::to_owned))
            .collect(),
    )
}

/// The message of a caught panic.
fn panicked(panic: &(dyn Any + Send)) -> String {
    let message = panic
        .downcast_ref::<&str>()
        .map(|text| (*text).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".into());
    format!("the node panicked: {message}")
}

/// A lock that a panic elsewhere did not spoil: what it guards is always whole.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
