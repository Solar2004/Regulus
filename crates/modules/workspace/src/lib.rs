//! Workspace (docs/13-blocks.md §3.3): `workspace` gives each execution a folder with a
//! quota, made by a swappable engine (git worktree by default, or a plain folder);
//! `merge_queue` merges workspaces one at a time once their gates pass; `artifacts` keeps
//! and lists the files an execution produced.
//!
//! Nothing slow runs on the bus thread: each workspace has a thread of its own that runs
//! its operations in order, and each merge of the queue runs on its own thread.

pub mod engine;
mod fsx;

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Direction, Manifest, Node, NodeCall, NodeEvent, NodeStart, WireKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub use engine::{Engine, Info, Merge, Options, Spec, ENGINE_CAPABILITY, ENGINE_METHOD};

const MANIFEST: &str = include_str!("../regulus-module.toml");

/// `{ execution_id, node_id }` of an artifacts node → its artifacts in that execution.
pub const ARTIFACTS: &str = "workspace.artifacts";

const WORKSPACE: &str = "workspace";
const MERGE_QUEUE: &str = "merge_queue";
const ARTIFACTS_NODE: &str = "artifacts";
const YES: &str = "yes";
/// Where `<root>` keeps archived folders of the folder engine.
const ARCHIVE_DIR: &str = "archive";
/// Prefixes of the merge queue's gate inputs.
const PASS: &str = "pass";
const FAIL: &str = "fail";

mod port {
    pub const ENGINE: &str = "engine";
    pub const CREATE: &str = "create";
    pub const WRITE: &str = "write";
    pub const MERGE: &str = "merge";
    pub const ARCHIVE: &str = "archive";
    pub const CHECK: &str = "check";
    pub const READY: &str = "ready";
    pub const INFO: &str = "info";
    pub const WRITTEN: &str = "written";
    pub const MERGED: &str = "merged";
    pub const CONFLICT: &str = "conflict";
    pub const ARCHIVED: &str = "archived";
    pub const QUOTA_EXCEEDED: &str = "quota_exceeded";
    pub const USAGE: &str = "usage";
    pub const ERROR: &str = "error";
    pub const IN: &str = "in";
    pub const QUEUED: &str = "queued";
    pub const REJECTED: &str = "rejected";
    pub const ADD: &str = "add";
    pub const LIST: &str = "list";
    pub const OUT: &str = "out";
    pub const ARTIFACTS: &str = "artifacts";
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A node in one execution and its "then" outputs.
#[derive(Debug, Clone)]
struct At {
    me: NodeRef,
    node: Node,
    outputs: Vec<String>,
}

impl At {
    fn of(call: &NodeCall) -> Self {
        Self {
            me: NodeRef {
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
            node: call.node.clone(),
        }
    }
}

/// An operation for a workspace's thread.
enum Op {
    Create,
    Write(Value),
    Merge,
    Archive,
    /// Measure and say so on `usage` (`true`) or only if past the quota (`false`).
    Check(bool),
    Stop,
}

/// One entry of a merge queue.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    entry: u64,
    info: Value,
    #[serde(default)]
    passed: BTreeSet<String>,
    #[serde(default)]
    checked: bool,
}

/// A running merge queue; `entries` and `next` are its checkpoint.
struct Line {
    at: At,
    entries: VecDeque<Entry>,
    next: u64,
    merging: bool,
    gates: Vec<String>,
    options: Options,
}

#[derive(Serialize, Deserialize, Default)]
struct LineState {
    entries: VecDeque<Entry>,
    next: u64,
}

pub(crate) struct Shared {
    ctx: OnceLock<ModuleCtx>,
    manifest: Manifest,
    has_store: OnceLock<bool>,
    memory: Mutex<BTreeMap<String, Value>>,
    workers: Mutex<HashMap<NodeRef, Sender<Op>>>,
    lines: Mutex<HashMap<NodeRef, Line>>,
    /// Every workspace this module's nodes made and still have, by folder: what a merge
    /// queue trusts without asking.
    made: Mutex<HashMap<PathBuf, Info>>,
    /// Taken while an artifact gets its number and is recorded.
    numbering: Mutex<()>,
}

impl Shared {
    fn ctx(&self) -> Result<&ModuleCtx, ModuleError> {
        self.ctx
            .get()
            .ok_or_else(|| ModuleError::Failed("the workspace module has no context yet".into()))
    }

    fn text(&self, node: &Node, field: &str) -> String {
        match self.manifest.setting(node, field) {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    fn number(&self, node: &Node, field: &str) -> u64 {
        match self.manifest.setting(node, field) {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn emit(&self, at: &At, port: &str, value: Value) {
        if !at.outputs.iter().any(|p| p == port) {
            return;
        }
        if let Ok(ctx) = self.ctx() {
            // A stopped execution takes nothing; whoever stopped it no longer wants it.
            let _ = ctx.emit(&at.me, port, value);
        }
    }

    /// Says `message` on `error` and returns it.
    fn error(&self, at: &At, message: String) -> ModuleError {
        eprintln!("regulus: workspace: {}: {message}", at.me.node_id);
        self.emit(at, port::ERROR, json!(message));
        ModuleError::Failed(message)
    }

    /// A path from the profile: as is if absolute, else under the node's own directory.
    fn place(&self, at: &At, field: &str) -> Result<Option<PathBuf>, ModuleError> {
        let text = self.text(&at.node, field);
        if text.is_empty() {
            return Ok(None);
        }
        let path = Path::new(&text);
        if path.is_absolute() {
            return Ok(Some(path.to_path_buf()));
        }
        Ok(Some(self.ctx()?.node_dir(&at.me)?.join(path)))
    }

    fn options(&self, node: &Node, id: &str) -> Options {
        Options {
            merge_mode: self.text(node, "merge_mode"),
            message: self
                .text(node, "commit_message")
                .replace("{{workspace}}", id),
            author_name: self.text(node, "author_name"),
            author_email: self.text(node, "author_email"),
            timeout_ms: self.number(node, "git_timeout_ms"),
        }
    }

    /// `merge_mode` must be one the engines know.
    fn check_mode(&self, node: &Node) -> Result<(), ModuleError> {
        let mode = self.text(node, "merge_mode");
        if engine::MERGE_MODES.contains(&mode.as_str()) {
            return Ok(());
        }
        Err(ModuleError::InvalidParams(format!(
            "`merge_mode` of `{}` is one of {}, not `{mode}`",
            node.id,
            engine::MERGE_MODES.join(", ")
        )))
    }

    /// Whether a merge queue may merge `info`, which came over a wire: a workspace this
    /// module made and still has, or a git one in the queue's own `repo` and under its
    /// `root`. Anything else could name any repository, folder or branch.
    fn trusted(&self, at: &At, info: &Info) -> Result<(), String> {
        if lock(&self.made).get(&info.path) == Some(info) {
            return Ok(());
        }
        let place = |field: &str| self.place(at, field).map_err(|e| e.to_string());
        let (Some(repo), Some(root)) = (place("repo")?, place("root")?) else {
            return Err(format!(
                "{} is not a workspace of this module; set the queue's `repo` and `root` to \
                 merge others",
                info.path.display()
            ));
        };
        let real = |path: &Path| fsx::real(path);
        let same_repo = info
            .repo
            .as_deref()
            .is_some_and(|theirs| matches!((real(theirs), real(&repo)), (Ok(a), Ok(b)) if a == b));
        if info.engine != engine::GIT || !same_repo {
            return Err(format!(
                "{}: only git workspaces of {} are merged here",
                info.path.display(),
                repo.display()
            ));
        }
        fsx::confine(&root, &info.path)?;
        Ok(())
    }

    fn spec(&self, at: &At) -> Result<Spec, ModuleError> {
        let id = at.me.execution_id.clone();
        let root = match self.place(at, "root")? {
            Some(root) => root,
            None => self.ctx()?.node_dir(&at.me)?,
        };
        Ok(Spec {
            path: root.join(&id),
            repo: self.place(at, "repo")?,
            base: self.text(&at.node, "base"),
            branch: format!("{}{id}", self.text(&at.node, "branch_prefix")),
            init: self.text(&at.node, "init") == YES,
            include_file: self.text(&at.node, "include_file"),
            seed: self.place(at, "seed")?,
            target: self.place(at, "target")?,
            archive_dir: root.join(ARCHIVE_DIR),
            options: self.options(&at.node, &id),
            id,
        })
    }

    /// The engine wired into `engine`, else the built-in one called `name`.
    fn engine<'a>(&'a self, me: &NodeRef, name: &str) -> Result<Box<dyn Engine + 'a>, String> {
        let ctx = self.ctx().map_err(|e| e.to_string())?;
        if let Some(endpoint) = ctx.resolve(me, port::ENGINE).into_iter().next() {
            return Ok(Box::new(engine::Wired { ctx, endpoint }));
        }
        engine::built_in(name)
    }

    fn kv_get(&self, me: &NodeRef, prefix: &str) -> Result<Vec<Value>, ModuleError> {
        let ctx = self.ctx()?;
        if *self.has_store.get_or_init(|| ctx.store(None).is_ok()) {
            let store = ctx.store(Some(me))?;
            return store
                .list(prefix)?
                .into_iter()
                .map(|(key, bytes)| {
                    serde_json::from_slice(&bytes)
                        .map_err(|e| ModuleError::Failed(format!("{key}: {e}")))
                })
                .collect();
        }
        Ok(lock(&self.memory)
            .range(prefix.to_string()..)
            .take_while(|(key, _)| key.starts_with(prefix))
            .map(|(_, value)| value.clone())
            .collect())
    }

    fn kv_set(&self, me: &NodeRef, key: &str, value: &Value) -> Result<(), ModuleError> {
        let ctx = self.ctx()?;
        if *self.has_store.get_or_init(|| ctx.store(None).is_ok()) {
            return ctx.store(Some(me))?.set_json(key, value);
        }
        lock(&self.memory).insert(key.into(), value.clone());
        Ok(())
    }
}

/// The state of one workspace, owned by its thread.
struct Ws {
    shared: Arc<Shared>,
    at: At,
    spec: Spec,
    engine: String,
    info: Option<Info>,
    quota: u64,
    over: bool,
    archive_on_stop: bool,
}

impl Ws {
    fn run(mut self, ops: mpsc::Receiver<Op>, every: Option<Duration>) {
        loop {
            let op = match every {
                Some(every) => match ops.recv_timeout(every) {
                    Ok(op) => op,
                    Err(RecvTimeoutError::Timeout) => Op::Check(false),
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match ops.recv() {
                    Ok(op) => op,
                    Err(_) => return,
                },
            };
            let stop = matches!(op, Op::Stop);
            if let Err(message) = self.handle(op) {
                let _ = self.shared.error(&self.at, message);
            }
            if stop {
                return;
            }
        }
    }

    fn handle(&mut self, op: Op) -> Result<(), String> {
        match op {
            Op::Create => self.create(),
            Op::Write(value) => self.write(value),
            Op::Merge => self.merge(),
            Op::Archive => self.archive(),
            Op::Check(say) => {
                self.check(say);
                Ok(())
            }
            Op::Stop if self.archive_on_stop && self.info.is_some() => self.archive(),
            Op::Stop => Ok(()),
        }
    }

    fn info(&self) -> Result<&Info, String> {
        self.info
            .as_ref()
            .ok_or_else(|| "the workspace does not exist yet: send `create` first".into())
    }

    fn create(&mut self) -> Result<(), String> {
        let info = match &self.info {
            Some(info) if info.path.exists() => info.clone(),
            _ => {
                let engine = self.shared.engine(&self.at.me, &self.engine)?;
                engine.create(&self.spec)?
            }
        };
        lock(&self.shared.made).insert(info.path.clone(), info.clone());
        let value = json!(info);
        if let Ok(ctx) = self.shared.ctx() {
            let _ = ctx.checkpoint(&self.at.me, value.clone());
        }
        self.shared.emit(&self.at, port::INFO, value);
        self.shared.emit(
            &self.at,
            port::READY,
            json!(info.path.display().to_string()),
        );
        self.info = Some(info);
        self.check(false);
        Ok(())
    }

    /// Measures the folder. Says `quota_exceeded` once per crossing; `usage` if asked.
    fn check(&mut self, say: bool) -> u64 {
        let Some(info) = &self.info else {
            return 0;
        };
        let limit = if self.quota == 0 {
            u64::MAX
        } else {
            self.quota
        };
        let used = fsx::usage(&info.path, limit);
        let over = self.quota > 0 && used > self.quota;
        let report = json!({ "id": info.id, "path": info.path, "used": used, "quota": self.quota });
        if over && !self.over {
            self.shared
                .emit(&self.at, port::QUOTA_EXCEEDED, report.clone());
        }
        self.over = over;
        if say {
            self.shared.emit(&self.at, port::USAGE, report);
        }
        used
    }

    fn write(&mut self, value: Value) -> Result<(), String> {
        let root = self.info()?.path.clone();
        let path = value
            .get("path")
            .and_then(Value::as_str)
            .ok_or("`write` takes `{ path, text }`")?;
        let text = match value.get("text") {
            Some(Value::String(text)) => text.clone(),
            Some(other) => serde_json::to_string_pretty(other).unwrap_or_default(),
            None => String::new(),
        };
        let target = fsx::confine(&root, &root.join(path))?;
        let used = self.check(false);
        if self.quota > 0 && (self.over || used.saturating_add(text.len() as u64) > self.quota) {
            if !self.over {
                self.over = true;
                self.shared.emit(
                    &self.at,
                    port::QUOTA_EXCEEDED,
                    json!({ "id": self.spec.id, "path": root, "used": used, "quota": self.quota }),
                );
            }
            return Err(format!(
                "{path}: refused, the workspace is at {used} of `quota_bytes` {}",
                self.quota
            ));
        }
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
        }
        fs::write(&target, text).map_err(|e| format!("{}: {e}", target.display()))?;
        self.shared
            .emit(&self.at, port::WRITTEN, json!(target.display().to_string()));
        self.check(false);
        Ok(())
    }

    fn merge(&mut self) -> Result<(), String> {
        let info = self.info()?.clone();
        let shared = self.shared.clone();
        let merged = shared
            .engine(&self.at.me, &info.engine)?
            .merge(&info, &self.spec.options)?;
        match merged {
            Merge::Done(value) => shared.emit(&self.at, port::MERGED, value),
            Merge::Conflict(value) => shared.emit(&self.at, port::CONFLICT, value),
        }
        self.check(false);
        Ok(())
    }

    fn archive(&mut self) -> Result<(), String> {
        let info = self.info()?.clone();
        let engine = self.shared.engine(&self.at.me, &info.engine)?;
        let archived = engine.archive(&info, &self.spec.options)?;
        lock(&self.shared.made).remove(&info.path);
        self.info = None;
        self.over = false;
        self.shared.emit(&self.at, port::ARCHIVED, archived);
        Ok(())
    }
}

/// Merge queue handling, under the lines lock.
impl Shared {
    fn line_start(self: &Arc<Self>, call: &NodeCall) -> Result<(), ModuleError> {
        let at = At::of(call);
        let gates: Vec<String> = call
            .node
            .ports
            .iter()
            .filter(|p| p.direction == Direction::Input && p.kind == WireKind::Then)
            .filter(|p| p.id == PASS || p.id.starts_with(&format!("{PASS}_")))
            .map(|p| p.id.clone())
            .collect();
        if gates.is_empty() {
            return Err(ModuleError::InvalidParams(format!(
                "merge queue `{}` has no gate: declare a `pass` input",
                at.me.node_id
            )));
        }
        self.check_mode(&call.node)?;
        let mut state: LineState = parse::<NodeStart>(call.args.clone())
            .ok()
            .and_then(|start| start.checkpoint)
            .and_then(|state| serde_json::from_value(state).ok())
            .unwrap_or_default();
        // After a restart the first entry goes to its gates again.
        for entry in &mut state.entries {
            entry.checked = false;
        }
        let options = self.options(&call.node, &at.me.execution_id);
        let mut lines = lock(&self.lines);
        lines.insert(
            at.me.clone(),
            Line {
                at: at.clone(),
                entries: state.entries,
                next: state.next,
                merging: false,
                gates,
                options,
            },
        );
        self.advance(&mut lines, &at.me);
        Ok(())
    }

    fn save_line(&self, line: &Line) {
        let state = json!({ "entries": line.entries, "next": line.next });
        if let Ok(ctx) = self.ctx() {
            let _ = ctx.checkpoint(&line.at.me, state);
        }
    }

    /// Sends the first entry to `check` once, and merges it if every gate passed.
    fn advance(self: &Arc<Self>, lines: &mut HashMap<NodeRef, Line>, me: &NodeRef) {
        let Some(line) = lines.get_mut(me) else {
            return;
        };
        self.save_line(line);
        if line.merging {
            return;
        }
        let Some(first) = line.entries.front_mut() else {
            return;
        };
        if !first.checked {
            first.checked = true;
            let check = json!({ "entry": first.entry, "info": first.info });
            self.emit(&line.at, port::CHECK, check);
        }
        if !line.gates.iter().all(|gate| first.passed.contains(gate)) {
            return;
        }
        line.merging = true;
        let entry = first.clone();
        let (at, options) = (line.at.clone(), line.options.clone());
        let shared = self.clone();
        let spawned = std::thread::Builder::new()
            .name("regulus-workspace-merge".into())
            .spawn(move || {
                let result = serde_json::from_value::<Info>(entry.info.clone())
                    .map_err(|e| format!("entry {} is not a workspace: {e}", entry.entry))
                    .and_then(|info| {
                        shared.trusted(&at, &info)?;
                        shared.engine(&at.me, &info.engine)?.merge(&info, &options)
                    });
                shared.merged(&at, entry.entry, result);
            });
        if spawned.is_err() {
            line.merging = false;
        }
    }

    /// A merge ended: out with the entry, say how, and on to the next one.
    fn merged(self: &Arc<Self>, at: &At, entry: u64, result: Result<Merge, String>) {
        let mut lines = lock(&self.lines);
        let Some(line) = lines.get_mut(&at.me) else {
            return; // stopped meanwhile: nobody wants the answer
        };
        line.merging = false;
        let mut info = Value::Null;
        if line.entries.front().is_some_and(|e| e.entry == entry) {
            info = line.entries.pop_front().map(|e| e.info).unwrap_or_default();
        }
        match result {
            Ok(Merge::Done(value)) => {
                self.emit(at, port::MERGED, json!({ "entry": entry, "merged": value }))
            }
            Ok(Merge::Conflict(value)) => self.emit(
                at,
                port::CONFLICT,
                json!({ "entry": entry, "conflict": value }),
            ),
            Err(message) => {
                let _ = self.error(at, format!("entry {entry}: {message}"));
                let rejected = json!({ "entry": entry, "info": info, "gate": Value::Null,
                                       "error": message });
                self.emit(at, port::REJECTED, rejected);
            }
        }
        self.advance(&mut lines, &at.me);
    }

    fn line_event(self: &Arc<Self>, at: &At, event: NodeEvent) -> Result<Value, ModuleError> {
        let mut lines = lock(&self.lines);
        let line = lines
            .get_mut(&at.me)
            .ok_or_else(|| ModuleError::Failed(format!("`{}` is not running", at.me.node_id)))?;
        let port_id = event.port.as_str();
        if port_id == port::IN {
            let info = serde_json::from_value::<Info>(event.value.clone())
                .map_err(|e| self.error(at, format!("`in` takes a workspace's `info`: {e}")))?;
            self.trusted(at, &info)
                .map_err(|e| self.error(at, format!("`in` refused: {e}")))?;
            let max = self.number(&at.node, "max_entries");
            if line.entries.len() as u64 >= max {
                return Err(self.error(at, format!("{max} entries are waiting (`max_entries`)")));
            }
            line.next += 1;
            let entry = line.next;
            line.entries.push_back(Entry {
                entry,
                info: event.value.clone(),
                passed: BTreeSet::new(),
                checked: false,
            });
            let queued =
                json!({ "entry": entry, "position": line.entries.len(), "info": event.value });
            self.emit(at, port::QUEUED, queued);
            self.advance(&mut lines, &at.me);
            return Ok(json!({ "entry": entry }));
        }
        let pass = port_id == PASS || port_id.starts_with(&format!("{PASS}_"));
        let fail = port_id == FAIL || port_id.starts_with(&format!("{FAIL}_"));
        if !pass && !fail {
            return Err(ModuleError::InvalidParams(format!(
                "merge queue takes nothing on `{port_id}`"
            )));
        }
        let named = event.value.get("entry").and_then(Value::as_u64);
        let index = match named {
            Some(n) => line.entries.iter().position(|e| e.entry == n),
            None => (!line.entries.is_empty()).then_some(0),
        }
        .ok_or_else(|| self.error(at, format!("`{port_id}`: no entry waiting for it")))?;
        if pass {
            line.entries[index].passed.insert(port_id.to_string());
        } else {
            if index == 0 && line.merging {
                return Err(self.error(at, "the first entry is merging already".into()));
            }
            let entry = line.entries.remove(index).expect("found above");
            let rejected = json!({ "entry": entry.entry, "info": entry.info, "gate": port_id,
                                   "value": event.value });
            self.emit(at, port::REJECTED, rejected);
        }
        self.advance(&mut lines, &at.me);
        Ok(Value::Null)
    }
}

/// Artifacts.
impl Shared {
    fn artifacts_prefix(me: &NodeRef) -> String {
        format!("artifact/{}/{}/", me.execution_id, me.node_id)
    }

    fn add_artifact(self: &Arc<Self>, at: &At, value: &Value) -> Result<Value, ModuleError> {
        let (path, name) = match value {
            Value::String(path) => (path.clone(), None),
            Value::Object(map) => (
                map.get("path")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                map.get("name").and_then(Value::as_str).map(str::to_string),
            ),
            _ => (String::new(), None),
        };
        if path.trim().is_empty() {
            return Err(self.error(
                at,
                format!("`add` takes a path or `{{ path, name? }}`, not {value}"),
            ));
        }
        let base = match self.place(at, "dir")? {
            Some(dir) => dir,
            None => self.ctx()?.node_dir(&at.me)?,
        };
        let source = base.join(path.trim());
        let source = if self.text(&at.node, "allow_outside") == YES {
            source
        } else {
            fsx::confine(&base, &source).map_err(|e| self.error(at, e))?
        };
        let meta = fs::metadata(&source)
            .map_err(|e| self.error(at, format!("{}: {e}", source.display())))?;
        let max = self.number(&at.node, "max_bytes");
        if !meta.is_file() || meta.len() > max {
            return Err(self.error(
                at,
                format!(
                    "{}: not a regular file of at most {max} bytes",
                    source.display()
                ),
            ));
        }
        let name = name.unwrap_or_else(|| {
            source
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string()
        });
        let prefix = Self::artifacts_prefix(&at.me);
        let key;
        let seq;
        {
            // Only the number is taken under the lock: a placeholder holds it.
            let _numbering = lock(&self.numbering);
            seq = self.kv_get(&at.me, &prefix)?.len() as u64 + 1;
            key = format!("{prefix}{seq:012}");
            self.kv_set(
                &at.me,
                &key,
                &json!({ "seq": seq, "name": name, "pending": true }),
            )?;
        }
        let copy_to = if self.text(&at.node, "copy") == YES {
            let file = Path::new(&name)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            Some(
                self.ctx()?
                    .node_dir(&at.me)?
                    .join(&at.me.execution_id)
                    .join(format!("{seq}-{file}")),
            )
        } else {
            None
        };
        // A copy of up to `max_bytes` runs on a thread of its own, not on the bus thread.
        let (shared, at) = (self.clone(), at.clone());
        let bytes = meta.len();
        let keep = move || {
            let kept = match &copy_to {
                Some(to) => to
                    .parent()
                    .map_or(Ok(()), fs::create_dir_all)
                    .and_then(|()| fs::copy(&source, to))
                    .map(|_| to.clone())
                    .map_err(|e| format!("{}: {e}", to.display())),
                None => Ok(source.clone()),
            };
            let record = match &kept {
                Ok(kept) => json!({ "seq": seq, "name": name, "path": kept, "source": source,
                                    "bytes": bytes, "at": now_ms() }),
                Err(error) => json!({ "seq": seq, "name": name, "source": source,
                                      "error": error, "at": now_ms() }),
            };
            if let Err(error) = shared.kv_set(&at.me, &key, &record) {
                let _ = shared.error(&at, error.to_string());
            }
            match kept {
                Ok(kept) => shared.emit(&at, port::OUT, json!(kept.display().to_string())),
                Err(error) => {
                    let _ = shared.error(&at, error);
                }
            }
        };
        std::thread::Builder::new()
            .name("regulus-workspace-artifact".into())
            .spawn(keep)
            .map_err(|e| ModuleError::Failed(e.to_string()))?;
        Ok(json!({ "seq": seq }))
    }
}

pub struct Workspace {
    shared: Arc<Shared>,
}

impl Workspace {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                ctx: OnceLock::new(),
                manifest: parse_manifest(MANIFEST).expect("workspace manifest is valid"),
                has_store: OnceLock::new(),
                memory: Mutex::default(),
                workers: Mutex::default(),
                lines: Mutex::default(),
                made: Mutex::default(),
                numbering: Mutex::default(),
            }),
        }
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let shared = &self.shared;
        match call.node.node_type.as_str() {
            WORKSPACE => {}
            MERGE_QUEUE => {
                shared.line_start(&call)?;
                return Ok(Value::Null);
            }
            _ => return Ok(Value::Null),
        }
        let at = At::of(&call);
        let engine = shared.text(&call.node, "engine");
        engine::built_in(&engine).map_err(ModuleError::InvalidParams)?;
        shared.check_mode(&call.node)?;
        let resumed: Option<Info> = parse::<NodeStart>(call.args.clone())
            .ok()
            .and_then(|start| start.checkpoint)
            .and_then(|state| serde_json::from_value(state).ok());
        let create = resumed.is_some() || shared.text(&call.node, "autostart") == YES;
        let ws = Ws {
            shared: shared.clone(),
            spec: shared.spec(&at)?,
            engine,
            info: resumed,
            quota: shared.number(&call.node, "quota_bytes"),
            over: false,
            archive_on_stop: shared.text(&call.node, "on_stop") == "archive",
            at: at.clone(),
        };
        let every = match shared.number(&call.node, "check_ms") {
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        };
        let (send, receive) = mpsc::channel();
        std::thread::Builder::new()
            .name("regulus-workspace".into())
            .spawn(move || ws.run(receive, every))
            .map_err(|e| ModuleError::Failed(e.to_string()))?;
        if create {
            let _ = send.send(Op::Create);
        }
        if let Some(old) = lock(&shared.workers).insert(at.me, send) {
            let _ = old.send(Op::Stop);
        }
        Ok(Value::Null)
    }

    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let me = At::of(&call).me;
        if let Some(worker) = lock(&self.shared.workers).remove(&me) {
            // Its thread finishes what it has, archives if asked, and ends: no wait here.
            let _ = worker.send(Op::Stop);
        }
        lock(&self.shared.lines).remove(&me);
        Ok(Value::Null)
    }

    fn event(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        let at = At::of(&call);
        match call.node.node_type.as_str() {
            WORKSPACE => {
                let op = match event.port.as_str() {
                    port::CREATE => Op::Create,
                    port::WRITE => Op::Write(event.value),
                    port::MERGE => Op::Merge,
                    port::ARCHIVE => Op::Archive,
                    port::CHECK => Op::Check(true),
                    other => {
                        return Err(ModuleError::InvalidParams(format!(
                            "a workspace takes nothing on `{other}`"
                        )))
                    }
                };
                let workers = lock(&self.shared.workers);
                let worker = workers.get(&at.me).ok_or_else(|| {
                    ModuleError::Failed(format!("workspace `{}` is not running", at.me.node_id))
                })?;
                worker
                    .send(op)
                    .map_err(|_| ModuleError::Failed("the workspace's thread is gone".into()))?;
                Ok(json!({ "queued": event.port }))
            }
            MERGE_QUEUE => self.shared.line_event(&at, event),
            ARTIFACTS_NODE => match event.port.as_str() {
                port::ADD => self.shared.add_artifact(&at, &event.value),
                port::LIST => {
                    let list = self
                        .shared
                        .kv_get(&at.me, &Shared::artifacts_prefix(&at.me))?;
                    self.shared.emit(&at, port::ARTIFACTS, json!(list));
                    Ok(json!({ "artifacts": list.len() }))
                }
                other => Err(ModuleError::InvalidParams(format!(
                    "artifacts takes nothing on `{other}`"
                ))),
            },
            other => Err(ModuleError::InvalidParams(format!(
                "`{other}` is not a node of this module"
            ))),
        }
    }
}

impl Default for Workspace {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct Target {
    execution_id: String,
    node_id: String,
}

impl Module for Workspace {
    fn manifest(&self) -> &Manifest {
        &self.shared.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        let _ = self.shared.ctx.set(ctx.clone());
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            method::NODE_START => self.start(parse(params)?),
            method::NODE_STOP => self.stop(parse(params)?),
            method::NODE_EVENT => self.event(parse(params)?),
            ARTIFACTS => {
                let target: Target = parse(params)?;
                let me = NodeRef {
                    execution_id: target.execution_id,
                    node_id: target.node_id,
                };
                Ok(json!(self
                    .shared
                    .kv_get(&me, &Shared::artifacts_prefix(&me))?))
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        // Dropping the senders ends each thread after its current operation.
        lock(&self.shared.workers).clear();
        lock(&self.shared.lines).clear();
        Ok(())
    }
}
