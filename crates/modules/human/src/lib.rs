//! Human in the loop (docs/13-blocks.md §3.3): **Approval** and **Review**. A value that
//! reaches `in` stops at the node under an id until a person decides, by id, through
//! `human.answer` (`regulus human answer`) or a value on `decide`; or until `timeout_ms`.
//!
//! Each pending decision is a row in the module's store (the node's project database), so
//! it outlives the core: the node checkpoints when something waits, `[store] resume =
//! checkpoint` resumes the execution, and `node/start` reloads its rows. Deadlines are
//! wall clock and run on one timer thread: nothing here waits on the bus thread.

mod timer;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, Node, NodeCall, NodeEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use timer::Timers;

const MANIFEST: &str = include_str!("../regulus-module.toml");

/// Decisions waiting in live executions: `{ execution_id? }` → `[Pending]`, oldest first.
pub const LIST: &str = "human.list";
/// One pending decision in full: `{ id }` → `Pending`.
pub const GET: &str = "human.get";
/// Decides one: [`Answer`] → `{ id, decision, execution_id, node_id, port }`.
pub const ANSWER: &str = "human.answer";

const APPROVAL: &str = "approval";
const REVIEW: &str = "review";
/// Where pending rows live in the store: `pending/<execution>/<id>`.
const ROW: &str = "pending/";
const YES: &str = "yes";

mod port {
    pub const IN: &str = "in";
    pub const DECIDE: &str = "decide";
    pub const PENDING: &str = "pending";
    pub const APPROVED: &str = "approved";
    pub const OUT: &str = "out";
    pub const REJECTED: &str = "rejected";
    pub const TIMEOUT: &str = "timeout";
    pub const DECISION: &str = "decision";
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

/// A fresh id: `h-` and 48 random bits from the operating system, in hex.
fn fresh_id() -> Result<String, ModuleError> {
    let mut bytes = [0u8; 6];
    getrandom::fill(&mut bytes).map_err(|e| ModuleError::Failed(e.to_string()))?;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!("h-{hex}"))
}

/// A decision waiting for a person. Also the row in the store.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Pending {
    pub id: String,
    /// `approval` or `review`.
    pub kind: String,
    pub execution_id: String,
    pub node_id: String,
    /// What waits: it leaves unchanged (or edited, for a review) once decided.
    pub value: Value,
    pub prompt: String,
    /// The JSON schema of the fields an approval fills in, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub form: Option<Value>,
    /// Milliseconds since the Unix epoch.
    pub created_ms: u64,
    /// When it times out, in the same clock; none: never.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deadline_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    #[serde(alias = "approved", alias = "yes")]
    Approve,
    #[serde(alias = "rejected", alias = "no")]
    Reject,
}

/// A person's decision on a pending id.
#[derive(Debug, Clone, Deserialize)]
pub struct Answer {
    pub id: String,
    pub decision: Decision,
    /// The form's fields. Text is read as the type the form gives the field.
    #[serde(default)]
    pub fields: Map<String, Value>,
    /// An approval's comment; a review's edited text.
    #[serde(default)]
    pub text: Option<String>,
}

#[derive(Deserialize)]
struct IdParams {
    id: String,
}

#[derive(Default, Deserialize)]
struct ListParams {
    #[serde(default)]
    execution_id: Option<String>,
}

type Key = (String, String);

fn key(node: &NodeRef) -> Key {
    (node.execution_id.clone(), node.node_id.clone())
}

#[derive(Default)]
struct State {
    /// Every started node, with its configuration.
    nodes: HashMap<Key, Node>,
    /// Every pending decision of a started node, by id.
    pending: HashMap<String, Pending>,
}

struct Inner {
    manifest: Manifest,
    ctx: ModuleCtx,
    state: Mutex<State>,
    timers: Timers,
}

pub struct Human {
    inner: Arc<Inner>,
}

/// Which outputs carry the value: they share the capability of `in`.
fn carried(node_type: &str) -> &'static [&'static str] {
    match node_type {
        APPROVAL => &[port::APPROVED, port::REJECTED, port::TIMEOUT],
        REVIEW => &[port::OUT, port::REJECTED, port::TIMEOUT],
        _ => &[],
    }
}

fn same_capability(node: &Node) -> Result<(), ModuleError> {
    let Some(input) = node.port(port::IN) else {
        return Ok(());
    };
    for id in carried(&node.node_type) {
        if let Some(port) = node.port(id).filter(|p| p.capability != input.capability) {
            return Err(ModuleError::InvalidParams(format!(
                "node `{}`: `{id}` is {} but `in` is {}; what waits leaves as it came",
                node.id, port.capability, input.capability
            )));
        }
    }
    Ok(())
}

/// Reads `raw`, text from a CLI, as the type the form's schema gives `field`.
fn coerce(field: &str, raw: Value, schema: Option<&Value>) -> Result<Value, String> {
    let Value::String(text) = &raw else {
        return Ok(raw);
    };
    let kind = schema
        .and_then(|s| s.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("string");
    let wrong = || format!("field `{field}` must be {kind}, got `{text}`");
    match kind {
        "number" => text
            .trim()
            .parse::<f64>()
            .ok()
            .and_then(|n| serde_json::Number::from_f64(n).map(Value::Number))
            .ok_or_else(wrong),
        "integer" => text
            .trim()
            .parse::<i64>()
            .map(Value::from)
            .map_err(|_| wrong()),
        "boolean" => match text.trim() {
            "true" | "yes" => Ok(Value::Bool(true)),
            "false" | "no" => Ok(Value::Bool(false)),
            _ => Err(wrong()),
        },
        "object" | "array" => serde_json::from_str(text).map_err(|_| wrong()),
        _ => Ok(raw),
    }
}

/// Checks `fields` against the form and returns them typed: what `required` names must be
/// there, and nothing outside `properties` if `additionalProperties` is false.
fn fill(form: &Value, fields: Map<String, Value>) -> Result<Map<String, Value>, String> {
    let properties = form.get("properties").and_then(Value::as_object);
    let closed = form.get("additionalProperties") == Some(&Value::Bool(false));
    let mut out = Map::new();
    for (name, raw) in fields {
        let schema = properties.and_then(|p| p.get(&name));
        if schema.is_none() && closed {
            return Err(format!("the form has no field `{name}`"));
        }
        out.insert(name.clone(), coerce(&name, raw, schema)?);
    }
    let missing: Vec<&str> = form
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter(|name| !out.contains_key(*name))
        .collect();
    if !missing.is_empty() {
        return Err(format!("the form needs {}", missing.join(", ")));
    }
    Ok(out)
}

impl Inner {
    fn text(&self, node: &Node, field: &str) -> String {
        self.manifest
            .setting(node, field)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    }

    fn number(&self, node: &Node, field: &str) -> u64 {
        self.manifest
            .setting(node, field)
            .and_then(Value::as_f64)
            .map_or(0, |n| n.max(0.0) as u64)
    }

    fn form(&self, node: &Node) -> Result<Option<Value>, ModuleError> {
        if node.node_type != APPROVAL {
            return Ok(None);
        }
        let form = match self.manifest.setting(node, "form") {
            None | Some(Value::Null) => return Ok(None),
            Some(Value::String(text)) if text.trim().is_empty() => return Ok(None),
            Some(Value::String(text)) => serde_json::from_str(text).map_err(|e| {
                ModuleError::InvalidParams(format!("node `{}`: `form` is not JSON: {e}", node.id))
            })?,
            Some(other) => other.clone(),
        };
        if !form.is_object() {
            return Err(ModuleError::InvalidParams(format!(
                "node `{}`: `form` must be a JSON schema object",
                node.id
            )));
        }
        Ok(Some(form))
    }

    fn row(pending: &Pending) -> String {
        format!("{ROW}{}/{}", pending.execution_id, pending.id)
    }

    /// Saves or deletes the row, then checkpoints how many wait so a restart resumes the
    /// execution. A core without a store keeps them in memory only.
    fn persist(&self, me: &NodeRef, pending: &Pending, keep: bool) {
        if let Ok(store) = self.ctx.store(Some(me)) {
            let saved = if keep {
                store.set_json(&Self::row(pending), pending)
            } else {
                store.delete(&Self::row(pending)).map(|_| ())
            };
            if let Err(error) = saved {
                eprintln!("regulus: human: `{}`: {error}", pending.id);
            }
        }
        let waiting = lock(&self.state)
            .pending
            .values()
            .filter(|p| p.execution_id == me.execution_id && p.node_id == me.node_id)
            .count();
        let _ = self.ctx.checkpoint(me, json!({ "pending": waiting }));
    }

    fn arm(self: &Arc<Self>, pending: &Pending) {
        let Some(deadline) = pending.deadline_ms else {
            return;
        };
        let inner = Arc::downgrade(self);
        let id = pending.id.clone();
        let delay = Duration::from_millis(deadline.saturating_sub(now_ms()));
        self.timers.after(delay, move || {
            if let Some(inner) = inner.upgrade() {
                inner.expire(&id);
            }
        });
    }

    fn start(self: &Arc<Self>, call: NodeCall) -> Result<Value, ModuleError> {
        same_capability(&call.node)?;
        self.form(&call.node)?;
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        // Rows of this execution left by a core that went down: a resume brings them back.
        let rows: Vec<Pending> = match self.ctx.store(Some(&me)) {
            Ok(store) => store
                .list(&format!("{ROW}{}/", me.execution_id))?
                .into_iter()
                .filter_map(|(_, bytes)| serde_json::from_slice::<Pending>(&bytes).ok())
                .filter(|p| p.node_id == me.node_id)
                .collect(),
            Err(_) => Vec::new(),
        };
        {
            let mut state = lock(&self.state);
            state.nodes.insert(key(&me), call.node.clone());
            for row in &rows {
                state.pending.insert(row.id.clone(), row.clone());
            }
        }
        for row in &rows {
            self.arm(row);
        }
        Ok(json!({ "pending": rows.len() }))
    }

    fn stop(&self, call: NodeCall) {
        // The rows stay: a core shutting down stops nodes too, and the next one resumes.
        let mut state = lock(&self.state);
        let node = (call.execution_id, call.node.id);
        state
            .pending
            .retain(|_, p| (&p.execution_id, &p.node_id) != (&node.0, &node.1));
        state.nodes.remove(&node);
    }

    fn event(self: &Arc<Self>, call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(call.args.clone())?;
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        match event.port.as_str() {
            port::IN => self.wait(&call.node, me, event.value),
            port::DECIDE => {
                let value = match event.value {
                    Value::String(text) => serde_json::from_str(&text).map_err(|e| {
                        ModuleError::InvalidParams(format!("`decide` is not JSON: {e}"))
                    })?,
                    other => other,
                };
                let answer: Answer = parse(value)?;
                self.answer(answer, Some(&me))
            }
            other => Err(ModuleError::InvalidParams(format!(
                "node `{}` takes no values on `{other}`",
                call.node.id
            ))),
        }
    }

    fn wait(
        self: &Arc<Self>,
        node: &Node,
        me: NodeRef,
        value: Value,
    ) -> Result<Value, ModuleError> {
        let timeout = self.number(node, "timeout_ms");
        let created = now_ms();
        let pending = Pending {
            id: fresh_id()?,
            kind: node.node_type.clone(),
            execution_id: me.execution_id.clone(),
            node_id: me.node_id.clone(),
            value,
            prompt: self.text(node, "prompt"),
            form: self.form(node)?,
            created_ms: created,
            deadline_ms: (timeout > 0).then(|| created.saturating_add(timeout)),
        };
        lock(&self.state)
            .pending
            .insert(pending.id.clone(), pending.clone());
        self.persist(&me, &pending, true);
        self.arm(&pending);
        if node.port(port::PENDING).is_some() {
            let notice = json!({
                "id": pending.id, "kind": pending.kind, "prompt": pending.prompt,
                "form": pending.form, "value": pending.value, "deadline_ms": pending.deadline_ms,
            });
            self.ctx.emit(&me, port::PENDING, notice)?;
        }
        Ok(json!({ "id": pending.id }))
    }

    /// Decides `answer.id`. With `only`, the decision must belong to that node. A wrong
    /// form leaves it pending.
    fn answer(&self, answer: Answer, only: Option<&NodeRef>) -> Result<Value, ModuleError> {
        let (pending, node, fields) = {
            let mut state = lock(&self.state);
            let pending = state
                .pending
                .get(&answer.id)
                .filter(|p| {
                    only.is_none_or(|n| n.execution_id == p.execution_id && n.node_id == p.node_id)
                })
                .cloned()
                .ok_or_else(|| {
                    ModuleError::InvalidParams(format!(
                        "no pending decision `{}` in a running execution",
                        answer.id
                    ))
                })?;
            let node = state
                .nodes
                .get(&(pending.execution_id.clone(), pending.node_id.clone()))
                .cloned()
                .ok_or_else(|| {
                    ModuleError::Failed(format!("node `{}` is not running", pending.node_id))
                })?;
            let fields = match (&pending.form, answer.decision) {
                (Some(form), Decision::Approve) => {
                    fill(form, answer.fields.clone()).map_err(ModuleError::InvalidParams)?
                }
                _ => answer.fields.clone(),
            };
            state.pending.remove(&answer.id);
            (pending, node, fields)
        };
        let me = NodeRef {
            execution_id: pending.execution_id.clone(),
            node_id: pending.node_id.clone(),
        };
        self.persist(&me, &pending, false);
        let merge = self.text(&node, "merge_fields") == YES;
        let (port, value) = match (pending.kind.as_str(), answer.decision) {
            (REVIEW, Decision::Approve) => (
                port::OUT,
                answer
                    .text
                    .clone()
                    .map_or(pending.value.clone(), Value::String),
            ),
            (_, Decision::Approve) if merge => {
                let mut extra = fields.clone();
                if let Some(text) = &answer.text {
                    extra.insert("comment".into(), Value::String(text.clone()));
                }
                let value = match pending.value.clone() {
                    Value::Object(mut object) => {
                        object.extend(extra);
                        Value::Object(object)
                    }
                    other => {
                        extra.insert("value".into(), other);
                        Value::Object(extra)
                    }
                };
                (port::APPROVED, value)
            }
            (_, Decision::Approve) => (port::APPROVED, pending.value.clone()),
            (_, Decision::Reject) => (port::REJECTED, pending.value.clone()),
        };
        self.ctx.emit(&me, port, value)?;
        let decision = match answer.decision {
            Decision::Approve => "approve",
            Decision::Reject => "reject",
        };
        if node.port(port::DECISION).is_some() {
            let record = json!({ "id": pending.id, "decision": decision, "fields": fields,
                                 "text": answer.text, "at": now_ms() });
            self.ctx.emit(&me, port::DECISION, record)?;
        }
        Ok(
            json!({ "id": pending.id, "decision": decision, "execution_id": me.execution_id,
                   "node_id": me.node_id, "port": port }),
        )
    }

    /// Nobody decided in time: the value leaves on `timeout`, or `rejected` without it.
    fn expire(&self, id: &str) {
        let (pending, node) = {
            let mut state = lock(&self.state);
            let Some(pending) = state.pending.get(id).cloned() else {
                return;
            };
            if pending.deadline_ms.is_none_or(|d| d > now_ms()) {
                return;
            }
            let Some(node) = state
                .nodes
                .get(&(pending.execution_id.clone(), pending.node_id.clone()))
                .cloned()
            else {
                return;
            };
            state.pending.remove(id);
            (pending, node)
        };
        let me = NodeRef {
            execution_id: pending.execution_id.clone(),
            node_id: pending.node_id.clone(),
        };
        self.persist(&me, &pending, false);
        let port = if node.port(port::TIMEOUT).is_some() {
            port::TIMEOUT
        } else {
            port::REJECTED
        };
        if let Err(error) = self.ctx.emit(&me, port, pending.value) {
            eprintln!("regulus: human: `{id}` timed out: {error}");
        }
    }

    fn list(&self, params: ListParams) -> Vec<Pending> {
        let mut list: Vec<Pending> = lock(&self.state)
            .pending
            .values()
            .filter(|p| {
                params
                    .execution_id
                    .as_deref()
                    .is_none_or(|e| e == p.execution_id)
            })
            .cloned()
            .collect();
        list.sort_by(|a, b| (a.created_ms, &a.id).cmp(&(b.created_ms, &b.id)));
        list
    }
}

impl Human {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                manifest: parse_manifest(MANIFEST).expect("human manifest is valid"),
                ctx: ModuleCtx::default(),
                state: Mutex::default(),
                timers: Timers::default(),
            }),
        }
    }
}

impl Default for Human {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Human {
    fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.inner = Arc::new(Inner {
            manifest: self.inner.manifest.clone(),
            ctx: ctx.clone(),
            state: Mutex::default(),
            timers: Timers::default(),
        });
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let inner = &self.inner;
        match name {
            method::NODE_START => inner.start(parse(params)?),
            method::NODE_STOP => {
                inner.stop(parse(params)?);
                Ok(Value::Null)
            }
            method::NODE_EVENT => inner.event(parse(params)?),
            LIST => {
                let params = if params.is_null() {
                    ListParams::default()
                } else {
                    parse(params)?
                };
                Ok(json!(inner.list(params)))
            }
            GET => {
                let params: IdParams = parse(params)?;
                let found = lock(&inner.state).pending.get(&params.id).cloned();
                found.map(|p| json!(p)).ok_or_else(|| {
                    ModuleError::InvalidParams(format!("no pending decision `{}`", params.id))
                })
            }
            ANSWER => inner.answer(parse(params)?, None),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.inner.timers.stop();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_the_nodes_and_methods() {
        let human = Human::new();
        for kind in [APPROVAL, REVIEW] {
            assert!(human.manifest().node_type(kind).is_some(), "{kind}");
        }
        assert_eq!(human.manifest().module.methods, [LIST, GET, ANSWER]);
    }

    #[test]
    fn a_form_types_cli_text_and_names_what_is_missing() {
        let form = json!({ "type": "object",
            "properties": { "tag": { "type": "string" }, "replicas": { "type": "integer" },
                            "canary": { "type": "boolean" } },
            "required": ["tag", "replicas"], "additionalProperties": false });
        let mut fields = Map::new();
        fields.insert("tag".into(), json!("v2"));
        fields.insert("replicas".into(), json!("3"));
        fields.insert("canary".into(), json!("yes"));
        let filled = fill(&form, fields).unwrap();
        assert_eq!(filled["replicas"], json!(3));
        assert_eq!(filled["canary"], json!(true));

        let mut short = Map::new();
        short.insert("tag".into(), json!("v2"));
        assert!(fill(&form, short).unwrap_err().contains("replicas"));
        let mut extra = Map::new();
        extra.insert("nope".into(), json!("x"));
        assert!(fill(&form, extra).unwrap_err().contains("nope"));
        let mut bad = Map::new();
        bad.insert("tag".into(), json!("v2"));
        bad.insert("replicas".into(), json!("three"));
        assert!(fill(&form, bad).unwrap_err().contains("integer"));
    }

    #[test]
    fn ports_that_carry_the_value_share_its_capability() {
        let node: Node = serde_json::from_value(json!({ "id": "a", "type": "approval", "ports": [
            { "id": "in", "capability": "text@1", "direction": "input", "kind": "then" },
            { "id": "approved", "capability": "json@1", "direction": "output", "kind": "then" },
        ]}))
        .unwrap();
        assert!(same_capability(&node).is_err());
    }
}
