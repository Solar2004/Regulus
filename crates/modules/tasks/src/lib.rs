//! Tasks (docs/13-blocks.md §3.3): a queue of tasks with states and dependencies kept in
//! the module's store (`queue`), a node that claims the next free one under a leased lock
//! (`claim`), and an append-only ledger of facts, plan and progress (`ledger`).
//!
//! Every change to a queue or a ledger happens under one module lock, loaded from the
//! store and saved back before the lock is released: two claims never get the same task.
//! Leases run out on a timer thread of the module, started with the first lease and
//! stopped at shutdown.

mod lease;
pub mod queue;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{
    method, Direction, Hook, HookAnswer, HookEffect, HookFailure, HookRequest, Manifest, Node,
    NodeCall, NodeEvent, WireKind, HOOK_DESCRIBE,
};
use serde::Deserialize;
use serde_json::{json, Value};

use lease::Leases;
use queue::{check_name, parse_new, Kv, Queue, State, Task};

const MANIFEST: &str = include_str!("../regulus-module.toml");

/// `{ execution_id, node_id, queue? }` → a queue's `{ queue, tasks, counts }`.
pub const LIST: &str = "tasks.list";
/// `{ execution_id, node_id, scope? }` → a ledger's `{ entries, text }`.
pub const LEDGER: &str = "tasks.ledger";

const QUEUE: &str = "queue";
const CLAIM: &str = "claim";
const LEDGER_NODE: &str = "ledger";
const YES: &str = "yes";
/// The capability an output may be declared with to carry a task's title only.
const TEXT: &str = "text@1";
const DEFAULT_QUEUE: &str = "tasks";
/// The ledger key of `scope = node`: execution ids never take this form.
const NODE_SCOPE: &str = "@node";
/// The kinds of ledger entries: the ports that append them.
const KINDS: [&str; 3] = ["fact", "plan", "progress"];

mod port {
    pub const ADD: &str = "add";
    pub const LIST: &str = "list";
    pub const ADDED: &str = "added";
    pub const TASKS: &str = "tasks";
    pub const ERROR: &str = "error";
    pub const NEXT: &str = "next";
    pub const COMPLETE: &str = "complete";
    pub const FAIL: &str = "fail";
    pub const TASK: &str = "task";
    pub const DONE: &str = "done";
    pub const FAILED: &str = "failed";
    pub const RETRY: &str = "retry";
    pub const EXPIRED: &str = "expired";
    pub const EMPTY: &str = "empty";
    pub const DRAINED: &str = "drained";
    pub const READ: &str = "read";
    pub const OUT: &str = "out";
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn failed(message: impl Into<String>) -> ModuleError {
    ModuleError::Failed(message.into())
}

/// A node in one execution, its "then" outputs with their capabilities, and its settings.
#[derive(Debug, Clone)]
struct At {
    me: NodeRef,
    node: Node,
    outputs: HashMap<String, String>,
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
                .map(|p| (p.id.clone(), p.capability.clone()))
                .collect(),
            node: call.node.clone(),
        }
    }
}

/// A running `claim` node.
#[derive(Debug, Clone)]
struct Claim {
    at: At,
    queue: String,
    owner: String,
    lease_ms: u64,
    wait: bool,
    auto: bool,
    hold: u64,
    max_attempts: u64,
    release: bool,
    /// Requests from `next` not served yet.
    wanted: u64,
}

pub(crate) struct Shared {
    ctx: OnceLock<ModuleCtx>,
    /// Whether the core has a store, asked on first use: during `init` the core is not
    /// up yet to answer.
    has_store: OnceLock<bool>,
    memory: Arc<Mutex<BTreeMap<String, Value>>>,
    /// The module lock: running claims, and every queue and ledger change happens under it.
    claims: Mutex<HashMap<NodeRef, Claim>>,
    leases: Leases,
    manifest: Manifest,
}

type Claims<'a> = MutexGuard<'a, HashMap<NodeRef, Claim>>;

impl Shared {
    fn lock(&self) -> Claims<'_> {
        self.claims.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn ctx(&self) -> Result<&ModuleCtx, ModuleError> {
        self.ctx
            .get()
            .ok_or_else(|| failed("the tasks module has no context yet"))
    }

    /// The module's space in the store of `me`'s project, or memory in a core without
    /// a store.
    fn kv(&self, me: &NodeRef) -> Result<Kv, ModuleError> {
        let ctx = self.ctx()?;
        if *self.has_store.get_or_init(|| ctx.store(None).is_ok()) {
            Ok(Kv::Store(Arc::new(ctx.store(Some(me))?)))
        } else {
            Ok(Kv::Memory(self.memory.clone()))
        }
    }

    fn setting<'a>(&'a self, node: &'a Node, field: &str) -> Option<&'a Value> {
        self.manifest.setting(node, field)
    }

    fn text(&self, node: &Node, field: &str) -> String {
        match self.setting(node, field) {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    fn number(&self, node: &Node, field: &str) -> u64 {
        match self.setting(node, field) {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn queue_name(&self, node: &Node) -> Result<String, ModuleError> {
        let name = self.text(node, "queue");
        let name = if name.is_empty() {
            DEFAULT_QUEUE.to_string()
        } else {
            name
        };
        check_name(&name)?;
        Ok(name)
    }

    /// Emits on `port` if the node declares it: a task's title if the port is `text@1`.
    fn emit(&self, at: &At, port: &str, value: Value) {
        let Some(capability) = at.outputs.get(port) else {
            return;
        };
        let value = match (capability.as_str(), &value) {
            (TEXT, Value::Object(map)) if map.contains_key("title") => map["title"].clone(),
            (TEXT, Value::String(_)) => value,
            (TEXT, other) => json!(other.to_string()),
            _ => value,
        };
        if let Ok(ctx) = self.ctx() {
            // A stopped execution takes nothing; whoever stopped it no longer wants it.
            let _ = ctx.emit(&at.me, port, value);
        }
    }

    fn task(&self, at: &At, port: &str, task: &Task) {
        self.emit(at, port, json!(task));
    }

    /// Reports `message` on the node's `error` output and returns it as the error.
    fn error(&self, at: &At, message: String) -> ModuleError {
        self.emit(at, port::ERROR, json!(message));
        failed(message)
    }

    /// How many tasks `claim` should take now.
    fn wants(claim: &Claim, queue: &Queue) -> u64 {
        if claim.auto {
            claim
                .hold
                .max(1)
                .saturating_sub(queue.held(&claim.owner).len() as u64)
        } else {
            claim.wanted
        }
    }

    /// Says on `expired` (and `failed`, if it had no attempts left) of the node that
    /// held it each task whose lease ran out, whichever operation found it.
    fn report(&self, claims: &HashMap<NodeRef, Claim>, queue: &str, reaped: Vec<(String, Task)>) {
        for (holder, task) in reaped {
            let Some(claim) = claims
                .values()
                .find(|c| c.queue == queue && c.owner == holder)
            else {
                continue;
            };
            self.task(&claim.at, port::EXPIRED, &task);
            if task.state == State::Failed {
                self.task(&claim.at, port::FAILED, &task);
            }
        }
    }

    /// Saves `queue`, reports the leases it reaped, and looks at it again when its next
    /// lease ends.
    fn close(
        self: &Arc<Self>,
        claims: &HashMap<NodeRef, Claim>,
        me: &NodeRef,
        queue: &mut Queue,
    ) -> Result<(), ModuleError> {
        queue.save()?;
        let reaped = queue.take_reaped();
        self.report(claims, queue.name(), reaped);
        if let Some(until) = queue.next_lease() {
            self.leases.at(self, until, queue.name(), me.clone());
        }
        Ok(())
    }

    /// Serves every running claim of `queue` that wants a task, in a fixed order.
    fn fill(self: &Arc<Self>, claims: &mut Claims, queue: &str) {
        let mut keys: Vec<NodeRef> = claims
            .iter()
            .filter(|(_, c)| c.queue == queue)
            .map(|(k, _)| k.clone())
            .collect();
        keys.sort_by(|a, b| (&a.execution_id, &a.node_id).cmp(&(&b.execution_id, &b.node_id)));
        let mut reaped = Vec::new();
        for key in keys {
            let Some(claim) = claims.get_mut(&key) else {
                continue;
            };
            match self.serve(claim) {
                Ok(found) => reaped.extend(found),
                Err(error) => {
                    let at = claim.at.clone();
                    let _ = self.error(&at, error.to_string());
                }
            }
        }
        self.report(claims, queue, reaped);
    }

    /// Claims for one node as many tasks as it wants and can get. Returns the leases it
    /// found run out, for [`Shared::report`].
    fn serve(self: &Arc<Self>, claim: &mut Claim) -> Result<Vec<(String, Task)>, ModuleError> {
        let kv = self.kv(&claim.at.me)?;
        let mut queue = Queue::load(kv, &claim.queue)?;
        let now = now_ms();
        let mut got = Vec::new();
        while Self::wants(claim, &queue) > 0 {
            let Some(task) = queue.claim(&claim.owner, claim.lease_ms, claim.max_attempts, now)
            else {
                break;
            };
            if !claim.auto {
                claim.wanted -= 1;
            }
            got.push(task);
        }
        queue.settle(now);
        queue.save()?;
        if let Some(until) = queue.next_lease() {
            self.leases
                .at(self, until, &claim.queue, claim.at.me.clone());
        }
        for task in &got {
            self.task(&claim.at, port::TASK, task);
        }
        Ok(queue.take_reaped())
    }

    /// A lease of `queue` may have run out: reaps it through `me` if that claim still
    /// runs, else through every running claim of the queue, so that one waiting gets the
    /// task even if the node that held it is gone.
    pub(crate) fn expire(self: &Arc<Self>, me: &NodeRef, name: &str) {
        let mut claims = self.lock();
        let through: Vec<NodeRef> = if claims.contains_key(me) {
            vec![me.clone()]
        } else {
            claims
                .iter()
                .filter(|(_, c)| c.queue == name)
                .map(|(k, _)| k.clone())
                .collect()
        };
        for node in through {
            let reaped = (|| -> Result<(), ModuleError> {
                let mut queue = Queue::load(self.kv(&node)?, name)?;
                queue.settle(now_ms());
                self.close(&claims, &node, &mut queue)
            })();
            if let Err(error) = reaped {
                eprintln!("regulus: tasks: leases of `{name}`: {error}");
            }
        }
        self.fill(&mut claims, name);
    }

    fn start(self: &Arc<Self>, call: NodeCall) -> Result<Value, ModuleError> {
        if call.node.node_type != CLAIM {
            if call.node.node_type == QUEUE {
                self.queue_name(&call.node)?;
            }
            return Ok(Value::Null);
        }
        let at = At::of(&call);
        let node = &call.node;
        let owner = match self.text(node, "owner") {
            owner if owner.is_empty() => format!("{}/{}", at.me.execution_id, at.me.node_id),
            owner => owner,
        };
        let claim = Claim {
            queue: self.queue_name(node)?,
            owner,
            lease_ms: self.number(node, "lease_ms"),
            wait: self.text(node, "wait") == YES,
            auto: self.text(node, "auto") == YES,
            hold: self.number(node, "hold"),
            max_attempts: self.number(node, "max_attempts"),
            release: self.text(node, "on_stop") != "keep",
            wanted: 0,
            at,
        };
        let mut claims = self.lock();
        let queue = claim.queue.clone();
        let me = claim.at.me.clone();
        claims.insert(me.clone(), claim);
        // Leases taken before a restart still run out.
        if let Ok(kv) = self.kv(&me) {
            if let Some(until) = Queue::load(kv, &queue).ok().and_then(|q| q.next_lease()) {
                self.leases.at(self, until, &queue, me);
            }
        }
        self.fill(&mut claims, &queue);
        Ok(Value::Null)
    }

    fn stop(self: &Arc<Self>, call: NodeCall) -> Result<Value, ModuleError> {
        let me = At::of(&call).me;
        let mut claims = self.lock();
        let Some(claim) = claims.remove(&me) else {
            return Ok(Value::Null);
        };
        if claim.release {
            let released = (|| -> Result<usize, ModuleError> {
                let mut queue = Queue::load(self.kv(&me)?, &claim.queue)?;
                let released = queue.release(&claim.owner, now_ms()).len();
                self.close(&claims, &me, &mut queue)?;
                Ok(released)
            })();
            match released {
                Ok(0) => {}
                Ok(_) => self.fill(&mut claims, &claim.queue),
                Err(error) => eprintln!("regulus: tasks: releasing `{}`: {error}", claim.owner),
            }
        }
        Ok(Value::Null)
    }

    fn event(self: &Arc<Self>, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        let at = At::of(&call);
        match (call.node.node_type.as_str(), event.port.as_str()) {
            (QUEUE, port::ADD) => self.add(&at, &event.value),
            (QUEUE, port::LIST) => {
                let claims = self.lock();
                let mut queue = Queue::load(self.kv(&at.me)?, &self.queue_name(&at.node)?)?;
                queue.settle(now_ms());
                self.close(&claims, &at.me, &mut queue)?;
                self.emit(&at, port::TASKS, queue.snapshot());
                Ok(queue.counts())
            }
            (CLAIM, port::NEXT) => self.next(&at),
            (CLAIM, port::COMPLETE | port::FAIL) => self.finish(&at, &event),
            (LEDGER_NODE, port::READ) => {
                let entries = self.entries(&at.me, &at.node)?;
                let as_json = at.outputs.get(port::OUT).is_some_and(|c| c != TEXT);
                let text = self.render(&at.node, &entries);
                self.emit(
                    &at,
                    port::OUT,
                    if as_json { json!(entries) } else { json!(text) },
                );
                Ok(json!({ "entries": entries.len() }))
            }
            (LEDGER_NODE, kind) if KINDS.contains(&kind) => self.append(&at, kind, event.value),
            (kind, port) => Err(ModuleError::InvalidParams(format!(
                "node type `{kind}` takes nothing on `{port}`"
            ))),
        }
    }

    fn add(self: &Arc<Self>, at: &At, value: &Value) -> Result<Value, ModuleError> {
        let new = parse_new(value).map_err(|e| self.error(at, e))?;
        let name = self.queue_name(&at.node)?;
        let mut claims = self.lock();
        let mut queue = Queue::load(self.kv(&at.me)?, &name)?;
        let max = self.number(&at.node, "max_tasks");
        let added = queue
            .add(new, max, now_ms())
            .map_err(|e| self.error(at, e))?;
        self.close(&claims, &at.me, &mut queue)?;
        self.emit(at, port::ADDED, json!(added));
        self.fill(&mut claims, &name);
        Ok(json!({ "added": added.len() }))
    }

    fn next(self: &Arc<Self>, at: &At) -> Result<Value, ModuleError> {
        let mut claims = self.lock();
        let claim = claims
            .get_mut(&at.me)
            .ok_or_else(|| failed(format!("claim `{}` is not running", at.me.node_id)))?;
        claim.wanted += 1;
        let queue = claim.queue.clone();
        self.serve(claim)?;
        if claim.wanted > 0 && !claim.wait {
            claim.wanted -= 1;
            let counts = Queue::load(self.kv(&at.me)?, &queue)?.counts();
            self.emit(at, port::EMPTY, counts);
            return Ok(json!({ "claimed": false }));
        }
        Ok(json!({ "claimed": claim.wanted == 0 }))
    }

    fn finish(self: &Arc<Self>, at: &At, event: &NodeEvent) -> Result<Value, ModuleError> {
        let mut claims = self.lock();
        let claim = claims
            .get(&at.me)
            .cloned()
            .ok_or_else(|| failed(format!("claim `{}` is not running", at.me.node_id)))?;
        let mut queue = Queue::load(self.kv(&at.me)?, &claim.queue)?;
        let now = now_ms();
        queue.settle(now);
        let (port, task) = if event.port == port::COMPLETE {
            let task = queue
                .complete(&claim.owner, &event.value, now)
                .map_err(|e| self.error(at, e))?;
            (port::DONE, task)
        } else {
            let (task, for_good) = queue
                .fail(&claim.owner, &event.value, claim.max_attempts, now)
                .map_err(|e| self.error(at, e))?;
            (if for_good { port::FAILED } else { port::RETRY }, task)
        };
        self.close(&claims, &at.me, &mut queue)?;
        self.task(at, port, &task);
        if queue.drained() {
            self.emit(at, port::DRAINED, queue.counts());
        }
        self.fill(&mut claims, &claim.queue);
        Ok(json!({ "task": task.id, "state": task.state.name() }))
    }

    fn ledger_prefix(&self, me: &NodeRef, node: &Node) -> String {
        let scope = if self.text(node, "scope") == "node" {
            NODE_SCOPE
        } else {
            me.execution_id.as_str()
        };
        format!("ledger/{scope}/{}/", me.node_id)
    }

    fn entries(&self, me: &NodeRef, node: &Node) -> Result<Vec<Value>, ModuleError> {
        let _claims = self.lock();
        let found = self.kv(me)?.list(&self.ledger_prefix(me, node))?;
        Ok(found.into_iter().map(|(_, value)| value).collect())
    }

    fn append(&self, at: &At, kind: &str, value: Value) -> Result<Value, ModuleError> {
        let _claims = self.lock();
        let kv = self.kv(&at.me)?;
        let prefix = self.ledger_prefix(&at.me, &at.node);
        let count = kv.list(&prefix)?.len() as u64;
        let max = self.number(&at.node, "max_entries");
        if count >= max {
            return Err(self.error(
                at,
                format!("ledger `{}` is full (`max_entries` {max})", at.me.node_id),
            ));
        }
        let seq = count + 1;
        let entry = json!({ "seq": seq, "kind": kind, "value": value, "at": now_ms() });
        kv.set(&format!("{prefix}{seq:012}"), &entry)?;
        Ok(json!({ "seq": seq }))
    }

    /// The ledger as text: one section per kind, in `KINDS` order, entries in order.
    fn render(&self, node: &Node, entries: &[Value]) -> String {
        let headings = self.setting(node, "headings");
        let mut sections = Vec::new();
        for kind in KINDS {
            let lines: Vec<String> = entries
                .iter()
                .filter(|e| e["kind"] == kind)
                .map(|e| format!("- {}", line(&e["value"])))
                .collect();
            if lines.is_empty() {
                continue;
            }
            let heading = headings
                .and_then(|h| h.get(kind))
                .and_then(Value::as_str)
                .unwrap_or(kind);
            sections.push(format!("## {heading}\n{}", lines.join("\n")));
        }
        sections.join("\n\n")
    }

    fn describe(&self, call: &NodeCall) -> Result<Value, ModuleError> {
        serde_json::to_value(Hook {
            event: self.text(&call.node, "event"),
            matcher: None,
            timeout_ms: None,
            returns: vec![HookEffect::Context],
            fail: HookFailure::Open,
        })
        .map_err(|e| failed(e.to_string()))
    }

    /// On its event, the ledger as context; on any other, or empty, nothing.
    fn hook(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let request: HookRequest = parse(std::mem::take(&mut call.args))?;
        let at = At::of(&call);
        let mut answer = HookAnswer::Nothing;
        if request.event == self.text(&call.node, "event") {
            let text = self.render(&call.node, &self.entries(&at.me, &call.node)?);
            if !text.is_empty() {
                answer = HookAnswer::Context { text };
            }
        }
        serde_json::to_value(answer).map_err(|e| failed(e.to_string()))
    }
}

/// One ledger entry as a line: text as is; a task as its title, state and result; any
/// other value as JSON.
fn line(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Object(map) if map.contains_key("title") => {
            let mut line = map["title"].as_str().unwrap_or_default().to_string();
            if let Some(state) = map.get("state").and_then(Value::as_str) {
                line.push_str(&format!(" [{state}]"));
            }
            match map.get("result") {
                Some(Value::String(result)) => line.push_str(&format!(": {result}")),
                Some(Value::Null) | None => {}
                Some(result) => line.push_str(&format!(": {result}")),
            }
            line
        }
        other => other.to_string(),
    }
}

#[derive(Deserialize)]
struct Target {
    execution_id: String,
    node_id: String,
    #[serde(default)]
    queue: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

impl Target {
    fn me(&self) -> NodeRef {
        NodeRef {
            execution_id: self.execution_id.clone(),
            node_id: self.node_id.clone(),
        }
    }
}

pub struct Tasks {
    shared: Arc<Shared>,
}

impl Tasks {
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                ctx: OnceLock::new(),
                has_store: OnceLock::new(),
                memory: Arc::default(),
                claims: Mutex::default(),
                leases: Leases::default(),
                manifest: parse_manifest(MANIFEST).expect("tasks manifest is valid"),
            }),
        }
    }

    fn list(&self, target: Target) -> Result<Value, ModuleError> {
        let shared = &self.shared;
        let claims = shared.lock();
        let name = target.queue.clone().unwrap_or_else(|| DEFAULT_QUEUE.into());
        let mut queue = Queue::load(shared.kv(&target.me())?, &name)?;
        queue.settle(now_ms());
        shared.close(&claims, &target.me(), &mut queue)?;
        Ok(queue.snapshot())
    }

    fn ledger(&self, target: Target) -> Result<Value, ModuleError> {
        let me = target.me();
        let mut config = serde_json::Map::new();
        if let Some(scope) = target.scope {
            config.insert("scope".into(), json!(scope));
        }
        let node: Node = parse(json!({ "id": me.node_id, "type": LEDGER_NODE, "config": config }))?;
        let entries = self.shared.entries(&me, &node)?;
        let text = self.shared.render(&node, &entries);
        Ok(json!({ "entries": entries, "text": text }))
    }
}

impl Default for Tasks {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Tasks {
    fn manifest(&self) -> &Manifest {
        &self.shared.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        let _ = self.shared.ctx.set(ctx.clone());
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            method::NODE_START => self.shared.start(parse(params)?),
            method::NODE_STOP => self.shared.stop(parse(params)?),
            method::NODE_EVENT => self.shared.event(parse(params)?),
            HOOK_DESCRIBE => self.shared.describe(&parse(params)?),
            method::NODE_HOOK => self.shared.hook(parse(params)?),
            LIST => self.list(parse(params)?),
            LEDGER => self.ledger(parse(params)?),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.shared.leases.shutdown();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_ledger_line_shows_text_tasks_and_other_values() {
        assert_eq!(line(&json!("plain")), "plain");
        assert_eq!(
            line(&json!({ "title": "Build", "state": "done", "result": "ok" })),
            "Build [done]: ok"
        );
        assert_eq!(line(&json!({ "a": 1 })), "{\"a\":1}");
    }

    #[test]
    fn the_manifest_declares_three_nodes_and_two_methods() {
        let tasks = Tasks::new();
        let manifest = tasks.manifest();
        for kind in [QUEUE, CLAIM, LEDGER_NODE] {
            assert!(manifest.node_type(kind).is_some(), "{kind}");
        }
        assert_eq!(manifest.module.methods, [LIST, LEDGER]);
    }
}
