//! Local modules: a module that runs as a process of its own and speaks JSON-RPC 2.0 over
//! its stdin and stdout, one object per line (docs/03-modules.md §6). [`ProcessModule`]
//! implements the same [`Module`] trait as an in-process module, so the core treats it like
//! any other: the wire is the trait, method by method.
//!
//! - core → process: `manifest`, `start` (`Module::init`), `call` (`Module::call`, with the
//!   same method names and params) and `stop` (`Module::shutdown`).
//! - process → core: the notification `emit` (`ModuleCtx::emit`). Anything else it asks is
//!   answered with "method not found".
//!
//! What the process writes to stderr is logged and its last lines say why it broke.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use regulus_proto::{Manifest, PROTOCOL};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::config::LocalModulesConfig;
use crate::module::{Module, ModuleCtx, ModuleError, NodeRef};

/// Asks the process for its manifest: a JSON object shaped like `regulus-module.toml`, or
/// that file's text.
pub const MANIFEST: &str = "manifest";
/// `Module::init`. Optional: a process that does not know it starts all the same.
pub const START: &str = "start";
/// `Module::call`: `{ method, params }`.
pub const CALL: &str = "call";
/// `Module::shutdown`. Optional; afterwards its stdin closes and it gets SIGTERM.
pub const STOP: &str = "stop";
/// From the process: `ModuleCtx::emit`, `{ execution_id, node_id, port, value }`.
pub const EMIT: &str = "emit";

const JSONRPC: &str = "2.0";
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
/// The version of a local module when neither `regulus.toml` nor its manifest says one.
const NO_VERSION: &str = "0.0.0";
/// How often a stopping process is looked at.
const POLL: Duration = Duration::from_millis(20);
/// Where a reason starts quoting the process's stderr.
const STDERR_LABEL: &str = "; stderr: ";
/// The thread that starts every local module's process.
const SPAWNER_THREAD: &str = "regulus-local-spawner";
/// The threads that read a local module's stdout and stderr.
const THREAD: &str = "regulus-local";

/// What the core needs to run one `[[modules.local]]`.
#[derive(Debug, Clone, Default)]
pub struct LocalSpec {
    pub project_id: String,
    /// The project's folder: relative paths start here.
    pub root: PathBuf,
    pub id: String,
    pub command: Vec<String>,
    pub cwd: Option<PathBuf>,
    pub env: Vec<(String, String)>,
    pub version: Option<String>,
    /// A manifest file read instead of asking the process.
    pub manifest: Option<PathBuf>,
}

impl LocalSpec {
    /// `<project>/<id>`: the module's id without version.
    pub fn base_id(&self) -> String {
        format!("{}/{}", self.project_id, self.id)
    }

    /// The manifest of a module that could not start: its id and nothing else, so it can be
    /// listed as broken.
    pub fn placeholder(&self) -> Manifest {
        let version = self.version.as_deref().unwrap_or(NO_VERSION);
        Manifest {
            module: regulus_proto::ModuleInfo {
                id: format!("{}@{version}", self.base_id()),
                name: self.id.clone(),
                requires_proto: None,
                methods: Vec::new(),
                workflow_id: None,
                origin: None,
            },
            nodes: Vec::new(),
        }
    }

    /// The program: one with a path separator is relative to the project, a bare name is
    /// looked up on `PATH`.
    fn program(&self) -> PathBuf {
        let program = Path::new(&self.command[0]);
        if program.components().count() > 1 && program.is_relative() {
            self.root.join(program)
        } else {
            program.to_path_buf()
        }
    }
}

/// A value the process emitted, waiting for a thread to put it on the bus.
#[derive(Debug, Deserialize)]
struct Emit {
    execution_id: String,
    node_id: String,
    port: String,
    #[serde(default)]
    value: Value,
}

/// What came back from the process and has not been taken yet.
#[derive(Default)]
struct Wire {
    replies: HashMap<u64, Result<Value, ModuleError>>,
    /// Requests a thread still waits for: an answer to any other id is dropped, so
    /// nothing piles up from late or made-up answers.
    pending: HashSet<u64>,
    /// Emitted while a call was in flight: the calling thread puts them on the bus, so they
    /// continue its chain and `bus.max_depth` still sees loops through the process.
    emits: VecDeque<Emit>,
    /// Threads waiting for an answer.
    waiting: usize,
    /// Why the process is gone, once it is.
    exited: Option<String>,
}

/// The pipes to one process and what came back on them.
struct Link {
    label: String,
    /// The id of the process, which leads its own process group.
    pid: u32,
    child: Mutex<Child>,
    stdin: Mutex<Option<ChildStdin>>,
    next_id: AtomicU64,
    wire: Mutex<Wire>,
    changed: Condvar,
    stderr: Mutex<VecDeque<String>>,
    ctx: Mutex<Option<ModuleCtx>>,
    config: LocalModulesConfig,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|p| p.into_inner())
}

fn error_of(error: &Value) -> ModuleError {
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("no message")
        .to_string();
    match error.get("code").and_then(Value::as_i64) {
        Some(METHOD_NOT_FOUND) => ModuleError::UnknownMethod(message),
        Some(INVALID_PARAMS) => ModuleError::InvalidParams(message),
        _ => ModuleError::Failed(message),
    }
}

fn millis(ms: u64) -> Option<Duration> {
    (ms > 0).then(|| Duration::from_millis(ms))
}

impl Link {
    fn wire(&self) -> MutexGuard<'_, Wire> {
        lock(&self.wire)
    }

    /// `what`, with the last lines of stderr if there are any.
    fn reason(&self, what: &str) -> String {
        let tail = lock(&self.stderr);
        // Already said, by the reader that saw it exit.
        if tail.is_empty() || what.contains(STDERR_LABEL) {
            return what.to_string();
        }
        let lines: Vec<&str> = tail.iter().map(String::as_str).collect();
        format!("{what}{STDERR_LABEL}{}", lines.join(" | "))
    }

    fn note(&self, line: String) {
        let mut tail = lock(&self.stderr);
        tail.push_back(line);
        while tail.len() > self.config.stderr_lines {
            tail.pop_front();
        }
    }

    fn exited(&self) -> Option<String> {
        self.wire().exited.clone()
    }

    /// Why the process is gone, waiting at most `timeout` for it to be known.
    fn exited_within(&self, timeout: Duration) -> Option<String> {
        let deadline = Instant::now() + timeout;
        let mut wire = self.wire();
        loop {
            if wire.exited.is_some() {
                return wire.exited.clone();
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            wire = match self.changed.wait_timeout(wire, deadline - now) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
    }

    fn send(&self, message: &Value) -> Result<(), String> {
        let mut stdin = lock(&self.stdin);
        let pipe = stdin
            .as_mut()
            .ok_or_else(|| format!("{} is stopped", self.label))?;
        let mut line = message.to_string();
        line.push('\n');
        pipe.write_all(line.as_bytes())
            .and_then(|()| pipe.flush())
            .map_err(|e| format!("cannot write to {}: {e}", self.label))
    }

    /// Sends a request and waits for its answer, putting on the bus what the process emits
    /// meanwhile. Without `timeout`, it waits until the answer comes or the process ends.
    fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, ModuleError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let message = json!({ "jsonrpc": JSONRPC, "id": id, "method": method, "params": params });
        self.wire().pending.insert(id);
        if let Err(error) = self.send(&message) {
            self.wire().pending.remove(&id);
            // A process that is ending says why soon after; wait for it a little.
            let grace = Duration::from_millis(self.config.stop_grace_ms);
            return Err(ModuleError::Failed(
                self.exited_within(grace).unwrap_or(error),
            ));
        }
        let deadline = timeout.map(|t| Instant::now() + t);
        let mut wire = self.wire();
        wire.waiting += 1;
        let result = loop {
            // Emitted before the answer, so on the bus before the call returns.
            if let Some(emit) = wire.emits.pop_front() {
                drop(wire);
                self.perform(emit);
                wire = self.wire();
                continue;
            }
            if let Some(result) = wire.replies.remove(&id) {
                break result;
            }
            if let Some(why) = &wire.exited {
                break Err(ModuleError::Failed(why.clone()));
            }
            match deadline {
                None => wire = self.changed.wait(wire).unwrap_or_else(|p| p.into_inner()),
                Some(at) => {
                    let now = Instant::now();
                    if now >= at {
                        let ms = timeout.unwrap_or_default().as_millis();
                        break Err(ModuleError::Failed(format!(
                            "{} did not answer `{method}` in {ms} ms",
                            self.label
                        )));
                    }
                    wire = match self.changed.wait_timeout(wire, at - now) {
                        Ok((guard, _)) => guard,
                        Err(poisoned) => poisoned.into_inner().0,
                    };
                }
            }
        };
        wire.waiting -= 1;
        wire.pending.remove(&id);
        wire.replies.remove(&id);
        // Nobody else waits: what is left is put on the bus here.
        while wire.waiting == 0 {
            let Some(emit) = wire.emits.pop_front() else {
                break;
            };
            drop(wire);
            self.perform(emit);
            wire = self.wire();
        }
        result
    }

    fn perform(&self, emit: Emit) {
        let Some(ctx) = lock(&self.ctx).clone() else {
            eprintln!(
                "regulus: {} emitted on `{}` before it started; dropped",
                self.label, emit.port
            );
            return;
        };
        let node = NodeRef {
            execution_id: emit.execution_id,
            node_id: emit.node_id,
        };
        if let Err(error) = ctx.emit(&node, &emit.port, emit.value) {
            eprintln!(
                "regulus: {} cannot emit on `{}`: {error}",
                self.label, emit.port
            );
        }
    }

    /// One line from the process's stdout.
    fn incoming(&self, message: Value) {
        let id = message.get("id").filter(|id| !id.is_null()).cloned();
        match message.get("method").and_then(Value::as_str) {
            None => {
                let Some(id) = id.as_ref().and_then(Value::as_u64) else {
                    self.note(format!("stdout: {message}"));
                    return;
                };
                let result = match message.get("error") {
                    Some(error) => Err(error_of(error)),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                let mut wire = self.wire();
                if wire.pending.contains(&id) {
                    wire.replies.insert(id, result);
                    self.changed.notify_all();
                }
            }
            Some(EMIT) => {
                if let Some(id) = &id {
                    let _ = self.send(&json!({ "jsonrpc": JSONRPC, "id": id, "result": null }));
                }
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                match serde_json::from_value::<Emit>(params) {
                    Ok(emit) => {
                        let mut wire = self.wire();
                        if wire.waiting > 0 {
                            wire.emits.push_back(emit);
                            self.changed.notify_all();
                        } else {
                            drop(wire);
                            self.perform(emit);
                        }
                    }
                    Err(error) => eprintln!("regulus: {} sent a bad `emit`: {error}", self.label),
                }
            }
            Some(other) => {
                if let Some(id) = id {
                    let error = json!({
                        "code": METHOD_NOT_FOUND,
                        "message": format!("`{other}` is not offered to local modules"),
                    });
                    let _ = self.send(&json!({ "jsonrpc": JSONRPC, "id": id, "error": error }));
                }
            }
        }
    }

    /// Whether the process is still running. Reaps it if it ended.
    fn running(&self) -> bool {
        matches!(lock(&self.child).try_wait(), Ok(None))
    }

    /// Sends SIGTERM (or SIGKILL with `kill`) to its process group, even after the
    /// process itself ended: what it started may still run. An empty group is harmless
    /// (ESRCH).
    fn signal(&self, kill: bool) {
        #[cfg(unix)]
        {
            let signal = if kill { libc::SIGKILL } else { libc::SIGTERM };
            let Ok(group) = libc::pid_t::try_from(self.pid) else {
                return;
            };
            if group > 1 {
                // SAFETY: `killpg` only sends a signal; it reads no memory of ours. The group
                // is the one the process was put in (`process_group(0)`); a group id is not
                // handed out again while any process of the group lives.
                unsafe {
                    libc::killpg(group, signal);
                }
            }
        }
        #[cfg(not(unix))]
        {
            let _ = kill;
            let _ = lock(&self.child).kill();
        }
    }

    /// Waits until the process ends or `deadline` passes; whether it ended.
    fn ended_by(&self, deadline: Instant) -> bool {
        loop {
            if !self.running() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(POLL);
        }
    }

    /// Ends the process and its group at once and reaps it.
    fn kill(&self) {
        lock(&self.stdin).take();
        self.signal(true);
        let _ = lock(&self.child).wait();
    }

    /// Stops it in order: `stop`, its stdin closed, SIGTERM to its group, and SIGKILL to
    /// whatever of the group is left once it ended or the grace passed.
    fn stop(&self) {
        let grace = Duration::from_millis(self.config.stop_grace_ms);
        if self.exited().is_none() {
            let _ = self.request(STOP, json!({}), Some(grace));
        }
        lock(&self.stdin).take();
        self.signal(false);
        self.ended_by(Instant::now() + grace);
        self.kill();
    }
}

/// One line read with a limit.
enum Line {
    Text(String),
    /// Longer than the limit: its first bytes. The rest of it is not read yet.
    TooLong(String),
    End,
}

/// Reads one line of at most `max` bytes (without its end), as UTF-8 where it is not.
fn read_line(reader: &mut impl BufRead, max: usize) -> io::Result<Line> {
    let mut line = Vec::new();
    loop {
        let chunk = match reader.fill_buf() {
            Ok(chunk) => chunk,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if chunk.is_empty() {
            return Ok(if line.is_empty() {
                Line::End
            } else {
                Line::Text(String::from_utf8_lossy(&line).into_owned())
            });
        }
        let end = chunk.iter().position(|b| *b == b'\n');
        let content = end.unwrap_or(chunk.len());
        let room = max.saturating_sub(line.len());
        if content > room {
            line.extend_from_slice(&chunk[..room]);
            reader.consume(room);
            return Ok(Line::TooLong(String::from_utf8_lossy(&line).into_owned()));
        }
        line.extend_from_slice(&chunk[..content]);
        match end {
            Some(_) => {
                reader.consume(content + 1);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(Line::Text(String::from_utf8_lossy(&line).into_owned()));
            }
            None => reader.consume(content),
        }
    }
}

/// Throws away the rest of a line that was too long.
fn skip_line(reader: &mut impl BufRead) -> io::Result<()> {
    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok(());
        }
        match chunk.iter().position(|b| *b == b'\n') {
            Some(at) => {
                reader.consume(at + 1);
                return Ok(());
            }
            None => {
                let all = chunk.len();
                reader.consume(all);
            }
        }
    }
}

/// Reads the process's stdout until it closes, then says why it is gone. A line longer
/// than `max_line_bytes` ends it: the process is killed and broken with that reason.
fn read_stdout(link: Arc<Link>, stdout: impl Read, stderr_done: mpsc::Receiver<()>) {
    let mut reader = BufReader::new(stdout);
    let max = link.config.max_line_bytes;
    let cause = loop {
        match read_line(&mut reader, max) {
            Ok(Line::End) => break None,
            Err(error) => break Some(format!("{}: cannot read its stdout: {error}", link.label)),
            Ok(Line::TooLong(_)) => {
                break Some(format!(
                "{} wrote a line longer than {max} bytes ([local_modules] max_line_bytes); killed",
                link.label
            ))
            }
            Ok(Line::Text(line)) if line.trim().is_empty() => {}
            Ok(Line::Text(line)) => match serde_json::from_str::<Value>(&line) {
                Ok(message) => link.incoming(message),
                Err(_) => link.note(format!("stdout: {line}")),
            },
        }
    };
    let grace = Duration::from_millis(link.config.stop_grace_ms);
    if cause.is_some() {
        link.kill();
    }
    // Its last words on stderr are part of why it ended.
    let _ = stderr_done.recv_timeout(grace);
    let what = match cause {
        Some(cause) => cause,
        None if link.ended_by(Instant::now() + grace) => {
            let status = lock(&link.child).try_wait().ok().flatten();
            match status {
                Some(status) => format!("{} exited ({status})", link.label),
                None => format!("{} exited", link.label),
            }
        }
        None => format!("{} closed its stdout", link.label),
    };
    let why = link.reason(&what);
    let mut wire = link.wire();
    wire.exited = Some(why);
    // Nobody will answer what is still asked; their threads see `exited`.
    let Wire {
        replies, pending, ..
    } = &mut *wire;
    replies.retain(|id, _| pending.contains(id));
    drop(wire);
    link.changed.notify_all();
}

/// Logs the process's stderr and keeps its last lines. A line that is not UTF-8 is read
/// lossily and one past `max_line_bytes` is cut; it goes on reading until the pipe closes.
fn read_stderr(link: Arc<Link>, stderr: impl Read, done: mpsc::Sender<()>) {
    let mut reader = BufReader::new(stderr);
    loop {
        let line = match read_line(&mut reader, link.config.max_line_bytes) {
            Ok(Line::End) | Err(_) => break,
            Ok(Line::Text(line)) => line,
            Ok(Line::TooLong(start)) => {
                if skip_line(&mut reader).is_err() {
                    break;
                }
                format!("{start}… (cut)")
            }
        };
        eprintln!("regulus: {}: {line}", link.label);
        link.note(line);
    }
    let _ = done.send(());
}

/// The manifest a process or a file gave, as the core validates any manifest: the shape of
/// `regulus-module.toml`, as a JSON object or as that file's text. Its id becomes
/// `<project>/<id>@<version>`; `module.id` and `module.name` may be left out.
fn manifest_of(answer: Value, spec: &LocalSpec) -> Result<Manifest, String> {
    let mut value = match answer {
        Value::String(text) => {
            let table: toml::Table =
                toml::from_str(&text).map_err(|e| format!("invalid manifest: {e}"))?;
            serde_json::to_value(table).map_err(|e| format!("invalid manifest: {e}"))?
        }
        other => other,
    };
    let module = value
        .as_object_mut()
        .ok_or("invalid manifest: it must be an object or the text of a TOML file")?
        .entry("module")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("invalid manifest: `module` must be a table")?;
    let declared = module.get("id").and_then(Value::as_str);
    let version = spec
        .version
        .clone()
        .or_else(|| Some(declared?.split_once('@')?.1.to_string()))
        .unwrap_or_else(|| NO_VERSION.into());
    module.insert("id".into(), json!(format!("{}@{version}", spec.base_id())));
    module
        .entry("name")
        .or_insert_with(|| json!(spec.id.clone()));
    let manifest: Manifest =
        serde_json::from_value(value).map_err(|e| format!("invalid manifest: {e}"))?;
    // The same check a source gets before it runs (`source.check`): a node type whose
    // ports cannot be shaped is not admitted.
    let problems = regulus_proto::shape::validate_manifest(&manifest);
    if !problems.is_empty() {
        return Err(format!("invalid manifest: {}", problems.join("; ")));
    }
    Ok(manifest)
}

/// A process to start, and where to say how it went.
type Launch = (Command, mpsc::Sender<io::Result<Child>>);

/// Starts `command` from one thread that lives as long as the core. On Linux the parent
/// death signal fires when the *thread* that started a process ends, so starting it from a
/// short-lived thread (a request, a parallel start) would end it at once.
fn launch(mut command: Command) -> io::Result<Child> {
    static SPAWNER: OnceLock<Option<Mutex<mpsc::Sender<Launch>>>> = OnceLock::new();
    let spawner = SPAWNER.get_or_init(|| {
        let (send, receive) = mpsc::channel::<Launch>();
        std::thread::Builder::new()
            .name(SPAWNER_THREAD.into())
            .spawn(move || {
                for (mut command, answer) in receive {
                    let _ = answer.send(command.spawn());
                }
            })
            .ok()
            .map(|_| Mutex::new(send))
    });
    let Some(spawner) = spawner else {
        return command.spawn();
    };
    let (answer, result) = mpsc::channel();
    lock(spawner)
        .send((command, answer))
        .map_err(|_| io::Error::other("the spawner thread is gone"))?;
    result
        .recv()
        .map_err(|_| io::Error::other("the spawner thread is gone"))?
}

/// On Linux, the process gets SIGTERM if the core dies without stopping it.
#[cfg(target_os = "linux")]
fn die_with_core(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    let core = std::process::id();
    // SAFETY: between fork and exec it only calls `prctl` and `getppid`, which are
    // async-signal-safe, and allocates nothing.
    unsafe {
        command.pre_exec(move || {
            if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM as libc::c_ulong) != 0 {
                return Err(io::Error::last_os_error());
            }
            // The core may have died before the call above.
            if u32::try_from(libc::getppid()).ok() != Some(core) {
                return Err(io::Error::other("the core is gone"));
            }
            Ok(())
        });
    }
}

/// Whether a local module's process has ended, for the registry to show it broken.
#[derive(Clone)]
pub struct Health(Arc<Link>);

impl Health {
    /// Why the process is gone, with the tail of its stderr; `None` while it runs.
    pub fn exited(&self) -> Option<String> {
        self.0.exited()
    }

    /// Stops the process as `Module::shutdown` does, without the module: a call that
    /// waits on it ends with the process.
    pub fn stop(&self) {
        self.0.stop();
    }
}

/// A module run as a process: the `Module` trait over JSON-RPC on stdio.
pub struct ProcessModule {
    manifest: Manifest,
    link: Arc<Link>,
    start: Value,
}

impl ProcessModule {
    /// Starts the process and asks its manifest (or reads `spec.manifest`). A process that
    /// cannot start, does not answer in `start_timeout_ms` or answers a manifest that is not
    /// valid is killed; the error says why, with the tail of its stderr.
    pub fn spawn(spec: &LocalSpec, config: &LocalModulesConfig) -> Result<Self, String> {
        if spec.command.first().is_none_or(String::is_empty) {
            return Err(format!("local module {} has no command", spec.base_id()));
        }
        let label = format!("local module {}", spec.base_id());
        let cwd = spec
            .cwd
            .as_ref()
            .map_or_else(|| spec.root.clone(), |cwd| spec.root.join(cwd));
        let mut command = Command::new(spec.program());
        // Only what is let through: the core's token and secrets never reach it.
        let passed = config
            .env_passthrough
            .iter()
            .filter_map(|name| Some((name.clone(), std::env::var_os(name)?)));
        command
            .args(&spec.command[1..])
            .current_dir(&cwd)
            .env_clear()
            .envs(passed)
            .envs(spec.env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own group, so stopping it reaches whatever it started.
            command.process_group(0);
        }
        #[cfg(target_os = "linux")]
        die_with_core(&mut command);
        let mut child =
            launch(command).map_err(|e| format!("cannot run `{}`: {e}", spec.command.join(" ")))?;
        let pid = child.id();
        let (Some(stdin), Some(stdout), Some(stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{label}: no pipes to its stdio"));
        };
        let link = Arc::new(Link {
            label,
            pid,
            child: Mutex::new(child),
            stdin: Mutex::new(Some(stdin)),
            next_id: AtomicU64::new(1),
            wire: Mutex::default(),
            changed: Condvar::new(),
            stderr: Mutex::default(),
            ctx: Mutex::default(),
            config: config.clone(),
        });
        let (done, stderr_done) = mpsc::channel();
        let reader = link.clone();
        let spawned = std::thread::Builder::new()
            .name(THREAD.into())
            .spawn(move || read_stderr(reader, stderr, done))
            .and_then(|_| {
                let reader = link.clone();
                std::thread::Builder::new()
                    .name(THREAD.into())
                    .spawn(move || read_stdout(reader, stdout, stderr_done))
            });
        if let Err(error) = spawned {
            link.kill();
            return Err(format!("{}: cannot start its reader: {error}", link.label));
        }
        let answer = match &spec.manifest {
            Some(path) => {
                let path = spec.root.join(path);
                std::fs::read_to_string(&path)
                    .map(Value::String)
                    .map_err(|e| format!("cannot read {}: {e}", path.display()))
            }
            None => {
                let params = json!({
                    "protocol": PROTOCOL, "project_id": spec.project_id, "id": spec.id,
                });
                let timeout = millis(config.start_timeout_ms);
                link.request(MANIFEST, params, timeout)
                    .map_err(|e| e.to_string())
            }
        };
        match answer.and_then(|answer| manifest_of(answer, spec)) {
            Ok(manifest) => Ok(Self {
                start: json!({
                    "protocol": PROTOCOL,
                    "module_id": manifest.module.id,
                    "project_id": spec.project_id,
                    "root": spec.root.display().to_string(),
                }),
                manifest,
                link,
            }),
            Err(error) => {
                link.kill();
                Err(link.reason(&error))
            }
        }
    }

    /// Whether its process has ended, for whoever lists it.
    pub fn health(&self) -> Health {
        Health(self.link.clone())
    }
}

impl Module for ProcessModule {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        *lock(&self.link.ctx) = Some(ctx.clone());
        let timeout = millis(self.link.config.start_timeout_ms);
        match self.link.request(START, self.start.clone(), timeout) {
            Ok(_) | Err(ModuleError::UnknownMethod(_)) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError> {
        let timeout = millis(self.link.config.call_timeout_ms);
        let params = json!({ "method": method, "params": params });
        self.link.request(CALL, params, timeout)
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.link.stop();
        Ok(())
    }
}

impl Drop for ProcessModule {
    /// Never leaves its process behind, whatever happened before.
    fn drop(&mut self) {
        self.link.kill();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn spec(script: &str) -> LocalSpec {
        LocalSpec {
            project_id: "p".into(),
            root: std::env::temp_dir(),
            id: "t".into(),
            command: vec!["sh".into(), "-c".into(), script.into()],
            ..LocalSpec::default()
        }
    }

    fn config() -> LocalModulesConfig {
        LocalModulesConfig {
            start_timeout_ms: 5_000,
            stop_grace_ms: 500,
            ..LocalModulesConfig::default()
        }
    }

    /// Answers request 1 with a manifest whose name is `$1`'s value, then waits.
    const ANSWER: &str = r#"read line; printf '{"jsonrpc":"2.0","id":1,"result":{"module":{"name":"%s"}}}\n' "$NAME"; cat >/dev/null"#;

    #[test]
    fn the_child_gets_only_the_passthrough_and_its_own_env() {
        std::env::set_var("REGULUS_PROCESS_TEST_SECRET", "leak");
        let script = ANSWER.replace("$NAME", "${REGULUS_PROCESS_TEST_SECRET}${MINE}");
        let mut spec = spec(&script);
        spec.env = vec![("MINE".into(), "own".into())];
        let mut module = ProcessModule::spawn(&spec, &config()).unwrap();
        assert_eq!(module.manifest().module.name, "own");
        assert_eq!(module.manifest().module.id, "p/t@0.0.0");
        module.shutdown().unwrap();
    }

    #[test]
    fn a_line_past_the_limit_on_stdout_kills_it() {
        let config = LocalModulesConfig {
            max_line_bytes: 64,
            ..config()
        };
        let script = "read line; head -c 1000 /dev/zero | tr '\\0' a; echo; cat >/dev/null";
        let error = ProcessModule::spawn(&spec(script), &config).err().unwrap();
        assert!(error.contains("longer than 64 bytes"), "{error}");
    }

    #[test]
    fn stderr_that_is_not_utf8_is_read_on_and_kept() {
        let script = "printf 'bad \\377 byte\\n' >&2; echo second >&2; exit 1";
        let error = ProcessModule::spawn(&spec(script), &config())
            .err()
            .unwrap();
        assert!(error.contains("bad \u{fffd} byte"), "{error}");
        assert!(error.contains("second"), "{error}");
    }

    #[test]
    fn lines_are_cut_at_the_limit_and_read_lossily() {
        let mut reader = io::Cursor::new(b"short\r\nlonger line\n\xffx".to_vec());
        assert!(matches!(read_line(&mut reader, 8), Ok(Line::Text(t)) if t == "short"));
        assert!(matches!(read_line(&mut reader, 8), Ok(Line::TooLong(t)) if t == "longer l"));
        skip_line(&mut reader).unwrap();
        assert!(matches!(read_line(&mut reader, 8), Ok(Line::Text(t)) if t == "\u{fffd}x"));
        assert!(matches!(read_line(&mut reader, 8), Ok(Line::End)));
    }
}
