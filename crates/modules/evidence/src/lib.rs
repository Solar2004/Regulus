//! Evidence (docs/15-evidence.md): a generic provenance store. A source is a location and
//! the text supplied for it, versioned by the hash of that text; an entry links a statement
//! to exact excerpts of sources. `store` registers both, from the bus and as tools for
//! agents; `report` checks a text that cites entries and appends the references.
//!
//! What is checked is textual: the sources exist, they are the version that was cited and
//! the excerpts are in them. Whether a statement is true, or follows from its excerpt, is
//! never checked here (`entailment = "not_checked"` in every verdict).

mod book;
mod model;
mod report;

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use regulus_contract_tool as tool;
use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Direction, Manifest, Node, NodeCall, NodeEvent, WireKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use book::{Book, EntryInput, Kv, Limits, SourceInput};
use model::{char_slice, cut, Match, Source, Status, CHECKED, ENTAILMENT};

const MANIFEST: &str = include_str!("../regulus-module.toml");
/// `{ execution_id, node_id }` of a node of this module → its collection: sources, entries
/// with their status now, limits.
pub const LIST: &str = "evidence.list";
/// `{ execution_id, node_id, source }` or `{ …, entry, context? }` → one record in full.
pub const INSPECT: &str = "evidence.inspect";
/// `{ execution_id, node_id, format?, include_text? }` → the whole collection as a document.
pub const EXPORT: &str = "evidence.export";
/// The `schema` of an export.
pub const EXPORT_SCHEMA: &str = "regulus.evidence.export/1";

const STORE: &str = "store";
const REPORT: &str = "report";
const SOURCE: &str = "source";
const CITE: &str = "cite";
const FORGET: &str = "forget";
const IN: &str = "in";
const SOURCED: &str = "sourced";
const CITED: &str = "cited";
const FORGOTTEN: &str = "forgotten";
const RESULT: &str = "result";
const REPORT_OUT: &str = "report";
const ERROR: &str = "error";
const YES: &str = "yes";
const PROJECT: &str = "project";

const TOOL_SOURCES: &str = "evidence_sources";
const TOOL_READ: &str = "evidence_read";
const TOOL_CITE: &str = "evidence_cite";
const TOOL_ENTRIES: &str = "evidence_entries";
const TOOL_ADD_SOURCE: &str = "evidence_add_source";
const TOOL_CHECK: &str = "evidence_check";

/// Nodes remembered for the module's methods, so a finished run can still be inspected.
const REMEMBERED_NODES: usize = 256;
/// Where the nodes seen are kept, in the module's global space of the store.
const NODES_KEY: &str = "nodes";
/// Characters of text around an excerpt that `evidence.inspect` shows.
const DEFAULT_CONTEXT: usize = 80;

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn to_json(value: impl Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(|e| ModuleError::Failed(e.to_string()))
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// The JSON a value carries: an object or array as is; a string that reads as one, parsed.
fn as_json(value: &Value) -> Value {
    if let Value::String(text) = value {
        if let Ok(parsed @ (Value::Object(_) | Value::Array(_))) = serde_json::from_str(text) {
            return parsed;
        }
    }
    value.clone()
}

/// A value or an array of values, as a list.
fn items(value: &Value) -> Vec<Value> {
    match as_json(value) {
        Value::Array(items) => items,
        other => vec![other],
    }
}

/// A node seen starting.
#[derive(Serialize, Deserialize)]
struct Seen {
    execution_id: String,
    node: Node,
}

pub struct Evidence {
    manifest: Manifest,
    ctx: ModuleCtx,
    /// Books when the core has no store.
    local: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    /// Held across a read-check-write, so limits hold under concurrent writes.
    writing: Mutex<()>,
    /// Serialises the read-modify-write of the index of nodes seen.
    index: Mutex<()>,
}

/// One node of one execution, with the settings the manifest defaults.
struct Here<'a> {
    evidence: &'a Evidence,
    node: Node,
    me: NodeRef,
}

impl Here<'_> {
    fn setting(&self, field: &str) -> Option<&Value> {
        self.evidence.manifest.setting(&self.node, field)
    }

    fn text(&self, field: &str) -> String {
        match self.setting(field) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    fn number(&self, field: &str) -> usize {
        match self.setting(field) {
            Some(Value::Number(n)) => n.as_f64().map_or(0, |n| n.max(0.0) as usize),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn is_store(&self) -> bool {
        self.node.node_type == STORE
    }

    fn has_output(&self, port: &str) -> bool {
        self.node
            .port(port)
            .is_some_and(|p| p.kind == WireKind::Then && p.direction == Direction::Output)
    }

    fn emit(&self, port: &str, value: Value) -> Result<(), ModuleError> {
        if !self.has_output(port) {
            return Ok(());
        }
        self.evidence.ctx.emit(&self.me, port, value)
    }

    /// `message` about this node, on `error` if it has one, and as the call's error.
    fn fail(&self, message: impl std::fmt::Display) -> ModuleError {
        let message = format!("{} `{}`: {message}", self.node.node_type, self.node.id);
        let _ = self.emit(ERROR, json!(message));
        ModuleError::Failed(message)
    }

    /// Says on `error` (if the node has one) why one value was not registered. The call
    /// itself still succeeds: the other values of a batch were registered, and a retry of
    /// the whole batch would duplicate them.
    fn complain(&self, message: &str) {
        let message = format!("{} `{}`: {message}", self.node.node_type, self.node.id);
        let _ = self.emit(ERROR, json!(message));
    }

    /// The collection this node reads or writes.
    fn book(&self) -> Result<Book, ModuleError> {
        // The core finds the node's project from its execution, running or stored.
        let kv = self
            .evidence
            .open(Some(&self.me))
            .map_err(|e| self.fail(e))?;
        let scope = if self.text("scope") == PROJECT {
            PROJECT.to_string()
        } else {
            self.me.execution_id.clone()
        };
        Book::new(kv, &scope, self.text("collection").trim()).map_err(|e| self.fail(e))
    }

    fn limits(&self) -> Limits {
        Limits {
            max_sources: self.number("max_sources").max(1),
            max_entries: self.number("max_entries").max(1),
            max_source_bytes: self.number("max_source_bytes").max(1),
            max_total_bytes: self.number("max_total_bytes").max(1),
            max_statement_chars: self.number("max_statement_chars").max(1),
            max_excerpt_chars: self.number("max_excerpt_chars").max(1),
            min_excerpt_chars: self.number("min_excerpt_chars"),
            max_supports: self.number("max_supports").max(1),
            max_versions: self.number("max_versions"),
            max_title_chars: self.number("max_title_chars").max(1),
        }
    }

    fn settings(&self) -> Result<report::Settings, String> {
        Ok(report::Settings {
            marker: report::Settings::marker(&self.text("marker"))?,
            min_citations: self.number("min_citations"),
            paragraphs: self.text("require_paragraph_citations") == YES,
            min_paragraph_chars: self.number("min_paragraph_chars"),
            max_excerpt_shown: self.number("max_excerpt_shown").max(1),
            append_references: self.text("references") != "none",
            header: self.text("references_header"),
        })
    }

    /// Registers one source within the node's limits.
    fn add_source(&self, input: SourceInput) -> Result<book::Sourced, String> {
        let book = self.book().map_err(|e| e.to_string())?;
        let _writing = lock(&self.evidence.writing);
        book.add_source(input, &self.text("source_prefix"), self.limits(), now_ms())
    }

    /// Registers one entry within the node's limits and checks it.
    fn add_entry(&self, input: EntryInput, by: &str) -> Result<model::EntryCheck, String> {
        let book = self.book().map_err(|e| e.to_string())?;
        let _writing = lock(&self.evidence.writing);
        let entry = book.add_entry(
            input,
            Match::parse(&self.text("match")),
            self.text("strict") != "no",
            &self.text("entry_prefix"),
            by,
            self.limits(),
            now_ms(),
        )?;
        book.check(&entry.id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("the entry `{}` was not saved", entry.id))
    }
}

impl Evidence {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("evidence manifest is valid"),
            ctx: ModuleCtx::default(),
            local: Arc::default(),
            writing: Mutex::default(),
            index: Mutex::default(),
        }
    }

    fn here(&self, call: NodeCall) -> Here<'_> {
        Here {
            evidence: self,
            me: NodeRef {
                execution_id: call.execution_id,
                node_id: call.node.id.clone(),
            },
            node: call.node,
        }
    }

    /// The store behind `node` (its project's), or the process's own memory when the core
    /// genuinely has no store. Any other failure of the store is an error: evidence must
    /// not quietly land somewhere that is gone at the next restart.
    fn open(&self, node: Option<&NodeRef>) -> Result<Kv, ModuleError> {
        match self.ctx.store(node) {
            Ok(store) => Ok(Kv::Store(store)),
            Err(ModuleError::Failed(reason))
                if reason == "this core has no store" || reason == "the core is not running" =>
            {
                Ok(Kv::Local(self.local.clone()))
            }
            Err(e) => Err(e),
        }
    }

    /// The nodes seen starting, oldest first: kept in the global space of the store, so the
    /// methods answer for a node of a run that ended, a core that restarted, or one of
    /// the last `REMEMBERED_NODES`.
    fn seen(&self) -> Result<Vec<Seen>, ModuleError> {
        let kv = self.open(None)?;
        Ok(match kv.get(NODES_KEY)? {
            Some(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
                ModuleError::Failed(format!(
                    "the index of evidence nodes is unreadable ({e}): remove the `{NODES_KEY}` row \
                     of the evidence space of the store"
                ))
            })?,
            None => Vec::new(),
        })
    }

    /// A node seen starting, by id, for the module's methods.
    fn known(&self, params: &Value) -> Result<Here<'_>, ModuleError> {
        #[derive(Deserialize)]
        struct Target {
            execution_id: String,
            node_id: String,
        }
        let target: Target = parse(params.clone())?;
        let seen = {
            let _index = lock(&self.index);
            self.seen()?
        }
        .into_iter()
        .find(|s| s.execution_id == target.execution_id && s.node.id == target.node_id)
        .ok_or_else(|| {
            ModuleError::InvalidParams(format!(
                "no evidence node `{}` has run in `{}`",
                target.node_id, target.execution_id
            ))
        })?;
        Ok(Here {
            evidence: self,
            me: NodeRef {
                execution_id: target.execution_id,
                node_id: target.node_id,
            },
            node: seen.node,
        })
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let _index = lock(&self.index);
        let mut seen = self.seen()?;
        seen.retain(|s| s.execution_id != call.execution_id || s.node.id != call.node.id);
        seen.push(Seen {
            execution_id: call.execution_id,
            node: call.node,
        });
        let extra = seen.len().saturating_sub(REMEMBERED_NODES);
        seen.drain(..extra);
        let bytes = serde_json::to_vec(&seen).map_err(|e| ModuleError::Failed(e.to_string()))?;
        self.open(None)?.set(NODES_KEY, &bytes)?;
        Ok(Value::Null)
    }

    fn event(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        let here = self.here(call);
        match (here.node.node_type.as_str(), event.port.as_str()) {
            (STORE, SOURCE) => add_sources(&here, &event.value),
            (STORE, CITE) => add_entries(&here, &event.value),
            (STORE, FORGET) => forget(&here, &event.value),
            (REPORT, IN) => check_report(&here, &event.value),
            (kind, port) => Err(ModuleError::InvalidParams(format!(
                "`{kind}` takes nothing on `{port}`"
            ))),
        }
    }

    fn tool_call(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let request: tool::Call = parse(std::mem::take(&mut call.args))?;
        let here = self.here(call);
        // What went wrong is an answer for the agent to read, not a broken call.
        Ok(match run_tool(&here, &request) {
            Ok(data) => to_json(tool::Outcome { data, error: false })?,
            Err(e) => to_json(tool::Outcome {
                data: Value::String(e),
                error: true,
            })?,
        })
    }
}

/// `source`: each value (or element of an array) becomes a source. One that cannot be
/// registered is reported on `error` and in `failed`; the others are registered anyway
/// (per item, not all-or-nothing), so the call itself succeeds.
fn add_sources(here: &Here, value: &Value) -> Result<Value, ModuleError> {
    let mut registered = Vec::new();
    let mut failed = Vec::new();
    for (index, item) in items(value).into_iter().enumerate() {
        let result = match serde_json::from_value::<SourceInput>(item) {
            Ok(input) => here.add_source(input),
            Err(e) => Err(format!("a source is `{{ location, text }}`: {e}")),
        };
        match result {
            Ok(sourced) => {
                here.emit(SOURCED, sourced.to_json())?;
                registered.push(sourced.source.id);
            }
            Err(e) => {
                here.complain(&e);
                failed.push(json!({ "index": index, "error": e }));
            }
        }
    }
    Ok(json!({ "sourced": registered, "failed": failed }))
}

/// `cite`: each value (or element of an array) becomes an entry; per item, like `source`.
fn add_entries(here: &Here, value: &Value) -> Result<Value, ModuleError> {
    let by = format!("node:{}", here.node.id);
    let mut registered = Vec::new();
    let mut failed = Vec::new();
    for (index, item) in items(value).into_iter().enumerate() {
        let result = match serde_json::from_value::<EntryInput>(item) {
            Ok(input) => here.add_entry(input, &by),
            Err(e) => Err(format!(
                "an entry is `{{ statement, source, excerpt }}` or `{{ statement, supports }}`: {e}"
            )),
        };
        match result {
            Ok(check) => {
                here.emit(CITED, to_json(&check)?)?;
                registered.push(check.id);
            }
            Err(e) => {
                here.complain(&e);
                failed.push(json!({ "index": index, "error": e }));
            }
        }
    }
    Ok(json!({ "cited": registered, "failed": failed }))
}

/// `forget`: an id (or `{ id }`) removes a source or an entry.
fn forget(here: &Here, value: &Value) -> Result<Value, ModuleError> {
    let id = match as_json(value) {
        Value::String(id) => id.trim().to_string(),
        Value::Object(object) => object
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        _ => String::new(),
    };
    let book = here.book()?;
    let removed = {
        let _writing = lock(&here.evidence.writing);
        book.forget(&id)
    };
    let (kind, affected) = removed.map_err(|e| here.fail(e))?;
    here.emit(
        FORGOTTEN,
        json!({ "removed": true, "kind": kind, "id": id, "affected_entries": affected }),
    )?;
    Ok(json!({ "forgotten": id }))
}

/// `in` of a `report`: checks the text, emits the verdict and the text or the reasons.
fn check_report(here: &Here, value: &Value) -> Result<Value, ModuleError> {
    let text = match as_json(value) {
        Value::String(text) => text,
        Value::Object(object) => object
            .get("text")
            .or_else(|| object.get("report"))
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| here.fail("a report is a text, or an object with `text`"))?,
        other => other.to_string(),
    };
    let settings = here.settings().map_err(|e| here.fail(e))?;
    let verdict = report::verify(&text, &here.book()?, &settings).map_err(|e| here.fail(e))?;
    here.emit(RESULT, verdict.to_json())?;
    if verdict.valid() {
        here.emit(
            REPORT_OUT,
            json!(report::rendered(&text, &verdict, &settings)),
        )?;
    } else {
        here.emit(
            ERROR,
            json!(format!(
                "the report does not validate:\n{}",
                verdict.problem_text()
            )),
        )?;
    }
    Ok(json!({ "valid": verdict.valid(), "problems": verdict.problems.len() }))
}

fn string_arg(args: &Value, name: &str) -> Option<String> {
    args.get(name).and_then(Value::as_str).map(String::from)
}

fn number_arg(args: &Value, name: &str) -> Option<usize> {
    args.get(name)
        .and_then(Value::as_f64)
        .map(|n| n.max(0.0) as usize)
}

fn run_tool(here: &Here, request: &tool::Call) -> Result<Value, String> {
    if !here.is_store() {
        if request.name == TOOL_CHECK {
            let text = string_arg(&request.args, "text").ok_or("`text` is required")?;
            let settings = here.settings()?;
            let book = here.book().map_err(|e| e.to_string())?;
            let verdict = report::verify(&text, &book, &settings)?;
            return Ok(verdict.to_json());
        }
        return Err(format!("no tool `{}`", request.name));
    }
    let book = here.book().map_err(|e| e.to_string())?;
    match request.name.as_str() {
        TOOL_SOURCES => {
            let sources = book.sources().map_err(|e| e.to_string())?;
            Ok(json!(sources
                .iter()
                .map(Source::summary)
                .collect::<Vec<_>>()))
        }
        TOOL_READ => {
            let id = string_arg(&request.args, "source").ok_or("`source` is required")?;
            let source = book
                .source(&id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("there is no source `{id}`"))?;
            let total = source.text.chars().count();
            let start = number_arg(&request.args, "start").unwrap_or(0).min(total);
            let cap = here.number("max_read_chars").max(1);
            let length = number_arg(&request.args, "length")
                .unwrap_or(cap)
                .clamp(1, cap);
            let end = (start + length).min(total);
            Ok(json!({
                "source": source.id, "location": source.location, "version": source.version,
                "start": start, "end": end, "total_chars": total,
                "next_start": (end < total).then_some(end),
                "text": char_slice(&source.text, start, end),
            }))
        }
        TOOL_ENTRIES => {
            let checked = book.checked().map_err(|e| e.to_string())?;
            Ok(json!(checked.iter().map(entry_line).collect::<Vec<_>>()))
        }
        TOOL_CITE if here.text("agent_cites") != "no" => {
            let args = &request.args;
            let mut input: EntryInput =
                serde_json::from_value(args.clone()).map_err(|e| e.to_string())?;
            // An agent never picks an id: it could overwrite an entry that is not its own.
            input.id = None;
            let check = here.add_entry(input, &request.caller.name)?;
            Ok(json!({ "cite_as": format!("[{}]", check.id), "entry": check }))
        }
        TOOL_ADD_SOURCE if here.text("agent_sources") == YES => {
            let mut input: SourceInput =
                serde_json::from_value(request.args.clone()).map_err(|e| e.to_string())?;
            input.id = None;
            Ok(here.add_source(input)?.to_json())
        }
        other => Err(format!("no tool `{other}`")),
    }
}

/// The tools a node offers agents.
fn tool_specs(here: &Here) -> Vec<tool::Spec> {
    let collection = here.text("collection");
    if !here.is_store() {
        return vec![tool::Spec {
            name: TOOL_CHECK.into(),
            description: format!(
                "Check a draft report against the evidence `{collection}` the way the final \
                 check will: every [E<n>] marker must name an entry that validates (source \
                 present, same version, excerpt in it). Returns the verdict and the problems \
                 to fix. Textual support only: it does not check that a statement is true."
            ),
            input: json!({ "type": "object", "properties": {
                "text": { "type": "string", "description": "The draft report." } },
                "required": ["text"] }),
        }];
    }
    let mut specs = vec![
        tool::Spec {
            name: TOOL_SOURCES.into(),
            description: format!(
                "List the sources of the evidence `{collection}`: id, location, title, version \
                 and size. Cite sources by id."
            ),
            input: json!({ "type": "object", "properties": {} }),
        },
        tool::Spec {
            name: TOOL_READ.into(),
            description: "Read the text of a source in pieces, to copy an excerpt exactly. \
                          Returns `next_start` while there is more."
                .into(),
            input: json!({ "type": "object", "properties": {
                "source": { "type": "string", "description": "The source id." },
                "start": { "type": "number", "description": "First character to read; default 0." },
                "length": { "type": "number", "description": "Characters to read; capped." } },
                "required": ["source"] }),
        },
        tool::Spec {
            name: TOOL_ENTRIES.into(),
            description: format!(
                "List the entries of the evidence `{collection}`: id, statement and whether \
                 they validate now."
            ),
            input: json!({ "type": "object", "properties": {} }),
        },
    ];
    if here.text("agent_cites") != "no" {
        specs.push(tool::Spec {
            name: TOOL_CITE.into(),
            description: format!(
                "Register a statement backed by an exact excerpt of a source of the evidence \
                 `{collection}`. The excerpt is checked against the source's text and refused \
                 if it is not there, is shorter than {} characters or longer than {}: copy it \
                 from evidence_read. On success you get the marker to write in the report \
                 (`cite_as`). Do not register what the excerpt does not say.",
                here.number("min_excerpt_chars"),
                here.number("max_excerpt_chars")
            ),
            input: json!({ "type": "object", "properties": {
                "statement": { "type": "string", "description": "The claim, in your words." },
                "source": { "type": "string", "description": "The source id." },
                "excerpt": { "type": "string", "description": "Exact text of the source that backs the claim." },
                "supports": { "type": "array", "description": "Several excerpts, instead of source and excerpt.",
                    "items": { "type": "object", "properties": {
                        "source": { "type": "string" }, "excerpt": { "type": "string" } },
                        "required": ["source", "excerpt"] } } },
                "required": ["statement"] }),
        });
    }
    if here.text("agent_sources") == YES {
        specs.push(tool::Spec {
            name: TOOL_ADD_SOURCE.into(),
            description: "Add a source: where it came from and its text. The text is whatever \
                          you give, so cite only text you actually got from that location."
                .into(),
            input: json!({ "type": "object", "properties": {
                "location": { "type": "string" }, "text": { "type": "string" },
                "title": { "type": "string" } }, "required": ["location", "text"] }),
        });
    }
    specs
}

/// An entry as listings show it.
fn entry_line(check: &model::EntryCheck) -> Value {
    json!({
        "id": check.id, "statement": check.statement, "status": check.status,
        "sources": check.supports.iter().map(|s| s.source.clone()).collect::<Vec<_>>(),
        "at": check.at, "by": check.by,
    })
}

fn rpc_list(here: &Here) -> Result<Value, ModuleError> {
    let book = here.book()?;
    let sources = book.sources()?;
    let checked = book.checked()?;
    let mut counts = BTreeMap::<String, usize>::new();
    for c in &checked {
        let status = to_json(c.status)?.as_str().unwrap_or("").to_string();
        *counts.entry(status).or_default() += 1;
    }
    Ok(json!({
        "collection": here.text("collection"),
        "scope": here.text("scope"),
        "check": CHECKED,
        "entailment": ENTAILMENT,
        "used_bytes": book.used_bytes()?,
        "limits": if here.is_store() { here.limits().to_json() } else { Value::Null },
        "sources": sources.iter().map(Source::summary).collect::<Vec<_>>(),
        "entries": checked.iter().map(entry_line).collect::<Vec<_>>(),
        "status_counts": counts,
    }))
}

fn rpc_inspect(here: &Here, params: &Value) -> Result<Value, ModuleError> {
    let book = here.book()?;
    if let Some(id) = string_arg(params, "entry") {
        let check = book
            .check(&id)?
            .ok_or_else(|| ModuleError::InvalidParams(format!("there is no entry `{id}`")))?;
        let width = number_arg(params, "context").unwrap_or(DEFAULT_CONTEXT);
        let mut context = Vec::new();
        for support in &check.supports {
            context.push(match book.source(&support.source)? {
                Some(source) if source.sha256 == support.pinned_sha256 => {
                    let before = support.start.saturating_sub(width);
                    json!({
                        "before": char_slice(&source.text, before, support.start),
                        "excerpt": char_slice(&source.text, support.start, support.end),
                        "after": char_slice(&source.text, support.end, support.end + width),
                    })
                }
                _ => Value::Null,
            });
        }
        let mut value = to_json(&check)?;
        value["context"] = Value::Array(context);
        return Ok(value);
    }
    if let Some(id) = string_arg(params, "source") {
        let source = book
            .source(&id)?
            .ok_or_else(|| ModuleError::InvalidParams(format!("there is no source `{id}`")))?;
        let cited_by: Vec<String> = book
            .entries()?
            .into_iter()
            .filter(|e| e.supports.iter().any(|s| s.source == id))
            .map(|e| e.id)
            .collect();
        let mut value = to_json(&source)?;
        if params.get("text").and_then(Value::as_bool) != Some(true) {
            value.as_object_mut().map(|o| o.remove("text"));
        }
        value["cited_by"] = json!(cited_by);
        return Ok(value);
    }
    Err(ModuleError::InvalidParams(
        "give `entry` or `source`: the id to inspect".into(),
    ))
}

fn rpc_export(here: &Here, params: &Value) -> Result<Value, ModuleError> {
    let book = here.book()?;
    let include_text = params.get("include_text").and_then(Value::as_bool) != Some(false);
    let sources = book.sources()?;
    let verdicts = book.checked()?;
    let markdown = string_arg(params, "format").as_deref() == Some("markdown");
    if markdown {
        return Ok(
            json!({ "format": "markdown", "markdown": export_markdown(here, &sources, &verdicts) }),
        );
    }
    let mut exported = Vec::new();
    for source in &sources {
        let mut value = to_json(source)?;
        if !include_text {
            value.as_object_mut().map(|o| o.remove("text"));
        }
        exported.push(value);
    }
    let valid = verdicts
        .iter()
        .filter(|v| v.status == Status::Valid)
        .count();
    Ok(json!({
        "schema": EXPORT_SCHEMA,
        "collection": here.text("collection"),
        "scope": here.text("scope"),
        "exported_at": now_ms(),
        "check": CHECKED,
        "entailment": ENTAILMENT,
        "sources": exported,
        "entries": book.entries()?,
        "verdicts": verdicts,
        "summary": { "sources": sources.len(), "entries": verdicts.len(), "valid": valid,
                     "not_valid": verdicts.len() - valid },
    }))
}

fn export_markdown(here: &Here, sources: &[Source], verdicts: &[model::EntryCheck]) -> String {
    let mut out = format!(
        "# Evidence `{}`\n\nChecked: excerpt inclusion. Not checked: whether any statement is \
         true or follows from its excerpt.\n\n## Sources\n",
        here.text("collection")
    );
    for s in sources {
        out.push_str(&format!(
            "\n- {} — {} (version {}, sha256 {}, {} bytes)",
            s.id,
            s.location,
            s.version,
            &s.sha256[..s.sha256.len().min(12)],
            s.bytes
        ));
    }
    out.push_str("\n\n## Entries\n");
    for v in verdicts {
        let status = to_json(v.status)
            .ok()
            .and_then(|s| s.as_str().map(String::from))
            .unwrap_or_default();
        out.push_str(&format!("\n- [{}] ({status}) {}", v.id, v.statement));
        for s in &v.supports {
            out.push_str(&format!(
                "\n  - {} v{} chars {}–{}: \"{}\"",
                s.source,
                s.pinned_version,
                s.start,
                s.end,
                cut(
                    &s.excerpt.split_whitespace().collect::<Vec<_>>().join(" "),
                    200
                )
            ));
        }
    }
    out.push('\n');
    out
}

impl Default for Evidence {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Evidence {
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
            method::NODE_STOP => Ok(Value::Null),
            method::NODE_EVENT => self.event(parse(params)?),
            tool::LIST => {
                let here = self.here(parse(params)?);
                to_json(tool_specs(&here))
            }
            tool::CALL => self.tool_call(parse(params)?),
            LIST => rpc_list(&self.known(&params)?),
            INSPECT => rpc_inspect(&self.known(&params)?, &params),
            EXPORT => rpc_export(&self.known(&params)?, &params),
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
        let evidence = Evidence::new();
        for kind in [STORE, REPORT] {
            assert!(evidence.manifest().node_type(kind).is_some(), "{kind}");
        }
        assert_eq!(evidence.manifest().module.methods, [LIST, INSPECT, EXPORT]);
    }

    #[test]
    fn values_are_one_or_many_and_may_arrive_as_text() {
        assert_eq!(items(&json!({"a": 1})).len(), 1);
        assert_eq!(items(&json!([{"a": 1}, {"b": 2}])).len(), 2);
        assert_eq!(items(&json!("{\"a\":1}")), [json!({"a": 1})]);
        assert_eq!(items(&json!("plain")), [json!("plain")]);
    }
}
