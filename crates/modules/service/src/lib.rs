//! Service (docs/13-blocks.md §3.3): Regulus outwards. An `endpoint` node offers workflows
//! with ports of its profile to outside systems: each request is checked against a key
//! (generated here, shown once, kept hashed), its scope and the policies that apply, then
//! runs as a child execution (`ModuleCtx::call_workflow`) whose first output is the
//! answer. Every request is logged in the module's store.
//!
//! The engine is swappable: the endpoint pulls neutral requests from whatever serves
//! `service.engine@1` on its `engine` input and answers them there. The `http` node is
//! the engine that ships; WebSocket, MCP or OpenAI-compatible ones are other nodes, in
//! other modules, that serve the same two methods ([`NEXT`], [`REPLY`]).

mod http;
pub mod policy;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, ChildEvent, ChildEventKind, Manifest, Node, NodeCall};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use policy::{Counters, Policy, Refused, Spent};

const MANIFEST: &str = include_str!("../regulus-module.toml");

/// `{ scope: [..], name?, policy? }` → the new key, shown this once.
pub const KEY_NEW: &str = "service.key.new";
/// `{}` → every key, without its secret.
pub const KEY_LIST: &str = "service.key.list";
/// `{ id }` → the key, revoked.
pub const KEY_REVOKE: &str = "service.key.revoke";
/// `{ last?, key?, workflow? }` → the latest requests, oldest first.
pub const LOG: &str = "service.log";
/// `{}` → where each live engine listens.
pub const ENDPOINTS: &str = "service.endpoints";

/// `service.engine@1`: `{ wait_ms }` → the next request, or `null` if none came in time.
/// A request is `{ id, op: "call" | "job", key, workflow?, input?, async?, job? }`.
pub const NEXT: &str = "service.engine.next";
/// `service.engine@1`: `{ id, status, body, retry_after_s? }` → the answer to request `id`.
/// Statuses are HTTP's; an engine of another protocol translates them.
pub const REPLY: &str = "service.engine.reply";

/// What every key starts with.
pub const KEY_PREFIX: &str = "rgk_";
/// The project of a profile that has none, in scopes.
pub const GLOBAL: &str = "global";
const ANY: &str = "*";

const ENDPOINT: &str = "endpoint";
const HTTP: &str = "http";
const ENGINE_PORT: &str = "engine";
const REQUESTS_PORT: &str = "requests";
/// The module's id path inside `node_dir` (`<data>/{global|projects/<p>}/regulus/service/…`).
const MODULE_SEGMENTS: usize = 2;
const PROJECTS_DIR: &str = "projects";
/// Log rows, numbered, and the number of the next one.
const LOG_ROW: &str = "log/";
const LOG_NEXT: &str = "log_next";

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn random_hex(bytes: usize) -> Result<String, ModuleError> {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).map_err(|e| ModuleError::Failed(e.to_string()))?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A key as the store keeps it: never the secret, only its SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyRecord {
    pub id: String,
    #[serde(default)]
    pub name: String,
    /// `<project>[/<workflow>]`; `*` stands for any project or workflow.
    pub scope: Vec<String>,
    /// The first characters of the secret, to tell keys apart.
    pub hint: String,
    #[serde(default)]
    pub hash: String,
    pub created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revoked_ms: Option<u64>,
    /// Applies to every request with this key, at every endpoint.
    #[serde(default, skip_serializing_if = "Policy::is_empty")]
    pub policy: Policy,
}

impl KeyRecord {
    fn public(&self) -> Value {
        let mut value = json!(self);
        if let Some(map) = value.as_object_mut() {
            map.remove("hash");
        }
        value
    }

    /// Whether this key may call `workflow` of `project`.
    pub fn allows(&self, project: &str, workflow: &str) -> bool {
        self.scope.iter().any(|entry| {
            let (p, w) = entry.split_once('/').unwrap_or((entry, ANY));
            (p == ANY || p == project) && (w == ANY || w == workflow)
        })
    }
}

fn valid_scope(entry: &str) -> bool {
    let (p, w) = entry.split_once('/').unwrap_or((entry, ANY));
    !p.is_empty() && !w.is_empty() && !w.contains('/')
}

#[derive(Deserialize)]
struct KeyNew {
    scope: Vec<String>,
    #[serde(default)]
    name: String,
    #[serde(default)]
    policy: Policy,
}

#[derive(Deserialize)]
struct IdParams {
    id: String,
}

#[derive(Deserialize, Default)]
struct LogParams {
    #[serde(default)]
    last: Option<usize>,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    workflow: Option<String>,
}

/// One line of the request log.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    pub at_ms: u64,
    #[serde(default)]
    pub key_id: Option<String>,
    #[serde(default)]
    pub key_name: Option<String>,
    pub project: String,
    #[serde(default)]
    pub workflow: Option<String>,
    /// `sync`, `async` or `job` (a poll).
    pub mode: String,
    pub status: u16,
    pub ms: u64,
    pub result: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_id: Option<String>,
}

/// A request in the neutral form engines hand over.
#[derive(Debug, Clone, Deserialize)]
struct Request {
    id: String,
    op: String,
    #[serde(default)]
    key: Option<String>,
    #[serde(default)]
    workflow: Option<String>,
    #[serde(default)]
    input: Value,
    #[serde(default, rename = "async")]
    is_async: bool,
    #[serde(default)]
    job: Option<String>,
}

struct Answer {
    status: u16,
    body: Value,
    retry_after_s: Option<u64>,
}

impl Answer {
    fn new(status: u16, body: Value) -> Self {
        Self {
            status,
            body,
            retry_after_s: None,
        }
    }

    fn error(status: u16, message: impl Into<String>) -> Self {
        Self::new(status, json!({ "error": message.into() }))
    }
}

/// What a child said.
enum Outcome {
    Returned { port: String, value: Value },
    Ended { state: Value },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Job {
    job: String,
    key_id: String,
    workflow: String,
    execution_id: String,
    /// `running`, `done` or `failed`.
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    outputs: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    created_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    finished_ms: Option<u64>,
}

/// A request running as a child execution.
struct Flight {
    me: NodeRef,
    key: KeyRecord,
    project: String,
    workflow: String,
    mode: &'static str,
    started: Instant,
    at_ms: u64,
    scopes: Vec<String>,
    /// Budgets to charge when it ends.
    budgets: Vec<String>,
    waiter: Option<mpsc::Sender<Outcome>>,
    job: Option<String>,
    /// Milliseconds until it returned, and what.
    returned: Option<u64>,
    outputs: Option<Value>,
}

#[derive(Default)]
struct Flights {
    by_child: HashMap<String, Flight>,
    /// Child news that came before its flight was registered.
    early: HashMap<String, Vec<ChildEventKind>>,
    /// Calls between `call_workflow` and their flight: only then can news come early.
    registering: usize,
}

/// A live endpoint node.
struct Live {
    node: Node,
    project: String,
    stopping: Arc<AtomicBool>,
    workers: Arc<AtomicUsize>,
}

type Key = (String, String);

fn key_of(node: &NodeRef) -> Key {
    (node.execution_id.clone(), node.node_id.clone())
}

struct Inner {
    manifest: Manifest,
    ctx: ModuleCtx,
    endpoints: Mutex<HashMap<Key, Arc<Live>>>,
    engines: Mutex<HashMap<Key, Arc<http::Engine>>>,
    flights: Mutex<Flights>,
    jobs: Mutex<HashMap<String, Job>>,
    counters: Mutex<Counters>,
    /// The next log row; read from the store on first use.
    log_next: Mutex<Option<u64>>,
}

pub struct Service {
    inner: Arc<Inner>,
}

impl Inner {
    fn setting<'a>(&'a self, node: &'a Node, field: &str) -> Option<&'a Value> {
        self.manifest.setting(node, field)
    }

    fn text(&self, node: &Node, field: &str) -> String {
        self.setting(node, field)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    }

    fn number(&self, node: &Node, field: &str) -> u64 {
        self.setting(node, field)
            .and_then(Value::as_f64)
            .map_or(0, |n| n.max(0.0) as u64)
    }

    fn global(&self) -> Result<regulus_core::ModuleStore, ModuleError> {
        self.ctx.store(None)
    }

    // --- keys -----------------------------------------------------------------------

    fn key_new(&self, params: KeyNew) -> Result<Value, ModuleError> {
        if params.scope.is_empty() {
            return Err(ModuleError::InvalidParams(
                "a key needs a scope: <project>[/<workflow>], `*` for any".into(),
            ));
        }
        if let Some(bad) = params.scope.iter().find(|s| !valid_scope(s)) {
            return Err(ModuleError::InvalidParams(format!(
                "`{bad}` is not <project>[/<workflow>]"
            )));
        }
        let store = self.global()?;
        let secret = format!("{KEY_PREFIX}{}", random_hex(32)?);
        let record = KeyRecord {
            id: format!("k-{}", random_hex(4)?),
            name: params.name,
            scope: params.scope,
            hint: secret[..KEY_PREFIX.len() + 6].to_string(),
            hash: hash(&secret),
            created_ms: now_ms(),
            revoked_ms: None,
            policy: params.policy,
        };
        store.set_json(&format!("key/{}", record.id), &record)?;
        store.set_json(&format!("hash/{}", record.hash), &record.id)?;
        let mut shown = record.public();
        shown["key"] = json!(secret);
        Ok(shown)
    }

    fn keys(&self) -> Result<Vec<KeyRecord>, ModuleError> {
        Ok(self
            .global()?
            .list("key/")?
            .into_iter()
            .filter_map(|(_, bytes)| serde_json::from_slice(&bytes).ok())
            .collect())
    }

    fn key_revoke(&self, id: &str) -> Result<Value, ModuleError> {
        let store = self.global()?;
        let mut record: KeyRecord = store
            .get_json(&format!("key/{id}"))?
            .ok_or_else(|| ModuleError::InvalidParams(format!("no key `{id}`")))?;
        if record.revoked_ms.is_none() {
            record.revoked_ms = Some(now_ms());
            store.set_json(&format!("key/{id}"), &record)?;
            store.delete(&format!("hash/{}", record.hash))?;
        }
        Ok(record.public())
    }

    /// The live key a secret names, if any.
    fn key_for(&self, secret: Option<&str>) -> Option<KeyRecord> {
        let secret = secret.filter(|s| s.starts_with(KEY_PREFIX))?;
        let store = self.global().ok()?;
        let id: String = store.get_json(&format!("hash/{}", hash(secret))).ok()??;
        let record: KeyRecord = store.get_json(&format!("key/{id}")).ok()??;
        record.revoked_ms.is_none().then_some(record)
    }

    // --- log ------------------------------------------------------------------------

    fn log(&self, node: &Node, me: &NodeRef, entry: LogEntry) {
        self.write_log(&entry, self.number(node, "log_max"));
        if node.port(REQUESTS_PORT).is_some() {
            let _ = self.ctx.emit(me, REQUESTS_PORT, json!(entry));
        }
    }

    /// Rows are numbered (`log/<n>`): writing row n deletes row n - `max`, so the log
    /// stays bounded without listing it.
    fn write_log(&self, entry: &LogEntry, max: u64) {
        let Ok(store) = self.global() else {
            return;
        };
        let mut next = lock(&self.log_next);
        let seq = match *next {
            Some(seq) => seq,
            None => store.get_json::<u64>(LOG_NEXT).ok().flatten().unwrap_or(0),
        };
        *next = Some(seq + 1);
        let written = store
            .set_json(&format!("{LOG_ROW}{seq:020}"), entry)
            .and_then(|()| store.set_json(LOG_NEXT, &(seq + 1)));
        if let Err(error) = written {
            eprintln!("regulus: service: log: {error}");
        }
        // 0: the caller does not know the limit (its endpoint is gone); trim next time.
        if let Some(old) = (max > 0).then(|| seq.checked_sub(max)).flatten() {
            let _ = store.delete(&format!("{LOG_ROW}{old:020}"));
        }
    }

    fn read_log(&self, params: LogParams) -> Result<Vec<LogEntry>, ModuleError> {
        let mut entries: Vec<LogEntry> = self
            .global()?
            .list(LOG_ROW)?
            .into_iter()
            .filter_map(|(_, bytes)| serde_json::from_slice::<LogEntry>(&bytes).ok())
            .filter(|e| {
                params
                    .key
                    .as_ref()
                    .is_none_or(|k| e.key_id.as_ref() == Some(k))
            })
            .filter(|e| {
                params
                    .workflow
                    .as_ref()
                    .is_none_or(|w| e.workflow.as_ref() == Some(w))
            })
            .collect();
        if let Some(last) = params.last {
            entries.drain(..entries.len().saturating_sub(last));
        }
        Ok(entries)
    }

    // --- endpoint -------------------------------------------------------------------

    /// The project of a node's execution, read from where its directory hangs
    /// (`<data>/{global | projects/<project>}/regulus/service/<workflow>/<node>`).
    fn project(&self, me: &NodeRef) -> Result<String, ModuleError> {
        let dir = self.ctx.node_dir(me)?;
        let mut parts = dir
            .components()
            .rev()
            .map(|c| c.as_os_str().to_string_lossy());
        let skip = me.node_id.split('/').count() + 1 + MODULE_SEGMENTS;
        let place = parts.nth(skip).unwrap_or_default().into_owned();
        let parent = parts.next().unwrap_or_default();
        Ok(if parent == PROJECTS_DIR {
            place
        } else {
            GLOBAL.into()
        })
    }

    fn exposed(&self, node: &Node) -> Vec<String> {
        match self.setting(node, "workflows") {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            Some(Value::String(text)) => text
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        }
    }

    fn policies(&self, node: &Node) -> Result<(Policy, HashMap<String, Policy>), ModuleError> {
        let bad = |e: serde_json::Error| {
            ModuleError::InvalidParams(format!("node `{}`: bad policy: {e}", node.id))
        };
        let default = match self.setting(node, "policy") {
            Some(value) if !value.is_null() => {
                serde_json::from_value(value.clone()).map_err(bad)?
            }
            _ => Policy::default(),
        };
        let table = match self.setting(node, "policies") {
            Some(value) if !value.is_null() => {
                serde_json::from_value(value.clone()).map_err(bad)?
            }
            _ => HashMap::new(),
        };
        Ok((default, table))
    }

    fn start(self: &Arc<Self>, call: NodeCall) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        match call.node.node_type.as_str() {
            HTTP => {
                let settings = http::Settings {
                    bind: self.text(&call.node, "bind"),
                    max_body: self.number(&call.node, "max_body_bytes") as usize,
                    read_timeout: Duration::from_millis(
                        self.number(&call.node, "timeout_ms").max(1),
                    ),
                    accept: Duration::from_millis(self.number(&call.node, "accept_ms").max(1)),
                    max_wait: Duration::from_millis(self.number(&call.node, "max_wait_ms").max(1)),
                    max_connections: self.number(&call.node, "max_connections").max(1) as usize,
                };
                let engine = http::Engine::open(settings).map_err(|e| {
                    ModuleError::InvalidParams(format!("http `{}`: {e}", call.node.id))
                })?;
                let addr = engine.addr;
                if let Some(old) = lock(&self.engines).insert(key_of(&me), engine) {
                    old.close();
                }
                eprintln!(
                    "regulus: service: `{}` of `{}` listens on http://{addr}",
                    me.node_id, me.execution_id
                );
                Ok(json!({ "listening": addr.to_string() }))
            }
            ENDPOINT => {
                if self.exposed(&call.node).is_empty() {
                    return Err(ModuleError::InvalidParams(format!(
                        "endpoint `{}`: `workflows` names nothing to expose",
                        call.node.id
                    )));
                }
                self.policies(&call.node)?;
                let live = Arc::new(Live {
                    node: call.node.clone(),
                    project: self.project(&me)?,
                    stopping: Arc::new(AtomicBool::new(false)),
                    workers: Arc::new(AtomicUsize::new(0)),
                });
                if let Some(old) = lock(&self.endpoints).insert(key_of(&me), live.clone()) {
                    old.stopping.store(true, Ordering::SeqCst);
                }
                let inner = Arc::downgrade(self);
                std::thread::Builder::new()
                    .name("regulus-service-endpoint".into())
                    .spawn(move || pull(inner, me, live))
                    .map_err(|e| ModuleError::Failed(e.to_string()))?;
                Ok(Value::Null)
            }
            _ => Ok(Value::Null),
        }
    }

    fn stop(&self, call: NodeCall) {
        let key = (call.execution_id.clone(), call.node.id.clone());
        if let Some(engine) = lock(&self.engines).remove(&key) {
            engine.close();
        }
        let live = lock(&self.endpoints).remove(&key);
        if let Some(live) = &live {
            live.stopping.store(true, Ordering::SeqCst);
            self.abandon(Some(&key), self.number(&live.node, "log_max"));
        }
    }

    /// Ends the requests of one endpoint (or all) that are still running: the core stops
    /// their children with the endpoint and sends no news of them. Each frees its slots,
    /// charges its budgets, fails its job and is logged as 503; a waiting caller gets 503.
    fn abandon(&self, only: Option<&Key>, log_max: u64) {
        let gone: Vec<(String, Flight)> = {
            let mut flights = lock(&self.flights);
            let children: Vec<String> = flights
                .by_child
                .iter()
                .filter(|(_, f)| only.is_none_or(|k| key_of(&f.me) == *k))
                .map(|(child, _)| child.clone())
                .collect();
            children
                .into_iter()
                .filter_map(|child| flights.by_child.remove(&child).map(|f| (child, f)))
                .collect()
        };
        for (child, flight) in gone {
            let status = if flight.returned.is_some() { 200 } else { 503 };
            self.settle(
                &child,
                flight,
                status,
                "stopped with the endpoint",
                None,
                log_max,
            );
        }
    }

    /// Handles one request of an endpoint and says what to answer.
    fn handle(self: &Arc<Self>, me: &NodeRef, live: &Live, request: Request) -> Answer {
        let at = now_ms();
        let started = Instant::now();
        let Some(key) = self.key_for(request.key.as_deref()) else {
            // Not logged: anyone can send these, with any workflow name in them.
            return Answer::error(401, "unknown or revoked key");
        };
        if request.op == "job" {
            return self.poll(key, request.job.as_deref().unwrap_or_default());
        }
        let entry = LogEntry {
            at_ms: at,
            key_id: Some(key.id.clone()),
            key_name: Some(key.name.clone()),
            project: live.project.clone(),
            workflow: request.workflow.clone(),
            mode: if request.is_async { "async" } else { "sync" }.into(),
            status: 0,
            ms: 0,
            result: String::new(),
            execution_id: None,
        };
        match self.admit_and_call(me, live, key, &request, at) {
            Ok(answer) => answer,
            Err(answer) => {
                let entry = LogEntry {
                    status: answer.status,
                    ms: started.elapsed().as_millis() as u64,
                    result: answer.body["error"].as_str().unwrap_or("ok").to_string(),
                    ..entry
                };
                self.log(&live.node, me, entry);
                answer
            }
        }
    }

    fn poll(&self, key: KeyRecord, job: &str) -> Answer {
        match lock(&self.jobs).get(job) {
            Some(job) if job.key_id == key.id => Answer::new(200, json!(job)),
            _ => Answer::error(404, format!("no job `{job}` for this key")),
        }
    }

    /// What a budget scope has spent, from the store.
    fn spent(&self, scope: &str) -> Spent {
        self.global()
            .ok()
            .and_then(|s| s.get_json(&format!("spent/{scope}")).ok().flatten())
            .unwrap_or_default()
    }

    /// Scope, exposure, input and policies; then the child. `Err` is a refusal to log
    /// now; `Ok` is the answer of a request that ran (logged when its child ends).
    fn admit_and_call(
        self: &Arc<Self>,
        me: &NodeRef,
        live: &Live,
        key: KeyRecord,
        request: &Request,
        at: u64,
    ) -> Result<Answer, Answer> {
        let node = &live.node;
        let workflow = request.workflow.clone().unwrap_or_default();
        if !key.allows(&live.project, &workflow) {
            return Err(Answer::error(
                403,
                "the key's scope does not cover this workflow",
            ));
        }
        if !self.exposed(node).contains(&workflow) {
            return Err(Answer::error(
                404,
                format!("workflow `{workflow}` is not exposed here"),
            ));
        }
        let Value::Object(input) = &request.input else {
            return Err(Answer::error(
                400,
                "the body must be a JSON object of input ports",
            ));
        };
        let (default, table) = self
            .policies(node)
            .map_err(|e| Answer::error(503, e.to_string()))?;
        let here = format!("{}/{}", me.execution_id, me.node_id);
        let mut rules: Vec<(String, Policy)> = Vec::new();
        if !key.policy.is_empty() {
            rules.push((format!("key:{}", key.id), key.policy));
        }
        let per_workflow = table.get(&workflow).copied().unwrap_or(default);
        if !per_workflow.is_empty() {
            rules.push((format!("{here}/workflow:{workflow}"), per_workflow));
        }
        for name in [
            format!("key:{}", key.id),
            format!("key:{}/{workflow}", key.id),
        ] {
            if let Some(policy) = table.get(&name) {
                rules.push((format!("{here}/{name}"), *policy));
            }
        }
        let job = if request.is_async {
            Some(format!(
                "j-{}",
                random_hex(8).map_err(|e| Answer::error(503, e.to_string()))?
            ))
        } else {
            None
        };
        // Spent budgets are read and the request counted under one lock, the same `settle`
        // charges under: concurrent requests cannot all slip under one budget.
        let (scopes, budgets): (Vec<String>, Vec<String>) = {
            let mut counters = lock(&self.counters);
            let spent: Vec<(String, Policy, Spent)> = rules
                .into_iter()
                .map(|(scope, policy)| {
                    let spent = if policy.budget_requests > 0 || policy.budget_ms > 0 {
                        self.spent(&scope)
                    } else {
                        Spent::default()
                    };
                    (scope, policy, spent)
                })
                .collect();
            counters.admit(&spent, at).map_err(
                |Refused {
                     reason,
                     retry_after_s,
                 }| Answer {
                    status: 429,
                    // The scope names execution and node: the outside sees only the policy.
                    body: json!({ "error": reason.replace(&format!("{here}/"), "") }),
                    retry_after_s,
                },
            )?;
            let budgets = spent
                .iter()
                .filter(|(_, p, _)| p.budget_requests > 0 || p.budget_ms > 0)
                .map(|(s, _, _)| s.clone())
                .collect();
            (spent.into_iter().map(|(s, _, _)| s).collect(), budgets)
        };
        let inputs: Vec<(String, Value)> =
            input.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        lock(&self.flights).registering += 1;
        let called = self.ctx.call_workflow(me, &workflow, inputs);
        let (waiter, outcome) = mpsc::channel();
        let (child, early) = {
            let mut flights = lock(&self.flights);
            flights.registering -= 1;
            let early = match &called {
                Ok(child) => {
                    flights.by_child.insert(
                        child.clone(),
                        Flight {
                            me: me.clone(),
                            key: key.clone(),
                            project: live.project.clone(),
                            workflow: workflow.clone(),
                            mode: if request.is_async { "async" } else { "sync" },
                            started: Instant::now(),
                            at_ms: at,
                            scopes: scopes.clone(),
                            budgets,
                            waiter: (!request.is_async).then_some(waiter),
                            job: job.clone(),
                            returned: None,
                            outputs: None,
                        },
                    );
                    flights.early.remove(child).unwrap_or_default()
                }
                Err(_) => Vec::new(),
            };
            if flights.registering == 0 {
                // Nobody else is between a call and its flight: what is left is news of
                // children that will never have one (stopped endpoints).
                flights.early.clear();
            }
            (called, early)
        };
        let child = match child {
            Ok(child) => child,
            Err(error) => {
                lock(&self.counters).refund(&scopes, at);
                let status = if matches!(error, ModuleError::InvalidParams(_)) {
                    400
                } else {
                    503
                };
                return Err(Answer::error(status, error.to_string()));
            }
        };
        if let Some(job) = &job {
            self.insert_job(job, &key, &workflow, &child, at);
        }
        for news in early {
            self.child_news(&child, news);
        }
        if let Some(job) = job {
            return Ok(Answer::new(
                202,
                json!({ "job": job, "execution_id": child }),
            ));
        }
        let timeout = Duration::from_millis(self.number(node, "timeout_ms").max(1));
        Ok(match outcome.recv_timeout(timeout) {
            Ok(outcome) => Self::sync_answer(&child, outcome),
            Err(mpsc::RecvTimeoutError::Timeout) => self.to_job(&child, &outcome),
            Err(mpsc::RecvTimeoutError::Disconnected) => Answer::error(503, "the endpoint stopped"),
        })
    }

    fn sync_answer(child: &str, outcome: Outcome) -> Answer {
        match outcome {
            Outcome::Returned { port, value } => Answer::new(
                200,
                json!({ "execution_id": child, "outputs": { port: value } }),
            ),
            Outcome::Ended { state } => Answer::new(
                502,
                json!({ "error": "the workflow ended without an answer", "state": state,
                        "execution_id": child }),
            ),
        }
    }

    fn insert_job(&self, job: &str, key: &KeyRecord, workflow: &str, child: &str, at: u64) {
        lock(&self.jobs).insert(
            job.into(),
            Job {
                job: job.into(),
                key_id: key.id.clone(),
                workflow: workflow.into(),
                execution_id: child.into(),
                status: "running".into(),
                outputs: None,
                error: None,
                created_ms: at,
                finished_ms: None,
            },
        );
    }

    /// A sync request ran out of time: it goes on as a job the caller can poll. The
    /// handoff happens under the flights lock, so an answer that came meanwhile is either
    /// already in `outcome` (and answered now) or goes to the job.
    fn to_job(&self, child: &str, outcome: &mpsc::Receiver<Outcome>) -> Answer {
        let mut flights = lock(&self.flights);
        let Some(flight) = flights.by_child.get_mut(child) else {
            drop(flights);
            return match outcome.try_recv() {
                Ok(outcome) => Self::sync_answer(child, outcome),
                Err(_) => Answer::error(503, "the endpoint stopped"),
            };
        };
        flight.waiter = None;
        if let Ok(outcome) = outcome.try_recv() {
            return Self::sync_answer(child, outcome);
        }
        let Ok(hex) = random_hex(8) else {
            return Answer::error(504, "no answer in time");
        };
        let job = format!("j-{hex}");
        flight.job = Some(job.clone());
        let (key, workflow, at) = (flight.key.clone(), flight.workflow.clone(), flight.at_ms);
        self.insert_job(&job, &key, &workflow, child, at);
        Answer::new(
            504,
            json!({ "error": "no answer in time; poll the job", "job": job }),
        )
    }

    fn child(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let news: ChildEvent = parse(call.args)?;
        let mut flights = lock(&self.flights);
        if !flights.by_child.contains_key(&news.execution_id) {
            // Early news is possible only while a call waits for its flight; anything
            // else is about a child whose request is already settled.
            if flights.registering > 0 {
                flights
                    .early
                    .entry(news.execution_id.clone())
                    .or_default()
                    .push(news.kind);
            }
            return Ok(Value::Null);
        }
        drop(flights);
        self.child_news(&news.execution_id, news.kind);
        Ok(Value::Null)
    }

    fn child_news(&self, child: &str, news: ChildEventKind) {
        match news {
            ChildEventKind::Returned { port, value, .. } => {
                let mut flights = lock(&self.flights);
                let Some(flight) = flights.by_child.get_mut(child) else {
                    return;
                };
                flight.returned = Some(flight.started.elapsed().as_millis() as u64);
                flight.outputs = Some(json!({ port.clone(): value.clone() }));
                if let Some(waiter) = flight.waiter.take() {
                    let _ = waiter.send(Outcome::Returned { port, value });
                }
                let done = flight.job.clone().zip(flight.outputs.clone());
                drop(flights);
                if let Some((job, outputs)) = done {
                    self.finish_job(&job, Some(outputs), None);
                }
            }
            ChildEventKind::Ended { state } => {
                let Some(mut flight) = lock(&self.flights).by_child.remove(child) else {
                    return;
                };
                let state = json!(state);
                if flight.returned.is_none() {
                    if let Some(waiter) = flight.waiter.take() {
                        let _ = waiter.send(Outcome::Ended {
                            state: state.clone(),
                        });
                    }
                }
                let node = lock(&self.endpoints)
                    .get(&key_of(&flight.me))
                    .map(|live| live.node.clone());
                let (status, result) = match flight.returned {
                    Some(_) => (200, "ok".to_string()),
                    None => (502, format!("ended without an answer: {state}")),
                };
                let max = node.as_ref().map_or(0, |node| self.number(node, "log_max"));
                self.settle(child, flight, status, &result, node.as_ref(), max);
            }
        }
    }

    /// Marks a job over, once: what comes after it finished changes nothing.
    fn finish_job(&self, job: &str, outputs: Option<Value>, error: Option<String>) {
        let mut jobs = lock(&self.jobs);
        if let Some(job) = jobs.get_mut(job).filter(|j| j.finished_ms.is_none()) {
            job.status = if error.is_some() { "failed" } else { "done" }.into();
            job.outputs = outputs;
            job.error = error;
            job.finished_ms = Some(now_ms());
        }
    }

    /// A request is over: charge its budgets and free its slots under one lock, finish its
    /// job if it has not, log it (and emit it if its endpoint still runs).
    fn settle(
        &self,
        child: &str,
        flight: Flight,
        status: u16,
        result: &str,
        node: Option<&Node>,
        log_max: u64,
    ) {
        let ms = flight
            .returned
            .unwrap_or_else(|| flight.started.elapsed().as_millis() as u64);
        {
            let mut counters = lock(&self.counters);
            if let Ok(store) = self.global() {
                for scope in &flight.budgets {
                    let mut spent = self.spent(scope);
                    spent.requests += 1;
                    spent.ms += ms;
                    let _ = store.set_json(&format!("spent/{scope}"), &spent);
                }
            }
            counters.release(&flight.scopes);
        }
        if let Some(job) = &flight.job {
            match &flight.outputs {
                Some(outputs) => self.finish_job(job, Some(outputs.clone()), None),
                None => self.finish_job(job, None, Some(result.to_string())),
            }
        }
        let entry = LogEntry {
            at_ms: flight.at_ms,
            key_id: Some(flight.key.id.clone()),
            key_name: Some(flight.key.name.clone()),
            project: flight.project.clone(),
            workflow: Some(flight.workflow.clone()),
            mode: flight.mode.into(),
            status,
            ms,
            result: if status == 200 {
                "ok".into()
            } else {
                result.into()
            },
            execution_id: Some(child.into()),
        };
        match node {
            Some(node) => self.log(node, &flight.me, entry),
            None => self.write_log(&entry, log_max),
        }
        self.prune_jobs();
    }

    /// Finished jobs older than the longest `job_ttl_ms` of a live endpoint go.
    fn prune_jobs(&self) {
        let ttl = lock(&self.endpoints)
            .values()
            .map(|live| self.number(&live.node, "job_ttl_ms"))
            .max()
            .unwrap_or(0);
        let now = now_ms();
        lock(&self.jobs).retain(|_, job| job.finished_ms.is_none_or(|f| f + ttl > now));
    }

    fn engines_list(&self) -> Vec<Value> {
        let mut list: Vec<Value> = lock(&self.engines)
            .iter()
            .map(|((execution, node), engine)| {
                json!({ "execution_id": execution, "node_id": node,
                        "url": format!("http://{}", engine.addr) })
            })
            .collect();
        list.sort_by_key(|v| v["url"].to_string());
        list
    }

    /// `service.engine@1` as the `http` node serves it.
    fn engine_call(&self, name: &str, call: NodeCall) -> Result<Value, ModuleError> {
        let engine = lock(&self.engines)
            .get(&(call.execution_id.clone(), call.node.id.clone()))
            .cloned()
            .ok_or_else(|| {
                ModuleError::Failed(format!("engine `{}` is not running", call.node.id))
            })?;
        if name == NEXT {
            let wait = call.args["wait_ms"].as_u64().unwrap_or(0);
            return Ok(engine
                .next(Duration::from_millis(wait))
                .unwrap_or(Value::Null));
        }
        let reply: http::Reply = parse(call.args)?;
        Ok(json!({ "delivered": engine.reply(reply) }))
    }
}

/// The endpoint's loop: takes requests from its engine, one thread each, until it stops.
fn pull(inner: std::sync::Weak<Inner>, me: NodeRef, live: Arc<Live>) {
    while !live.stopping.load(Ordering::SeqCst) {
        let Some(core) = inner.upgrade() else {
            return;
        };
        let wait = core.number(&live.node, "pull_ms").max(1);
        let Some(engine) = core.ctx.resolve(&me, ENGINE_PORT).into_iter().next() else {
            drop(core);
            std::thread::sleep(Duration::from_millis(wait));
            continue;
        };
        let request = match core.ctx.call(&engine, NEXT, json!({ "wait_ms": wait })) {
            Ok(Value::Null) => continue,
            Ok(request) => request,
            Err(_) => {
                drop(core);
                std::thread::sleep(Duration::from_millis(wait));
                continue;
            }
        };
        let Ok(request) = serde_json::from_value::<Request>(request) else {
            continue;
        };
        let max = core.number(&live.node, "workers").max(1) as usize;
        if live.workers.fetch_add(1, Ordering::SeqCst) >= max {
            live.workers.fetch_sub(1, Ordering::SeqCst);
            answer(
                &core,
                &engine,
                &request.id,
                Answer::error(503, "the endpoint is busy"),
            );
            continue;
        }
        let (core2, me2, live2) = (core.clone(), me.clone(), live.clone());
        let spawned = std::thread::Builder::new()
            .name("regulus-service-worker".into())
            .spawn(move || {
                let id = request.id.clone();
                let reply = core2.handle(&me2, &live2, request);
                answer(&core2, &engine, &id, reply);
                live2.workers.fetch_sub(1, Ordering::SeqCst);
            });
        if spawned.is_err() {
            live.workers.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

fn answer(core: &Inner, engine: &Endpoint, id: &str, answer: Answer) {
    let reply = json!({ "id": id, "status": answer.status, "body": answer.body,
                        "retry_after_s": answer.retry_after_s });
    if let Err(error) = core.ctx.call(engine, REPLY, reply) {
        eprintln!("regulus: service: cannot answer `{id}`: {error}");
    }
}

impl Service {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Self::inner(ModuleCtx::default())),
        }
    }

    fn inner(ctx: ModuleCtx) -> Inner {
        Inner {
            manifest: parse_manifest(MANIFEST).expect("service manifest is valid"),
            ctx,
            endpoints: Mutex::default(),
            engines: Mutex::default(),
            flights: Mutex::default(),
            jobs: Mutex::default(),
            counters: Mutex::default(),
            log_next: Mutex::default(),
        }
    }
}

impl Default for Service {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Service {
    fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.inner = Arc::new(Self::inner(ctx.clone()));
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
            method::NODE_CHILD => inner.child(parse(params)?),
            NEXT | REPLY => inner.engine_call(name, parse(params)?),
            KEY_NEW => inner.key_new(parse(params)?),
            KEY_LIST => Ok(Value::Array(
                inner.keys()?.iter().map(KeyRecord::public).collect(),
            )),
            KEY_REVOKE => {
                let params: IdParams = parse(params)?;
                inner.key_revoke(&params.id)
            }
            LOG => Ok(json!(inner.read_log(parse(params)?)?)),
            ENDPOINTS => Ok(Value::Array(inner.engines_list())),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        for live in lock(&self.inner.endpoints).drain().map(|(_, l)| l) {
            live.stopping.store(true, Ordering::SeqCst);
        }
        let engines: Vec<_> = lock(&self.inner.engines).drain().map(|(_, e)| e).collect();
        for engine in engines {
            engine.close();
        }
        self.inner.abandon(None, 0);
        Ok(())
    }
}

/// Parses `name=value,name=value` into a policy, as the CLI and the profile write it.
pub fn parse_policy(text: &str) -> Result<Policy, String> {
    let mut policy = Policy::default();
    for pair in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (name, value) = pair
            .split_once('=')
            .ok_or_else(|| format!("`{pair}` is not name=value"))?;
        let value: u64 = value
            .trim()
            .parse()
            .map_err(|_| format!("`{pair}`: the value must be a whole number"))?;
        policy.set(name.trim(), value)?;
    }
    Ok(policy)
}

/// A policy as a TOML inline table, for `regulus service expose`.
pub fn policy_toml(policy: &Policy) -> String {
    let value = json!(policy);
    let pairs: Vec<String> = value
        .as_object()
        .map(Map::iter)
        .into_iter()
        .flatten()
        .filter(|(_, v)| v.as_u64() != Some(0))
        .map(|(k, v)| format!("{k} = {v}"))
        .collect();
    format!("{{ {} }}", pairs.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_the_nodes_and_methods() {
        let service = Service::new();
        for kind in [ENDPOINT, HTTP] {
            assert!(service.manifest().node_type(kind).is_some(), "{kind}");
        }
        assert_eq!(
            service.manifest().module.methods,
            [KEY_NEW, KEY_LIST, KEY_REVOKE, LOG, ENDPOINTS]
        );
    }

    #[test]
    fn scopes_match_project_and_workflow() {
        let key = |scope: &[&str]| KeyRecord {
            id: "k".into(),
            name: String::new(),
            scope: scope.iter().map(|s| s.to_string()).collect(),
            hint: String::new(),
            hash: String::new(),
            created_ms: 0,
            revoked_ms: None,
            policy: Policy::default(),
        };
        assert!(key(&["p"]).allows("p", "w"));
        assert!(key(&["p/w"]).allows("p", "w"));
        assert!(!key(&["p/w"]).allows("p", "x"));
        assert!(!key(&["q"]).allows("p", "w"));
        assert!(key(&["*/w"]).allows("p", "w"));
        assert!(!valid_scope("p/w/x"));
        assert!(!valid_scope(""));
    }

    #[test]
    fn policies_parse_and_print() {
        let policy = parse_policy("requests=5, window_ms=60000").unwrap();
        assert_eq!(policy.requests, 5);
        assert_eq!(policy_toml(&policy), "{ requests = 5, window_ms = 60000 }");
        assert!(parse_policy("nope=1").is_err());
        assert!(parse_policy("requests=x").is_err());
    }
}
