//! Data (docs/16-experiments.md): the generic pieces behind the RAG lab and the topic
//! watch. `collect` keeps a bounded, keyed dataset; `snapshot` compares the current state
//! of a source with the last one it stored (added, changed, removed); `rank` orders keyed
//! metrics. Each node takes values on the bus, offers `tool@1` to agents and answers the
//! module's methods (`data.list`, `data.inspect`, `data.export`) for the CLI and the app.
//!
//! State is one JSON row per dataset or snapshot, in the module's space of the store: the
//! node's project database, or the global one outside a project. Rows are read and
//! written whole under the module's write lock. Every bound is a field of the node.

mod collect;
mod json;
mod rank;
mod snapshot;
mod state;

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex};

use regulus_contract_tool as tool;
use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Direction, Manifest, Node, NodeCall, NodeEvent, WireKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use state::{Kind, Kv, Row};

const MANIFEST: &str = include_str!("../regulus-module.toml");
/// `{ execution_id, node_id }` of a live node → every dataset and snapshot of its project.
pub const LIST: &str = "data.list";
/// `{ execution_id, node_id, limit? }` → the node's own dataset or snapshot, summarised.
pub const INSPECT: &str = "data.inspect";
/// `{ execution_id, node_id }` → the whole content.
pub const EXPORT: &str = "data.export";

/// What the core says when it was built without a store.
const NO_STORE: &str = "this core has no store";
const COLLECT: &str = "collect";
const SNAPSHOT: &str = "snapshot";
const RANK: &str = "rank";

const ERROR: &str = "error";
/// The `scope` every execution of the project shares.
const PROJECT: &str = "project";
/// Bounds a node falls back on when its field is 0: nothing is ever unbounded.
const DEFAULT_MAX_ITEMS: usize = 1000;
const DEFAULT_MAX_BYTES: usize = 1 << 20;
const DEFAULT_MAX_RECORD_BYTES: usize = 64 << 10;
/// How many rejected records a summary lists.
const REJECTED_LISTED: usize = 20;

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn to_json(value: impl Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(|e| ModuleError::Failed(e.to_string()))
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

pub(crate) fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

type Key = (String, String);

/// What bounds one dataset or snapshot.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max_items: usize,
    /// Keys and records together, as compact JSON.
    pub max_bytes: usize,
    pub max_record_bytes: usize,
}

/// A record the module would not keep, and why.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct Rejected {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub reason: String,
}

pub struct Data {
    manifest: Manifest,
    ctx: ModuleCtx,
    /// Rows when the core has no store.
    local: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    /// Held across a read-compare-write of a row, so concurrent inputs apply one by one.
    writing: Mutex<()>,
    /// Live nodes, for the module's methods.
    live: Mutex<HashMap<Key, Node>>,
}

/// One node of one execution, with the settings the manifest defaults.
pub(crate) struct Here<'a> {
    data: &'a Data,
    pub(crate) node: Node,
    pub(crate) me: NodeRef,
}

impl Here<'_> {
    fn setting(&self, field: &str) -> Option<&Value> {
        self.data.manifest.setting(&self.node, field)
    }

    pub(crate) fn text(&self, field: &str) -> String {
        match self.setting(field) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    pub(crate) fn number(&self, field: &str) -> f64 {
        match self.setting(field) {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// A count; 0 (or less) is the `default`, so a bound is never switched off.
    pub(crate) fn bound(&self, field: &str, default: usize) -> usize {
        match self.number(field) {
            n if n >= 1.0 => n as usize,
            _ => default,
        }
    }

    pub(crate) fn map(&self, field: &str) -> BTreeMap<String, String> {
        match self.setting(field) {
            Some(Value::Object(map)) => map
                .iter()
                .map(|(k, v)| {
                    (
                        k.clone(),
                        v.as_str().map_or_else(|| v.to_string(), String::from),
                    )
                })
                .collect(),
            _ => BTreeMap::new(),
        }
    }

    pub(crate) fn limits(&self) -> Limits {
        Limits {
            max_items: self.bound("max_items", DEFAULT_MAX_ITEMS),
            max_bytes: self.bound("max_bytes", DEFAULT_MAX_BYTES),
            max_record_bytes: self.bound("max_record_bytes", DEFAULT_MAX_RECORD_BYTES),
        }
    }

    fn has_output(&self, port: &str) -> bool {
        self.node
            .port(port)
            .is_some_and(|p| p.kind == WireKind::Then && p.direction == Direction::Output)
    }

    /// Puts `value` on `port` if the node declares it.
    pub(crate) fn emit(&self, port: &str, value: Value) -> Result<(), ModuleError> {
        if !self.has_output(port) {
            return Ok(());
        }
        self.data.ctx.emit(&self.me, port, value)
    }

    /// `message` about this node, on `error` if it has one, and as the call's error.
    pub(crate) fn fail(&self, message: impl std::fmt::Display) -> ModuleError {
        let message = format!("{} `{}`: {message}", self.node.node_type, self.node.id);
        let _ = self.emit(ERROR, json!(message));
        ModuleError::Failed(message)
    }

    /// The node's store. Only a core that has none at all falls back to the process's own
    /// memory (as in some tests); any other failure is an error, never a quiet detour to
    /// state that is shared across projects and lost at a restart.
    pub(crate) fn kv(&self) -> Result<Kv, String> {
        self.data.open(Some(&self.me)).map_err(|e| e.to_string())
    }

    /// `project`, or this execution.
    pub(crate) fn scope(&self) -> String {
        if self.text("scope") == PROJECT {
            PROJECT.to_string()
        } else {
            self.me.execution_id.clone()
        }
    }

    /// The row this node keeps: its dataset or snapshot, in its scope.
    pub(crate) fn row(&self, kind: Kind) -> Result<Row, String> {
        let name = self.text(kind.name());
        Row::new(self.kv()?, kind, &self.scope(), name.trim())
    }

    /// Runs `work` holding the write lock.
    pub(crate) fn writing<T>(&self, work: impl FnOnce() -> T) -> T {
        let _writing = lock(&self.data.writing);
        work()
    }
}

/// What a module method asks about. A node (`execution_id`, `node_id`) opens its
/// project's store; it may have stopped, or run before the core restarted. `project`
/// opens that project's store with nothing of it running, and `global` the global one
/// (profiles outside any project). A node that is not running no
/// longer says which dataset or snapshot it kept, so `kind` and `name` (and `scope`, if not
/// `project`) are named.
#[derive(Deserialize)]
struct Target {
    #[serde(default)]
    execution_id: Option<String>,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    global: bool,
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
}

impl Data {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("data manifest is valid"),
            ctx: ModuleCtx::default(),
            local: Arc::default(),
            writing: Mutex::default(),
            live: Mutex::default(),
        }
    }

    fn here(&self, call: NodeCall) -> Here<'_> {
        Here {
            data: self,
            me: NodeRef {
                execution_id: call.execution_id,
                node_id: call.node.id.clone(),
            },
            node: call.node,
        }
    }

    /// The store behind `node` (its project's), or the global one without a node.
    fn open(&self, node: Option<&NodeRef>) -> Result<Kv, ModuleError> {
        match self.ctx.store(node) {
            Ok(store) => Ok(Kv::Store(store)),
            Err(ModuleError::Failed(reason)) if reason == NO_STORE => {
                Ok(Kv::Local(self.local.clone()))
            }
            Err(other) => Err(other),
        }
    }

    /// The node a method names, if it is running: its settings say what it keeps.
    fn running(&self, target: &Target) -> Option<Here<'_>> {
        let (execution_id, node_id) = (target.execution_id.clone()?, target.node_id.clone()?);
        let node = lock(&self.live)
            .get(&(execution_id.clone(), node_id.clone()))
            .cloned()?;
        Some(Here {
            data: self,
            me: NodeRef {
                execution_id,
                node_id,
            },
            node,
        })
    }

    /// The store a method reads, by whichever way the target names it.
    fn store_of(&self, target: &Target) -> Result<Kv, ModuleError> {
        if let Some(project) = &target.project {
            return match self.ctx.project_store(project) {
                Ok(store) => Ok(Kv::Store(store)),
                Err(ModuleError::Failed(reason)) if reason == NO_STORE => {
                    Ok(Kv::Local(self.local.clone()))
                }
                Err(other) => Err(other),
            };
        }
        if target.global {
            return self.open(None);
        }
        match (&target.execution_id, &target.node_id) {
            (Some(execution_id), Some(node_id)) => self.open(Some(&NodeRef {
                execution_id: execution_id.clone(),
                node_id: node_id.clone(),
            })),
            _ => Err(ModuleError::InvalidParams(
                "name a node (`execution_id` and `node_id`), a `project` or `global: true`".into(),
            )),
        }
    }

    fn start(&self, call: NodeCall) -> Value {
        let key = (call.execution_id.clone(), call.node.id.clone());
        lock(&self.live).insert(key, call.node);
        Value::Null
    }

    fn stop(&self, call: NodeCall) {
        lock(&self.live).remove(&(call.execution_id, call.node.id));
    }

    fn event(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        let here = self.here(call);
        match (here.node.node_type.as_str(), event.port.as_str()) {
            (COLLECT, "flush") => collect::flush(&here),
            (COLLECT, "clear") => collect::clear(&here),
            // Every other input carries data: `in` (json@1), `text` (text@1).
            (COLLECT, _) => collect::add(&here, &event.value),
            (SNAPSHOT, _) => snapshot::compare(&here, &event.value),
            (RANK, _) => rank::rank(&here, &event.value),
            (kind, _) => Err(ModuleError::InvalidParams(format!(
                "`{kind}` is not a node of this module"
            ))),
        }
    }

    fn tool_list(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let here = self.here(call);
        to_json(match here.node.node_type.as_str() {
            COLLECT => collect::tool_specs(&here),
            SNAPSHOT => snapshot::tool_specs(&here),
            RANK => rank::tool_specs(&here),
            other => return Err(ModuleError::InvalidParams(format!("no tools on `{other}`"))),
        })
    }

    fn tool_call(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let request: tool::Call = parse(std::mem::take(&mut call.args))?;
        let here = self.here(call);
        let data = match here.node.node_type.as_str() {
            COLLECT => collect::run_tool(&here, &request),
            SNAPSHOT => snapshot::run_tool(&here, &request),
            RANK => rank::run_tool(&here, &request),
            other => Err(format!("no tools on `{other}`")),
        };
        // What went wrong is an answer for the agent to read, not a broken call.
        Ok(match data {
            Ok(data) => to_json(tool::Outcome { data, error: false })?,
            Err(e) => to_json(tool::Outcome {
                data: Value::String(e),
                error: true,
            })?,
        })
    }

    /// `data.list`: every dataset and snapshot of the target's project.
    fn list(&self, params: Value) -> Result<Value, ModuleError> {
        let target: Target = parse(params)?;
        let mut listed = Vec::new();
        for found in state::rows(&self.store_of(&target)?)? {
            let summary = match found.kind {
                Kind::Dataset => collect::summary(&found.bytes),
                Kind::Snapshot => snapshot::summary(&found.bytes),
            };
            listed.push(json!({ "kind": found.kind.name(), "scope": found.scope,
                                "name": found.name, "state": summary }));
        }
        Ok(json!(listed))
    }

    /// `data.inspect` and `data.export`: the running node's own row, or the one `kind`,
    /// `name` and `scope` name.
    fn show(&self, params: Value, whole: bool) -> Result<Value, ModuleError> {
        let target: Target = parse(params)?;
        let here = self.running(&target);
        let kind = match (target.kind.as_deref(), &here) {
            (Some(kind), _) => Kind::parse(kind).ok_or_else(|| {
                ModuleError::InvalidParams(format!("`{kind}` is not a kind: dataset or snapshot"))
            })?,
            (None, Some(here)) => match here.node.node_type.as_str() {
                COLLECT => Kind::Dataset,
                SNAPSHOT => Kind::Snapshot,
                other => {
                    return Err(ModuleError::InvalidParams(format!(
                        "a `{other}` node keeps nothing: name a `kind` to look at one"
                    )))
                }
            },
            (None, None) => {
                return Err(ModuleError::InvalidParams(
                    "that node is not running, so name the `kind` (dataset or snapshot) and \
                     the `name` to look at"
                        .into(),
                ))
            }
        };
        let name = match (&target.name, &here) {
            (Some(name), _) => name.clone(),
            (None, Some(here)) => here.text(kind.name()),
            (None, None) => {
                return Err(ModuleError::InvalidParams(format!(
                    "that node is not running, so name the {} (`name`)",
                    kind.name()
                )))
            }
        };
        let scope = match (&target.scope, &here) {
            (Some(scope), _) => scope.clone(),
            (None, Some(here)) => here.scope(),
            (None, None) => PROJECT.to_string(),
        };
        let row = Row::new(self.store_of(&target)?, kind, &scope, name.trim())
            .map_err(ModuleError::InvalidParams)?;
        let limit = match (target.limit, &here) {
            (_, _) if whole => usize::MAX,
            (Some(limit), _) => limit,
            (None, Some(here)) => here.bound("max_results", 20),
            (None, None) => 20,
        };
        let shown = match kind {
            Kind::Dataset => collect::show(&row, &name, &scope, limit),
            Kind::Snapshot => snapshot::show(&row, &name, &scope, limit),
        };
        shown.map_err(ModuleError::Failed)
    }
}

impl Default for Data {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Data {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            method::NODE_START => Ok(self.start(parse(params)?)),
            method::NODE_STOP => {
                self.stop(parse(params)?);
                Ok(Value::Null)
            }
            method::NODE_EVENT => self.event(parse(params)?),
            tool::LIST => self.tool_list(parse(params)?),
            tool::CALL => self.tool_call(parse(params)?),
            LIST => self.list(params),
            INSPECT => self.show(params, false),
            EXPORT => self.show(params, true),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_the_nodes_and_its_methods() {
        let data = Data::new();
        for kind in [COLLECT, SNAPSHOT, RANK] {
            assert!(data.manifest().node_type(kind).is_some(), "{kind}");
        }
        assert_eq!(data.manifest().module.methods, [LIST, INSPECT, EXPORT]);
    }

    #[test]
    fn a_bound_of_zero_is_its_default_never_no_bound() {
        let data = Data::new();
        let node: Node = serde_json::from_value(json!({
            "id": "d", "type": "collect", "module_id": "regulus/data@0.1.0",
            "config": { "max_items": 0, "max_bytes": -5 } }))
        .unwrap();
        let here = data.here(NodeCall {
            execution_id: "e".into(),
            node,
            args: Value::Null,
        });
        let limits = here.limits();
        assert_eq!(limits.max_items, DEFAULT_MAX_ITEMS);
        assert_eq!(limits.max_bytes, DEFAULT_MAX_BYTES);
        assert_eq!(limits.max_record_bytes, DEFAULT_MAX_RECORD_BYTES);
    }
}
