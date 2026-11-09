//! Delegation (docs/13-blocks.md §3.3): workers that Regulus keeps alive for an agent or a
//! person. A `delegate` node starts a kept child execution (`ChildMode::Keep`) per worker,
//! of one of the workflows its `workers` field names, tagged with a stable id `w-<n>`; it
//! briefs the worker on the child's "then" input `prompt_port`, buffers what comes out of
//! `result_port`, sends it more text and stops it on request.
//!
//! Three ways in, one set of operations: `tool@1` for an agent (through the Tools box),
//! `participant@1` for a channel (the node is a group; `@w-1` reaches one worker, no
//! mention all of them), and module methods for the CLI and clients (`delegation.*`).
//! Whatever is wired into the node's "uses" inputs goes to the worker's "uses" inputs of
//! the same id as bindings; for `engine`, only the engine `worker_create` names.
//!
//! The worker table is checkpointed. Workers die with the core: after a restart they show
//! as `interrupted`.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regulus_contract_participant as participant;
use regulus_contract_tool as tool;
use regulus_core::{
    parse_manifest, ChildMode, ChildOptions, Endpoint, Module, ModuleCtx, ModuleError, NodeRef,
};
use regulus_proto::{
    method, ChildEvent, ChildEventKind, Direction, ExecutionState, Manifest, Node, NodeCall,
    NodeEvent, NodeStart, WireKind,
};
use serde::Deserialize;
use serde_json::{json, Map, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const ENGINE_PORT: &str = "engine";
const CREATED: &str = "created";
const RESULT: &str = "result";
const CLOSED: &str = "closed";
const WORKERS_METHOD: &str = "delegation.workers";
const CREATE_METHOD: &str = "delegation.create";
const SEND_METHOD: &str = "delegation.send";
const RESULT_METHOD: &str = "delegation.result";
const CLOSE_METHOD: &str = "delegation.close";
/// "Uses" inputs of a worker and what they are bound to.
type Bindings = Vec<(String, Vec<Endpoint>)>;
/// What a channel calls this node: a group of workers.
const PARTICIPANT_KIND: &str = "group";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ModuleError> {
    serde_json::from_value(value).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn to_json(value: impl serde::Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(|e| ModuleError::Failed(e.to_string()))
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// `worker_create` and `delegation.create`.
#[derive(Debug, Default, Deserialize)]
struct CreateArgs {
    #[serde(default)]
    brief: String,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    engine: Option<String>,
}

#[derive(Debug, Deserialize)]
struct WorkerArgs {
    worker: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    wait_ms: u64,
}

/// A module method on one node: `{ execution_id, node_id, …rest }`.
#[derive(Debug, Deserialize)]
struct OnNode {
    execution_id: String,
    node_id: String,
    #[serde(flatten)]
    rest: Value,
}

struct Worker {
    id: String,
    kind: String,
    name: String,
    engine: Option<String>,
    /// The child execution; empty while it starts.
    execution: String,
    state: String,
    results: VecDeque<Value>,
    last_output_at: Option<u64>,
    /// The last value on an output that is not `result_port`.
    last_other: Option<Value>,
    /// Stopped on request: its end reads `closed`.
    closing: bool,
}

impl Worker {
    fn live(&self) -> bool {
        matches!(self.state.as_str(), "starting" | "running")
    }

    fn view(&self) -> Value {
        let mut view = json!({ "worker": self.id, "kind": self.kind, "name": self.name,
                               "execution": self.execution, "state": self.state,
                               "pending_results": self.results.len(),
                               "last_output_at": self.last_output_at });
        if let Some(engine) = &self.engine {
            view["engine"] = json!(engine);
        }
        if let Some(other) = &self.last_other {
            view["last_other"] = other.clone();
        }
        view
    }

    fn saved(&self) -> Value {
        json!({ "worker": self.id, "kind": self.kind, "name": self.name,
                "execution": self.execution, "engine": self.engine, "state": self.state })
    }
}

#[derive(Default)]
struct Table {
    next: u64,
    workers: Vec<Worker>,
}

/// One running `delegate` node: its settings, as of its start, and its workers.
struct Group {
    me: NodeRef,
    node: Node,
    table: Mutex<Table>,
    /// Wakes `worker_result` calls waiting for a result.
    wake: Condvar,
}

pub struct Delegation {
    manifest: Manifest,
    ctx: ModuleCtx,
    groups: Mutex<HashMap<NodeRef, Arc<Group>>>,
}

impl Default for Delegation {
    fn default() -> Self {
        Self::new()
    }
}

impl Delegation {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("delegation manifest is valid"),
            ctx: ModuleCtx::default(),
            groups: Mutex::default(),
        }
    }

    fn setting<'a>(&'a self, group: &'a Group, field: &str) -> Option<&'a Value> {
        self.manifest.setting(&group.node, field)
    }

    fn text(&self, group: &Group, field: &str) -> String {
        match self.setting(group, field) {
            Some(Value::String(text)) => text.trim().to_string(),
            _ => String::new(),
        }
    }

    fn number(&self, group: &Group, field: &str) -> u64 {
        match self.setting(group, field) {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn group(&self, me: &NodeRef) -> Result<Arc<Group>, String> {
        lock(&self.groups)
            .get(me)
            .cloned()
            .ok_or_else(|| format!("delegate `{}` is not running", me.node_id))
    }

    fn emit(&self, group: &Group, port: &str, value: Value) {
        let has = group
            .node
            .port(port)
            .is_some_and(|p| p.kind == WireKind::Then && p.direction == Direction::Output);
        if has {
            if let Err(error) = self.ctx.emit(&group.me, port, value) {
                eprintln!("regulus: delegate `{}`: {error}", group.me.node_id);
            }
        }
    }

    /// The worker table as a checkpoint; a core without a store keeps it in memory.
    fn save(&self, group: &Group) {
        let state = {
            let table = lock(&group.table);
            let workers: Vec<Value> = table.workers.iter().map(Worker::saved).collect();
            json!({ "next": table.next, "workers": workers })
        };
        let _ = self.ctx.checkpoint(&group.me, state);
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        let start: NodeStart = serde_json::from_value(call.args.clone()).unwrap_or_default();
        let mut table = Table::default();
        if let Some(saved) = start.checkpoint {
            table.next = saved["next"].as_u64().unwrap_or(0);
            for worker in saved["workers"].as_array().into_iter().flatten() {
                let text = |key: &str| worker[key].as_str().unwrap_or_default().to_string();
                let state = match text("state").as_str() {
                    "starting" | "running" => "interrupted".to_string(),
                    other => other.to_string(),
                };
                table.workers.push(Worker {
                    id: text("worker"),
                    kind: text("kind"),
                    name: text("name"),
                    engine: worker["engine"].as_str().map(String::from),
                    execution: text("execution"),
                    state,
                    results: VecDeque::new(),
                    last_output_at: None,
                    last_other: None,
                    closing: false,
                });
            }
        }
        let group = Arc::new(Group {
            me: me.clone(),
            node: call.node,
            table: Mutex::new(table),
            wake: Condvar::new(),
        });
        lock(&self.groups).insert(me, group.clone());
        self.save(&group);
        Ok(Value::Null)
    }

    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        let Some(group) = lock(&self.groups).remove(&me) else {
            return Ok(Value::Null);
        };
        let live: Vec<String> = {
            let mut table = lock(&group.table);
            table
                .workers
                .iter_mut()
                .filter(|w| w.live() && !w.execution.is_empty())
                .map(|w| {
                    w.closing = true;
                    w.execution.clone()
                })
                .collect()
        };
        group.wake.notify_all();
        if self.text(&group, "close_on_stop") != "no" {
            for child in live {
                let _ = self.ctx.stop_child(&me, &child);
            }
        }
        Ok(Value::Null)
    }

    /// The workflow of `kind`, or of the default kind.
    fn workflow_of(&self, group: &Group, kind: Option<&str>) -> Result<(String, String), String> {
        let kinds: Map<String, Value> = match self.setting(group, "workers") {
            Some(Value::Object(kinds)) => kinds.clone(),
            _ => Map::new(),
        };
        let default = self.text(group, "default_kind");
        let kind = match kind.map(str::trim).filter(|k| !k.is_empty()) {
            Some(kind) => kind.to_string(),
            None if !default.is_empty() => default,
            None => kinds
                .keys()
                .next()
                .cloned()
                .ok_or_else(|| format!("delegate `{}` has no `workers`", group.me.node_id))?,
        };
        let workflow = kinds
            .get(&kind)
            .and_then(Value::as_str)
            .filter(|w| !w.trim().is_empty())
            .ok_or_else(|| {
                let known: Vec<&String> = kinds.keys().collect();
                format!("no worker kind `{kind}`; there are {known:?}")
            })?;
        Ok((kind, workflow.trim().to_string()))
    }

    /// What the worker gets on its "uses" inputs: each of the node's own, and for
    /// `engine` the one named (by node id), or the first.
    fn bindings(
        &self,
        group: &Group,
        engine: Option<&str>,
    ) -> Result<(Bindings, Option<String>), String> {
        let mut bindings = Vec::new();
        let mut chosen = None;
        for port in group
            .node
            .ports
            .iter()
            .filter(|p| p.direction == Direction::Input && p.kind == WireKind::Uses)
        {
            let endpoints = self.ctx.resolve(&group.me, &port.id);
            if port.id != ENGINE_PORT {
                if !endpoints.is_empty() {
                    bindings.push((port.id.clone(), endpoints));
                }
                continue;
            }
            let picked = match engine.map(str::trim).filter(|e| !e.is_empty()) {
                Some(wanted) => Some(
                    endpoints
                        .iter()
                        .find(|e| e.node().node_id == wanted)
                        .cloned()
                        .ok_or_else(|| {
                            let ids: Vec<&str> = endpoints
                                .iter()
                                .map(|e| e.node().node_id.as_str())
                                .collect();
                            format!("no engine `{wanted}` is wired; there are {ids:?}")
                        })?,
                ),
                None => endpoints.first().cloned(),
            };
            if let Some(picked) = picked {
                chosen = Some(picked.node().node_id.clone());
                bindings.push((port.id.clone(), vec![picked]));
            }
        }
        if engine.is_some_and(|e| !e.trim().is_empty()) && chosen.is_none() {
            return Err("no engine is wired into `engine`".into());
        }
        Ok((bindings, chosen))
    }

    /// Starts a kept child with `opts`, dropping the bindings the workflow does not take:
    /// the core refuses them and the node cannot see the workflow's ports.
    fn start_child(
        &self,
        me: &NodeRef,
        workflow: &str,
        inputs: Vec<(String, Value)>,
        mut opts: ChildOptions,
    ) -> Result<String, ModuleError> {
        loop {
            match self
                .ctx
                .start_child(me, workflow, inputs.clone(), opts.clone())
            {
                Err(ModuleError::InvalidParams(message)) => {
                    let refused = opts.bindings.iter().position(|(port, _)| {
                        message
                            == format!("`{port}` is not a \"uses\" input of workflow `{workflow}`")
                    });
                    match refused {
                        Some(index) => {
                            opts.bindings.remove(index);
                        }
                        None => return Err(ModuleError::InvalidParams(message)),
                    }
                }
                other => return other,
            }
        }
    }

    fn create(&self, group: &Arc<Group>, args: CreateArgs) -> Result<Value, String> {
        let (kind, workflow) = self.workflow_of(group, args.kind.as_deref())?;
        let (bindings, engine) = self.bindings(group, args.engine.as_deref())?;
        let max = self.number(group, "max_workers");
        let id = {
            let mut table = lock(&group.table);
            let live = table.workers.iter().filter(|w| w.live()).count() as u64;
            if max > 0 && live >= max {
                return Err(format!(
                    "{live} workers are live already (`max_workers` {max}): close one first"
                ));
            }
            table.next += 1;
            let id = format!("w-{}", table.next);
            let name = args
                .name
                .clone()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| id.clone());
            table.workers.push(Worker {
                id: id.clone(),
                kind: kind.clone(),
                name,
                engine: engine.clone(),
                execution: String::new(),
                state: "starting".into(),
                results: VecDeque::new(),
                last_output_at: None,
                last_other: None,
                closing: false,
            });
            id
        };
        let prompt_port = self.text(group, "prompt_port");
        let inputs = if args.brief.trim().is_empty() {
            Vec::new()
        } else {
            vec![(prompt_port, Value::String(args.brief))]
        };
        let opts = ChildOptions {
            mode: ChildMode::Keep,
            tag: Some(id.clone()),
            bindings,
        };
        let started = self.start_child(&group.me, &workflow, inputs, opts);
        let view = {
            let mut table = lock(&group.table);
            let Some(index) = table.workers.iter().position(|w| w.id == id) else {
                return Err("the delegate stopped".into());
            };
            match started {
                Ok(child) => {
                    let worker = &mut table.workers[index];
                    worker.execution = child;
                    if worker.state == "starting" {
                        worker.state = "running".into();
                    }
                    worker.view()
                }
                Err(error) => {
                    table.workers.remove(index);
                    return Err(error.to_string());
                }
            }
        };
        self.save(group);
        let created = json!({ "worker": id, "execution": view["execution"], "kind": kind,
                              "name": view["name"], "engine": engine });
        self.emit(group, CREATED, created);
        Ok(view)
    }

    fn with_worker<T>(
        &self,
        group: &Group,
        worker: &str,
        f: impl FnOnce(&mut Worker) -> T,
    ) -> Result<T, String> {
        let mut table = lock(&group.table);
        table
            .workers
            .iter_mut()
            .find(|w| w.id == worker || w.name == worker)
            .map(f)
            .ok_or_else(|| format!("no worker `{worker}`"))
    }

    fn status(&self, group: &Group, worker: &str) -> Result<Value, String> {
        self.with_worker(group, worker, |w| w.view())
    }

    fn list(&self, group: &Group) -> Value {
        let table = lock(&group.table);
        Value::Array(table.workers.iter().map(Worker::view).collect())
    }

    /// The next result of a worker, waiting up to `wait_ms` (capped by `max_wait_ms`).
    fn result(&self, group: &Group, worker: &str, wait_ms: u64) -> Result<Value, String> {
        let wait = Duration::from_millis(wait_ms.min(self.number(group, "max_wait_ms")));
        let deadline = Instant::now() + wait;
        let mut table = lock(&group.table);
        loop {
            let found = table
                .workers
                .iter_mut()
                .find(|w| w.id == worker || w.name == worker)
                .ok_or_else(|| format!("no worker `{worker}`"))?;
            let id = found.id.clone();
            if let Some(result) = found.results.pop_front() {
                let mut answer = json!({ "worker": id });
                if let Value::Object(fields) = result {
                    answer
                        .as_object_mut()
                        .into_iter()
                        .for_each(|a| a.extend(fields.clone()));
                }
                return Ok(answer);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if !found.live() || left.is_zero() {
                let pending = found.live();
                return Ok(json!({ "worker": id, "pending": pending, "state": found.state }));
            }
            table = group
                .wake
                .wait_timeout(table, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn send(&self, group: &Group, worker: &str, text: String) -> Result<Value, String> {
        let (id, child) = self.with_worker(group, worker, |w| {
            (w.id.clone(), w.live().then(|| w.execution.clone()))
        })?;
        let child = child
            .filter(|c| !c.is_empty())
            .ok_or_else(|| format!("worker `{id}` is not running"))?;
        let port = self.text(group, "prompt_port");
        self.ctx
            .send_child(&group.me, &child, &port, Value::String(text))
            .map_err(|e| e.to_string())?;
        Ok(json!({ "worker": id, "sent": true }))
    }

    fn close(&self, group: &Group, worker: &str) -> Result<Value, String> {
        let (id, child) = self.with_worker(group, worker, |w| {
            let child = w.live().then(|| w.execution.clone());
            if child.is_some() {
                w.closing = true;
            }
            (w.id.clone(), child)
        })?;
        if let Some(child) = child.filter(|c| !c.is_empty()) {
            self.ctx
                .stop_child(&group.me, &child)
                .map_err(|e| e.to_string())?;
            // Stopped now; its end reaches `node/child` later and says the same.
            self.with_worker(group, &id, |w| w.state = "closed".into())?;
            group.wake.notify_all();
            self.save(group);
        }
        self.status(group, &id)
    }

    /// News from a worker: a result, another output, or its end.
    fn child(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        let news: ChildEvent = parse(call.args)?;
        let Ok(group) = self.group(&me) else {
            return Ok(Value::Null);
        };
        let Some(tag) = news.tag else {
            return Ok(Value::Null);
        };
        let result_port = self.text(&group, "result_port");
        let keep = usize::try_from(self.number(&group, "max_results")).unwrap_or(usize::MAX);
        match news.kind {
            ChildEventKind::Output { port, value, .. }
            | ChildEventKind::Returned { port, value, .. } => {
                let is_result = result_port.is_empty() || port == result_port;
                let found = self.with_worker(&group, &tag, |w| {
                    w.last_output_at = Some(now_ms());
                    if !is_result {
                        w.last_other = Some(json!({ "port": port, "value": value }));
                        return;
                    }
                    w.results.push_back(json!({ "port": port, "value": value }));
                    while w.results.len() > keep.max(1) {
                        w.results.pop_front();
                    }
                });
                if found.is_ok() && is_result {
                    group.wake.notify_all();
                    self.emit(
                        &group,
                        RESULT,
                        json!({ "worker": tag, "port": port, "value": value }),
                    );
                }
            }
            ChildEventKind::Ended { state } => {
                let ended = self.with_worker(&group, &tag, |w| {
                    w.state = if w.closing {
                        "closed".into()
                    } else {
                        match &state {
                            ExecutionState::Failed { .. } => "failed".into(),
                            ExecutionState::Interrupted { .. } => "interrupted".into(),
                            _ => "stopped".into(),
                        }
                    };
                    w.state.clone()
                });
                if let Ok(now) = ended {
                    group.wake.notify_all();
                    self.save(&group);
                    let mut closed = json!({ "worker": tag, "state": now });
                    if let ExecutionState::Failed { reason } = &state {
                        closed["reason"] = json!(reason);
                    }
                    self.emit(&group, CLOSED, closed);
                }
            }
        }
        Ok(Value::Null)
    }

    fn tool_specs(&self, group: &Group) -> Vec<tool::Spec> {
        let worker = || json!({ "type": "string", "description": "the worker's id, w-<n>" });
        let kinds: Vec<String> = match self.setting(group, "workers") {
            Some(Value::Object(kinds)) => kinds.keys().cloned().collect(),
            _ => Vec::new(),
        };
        let engines: Vec<String> = self
            .ctx
            .resolve(&group.me, ENGINE_PORT)
            .iter()
            .map(|e| e.node().node_id.clone())
            .collect();
        let spec = |name: &str, description: String, input: Value| tool::Spec {
            name: name.into(),
            description,
            input,
        };
        vec![
            spec(
                "worker_create",
                format!(
                    "Starts a worker that keeps running, briefed with `brief`. Kinds: {kinds:?}; \
                     engines: {engines:?} (the first if none is named). Answers its id, w-<n>."
                ),
                json!({ "type": "object", "properties": {
                    "brief": { "type": "string" }, "kind": { "type": "string" },
                    "name": { "type": "string" }, "engine": { "type": "string" } },
                    "required": ["brief"] }),
            ),
            spec(
                "worker_status",
                "Whether a worker runs, and how many results wait to be read.".into(),
                json!({ "type": "object", "properties": { "worker": worker() },
                        "required": ["worker"] }),
            ),
            spec(
                "worker_result",
                "The next result of a worker, waiting up to `wait_ms` for one; `pending` if \
                 none came yet."
                    .into(),
                json!({ "type": "object", "properties": { "worker": worker(),
                        "wait_ms": { "type": "integer" } }, "required": ["worker"] }),
            ),
            spec(
                "worker_send",
                "Sends a worker more text: a follow-up, a correction, a new task.".into(),
                json!({ "type": "object", "properties": { "worker": worker(),
                        "text": { "type": "string" } }, "required": ["worker", "text"] }),
            ),
            spec(
                "worker_close",
                "Stops a worker for good.".into(),
                json!({ "type": "object", "properties": { "worker": worker() },
                        "required": ["worker"] }),
            ),
            spec(
                "worker_list",
                "Every worker of this group, live or not.".into(),
                json!({ "type": "object", "properties": {} }),
            ),
        ]
    }

    fn run_tool(&self, group: &Arc<Group>, request: tool::Call) -> Result<Value, String> {
        let args = request.args;
        let worker = || -> Result<WorkerArgs, String> {
            serde_json::from_value(args.clone()).map_err(|e| e.to_string())
        };
        match request.name.as_str() {
            "worker_create" => {
                let args: CreateArgs = match &args {
                    Value::String(brief) => CreateArgs {
                        brief: brief.clone(),
                        ..CreateArgs::default()
                    },
                    other => serde_json::from_value(other.clone()).map_err(|e| e.to_string())?,
                };
                self.create(group, args)
            }
            "worker_status" => self.status(group, &worker()?.worker),
            "worker_result" => {
                let args = worker()?;
                self.result(group, &args.worker, args.wait_ms)
            }
            "worker_send" => {
                let args = worker()?;
                self.send(group, &args.worker, args.text)
            }
            "worker_close" => self.close(group, &worker()?.worker),
            "worker_list" => Ok(self.list(group)),
            other => Err(format!("no tool `{other}`")),
        }
    }

    fn tool(&self, method: &str, mut call: NodeCall) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        let group = self.group(&me).map_err(ModuleError::Failed)?;
        if method == tool::LIST {
            return to_json(self.tool_specs(&group));
        }
        let request: tool::Call = parse(std::mem::take(&mut call.args))?;
        // What went wrong is an answer for the agent to read, not a broken call.
        let outcome = match self.run_tool(&group, request) {
            Ok(data) => tool::Outcome { data, error: false },
            Err(error) => tool::Outcome {
                data: Value::String(error),
                error: true,
            },
        };
        to_json(outcome)
    }

    fn participant(&self, method: &str, call: NodeCall) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        let group = self.group(&me).map_err(ModuleError::Failed)?;
        let name = match self.text(&group, "name") {
            name if name.is_empty() => me.node_id.clone(),
            name => name,
        };
        if method == participant::DESCRIBE {
            return to_json(participant::Identity {
                name,
                kind: PARTICIPANT_KIND.into(),
                state: participant::State::Idle,
            });
        }
        let delivery: participant::Delivery = parse(call.args)?;
        let live: Vec<(String, String)> = lock(&group.table)
            .workers
            .iter()
            .filter(|w| w.live())
            .map(|w| (w.id.clone(), w.name.clone()))
            .collect();
        let mentioned: Vec<&str> = delivery
            .text
            .split_whitespace()
            .filter_map(|word| word.strip_prefix('@'))
            .map(|word| {
                word.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-' && c != '_')
            })
            .collect();
        let targets: Vec<&(String, String)> = {
            let named: Vec<&(String, String)> = live
                .iter()
                .filter(|(id, name)| mentioned.iter().any(|m| m == id || m == name))
                .collect();
            if named.is_empty() {
                live.iter().collect()
            } else {
                named
            }
        };
        let text = format!(
            "[message from @{} in channel `{}`]\n{}",
            delivery.from, delivery.channel, delivery.text
        );
        let mut reached = Vec::new();
        for (id, _) in targets {
            if self.send(&group, id, text.clone()).is_ok() {
                reached.push(id.clone());
            }
        }
        let reply = if reached.is_empty() {
            "no live worker to deliver to".to_string()
        } else {
            format!("delivered to {}", reached.join(", "))
        };
        to_json(participant::Reply { text: reply })
    }

    /// A value on `create`: one worker per brief.
    fn event(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        let me = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        let group = self.group(&me).map_err(ModuleError::Failed)?;
        let briefs = match event.value {
            Value::Array(items) => items,
            one => vec![one],
        };
        let mut created = Vec::new();
        for brief in briefs {
            let args = match brief {
                Value::String(brief) => CreateArgs {
                    brief,
                    ..CreateArgs::default()
                },
                other => parse(other)?,
            };
            created.push(self.create(&group, args).map_err(ModuleError::Failed)?);
        }
        Ok(Value::Array(created))
    }

    /// `delegation.*`: the same operations for the CLI and clients, on a node by id.
    fn method(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let on: OnNode = parse(params)?;
        let me = NodeRef {
            execution_id: on.execution_id,
            node_id: on.node_id,
        };
        let group = self.group(&me).map_err(ModuleError::InvalidParams)?;
        let worker = || -> Result<WorkerArgs, ModuleError> { parse(on.rest.clone()) };
        let result = match name {
            WORKERS_METHOD => Ok(self.list(&group)),
            CREATE_METHOD => self.create(&group, parse(on.rest.clone())?),
            SEND_METHOD => {
                let args = worker()?;
                self.send(&group, &args.worker, args.text)
            }
            RESULT_METHOD => {
                let args = worker()?;
                self.result(&group, &args.worker, args.wait_ms)
            }
            CLOSE_METHOD => self.close(&group, &worker()?.worker),
            other => return Err(ModuleError::UnknownMethod(other.into())),
        };
        result.map_err(ModuleError::Failed)
    }
}

impl Module for Delegation {
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
            method::NODE_STOP => self.stop(parse(params)?),
            method::NODE_CHILD => self.child(parse(params)?),
            method::NODE_EVENT => self.event(parse(params)?),
            tool::LIST | tool::CALL => self.tool(name, parse(params)?),
            participant::DESCRIBE | participant::DELIVER => self.participant(name, parse(params)?),
            WORKERS_METHOD | CREATE_METHOD | SEND_METHOD | RESULT_METHOD | CLOSE_METHOD => {
                self.method(name, params)
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        lock(&self.groups).clear();
        Ok(())
    }
}
