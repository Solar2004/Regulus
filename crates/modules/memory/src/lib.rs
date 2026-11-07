//! Memory (docs/13-blocks.md §3.3): `store` keeps keyed texts that agents read and write
//! as tools (`tool@1`) and that values on the bus write; `inject` adds them to an agent's
//! turn as a hook (`hook@1`); `reflect` piles up failures for a lesson and writes the
//! lesson back; `library` grows a folder of skills the Skills module can load.
//!
//! Entries live in the module's space of the store, per memory name and scope (the
//! project, or one execution), within `max_entries` and `max_bytes`.

mod entries;
mod library;

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use regulus_contract_tool as tool;
use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{
    method, Direction, Hook, HookAnswer, HookEffect, HookFailure, HookRequest, Manifest, Node,
    NodeCall, NodeEvent, NodeStart, WireKind, HOOK_DESCRIBE,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub use entries::Entry;
use entries::{Kv, Limits, Place};

const MANIFEST: &str = include_str!("../regulus-module.toml");
/// `{ execution_id, node_id }` of a live node → its memory's entries, oldest first.
pub const ENTRIES: &str = "memory.entries";
/// `{ execution_id, node_id }` of a live `store` → the tools it offers agents now.
pub const TOOLS: &str = "memory.tools";

const STORE: &str = "store";
const INJECT: &str = "inject";
const REFLECT: &str = "reflect";
const LIBRARY: &str = "library";

const OUT: &str = "out";
const WRITTEN: &str = "written";
const ERROR: &str = "error";
const FAIL: &str = "fail";
const FLUSH: &str = "flush";
const LESSON: &str = "lesson";
const PATH: &str = "path";
const YES: &str = "yes";
/// The `scope` every execution of the project shares.
const PROJECT: &str = "project";

const TOOL_LIST: &str = "memory_list";
const TOOL_READ: &str = "memory_read";
const TOOL_SEARCH: &str = "memory_search";
const TOOL_WRITE: &str = "memory_write";
const TOOL_DELETE: &str = "memory_delete";
/// `max_key_chars` when a node sets 0: keys are never unbounded.
const DEFAULT_KEY_CHARS: usize = 128;
/// How many characters of an entry `memory_list` shows.
const PREVIEW_CHARS: usize = 80;
/// The fields of a failure that say what went wrong (`error@1` and the like).
const MESSAGE_FIELDS: &[&str] = &["message", "error", "reason", "stderr"];

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

/// Text as is; anything else as its JSON.
fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// A failure as text: a string as is; an object by its message (and the value that
/// failed, if it carries one); anything else as JSON.
fn failure_text(value: &Value) -> String {
    let Value::Object(object) = value else {
        return as_text(value);
    };
    let Some(message) = MESSAGE_FIELDS
        .iter()
        .find_map(|f| object.get(*f).filter(|v| !v.is_null()))
    else {
        return value.to_string();
    };
    match object.get("value").filter(|v| !v.is_null()) {
        Some(input) => format!("{}\nInput: {}", as_text(message), as_text(input)),
        None => as_text(message),
    }
}

fn cut(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

type Key = (String, String);

/// A Reflection's pile, also its checkpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Pending {
    failures: Vec<String>,
}

pub struct Memory {
    manifest: Manifest,
    ctx: ModuleCtx,
    /// Entries when the core has no store.
    local: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    /// Held across a read-check-write of entries, so limits hold under concurrent writes.
    writing: Mutex<()>,
    /// Live nodes, for the module's methods.
    live: Mutex<HashMap<Key, Node>>,
    pending: Mutex<HashMap<Key, Pending>>,
}

/// One node of one execution, with the settings the manifest defaults.
struct Here<'a> {
    memory: &'a Memory,
    node: Node,
    me: NodeRef,
}

impl Here<'_> {
    fn setting(&self, field: &str) -> Option<&Value> {
        self.memory.manifest.setting(&self.node, field)
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

    fn has_output(&self, port: &str) -> bool {
        self.node
            .port(port)
            .is_some_and(|p| p.kind == WireKind::Then && p.direction == Direction::Output)
    }

    fn emit(&self, port: &str, value: Value) -> Result<(), ModuleError> {
        if !self.has_output(port) {
            return Ok(());
        }
        self.memory.ctx.emit(&self.me, port, value)
    }

    /// `message` about this node, on `error` if it has one, and as the call's error.
    fn fail(&self, message: impl std::fmt::Display) -> ModuleError {
        let message = format!("{} `{}`: {message}", self.node.node_type, self.node.id);
        let _ = self.emit(ERROR, json!(message));
        ModuleError::Failed(message)
    }

    /// The memory this node reads or writes.
    fn place(&self) -> Result<Place, ModuleError> {
        let kv = match self.memory.ctx.store(Some(&self.me)) {
            Ok(store) => Kv::Store(store),
            Err(_) => Kv::Local(self.memory.local.clone()),
        };
        let scope = if self.text("scope") == PROJECT {
            PROJECT.to_string()
        } else {
            self.me.execution_id.clone()
        };
        Place::new(kv, &scope, self.text("memory").trim()).map_err(|e| self.fail(e))
    }

    fn limits(&self) -> Limits {
        Limits {
            max_entries: self.number("max_entries"),
            max_bytes: self.number("max_bytes"),
            max_key_chars: match self.number("max_key_chars") {
                0 => DEFAULT_KEY_CHARS,
                n => n,
            },
            evict_oldest: self.text("evict") != "refuse",
        }
    }

    /// Writes one entry within the node's limits.
    fn write(&self, key: Option<String>, text: String, by: String) -> Result<Entry, String> {
        // Resolved before the lock: a bad name reports (and emits) without holding it.
        let place = self.place().map_err(|e| e.to_string())?;
        let _writing = lock(&self.memory.writing);
        let now = now_ms();
        let key = match key.filter(|k| !k.trim().is_empty()) {
            Some(key) => key,
            None => {
                let stem = match self.text("key_prefix").trim() {
                    "" => self.node.id.clone(),
                    prefix => prefix.to_string(),
                };
                place.fresh_key(&stem, now).map_err(|e| e.to_string())?
            }
        };
        let entry = Entry {
            key,
            text,
            at: now,
            by,
        };
        let evicted = place.write(&entry, self.limits())?;
        if !evicted.is_empty() {
            eprintln!(
                "regulus: memory: `{}` evicted {} to make room: {}",
                self.node.id,
                evicted.len(),
                evicted.join(", ")
            );
        }
        Ok(entry)
    }
}

impl Memory {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("memory manifest is valid"),
            ctx: ModuleCtx::default(),
            local: Arc::default(),
            writing: Mutex::default(),
            live: Mutex::default(),
            pending: Mutex::default(),
        }
    }

    fn here(&self, call: NodeCall) -> Here<'_> {
        Here {
            memory: self,
            me: NodeRef {
                execution_id: call.execution_id,
                node_id: call.node.id.clone(),
            },
            node: call.node,
        }
    }

    /// A live node by id, for the module's methods.
    fn live(&self, params: Value) -> Result<Here<'_>, ModuleError> {
        #[derive(Deserialize)]
        struct Target {
            execution_id: String,
            node_id: String,
        }
        let target: Target = parse(params)?;
        let node = lock(&self.live)
            .get(&(target.execution_id.clone(), target.node_id.clone()))
            .cloned()
            .ok_or_else(|| {
                ModuleError::InvalidParams(format!(
                    "no memory node `{}` running in `{}`",
                    target.node_id, target.execution_id
                ))
            })?;
        Ok(Here {
            memory: self,
            me: NodeRef {
                execution_id: target.execution_id,
                node_id: target.node_id,
            },
            node,
        })
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let key = (call.execution_id.clone(), call.node.id.clone());
        lock(&self.live).insert(key.clone(), call.node.clone());
        if call.node.node_type == REFLECT {
            let start: NodeStart = serde_json::from_value(call.args).unwrap_or_default();
            let pending = start
                .checkpoint
                .and_then(|c| serde_json::from_value(c).ok())
                .unwrap_or_default();
            lock(&self.pending).insert(key, pending);
        }
        Ok(Value::Null)
    }

    fn stop(&self, call: NodeCall) {
        let key = (call.execution_id, call.node.id);
        lock(&self.live).remove(&key);
        lock(&self.pending).remove(&key);
    }

    fn event(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        let here = self.here(call);
        match (here.node.node_type.as_str(), event.port.as_str()) {
            (STORE, _) => store_value(&here, &event.value),
            (INJECT, _) => {
                let text = recall(&here, Some(&as_text(&event.value)))?;
                here.emit(OUT, json!(text))?;
                Ok(json!({ "chars": text.chars().count() }))
            }
            (REFLECT, FAIL) => self.pile(&here, &event.value),
            (REFLECT, FLUSH) => self.flush(&here),
            (REFLECT, LESSON) => {
                let lesson = as_text(&event.value).trim().to_string();
                if lesson.is_empty() {
                    return Err(here.fail("an empty lesson is not written"));
                }
                let by = format!("node:{}", here.node.id);
                let entry = here.write(None, lesson, by).map_err(|e| here.fail(e))?;
                here.emit(WRITTEN, to_json(&entry)?)?;
                Ok(json!({ "written": entry.key }))
            }
            (LIBRARY, _) => grow(&here, &event.value),
            (kind, port) => Err(ModuleError::InvalidParams(format!(
                "`{kind}` takes nothing on `{port}`"
            ))),
        }
    }

    /// Adds a failure to the pile; at `count`, sends the batch.
    fn pile(&self, here: &Here, value: &Value) -> Result<Value, ModuleError> {
        let key = (here.me.execution_id.clone(), here.me.node_id.clone());
        let failure = cut(
            &failure_text(value),
            here.number("max_failure_chars").max(1),
        );
        let saved = {
            let mut pending = lock(&self.pending);
            let pile = pending.entry(key).or_default();
            pile.failures.push(failure);
            let keep = here.number("max_pending").max(1);
            let extra = pile.failures.len().saturating_sub(keep);
            pile.failures.drain(..extra);
            pile.clone()
        };
        // Saved after the lock is released: the store is not waited on under it.
        let _ = self.ctx.checkpoint(&here.me, json!(saved));
        let size = saved.failures.len();
        if size >= here.number("count").max(1) {
            return self.flush(here);
        }
        Ok(json!({ "pending": size }))
    }

    /// Sends the pile as one text on `out` and empties it. An empty pile sends nothing.
    fn flush(&self, here: &Here) -> Result<Value, ModuleError> {
        let key = (here.me.execution_id.clone(), here.me.node_id.clone());
        let failures = {
            let mut pending = lock(&self.pending);
            let pile = pending.entry(key).or_default();
            std::mem::take(&mut pile.failures)
        };
        let _ = self.ctx.checkpoint(&here.me, json!(Pending::default()));
        if failures.is_empty() {
            return Ok(json!({ "sent": 0 }));
        }
        let separator = here.text("separator");
        let numbered: Vec<String> = failures
            .iter()
            .enumerate()
            .map(|(n, f)| format!("Failure {}:\n{f}", n + 1))
            .collect();
        let text = format!(
            "{}{separator}{}",
            here.text("prompt"),
            numbered.join(&separator)
        );
        here.emit(OUT, json!(text))?;
        Ok(json!({ "sent": failures.len() }))
    }

    fn tool_call(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let request: tool::Call = parse(std::mem::take(&mut call.args))?;
        let here = self.here(call);
        let data = run_tool(&here, &request);
        // What went wrong is an answer for the agent to read, not a broken call.
        Ok(match data {
            Ok(data) => to_json(tool::Outcome { data, error: false })?,
            Err(e) => to_json(tool::Outcome {
                data: Value::String(e),
                error: true,
            })?,
        })
    }
}

/// `store`: a value on `in` becomes an entry.
fn store_value(here: &Here, value: &Value) -> Result<Value, ModuleError> {
    let (key, text) = match value {
        Value::Object(object) if object.contains_key("text") => (
            object.get("key").and_then(Value::as_str).map(String::from),
            as_text(&object["text"]),
        ),
        other => (None, as_text(other)),
    };
    let key = key.or_else(|| Some(here.text("key").trim().to_string()));
    let by = format!("node:{}", here.node.id);
    let entry = here.write(key, text, by).map_err(|e| here.fail(e))?;
    here.emit(WRITTEN, to_json(&entry)?)?;
    Ok(json!({ "written": entry.key }))
}

/// The memory as the agent should see it, matched against `query` with `select = match`.
fn recall(here: &Here, query: Option<&str>) -> Result<String, ModuleError> {
    let mut entries = here.place()?.entries()?;
    entries = if here.text("select") == "match" {
        entries::matching(entries, query.unwrap_or_default())
    } else {
        entries.reverse();
        entries
    };
    Ok(entries::render(
        &here.text("header"),
        &entries,
        here.number("max_chars"),
    ))
}

fn describe_hook(here: &Here) -> Result<Value, ModuleError> {
    let fail: HookFailure = match here.text("fail").as_str() {
        "" => HookFailure::Open,
        fail => parse(json!(fail))?,
    };
    to_json(Hook {
        event: here.text("event"),
        matcher: None,
        timeout_ms: None,
        returns: vec![HookEffect::Context],
        fail,
    })
}

/// On its event, the memory as context; nothing stored, or any other event, is nothing.
fn answer_hook(here: &Here, request: HookRequest) -> Result<Value, ModuleError> {
    if request.event != here.text("event") {
        return to_json(HookAnswer::Nothing);
    }
    let text = recall(here, request.text.as_deref())?;
    to_json(if text.is_empty() {
        HookAnswer::Nothing
    } else {
        HookAnswer::Context { text }
    })
}

/// `library`: a value becomes a skill folder.
fn grow(here: &Here, value: &Value) -> Result<Value, ModuleError> {
    let skill = library::parse(value).map_err(|e| here.fail(e))?;
    let base = base(here)?;
    let folder = library::write(
        &base,
        &skill,
        here.text("existing") == "keep",
        here.text("allow_outside") == YES,
        here.number("max_bytes"),
    )
    .map_err(|e| here.fail(e))?;
    let path = folder.display().to_string();
    here.emit(PATH, json!(path))?;
    Ok(json!({ "skill": skill.name, "path": path }))
}

/// `dir` if absolute, else `dir` under the node's own directory.
fn base(here: &Here) -> Result<PathBuf, ModuleError> {
    let dir = here.text("dir");
    let dir = Path::new(dir.trim());
    if dir.is_absolute() {
        return Ok(dir.to_path_buf());
    }
    Ok(here.memory.ctx.node_dir(&here.me)?.join(dir))
}

fn string_arg(args: &Value, name: &str) -> Option<String> {
    args.get(name).and_then(Value::as_str).map(String::from)
}

fn run_tool(here: &Here, request: &tool::Call) -> Result<Value, String> {
    let writes = here.text("agent_writes") != "no";
    let max = here.number("max_results").max(1);
    let place = here.place().map_err(|e| e.to_string())?;
    let all = || place.entries().map_err(|e| e.to_string());
    match request.name.as_str() {
        TOOL_LIST => {
            let listed: Vec<Value> = all()?
                .iter()
                .rev()
                .map(
                    |e| json!({ "key": e.key, "at": e.at, "preview": cut(&e.text, PREVIEW_CHARS) }),
                )
                .collect();
            Ok(json!(listed))
        }
        TOOL_READ => match string_arg(&request.args, "key") {
            Some(key) => place
                .get(&key)
                .map_err(|e| e.to_string())?
                .map(|e| json!(e))
                .ok_or_else(|| format!("no entry `{key}`")),
            None => Ok(json!(all()?
                .into_iter()
                .rev()
                .take(max)
                .collect::<Vec<_>>())),
        },
        TOOL_SEARCH => {
            let query = string_arg(&request.args, "query").unwrap_or_default();
            let found: Vec<Entry> = entries::matching(all()?, &query)
                .into_iter()
                .take(max)
                .collect();
            Ok(json!(found))
        }
        TOOL_WRITE if writes => {
            let key = string_arg(&request.args, "key").ok_or("`key` is required")?;
            let text = string_arg(&request.args, "text").ok_or("`text` is required")?;
            let entry = here.write(Some(key), text, request.caller.name.clone())?;
            Ok(json!(entry))
        }
        TOOL_DELETE if writes => {
            let key = string_arg(&request.args, "key").ok_or("`key` is required")?;
            let _writing = lock(&here.memory.writing);
            Ok(json!({ "deleted": place.delete(&key).map_err(|e| e.to_string())? }))
        }
        other => Err(format!("no tool `{other}`")),
    }
}

/// The tools a `store` offers, with what it holds now in `memory_read`'s description.
fn tool_specs(here: &Here) -> Result<Vec<tool::Spec>, ModuleError> {
    let memory = here.text("memory");
    let mut read = format!(
        "Read the memory `{memory}`: the entry with `key`, or without it the newest entries."
    );
    let entries = here.place()?.entries()?;
    let shown = match here.text("describe").as_str() {
        "none" => String::new(),
        "entries" => entries::render(" Stored now:", &entries, usize::MAX),
        _ if entries.is_empty() => " Nothing is stored yet.".into(),
        _ => {
            let keys: Vec<&str> = entries.iter().map(|e| e.key.as_str()).collect();
            format!(" Keys stored now ({}): {}.", keys.len(), keys.join(", "))
        }
    };
    read.push_str(&cut(&shown, here.number("describe_max_chars")));
    let key = json!({ "type": "string", "description": "The entry's key." });
    let mut specs = vec![
        tool::Spec {
            name: TOOL_LIST.into(),
            description: format!(
                "List the entries of the memory `{memory}`, newest first: key, time and the \
                 start of the text."
            ),
            input: json!({ "type": "object", "properties": {} }),
        },
        tool::Spec {
            name: TOOL_READ.into(),
            description: read,
            input: json!({ "type": "object", "properties": { "key": key } }),
        },
        tool::Spec {
            name: TOOL_SEARCH.into(),
            description: format!(
                "Find entries of the memory `{memory}` that share words with `query`, best \
                 first."
            ),
            input: json!({ "type": "object", "properties": {
                "query": { "type": "string", "description": "Words to look for." } },
                "required": ["query"] }),
        },
    ];
    if here.text("agent_writes") != "no" {
        specs.push(tool::Spec {
            name: TOOL_WRITE.into(),
            description: format!(
                "Remember something in the memory `{memory}` under `key`, replacing what that \
                 key held. Keep entries short and general: they come back in later work."
            ),
            input: json!({ "type": "object", "properties": {
                "key": key, "text": { "type": "string", "description": "What to remember." } },
                "required": ["key", "text"] }),
        });
        specs.push(tool::Spec {
            name: TOOL_DELETE.into(),
            description: format!("Forget the entry with `key` in the memory `{memory}`."),
            input: json!({ "type": "object", "properties": { "key": key },
                           "required": ["key"] }),
        });
    }
    Ok(specs)
}

impl Default for Memory {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Memory {
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
            method::NODE_STOP => {
                self.stop(parse(params)?);
                Ok(Value::Null)
            }
            method::NODE_EVENT => self.event(parse(params)?),
            HOOK_DESCRIBE => describe_hook(&self.here(parse(params)?)),
            method::NODE_HOOK => {
                let mut call: NodeCall = parse(params)?;
                let request = parse(std::mem::take(&mut call.args))?;
                answer_hook(&self.here(call), request)
            }
            tool::LIST => to_json(tool_specs(&self.here(parse(params)?))?),
            tool::CALL => self.tool_call(parse(params)?),
            ENTRIES => {
                let here = self.live(params)?;
                to_json(here.place()?.entries()?)
            }
            TOOLS => to_json(tool_specs(&self.live(params)?)?),
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
        let memory = Memory::new();
        for kind in [STORE, INJECT, REFLECT, LIBRARY] {
            assert!(memory.manifest().node_type(kind).is_some(), "{kind}");
        }
        assert_eq!(memory.manifest().module.methods, [ENTRIES, TOOLS]);
    }

    #[test]
    fn failures_read_as_their_message_and_input() {
        assert_eq!(failure_text(&json!("boom")), "boom");
        assert_eq!(
            failure_text(&json!({ "from": {}, "message": "exit 1", "value": "cargo test" })),
            "exit 1\nInput: cargo test"
        );
        assert_eq!(failure_text(&json!({ "code": 2 })), "{\"code\":2}");
        assert_eq!(cut("abcdef", 3), "abc…");
    }
}
