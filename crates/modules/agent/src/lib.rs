//! The Agent box (docs/12-agents.md §2). Regulus has no agents of its own: this node
//! holds what is wired into it and hands the work to its engine, reached only through
//! the `agent.engine@1` capability. It never knows which CLI is behind it.
//!
//! Prompts reach it three ways: the `agent.prompt` method, a channel (`participant@1`) and
//! its "then" input `prompt`. Values on `prompt` queue per node and run one at a time on a
//! thread of the box, so the bus that delivered them goes on at once; the answer leaves
//! by `reply`, or by `failed`, from that thread (a new chain on the bus, depth 0).
//!
//! The "then" input `workdir` moves the agent: the next turn runs in a session opened
//! there (ACP fixes the working directory when a session opens), and so does every turn
//! after it until another value comes. The `workdir` field is where it starts.
//!
//! During a turn the box reads its engine's session events (`engine.session.events`),
//! keeps them with the conversation in its own history ([`history`]), puts each one on
//! the "then" output `activity` and, with `permissions = "ask"`, hands each permission
//! request to whatever decides over its `decisions` input (`decision@1`).

mod history;

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::Duration;

use regulus_contract_agent_engine as engine;
use regulus_contract_decision as decision;
use regulus_contract_llm_endpoint as llm;
use regulus_contract_participant as participant;
use regulus_contract_skill as skill;
use regulus_contract_toolset as toolset;
use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Direction, Manifest, Node, NodeCall, NodeEvent};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use history::{Conversation, AGENT, SYSTEM, USER};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const PROMPT: &str = "agent.prompt";
/// Queues a prompt like the `prompt` input and answers its turn id at once.
const SEND: &str = "agent.send";
/// The conversation the box recorded, after a cursor, and where its session stands.
const HISTORY: &str = "agent.history";
/// Asks the engine to end the turn in flight.
const CANCEL: &str = "agent.cancel";
/// Who answers the engine's permission requests with `permissions = "ask"`.
const DECISIONS_PORT: &str = "decisions";
/// Each session event, as it is read.
const ACTIVITY_PORT: &str = "activity";
const PERMISSIONS_FIELD: &str = "permissions";
/// `yes`: the box records the conversation in its store.
const HISTORY_FIELD: &str = "history";
/// How many entries, and how many activity events, the history keeps.
const HISTORY_MAX_FIELD: &str = "history_max";
/// `native`: a new execution loads the engine's last session, if the engine can.
const RESUME_FIELD: &str = "resume";
const NATIVE: &str = "native";
/// Milliseconds between reads of the session's events during a turn.
const EVENTS_POLL_FIELD: &str = "events_poll_ms";
/// Milliseconds between reads of a pending decision.
const DECISION_POLL_FIELD: &str = "decision_poll_ms";
/// `yes`: each session event leaves on `activity`.
const ACTIVITY_EMIT_FIELD: &str = "activity_emit";
/// The threads that wait on a decision for a permission request.
const DECISION_THREAD: &str = "regulus-agent-decision";
/// Whose conversation a node continues: `execution` (its own, per execution and node),
/// `workflow` (one per workflow node across executions, the earlier layout) or `named`
/// (the one called `conversation_id`, for any node that names it).
const CONVERSATION_FIELD: &str = "conversation";
const CONVERSATION_ID_FIELD: &str = "conversation_id";
const BY_EXECUTION: &str = "execution";
const BY_WORKFLOW: &str = "workflow";
const BY_NAME: &str = "named";
/// The store prefixes of conversations: `chat/exec/<execution>/<node>`, `chat/id/<id>` and
/// the earlier `conv/<workflow>/<node>`, which `conversation = workflow` still reads.
const EXECUTION_CONVERSATIONS: &str = "chat/exec";
const NAMED_CONVERSATIONS: &str = "chat/id";
const WORKFLOW_CONVERSATIONS: &str = "conv";
/// The kind prefix of the option that refuses a permission request.
const REJECT_KIND: &str = "reject";
const ENGINE_PORT: &str = "engine";
const PROVIDER_PORT: &str = "provider";
const SKILLS_PORT: &str = "skills";
const TOOLS_PORT: &str = "tools";
const HOOKS_PORT: &str = "hooks";
/// The flags of the hook bridge (`regulus hook`) the box fills in for each hook.
const HOOK_FLAGS: [&str; 7] = [
    "--execution",
    "--node",
    "--hook",
    "--event",
    "--timeout-ms",
    "--returns",
    "--fail",
];
const NAME_FIELD: &str = "name";
/// `yes`: a new engine session before each prompt, so nothing carries over.
const FRESH_FIELD: &str = "fresh_context";
/// How many values may wait on `prompt` while one runs.
const MAX_QUEUE_FIELD: &str = "max_queue";
/// How much of a dropped value `failed` repeats, in characters.
const ECHO_FIELD: &str = "failed_echo_chars";
const YES: &str = "yes";
/// The "then" input whose values are prompted into the session.
const PROMPT_PORT: &str = "prompt";
/// The "then" input that moves the following turns to another directory, and the field
/// that says where the first ones run.
const WORKDIR_PORT: &str = "workdir";
const WORKDIR_FIELD: &str = "workdir";
/// The "then" outputs: the reply, the error instead of it, and the tokens it took.
const REPLY_PORT: &str = "reply";
const FAILED_PORT: &str = "failed";
const USAGE_PORT: &str = "usage";
/// The token counts `usage` carries, each under the names engines may give it.
const USAGE_KEYS: [(&str, [&str; 2]); 2] = [
    ("input_tokens", ["input_tokens", "inputTokens"]),
    ("output_tokens", ["output_tokens", "outputTokens"]),
];
/// The threads that prompt values from `prompt`.
const THREAD: &str = "regulus-agent";
/// The threads that wait on a turn with a time limit.
const TURN_THREAD: &str = "regulus-agent-turn";
/// Milliseconds a turn may take before it is cancelled and fails; 0: no limit.
const TURN_TIMEOUT_FIELD: &str = "turn_timeout_ms";
/// Milliseconds a cancelled turn has to end before its session is dropped.
const CANCEL_GRACE_FIELD: &str = "cancel_grace_ms";
/// What the box says it is to channels.
const KIND: &str = "agent";

/// A started agent node: the engine behind it, the session it opened there and what it
/// needs to open another one.
struct Running {
    engine: Endpoint,
    session_id: String,
    /// The node as it started: its fields open the next session with `fresh_context`.
    node: Node,
    /// Whether a prompt went into the session: a fresh one is not opened again.
    used: bool,
    /// The directory the session was opened in.
    cwd: PathBuf,
    /// The last value of the `workdir` input, if one came: it wins over the field.
    workdir: Option<String>,
    /// The session was dropped after a turn ran out of time: the next turn opens another.
    dropped: bool,
    /// Held for a whole prompt: one turn at a time per node, whoever sends it.
    turn: Arc<Mutex<()>>,
    /// The conversation the box records, and its key; none with `history = "no"`.
    conv: Option<Arc<Mutex<Conversation>>>,
    conv_key: Option<String>,
    /// Where the box is in the session's events.
    feed: Arc<Mutex<Feed>>,
    /// What the session answers permission requests with, after the downgrade of `ask`
    /// when nothing decides.
    permissions: engine::Permissions,
    decisions: Option<Endpoint>,
    /// The engine's own id of the session, and whether it is a loaded earlier one.
    native_session_id: Option<String>,
    native_resume: bool,
    /// The turn in flight, `t-<n>`.
    turn_id: Option<String>,
    /// Turn numbers when there is no conversation to count them.
    turns: u64,
}

/// The box's cursor in one session's events.
#[derive(Default)]
struct Feed {
    session_id: String,
    cursor: u64,
    /// False once the engine answered that it has no events.
    unsupported: bool,
    /// Permission requests handed to the decider, and those already answered.
    handled: HashSet<String>,
    resolved: HashSet<String>,
}

/// What `agent.history` still answers about a node that stopped.
struct Finished {
    conv: Option<Arc<Mutex<Conversation>>>,
    native_session_id: Option<String>,
    native_resume: bool,
}

/// A permission request on its way to the decider.
struct Request {
    id: String,
    tool: Option<String>,
    title: String,
    options: Vec<engine::PermissionOption>,
}

/// The values waiting on a node's `prompt` input.
#[derive(Default)]
struct Inbox {
    /// A thread is prompting this node's values.
    busy: bool,
    /// Each value with its turn id, if one was given when it was queued.
    queue: VecDeque<(String, Option<String>)>,
    /// Set by `node/stop` and shutdown: the thread emits nothing more and ends.
    stop: Arc<AtomicBool>,
}

/// How to start the hook bridge: the host's own program and the arguments that reach the
/// running core and name the bridge. Given by the host, the only one that knows them.
#[derive(Debug, Clone, Default)]
pub struct HookBridge {
    pub program: PathBuf,
    pub args: Vec<String>,
}

pub struct Agent {
    inner: Arc<Inner>,
}

/// The box's state, shared with the threads that prompt values from `prompt`.
struct Inner {
    manifest: Manifest,
    ctx: ModuleCtx,
    running: Mutex<HashMap<NodeRef, Running>>,
    inboxes: Mutex<HashMap<NodeRef, Inbox>>,
    bridge: HookBridge,
    /// Every conversation loaded, by its key: a node that goes on with one (its own
    /// execution started again, or a shared one) finds it here without a store.
    conversations: Mutex<HashMap<String, Arc<Mutex<Conversation>>>>,
    /// Which running node holds each conversation: one at a time, so two agents never
    /// write into one transcript or resume one engine session at once.
    holders: Mutex<HashMap<String, NodeRef>>,
    finished: Mutex<HashMap<NodeRef, Finished>>,
}

#[derive(Deserialize)]
struct PromptParams {
    execution_id: String,
    node_id: String,
    text: String,
    /// Given when the value was queued; else the turn takes the next id.
    #[serde(skip)]
    turn: Option<String>,
}

#[derive(Deserialize)]
struct NodeParams {
    execution_id: String,
    node_id: String,
}

#[derive(Deserialize)]
struct HistoryParams {
    execution_id: String,
    node_id: String,
    #[serde(default)]
    since: u64,
    #[serde(default)]
    limit: Option<usize>,
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, ModuleError> {
    serde_json::from_value(value).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn json(value: impl serde::Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(|e| ModuleError::Failed(e.to_string()))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The text a value is prompted as: itself if text, its JSON otherwise.
fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// The token counts of a turn, if its engine gave any: `{ input_tokens, output_tokens }`,
/// each one only when known. `agent.engine@1` has no usage yet; an engine that adds it
/// to its turn as `usage` is read as is.
fn usage(turn: &Value) -> Option<Value> {
    let given = turn.get("usage")?.as_object()?;
    let counts: Map<String, Value> = USAGE_KEYS
        .iter()
        .filter_map(|(name, keys)| {
            let count = keys.iter().find_map(|k| given.get(*k)?.as_u64())?;
            Some(((*name).to_string(), json!(count)))
        })
        .collect();
    (!counts.is_empty()).then_some(Value::Object(counts))
}

/// The panic's message, when it carried one.
fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".into())
}

/// At most `max` characters of `text`, marked when cut.
fn truncated(text: &str, max: usize) -> String {
    match text.char_indices().nth(max) {
        None => text.to_string(),
        Some((at, _)) => format!("{}…", &text[..at]),
    }
}

/// Clears `busy` when a drain thread ends without doing it itself (a panic): the next
/// value starts a thread again instead of queueing forever.
struct Busy<'a> {
    inner: &'a Inner,
    me: &'a NodeRef,
    stop: &'a Arc<AtomicBool>,
    armed: bool,
}

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let mut inboxes = lock(&self.inner.inboxes);
        if let Some(inbox) = inboxes
            .get_mut(self.me)
            .filter(|inbox| Arc::ptr_eq(&inbox.stop, self.stop))
        {
            inbox.busy = false;
        }
    }
}

/// A conversation taken by a node that is starting: given back when dropped armed, so a
/// start that fails, or panics, does not keep it from everyone else.
struct Claim<'a> {
    inner: &'a Inner,
    key: String,
    me: NodeRef,
    armed: bool,
}

impl Drop for Claim<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.inner.release(&self.key, &self.me);
        }
    }
}

/// The directory a value of `workdir` names: a path as text, or an object with a `path`
/// (what a workspace's `info` carries). Empty text: back to the field.
fn workdir_value(value: &Value) -> Result<String, ModuleError> {
    match value {
        Value::String(path) => Ok(path.trim().to_string()),
        Value::Object(map) => match map.get("path") {
            Some(Value::String(path)) => Ok(path.trim().to_string()),
            _ => Err(ModuleError::InvalidParams(
                "a `workdir` object needs a `path` text".into(),
            )),
        },
        other => Err(ModuleError::InvalidParams(format!(
            "`workdir` takes a path, not {other}"
        ))),
    }
}

/// Whether two paths name one directory: `/a/../b` and `/b`, or a link and its target,
/// do. A path that cannot be resolved is compared as it is.
fn same_dir(a: &std::path::Path, b: &std::path::Path) -> bool {
    let real = |p: &std::path::Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    a == b || real(a) == real(b)
}

fn workdir(setting: Option<&str>) -> Result<PathBuf, ModuleError> {
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let dir = match setting.map(str::trim).filter(|s| !s.is_empty()) {
        None => home,
        Some(path) => match path.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(path),
        },
    };
    if !dir.is_absolute() || !dir.is_dir() {
        return Err(ModuleError::Failed(format!(
            "workdir `{}` is not an existing absolute directory",
            dir.display()
        )));
    }
    Ok(dir)
}

impl Agent {
    /// A box on a host that cannot start the hook bridge: wiring a hook fails its start.
    pub fn new() -> Self {
        Self::with_bridge(HookBridge::default())
    }

    pub fn with_bridge(bridge: HookBridge) -> Self {
        Self {
            inner: Arc::new(Inner {
                manifest: parse_manifest(MANIFEST).expect("agent manifest is valid"),
                ctx: ModuleCtx::default(),
                running: Mutex::default(),
                inboxes: Mutex::default(),
                bridge,
                conversations: Mutex::default(),
                holders: Mutex::default(),
                finished: Mutex::default(),
            }),
        }
    }
}

impl Inner {
    /// Each hook wired in, as it describes itself, with the bridge command that asks it
    /// on behalf of this box.
    fn hooks(&self, me: &NodeRef) -> Result<Vec<engine::HookMount>, ModuleError> {
        let wired = self.ctx.resolve(me, HOOKS_PORT);
        if wired.is_empty() {
            return Ok(Vec::new());
        }
        if self.bridge.program.as_os_str().is_empty() {
            return Err(ModuleError::Failed(
                "this host cannot start the hook bridge".into(),
            ));
        }
        wired
            .iter()
            .map(|endpoint| {
                let hook: engine::Hook = parse(self.ctx.call(
                    endpoint,
                    regulus_proto::HOOK_DESCRIBE,
                    Value::Null,
                )?)?;
                let node = endpoint.node().node_id.clone();
                let [execution, at, only, event, timeout, returns, fail] = HOOK_FLAGS;
                let mut args = self.bridge.args.clone();
                for (flag, value) in [
                    (execution, me.execution_id.as_str()),
                    (at, me.node_id.as_str()),
                    (only, node.as_str()),
                    (event, hook.event.as_str()),
                ] {
                    args.extend([flag.to_string(), value.to_string()]);
                }
                if let Some(ms) = hook.timeout_ms {
                    args.extend([timeout.to_string(), ms.to_string()]);
                }
                if !hook.returns.is_empty() {
                    let names: Vec<&str> = hook.returns.iter().map(|e| e.name()).collect();
                    args.extend([returns.to_string(), names.join(",")]);
                }
                if !hook.fail.is_open() {
                    args.extend([fail.to_string(), "closed".to_string()]);
                }
                Ok(engine::HookMount {
                    node,
                    hook,
                    command: self.bridge.program.clone(),
                    args,
                })
            })
            .collect()
    }

    fn slot_ports<'a>(&'a self, node_type: &str) -> impl Iterator<Item = &'a str> {
        self.manifest
            .node_type(node_type)
            .into_iter()
            .flat_map(|t| t.ports.iter())
            .filter(|p| {
                p.direction == Direction::Input
                    && p.capability == llm::CAPABILITY
                    && p.id != PROVIDER_PORT
            })
            .map(|p| p.id.as_str())
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<NodeRef, Running>> {
        lock(&self.running)
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        let node = call.node;
        let cwd = self.cwd(&node, None)?;
        let key = match self.text(&node, HISTORY_FIELD) == YES {
            true => Some(self.conversation_key(&me, &node)?),
            false => None,
        };
        // Held from here; given back if the start fails before the node runs.
        let claim = key.as_deref().map(|key| self.claim(key, &me)).transpose()?;
        let conv = key.as_ref().map(|key| self.conversation(key, &me, &node));
        let decisions = self.ctx.resolve(&me, DECISIONS_PORT).into_iter().next();
        let mut notes = Vec::new();
        let permissions = match self.text(&node, PERMISSIONS_FIELD).as_str() {
            "allow" => engine::Permissions::Allow,
            "ask" if decisions.is_some() => engine::Permissions::Ask,
            "ask" => {
                notes.push(format!(
                    "permissions = ask but nothing is wired into `{DECISIONS_PORT}`: \
                     permission requests are rejected"
                ));
                engine::Permissions::Reject
            }
            _ => engine::Permissions::Reject,
        };
        // A native resume carries the engine's context over; a fresh context wants none.
        let native =
            self.text(&node, RESUME_FIELD) == NATIVE && self.text(&node, FRESH_FIELD) != YES;
        let resume = if native {
            conv.as_ref().and_then(|c| lock(c).native_session_id())
        } else {
            None
        };
        let (engine, started) = self.open(&me, &node, &cwd, permissions, resume.clone())?;
        if let Some(asked) = resume.as_ref().filter(|_| !started.resumed) {
            notes.push(format!(
                "the engine did not resume its session `{asked}`: this is a new session, and \
                 the history here is Regulus' record, not the agent's context"
            ));
        }
        if native && started.capabilities.load_session {
            if let Some(conv) = &conv {
                lock(conv).set_native_session_id(started.native_session_id.clone());
            }
        }
        for note in &notes {
            eprintln!("regulus: agent `{}`: {note}", me.node_id);
            if let Some(conv) = &conv {
                lock(conv).add_entry(SYSTEM, note, None, &me.execution_id);
            }
        }
        let answer = json(&started)?;
        if let Some(mut claim) = claim {
            claim.armed = false;
        }
        self.running().insert(
            me,
            Running {
                engine,
                session_id: started.session_id,
                node,
                used: false,
                cwd,
                workdir: None,
                dropped: false,
                turn: Arc::default(),
                conv,
                conv_key: key,
                feed: Arc::default(),
                permissions,
                decisions,
                native_session_id: started.native_session_id,
                native_resume: started.resumed,
                turn_id: None,
                turns: 0,
            },
        );
        Ok(answer)
    }

    /// A text field: the node's value, else the manifest default, trimmed.
    fn text(&self, node: &Node, field: &str) -> String {
        self.manifest
            .setting(node, field)
            .and_then(Value::as_str)
            .map(str::trim)
            .unwrap_or_default()
            .to_string()
    }

    /// Where the node's conversation lives, in the store of the node's project, as its
    /// `conversation` field says: `chat/exec/<execution>/<node>` by default, so every
    /// execution, and every child execution of one workflow, starts its own;
    /// `chat/id/<conversation_id>` for `named`; `conv/<workflow>/<node>`, the earlier
    /// layout, for `workflow` (a core without a data directory keys that one by
    /// execution).
    fn conversation_key(&self, me: &NodeRef, node: &Node) -> Result<String, ModuleError> {
        match self.text(node, CONVERSATION_FIELD).as_str() {
            BY_WORKFLOW => {
                let depth = me.node_id.split(regulus_proto::BOX_SEPARATOR).count();
                let workflow = self.ctx.node_dir(me).ok().and_then(|dir| {
                    dir.ancestors()
                        .nth(depth)
                        .and_then(|d| d.file_name())
                        .and_then(|d| d.to_str())
                        .map(String::from)
                });
                let scope = workflow.unwrap_or_else(|| me.execution_id.clone());
                Ok(format!("{WORKFLOW_CONVERSATIONS}/{scope}/{}", me.node_id))
            }
            BY_NAME => {
                let id = self.text(node, CONVERSATION_ID_FIELD);
                let fits = |c: char| c.is_ascii_alphanumeric() || "-_.:".contains(c);
                if id.is_empty() || !id.chars().all(fits) {
                    return Err(ModuleError::InvalidParams(format!(
                        "`{CONVERSATION_FIELD} = {BY_NAME}` needs a `{CONVERSATION_ID_FIELD}` \
                         of letters, digits, `-`, `_`, `.` or `:`; got `{id}`"
                    )));
                }
                Ok(format!("{NAMED_CONVERSATIONS}/{id}"))
            }
            "" | BY_EXECUTION => Ok(format!(
                "{EXECUTION_CONVERSATIONS}/{}/{}",
                me.execution_id, me.node_id
            )),
            other => Err(ModuleError::InvalidParams(format!(
                "`{CONVERSATION_FIELD}` is `{BY_EXECUTION}`, `{BY_WORKFLOW}` or `{BY_NAME}`, \
                 not `{other}`"
            ))),
        }
    }

    /// Takes `key` for `me` until it stops. A conversation another running node holds is
    /// refused: two agents in one transcript would mix their turns and resume each
    /// other's engine session.
    fn claim<'a>(&'a self, key: &str, me: &NodeRef) -> Result<Claim<'a>, ModuleError> {
        let mut holders = lock(&self.holders);
        if let Some(holder) = holders.get(key).filter(|holder| *holder != me) {
            return Err(ModuleError::Failed(format!(
                "conversation `{key}` is in use by agent `{}` in execution `{}`: stop that \
                 execution, or give this node its own (`{CONVERSATION_FIELD} = {BY_EXECUTION}`, \
                 or another `{CONVERSATION_ID_FIELD}`)",
                holder.node_id, holder.execution_id
            )));
        }
        holders.insert(key.to_string(), me.clone());
        Ok(Claim {
            inner: self,
            key: key.to_string(),
            me: me.clone(),
            armed: true,
        })
    }

    /// Gives `key` back, if `me` holds it.
    fn release(&self, key: &str, me: &NodeRef) {
        let mut holders = lock(&self.holders);
        if holders.get(key) == Some(me) {
            holders.remove(key);
        }
    }

    /// The conversation at `key`, loaded once: from the store of the node's project, or
    /// in memory when the core has no store.
    fn conversation(&self, key: &str, me: &NodeRef, node: &Node) -> Arc<Mutex<Conversation>> {
        let max = usize::try_from(self.count(node, HISTORY_MAX_FIELD)).unwrap_or(usize::MAX);
        let store = self.ctx.store(Some(me)).ok();
        lock(&self.conversations)
            .entry(key.to_string())
            .or_insert_with(|| {
                Arc::new(Mutex::new(Conversation::load(key.to_string(), store, max)))
            })
            .clone()
    }

    /// The next turn id of a running node.
    fn next_turn(&self, me: &NodeRef) -> Result<String, ModuleError> {
        let mut running = self.running();
        let found = running.get_mut(me).ok_or_else(|| Self::not_running(me))?;
        Ok(match &found.conv {
            Some(conv) => lock(conv).next_turn(),
            None => {
                found.turns += 1;
                format!("t-{}", found.turns)
            }
        })
    }

    /// Writes into the node's conversation, if it keeps one.
    fn record(&self, me: &NodeRef, role: &str, text: &str, turn: Option<&str>) {
        let conv = self.running().get(me).and_then(|r| r.conv.clone());
        if let Some(conv) = conv {
            lock(&conv).add_entry(role, text, turn, &me.execution_id);
        }
    }

    /// Where the node's turns run: the last value of its `workdir` input, else its field.
    fn cwd(&self, node: &Node, given: Option<&str>) -> Result<PathBuf, ModuleError> {
        let field = self
            .manifest
            .setting(node, WORKDIR_FIELD)
            .and_then(Value::as_str);
        workdir(given.filter(|g| !g.is_empty()).or(field))
    }

    /// Opens an engine session for `node` in `cwd` with everything wired into it.
    fn open(
        &self,
        me: &NodeRef,
        node: &Node,
        cwd: &std::path::Path,
        permissions: engine::Permissions,
        resume: Option<String>,
    ) -> Result<(Endpoint, engine::Started), ModuleError> {
        let engine = self
            .ctx
            .resolve(me, ENGINE_PORT)
            .into_iter()
            .next()
            .ok_or_else(|| {
                ModuleError::Failed(format!(
                    "no engine wired into `{ENGINE_PORT}`: connect Claude Code, opencode…"
                ))
            })?;
        let describe = |port: &str| -> Result<Option<llm::LlmEndpoint>, ModuleError> {
            match self.ctx.resolve(me, port).first() {
                None => Ok(None),
                Some(endpoint) => {
                    parse(self.ctx.call(endpoint, llm::DESCRIBE, Value::Null)?).map(Some)
                }
            }
        };
        let provider = describe(PROVIDER_PORT)?;
        // Every other `llm.endpoint` input of the box is a slot named after its port, so a
        // new slot is a manifest change (docs/12-agents.md §6.5).
        let mut slots = BTreeMap::new();
        for port in self.slot_ports(&node.node_type) {
            if let Some(endpoint) = describe(port)? {
                slots.insert(port.to_string(), endpoint);
            }
        }
        let skills = self
            .ctx
            .resolve(me, SKILLS_PORT)
            .iter()
            .map(|endpoint| parse(self.ctx.call(endpoint, skill::DESCRIBE, Value::Null)?))
            .collect::<Result<Vec<skill::Skill>, _>>()?;
        // Tools are mounted in the box's own name: the agent cannot pick another one.
        let caller = self.name(node);
        let tools = self
            .ctx
            .resolve(me, TOOLS_PORT)
            .iter()
            .map(|endpoint| {
                let args = json(toolset::MountArgs {
                    caller: caller.clone(),
                })?;
                parse(self.ctx.call(endpoint, toolset::MOUNT, args)?)
            })
            .collect::<Result<Vec<toolset::Mount>, _>>()?;
        let hooks = self.hooks(me)?;
        let args = engine::StartArgs {
            cwd: cwd.to_path_buf(),
            permissions,
            provider,
            slots,
            skills,
            tools,
            hooks,
            resume,
        };
        let started: engine::Started =
            parse(self.ctx.call(&engine, engine::SESSION_START, json(args)?)?)?;
        Ok((engine, started))
    }

    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        // Waiting values are dropped; the one in flight ends with its session, below.
        if let Some(inbox) = lock(&self.inboxes).remove(&me) {
            inbox.stop.store(true, Ordering::SeqCst);
        }
        // Out of the map before the engine is called: a slow engine must not hold the lock
        // every other node needs.
        let removed = self.running().remove(&me);
        if let Some(key) = removed.as_ref().and_then(|r| r.conv_key.as_deref()) {
            self.release(key, &me);
        }
        if let Some(running) = &removed {
            lock(&self.finished).insert(
                me.clone(),
                Finished {
                    conv: running.conv.clone(),
                    native_session_id: running.native_session_id.clone(),
                    native_resume: running.native_resume,
                },
            );
        }
        if let Some(running) = removed {
            let args = engine::StopArgs {
                session_id: running.session_id,
            };
            self.ctx
                .call(&running.engine, engine::SESSION_STOP, json(args)?)?;
        }
        Ok(Value::Null)
    }

    /// The name channels and tools know the agent by: its `name` field, or the node id.
    fn name(&self, node: &regulus_proto::Node) -> String {
        self.manifest
            .setting(node, NAME_FIELD)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&node.id)
            .to_string()
    }

    /// The agent as a participant: its name and where its session stands, asked to the
    /// engine (ACP knows when a turn is running).
    fn describe(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let name = self.name(&call.node);
        let me = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        let running = self
            .running()
            .get(&me)
            .map(|r| (r.engine.clone(), r.session_id.clone()));
        let state = match running {
            None => participant::State::Gone,
            Some((engine, session_id)) => {
                let args = json(engine::StatusArgs { session_id })?;
                match self.ctx.call(&engine, engine::SESSION_STATUS, args) {
                    Ok(answer) => match parse::<engine::Status>(answer)?.state {
                        engine::SessionState::Idle => participant::State::Idle,
                        engine::SessionState::Busy => participant::State::Busy,
                        engine::SessionState::Ended => participant::State::Gone,
                    },
                    Err(ModuleError::UnknownMethod(_)) => participant::State::Unknown,
                    Err(e) => return Err(e),
                }
            }
        };
        json(participant::Identity {
            name,
            kind: KIND.into(),
            state,
        })
    }

    /// A message from a channel, prompted into the session as the user would type it.
    fn deliver(self: &Arc<Self>, call: NodeCall) -> Result<Value, ModuleError> {
        let delivery: participant::Delivery = parse(call.args)?;
        let text = format!(
            "[message from @{} in channel `{}`]\n{}",
            delivery.from, delivery.channel, delivery.text
        );
        let turn: engine::Turn = parse(self.prompt(PromptParams {
            execution_id: call.execution_id,
            node_id: call.node.id,
            text,
            turn: None,
        })?)?;
        json(participant::Reply { text: turn.text })
    }

    /// A whole number field: the node's value (`16` or `16.0`), else the manifest default.
    fn count(&self, node: &Node, field: &str) -> u64 {
        let whole = |value: &Value| {
            value
                .as_u64()
                .or_else(|| value.as_f64().filter(|n| *n >= 0.0).map(|n| n as u64))
        };
        node.config
            .get(field)
            .and_then(whole)
            .or_else(|| {
                let field = self.manifest.node_type(&node.node_type)?.field(field)?;
                field.default.as_ref().and_then(whole)
            })
            .unwrap_or_default()
    }

    fn not_running(me: &NodeRef) -> ModuleError {
        ModuleError::InvalidParams(format!(
            "no running agent `{}` in execution `{}`",
            me.node_id, me.execution_id
        ))
    }

    /// One turn, whoever sends it. Turns of one node go one at a time; with
    /// `fresh_context`, each one in a session of its own.
    fn prompt(self: &Arc<Self>, params: PromptParams) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: params.execution_id,
            node_id: params.node_id,
        };
        let turn = self
            .running()
            .get(&me)
            .map(|r| r.turn.clone())
            .ok_or_else(|| Self::not_running(&me))?;
        let _turn = lock(&turn);
        let turn_id = match params.turn {
            Some(id) => id,
            None => self.next_turn(&me)?,
        };
        self.record(&me, USER, &params.text, Some(&turn_id));
        if let Some(found) = self.running().get_mut(&me) {
            found.turn_id = Some(turn_id.clone());
        }
        let outcome = self.run_turn(&me, params.text);
        if let Some(found) = self.running().get_mut(&me) {
            found.turn_id = None;
        }
        match &outcome {
            Ok(answer) => {
                let text = answer
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                self.record(&me, AGENT, text, Some(&turn_id));
            }
            Err(error) => self.record(&me, SYSTEM, &error.to_string(), Some(&turn_id)),
        }
        outcome
    }

    /// The turn itself, in the node's session or in a new one when it must move.
    fn run_turn(self: &Arc<Self>, me: &NodeRef, text: String) -> Result<Value, ModuleError> {
        let me = me.clone();
        let renewed = {
            let mut running = self.running();
            let found = running.get_mut(&me).ok_or_else(|| Self::not_running(&me))?;
            // A `workdir` that names no directory fails the turn: it never runs elsewhere.
            let cwd = self.cwd(&found.node, found.workdir.as_deref())?;
            let used = std::mem::replace(&mut found.used, true);
            let fresh = self
                .manifest
                .setting(&found.node, FRESH_FIELD)
                .and_then(Value::as_str)
                == Some(YES);
            // A session keeps the directory it opened in: moving is opening another.
            let renew = (fresh && used || !same_dir(&cwd, &found.cwd) || found.dropped)
                .then(|| (found.node.clone(), cwd));
            (
                found.engine.clone(),
                found.session_id.clone(),
                found.node.clone(),
                renew,
                found.permissions,
            )
        };
        let (engine, session_id, node, renew, permissions) = renewed;
        let (engine, session_id) = match renew {
            Some((node, cwd)) => self.renew(&me, &node, &cwd, engine, session_id, permissions)?,
            None => (engine, session_id),
        };
        self.timed_turn(&me, &node, engine, session_id, text)
    }

    /// Whether the box reads the session's events during a turn: it records them, puts
    /// them on `activity` or answers permission requests through a decider.
    fn watching(&self, me: &NodeRef, node: &Node) -> bool {
        let running = self.running();
        let Some(found) = running.get(me) else {
            return false;
        };
        let emits =
            self.text(node, ACTIVITY_EMIT_FIELD) == YES && node.port(ACTIVITY_PORT).is_some();
        let asks = found.permissions == engine::Permissions::Ask && found.decisions.is_some();
        (found.conv.is_some() || emits || asks) && !lock(&found.feed).unsupported
    }

    /// Reads what happened in the node's session since the last read: records it, puts
    /// it on `activity` and hands new permission requests to the decider.
    fn sync(self: &Arc<Self>, me: &NodeRef) {
        let found = self.running().get(me).map(|r| {
            (
                r.engine.clone(),
                r.session_id.clone(),
                r.feed.clone(),
                r.conv.clone(),
                r.turn_id.clone(),
                r.node.clone(),
                r.permissions,
                r.decisions.clone(),
            )
        });
        let Some((engine, session_id, feed, conv, turn, node, permissions, decisions)) = found
        else {
            return;
        };
        let mut feed_now = lock(&feed);
        if feed_now.session_id != session_id {
            // A new session numbers its events from the start.
            *feed_now = Feed {
                session_id: session_id.clone(),
                unsupported: feed_now.unsupported,
                ..Feed::default()
            };
        }
        if feed_now.unsupported {
            return;
        }
        let args = engine::EventsArgs {
            session_id: session_id.clone(),
            since: feed_now.cursor,
        };
        let answer =
            json(args).and_then(|args| self.ctx.call(&engine, engine::SESSION_EVENTS, args));
        let events: engine::Events = match answer.and_then(parse) {
            Ok(events) => events,
            Err(ModuleError::UnknownMethod(_)) => {
                feed_now.unsupported = true;
                return;
            }
            // The session may be gone; the next read tries again.
            Err(_) => return,
        };
        feed_now.cursor = events.next;
        let emit = self.text(&node, ACTIVITY_EMIT_FIELD) == YES;
        for event in events.events {
            match &event.kind {
                engine::SessionEventKind::PermissionResolved { id, .. } => {
                    feed_now.resolved.insert(id.clone());
                }
                engine::SessionEventKind::PermissionRequest {
                    id,
                    tool,
                    title,
                    options,
                } if permissions == engine::Permissions::Ask => {
                    if let Some(decider) = decisions
                        .as_ref()
                        .filter(|_| feed_now.handled.insert(id.clone()))
                    {
                        let request = Request {
                            id: id.clone(),
                            tool: tool.clone(),
                            title: title.clone(),
                            options: options.clone(),
                        };
                        self.hand_over(me, &node, &engine, &session_id, decider, &feed, request);
                    }
                }
                _ => {}
            }
            if let Some(conv) = &conv {
                lock(conv).add_activity(turn.as_deref(), event.clone());
            }
            if emit {
                match serde_json::to_value(&event) {
                    Ok(value) => self.emit(me, &node, ACTIVITY_PORT, value),
                    Err(error) => eprintln!("regulus: agent `{}`: {error}", me.node_id),
                }
            }
        }
    }

    /// Asks the decider about a permission request on a thread of its own, polls until it
    /// decides and answers the engine with the choice. A refusal with no option, or a
    /// decider that fails, answers with the request's reject option.
    #[allow(clippy::too_many_arguments)]
    fn hand_over(
        self: &Arc<Self>,
        me: &NodeRef,
        node: &Node,
        engine: &Endpoint,
        session_id: &str,
        decider: &Endpoint,
        feed: &Arc<Mutex<Feed>>,
        request: Request,
    ) {
        let inner = self.clone();
        let (me, engine, decider, feed) =
            (me.clone(), engine.clone(), decider.clone(), feed.clone());
        let session_id = session_id.to_string();
        let name = self.name(node);
        let poll = Duration::from_millis(self.count(node, DECISION_POLL_FIELD).max(1));
        let spawned = std::thread::Builder::new()
            .name(DECISION_THREAD.into())
            .spawn(move || {
                let reject = request
                    .options
                    .iter()
                    .find(|o| o.kind.starts_with(REJECT_KIND))
                    .map(|o| o.id.clone());
                let decide = |option: Option<String>| {
                    let args = engine::DecideArgs {
                        session_id: session_id.clone(),
                        request_id: request.id.clone(),
                        option,
                    };
                    let answered = json(args)
                        .and_then(|args| inner.ctx.call(&engine, engine::SESSION_DECIDE, args));
                    if let Err(error) = answered {
                        eprintln!(
                            "regulus: agent `{}`: cannot answer `{}`: {error}",
                            me.node_id, request.id
                        );
                    }
                };
                let args = decision::AskArgs {
                    prompt: format!("Agent `{name}` asks permission: {}", request.title),
                    options: request
                        .options
                        .iter()
                        .map(|o| decision::Choice {
                            id: o.id.clone(),
                            name: o.name.clone(),
                            kind: Some(o.kind.clone()),
                        })
                        .collect(),
                    context: json!({ "tool": request.tool, "title": request.title,
                        "execution_id": me.execution_id, "node_id": me.node_id,
                        "request_id": request.id }),
                    timeout_ms: None,
                };
                let asked = json(args)
                    .and_then(|args| inner.ctx.call(&decider, decision::ASK, args))
                    .and_then(parse::<decision::Asked>);
                let asked = match asked {
                    Ok(asked) => asked,
                    Err(error) => {
                        let note = format!(
                            "could not ask `{DECISIONS_PORT}` ({error}): the request was rejected"
                        );
                        eprintln!("regulus: agent `{}`: {note}", me.node_id);
                        inner.record(&me, SYSTEM, &note, None);
                        decide(reject);
                        return;
                    }
                };
                loop {
                    std::thread::sleep(poll);
                    // Answered elsewhere (a cancel, the engine's own limit) or gone.
                    let live = inner
                        .running()
                        .get(&me)
                        .is_some_and(|r| r.session_id == session_id);
                    if !live || lock(&feed).resolved.contains(&request.id) {
                        return;
                    }
                    let args = json(decision::PollArgs {
                        id: asked.id.clone(),
                    });
                    let polled = args
                        .and_then(|args| inner.ctx.call(&decider, decision::POLL, args))
                        .and_then(parse::<decision::Poll>);
                    match polled {
                        Ok(poll) if poll.pending => {}
                        Ok(poll) => return decide(poll.option.or(reject)),
                        Err(error) => {
                            eprintln!(
                                "regulus: agent `{}`: the decision failed ({error}): rejected",
                                me.node_id
                            );
                            return decide(reject);
                        }
                    }
                }
            });
        if let Err(error) = spawned {
            eprintln!("regulus: agent: cannot start a decision thread: {error}");
        }
    }

    /// The engine's turn, bounded by `turn_timeout_ms`. When it runs out, the turn is
    /// cancelled (`engine.session.cancel`) and given `cancel_grace_ms` to end; an engine
    /// without cancel, or a turn that still does not end, loses its session, and the next
    /// turn opens another. Either way the turn fails.
    fn timed_turn(
        self: &Arc<Self>,
        me: &NodeRef,
        node: &Node,
        engine: Endpoint,
        session_id: String,
        text: String,
    ) -> Result<Value, ModuleError> {
        let args = json(engine::PromptArgs {
            session_id: session_id.clone(),
            text,
        })?;
        let limit = self.count(node, TURN_TIMEOUT_FIELD);
        let watching = self.watching(me, node);
        if limit == 0 && !watching {
            return self.ctx.call(&engine, engine::SESSION_PROMPT, args);
        }
        // The engine's call blocks until the turn ends, so it waits on a thread of its own.
        let (answer, answered) = mpsc::channel();
        let (ctx, to) = (self.ctx.clone(), engine.clone());
        std::thread::Builder::new()
            .name(TURN_THREAD.into())
            .spawn(move || {
                let _ = answer.send(ctx.call(&to, engine::SESSION_PROMPT, args));
            })
            .map_err(|e| ModuleError::Failed(format!("cannot start a turn thread: {e}")))?;
        let began = std::time::Instant::now();
        let every = Duration::from_millis(self.count(node, EVENTS_POLL_FIELD).max(1));
        loop {
            let left =
                (limit > 0).then(|| Duration::from_millis(limit).saturating_sub(began.elapsed()));
            let wait = match (watching, left) {
                (true, Some(left)) => every.min(left),
                (true, None) => every,
                (false, Some(left)) => left,
                (false, None) => Duration::MAX,
            };
            let outcome = match answered.recv_timeout(wait) {
                Ok(outcome) => Some(outcome),
                Err(mpsc::RecvTimeoutError::Disconnected) => Some(Err(ModuleError::Failed(
                    "the turn ended without an answer".into(),
                ))),
                Err(mpsc::RecvTimeoutError::Timeout) if left.is_some_and(|l| l <= wait) => None,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    self.sync(me);
                    continue;
                }
            };
            if watching {
                self.sync(me);
            }
            if let Some(outcome) = outcome {
                return outcome;
            }
            break;
        }
        // The answer may have come just as the time ran out: it is the turn's.
        if let Ok(outcome) = answered.try_recv() {
            return outcome;
        }
        let grace = self.count(node, CANCEL_GRACE_FIELD);
        let cancel = json(engine::CancelArgs {
            session_id: session_id.clone(),
        })?;
        // Only a turn that ends well after the cancel keeps its session; an engine
        // that cannot cancel, or a session that died meanwhile, loses it.
        let how = match self.ctx.call(&engine, engine::SESSION_CANCEL, cancel) {
            Err(error) => format!("the engine could not cancel it ({error})"),
            Ok(_) => match answered.recv_timeout(Duration::from_millis(grace)) {
                Ok(Ok(_)) => {
                    return Err(ModuleError::Failed(format!(
                        "turn timed out after {limit} ms (it was cancelled)"
                    )))
                }
                Ok(Err(error)) => {
                    format!("it ended with an error once cancelled ({error})")
                }
                Err(_) => {
                    format!("it did not end within `{CANCEL_GRACE_FIELD}` ({grace} ms)")
                }
            },
        };
        self.drop_session(me, &engine, session_id);
        Err(ModuleError::Failed(format!(
            "turn timed out after {limit} ms; {how}: its session was dropped"
        )))
    }

    /// Ends a session whose turn would not end; the node's next turn opens another.
    fn drop_session(&self, me: &NodeRef, engine: &Endpoint, session_id: String) {
        if let Some(found) = self
            .running()
            .get_mut(me)
            .filter(|found| found.session_id == session_id)
        {
            found.dropped = true;
        }
        let args = engine::StopArgs { session_id };
        if let Err(error) =
            json(args).and_then(|args| self.ctx.call(engine, engine::SESSION_STOP, args))
        {
            eprintln!(
                "regulus: agent `{}`: cannot drop its session: {error}",
                me.node_id
            );
        }
    }

    /// Ends the node's session and opens another, so the next turn starts from nothing.
    /// A node stopped meanwhile keeps no session: the new one is ended too.
    fn renew(
        &self,
        me: &NodeRef,
        node: &Node,
        cwd: &std::path::Path,
        engine: Endpoint,
        session_id: String,
        permissions: engine::Permissions,
    ) -> Result<(Endpoint, String), ModuleError> {
        self.ctx.call(
            &engine,
            engine::SESSION_STOP,
            json(engine::StopArgs { session_id })?,
        )?;
        let (engine, started) = self.open(me, node, cwd, permissions, None)?;
        let session_id = started.session_id;
        let native = self.text(node, RESUME_FIELD) == NATIVE && started.capabilities.load_session;
        let mut running = self.running();
        match running.get_mut(me) {
            Some(found) => {
                // The newest session is the one a later execution would resume.
                if let Some(conv) = found.conv.as_ref().filter(|_| native) {
                    lock(conv).set_native_session_id(started.native_session_id.clone());
                }
                found.native_session_id = started.native_session_id;
                found.native_resume = false;
                found.engine = engine.clone();
                found.session_id = session_id.clone();
                found.used = true;
                found.cwd = cwd.to_path_buf();
                found.dropped = false;
                Ok((engine, session_id))
            }
            None => {
                drop(running);
                let args = json(engine::StopArgs { session_id })?;
                let _ = self.ctx.call(&engine, engine::SESSION_STOP, args);
                Err(Self::not_running(me))
            }
        }
    }

    /// A value on `prompt`: queued behind the one in flight, or prompted now on a thread
    /// of its own that then takes the queue in order.
    fn event(self: &Arc<Self>, call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(call.args)?;
        if event.port == WORKDIR_PORT {
            return self.move_to(call.execution_id, call.node.id, &event.value);
        }
        if event.port != PROMPT_PORT {
            return Err(ModuleError::InvalidParams(format!(
                "`{}` is not an input of the agent",
                event.port
            )));
        }
        let me = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id.clone(),
        };
        let turn = self.next_turn(&me).ok();
        self.enqueue(me, call.node, as_text(&event.value), turn)
            .map(|(answer, _)| answer)
    }

    /// `agent.send`: queued like a value on `prompt`, with its turn id given back at once.
    fn send(self: &Arc<Self>, params: PromptParams) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: params.execution_id,
            node_id: params.node_id,
        };
        let node = self
            .running()
            .get(&me)
            .map(|r| r.node.clone())
            .ok_or_else(|| Self::not_running(&me))?;
        let turn = self.next_turn(&me)?;
        let (_, dropped) = self.enqueue(me, node, params.text, Some(turn.clone()))?;
        if let Some(error) = dropped {
            return Err(ModuleError::Failed(error));
        }
        Ok(json!({ "turn": turn }))
    }

    /// Queues `text` behind the turn in flight, or prompts it now on a thread of its own
    /// that then takes the queue in order. Also answers why it was dropped, if it was.
    fn enqueue(
        self: &Arc<Self>,
        me: NodeRef,
        node: Node,
        text: String,
        turn: Option<String>,
    ) -> Result<(Value, Option<String>), ModuleError> {
        let call_node = node;
        let max_queue = self.count(&call_node, MAX_QUEUE_FIELD);
        let stop = {
            let mut inboxes = lock(&self.inboxes);
            let inbox = inboxes.entry(me.clone()).or_default();
            if inbox.busy {
                if (inbox.queue.len() as u64) < max_queue {
                    inbox.queue.push_back((text, turn));
                    return Ok((json!({ "queued": inbox.queue.len() }), None));
                }
                drop(inboxes);
                // One signal, with the value, so whoever listens on `failed` can resend it.
                let echo =
                    usize::try_from(self.count(&call_node, ECHO_FIELD)).unwrap_or(usize::MAX);
                let error = format!(
                    "agent `{}`: a prompt is running and {max_queue} wait (`{MAX_QUEUE_FIELD}`): \
                     this one is dropped: {}",
                    me.node_id,
                    truncated(&text, echo)
                );
                eprintln!("regulus: {error}");
                self.record(&me, SYSTEM, &error, turn.as_deref());
                self.emit(&me, &call_node, FAILED_PORT, json!(error));
                return Ok((json!({ "dropped": true }), Some(error)));
            }
            inbox.busy = true;
            inbox.stop.clone()
        };
        let inner = self.clone();
        let node = call_node;
        std::thread::Builder::new()
            .name(THREAD.into())
            .spawn(move || inner.drain(me, node, stop, (text, turn)))
            .map_err(|e| ModuleError::Failed(format!("cannot start an agent thread: {e}")))?;
        Ok((json!({ "prompting": true }), None))
    }

    /// A value on `workdir`: the turns that start from now on run there. Kept even when it
    /// names no directory, so those turns fail instead of running somewhere else; the
    /// delivery says so at once too.
    fn move_to(
        &self,
        execution_id: String,
        node_id: String,
        value: &Value,
    ) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id,
            node_id,
        };
        let path = workdir_value(value)?;
        let mut running = self.running();
        let found = running.get_mut(&me).ok_or_else(|| Self::not_running(&me))?;
        found.workdir = Some(path.clone());
        let checked = self.cwd(&found.node, Some(&path));
        drop(running);
        let cwd = checked?;
        Ok(json!({ "workdir": cwd }))
    }

    /// Prompts `text`, then every value that queued meanwhile, emitting each answer.
    fn drain(
        self: &Arc<Self>,
        me: NodeRef,
        node: Node,
        stop: Arc<AtomicBool>,
        first: (String, Option<String>),
    ) {
        let (mut text, mut turn) = first;
        let mut busy = Busy {
            inner: self,
            me: &me,
            stop: &stop,
            armed: true,
        };
        loop {
            if stop.load(Ordering::SeqCst) {
                return;
            }
            let params = PromptParams {
                execution_id: me.execution_id.clone(),
                node_id: me.node_id.clone(),
                text,
                turn: turn.take(),
            };
            // A panic in the turn is that turn's failure, not the end of the box.
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.prompt(params)))
                    .unwrap_or_else(|panic| {
                        Err(ModuleError::Failed(format!(
                            "the turn panicked: {}",
                            panic_text(panic.as_ref())
                        )))
                    });
            if stop.load(Ordering::SeqCst) {
                return;
            }
            self.answer(&me, &node, outcome);
            let mut inboxes = lock(&self.inboxes);
            let Some(inbox) = inboxes
                .get_mut(&me)
                .filter(|inbox| Arc::ptr_eq(&inbox.stop, &stop))
            else {
                return;
            };
            match inbox.queue.pop_front() {
                Some(next) => (text, turn) = next,
                None => {
                    // Under the same lock as the check: a value arriving now starts a
                    // thread of its own instead of waiting in a queue nobody drains.
                    inbox.busy = false;
                    busy.armed = false;
                    return;
                }
            }
        }
    }

    /// The answer to one value: its usage, if the engine gave it, and the reply; or the
    /// error on `failed`. Never both.
    fn answer(&self, me: &NodeRef, node: &Node, outcome: Result<Value, ModuleError>) {
        let turn = outcome.and_then(|answer| {
            let turn: engine::Turn = parse(answer.clone())?;
            Ok((turn, usage(&answer)))
        });
        match turn {
            Ok((turn, usage)) => {
                if let Some(usage) = usage {
                    self.emit(me, node, USAGE_PORT, usage);
                }
                self.emit(me, node, REPLY_PORT, json!(turn.text));
            }
            Err(error) => {
                let error = format!("agent `{}`: {error}", me.node_id);
                eprintln!("regulus: {error}");
                self.emit(me, node, FAILED_PORT, json!(error));
            }
        }
    }

    /// Emits if the node has `port`: a box without `reply` wired out simply keeps quiet.
    /// An execution that stopped meanwhile takes nothing, and nobody wants it anymore.
    fn emit(&self, me: &NodeRef, node: &Node, port: &str, value: Value) {
        if node.port(port).is_some() {
            let _ = self.ctx.emit(me, port, value);
        }
    }

    /// `agent.history`: the recorded conversation after `since`, the session's events
    /// read first, and where the session stands.
    fn history(self: &Arc<Self>, params: HistoryParams) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: params.execution_id,
            node_id: params.node_id,
        };
        let live = self.running().get(&me).map(|r| {
            (
                r.conv.clone(),
                r.native_session_id.clone(),
                r.native_resume,
                r.engine.clone(),
                r.session_id.clone(),
            )
        });
        let (conv, native_session_id, native_resume, state) = match live {
            Some((conv, native, resumed, engine, session_id)) => {
                self.sync(&me);
                let args = json(engine::StatusArgs { session_id })?;
                let state = match self.ctx.call(&engine, engine::SESSION_STATUS, args) {
                    Ok(answer) => parse::<engine::Status>(answer)
                        .ok()
                        .and_then(|s| serde_json::to_value(s.state).ok())
                        .unwrap_or_else(|| json!("unknown")),
                    Err(_) => json!("unknown"),
                };
                (conv, native, resumed, state)
            }
            None => {
                let finished = lock(&self.finished);
                let found = finished.get(&me).ok_or_else(|| Self::not_running(&me))?;
                (
                    found.conv.clone(),
                    found.native_session_id.clone(),
                    found.native_resume,
                    json!("stopped"),
                )
            }
        };
        let (entries, activity, next) = match conv {
            Some(conv) => {
                let page = lock(&conv).page(params.since, params.limit);
                (json(page.entries)?, json(page.activity)?, page.next)
            }
            None => (json!([]), json!([]), params.since),
        };
        Ok(json!({
            "entries": entries, "activity": activity, "next": next,
            "session": { "engine_session_id": native_session_id,
                         "native_resume": native_resume, "state": state },
        }))
    }

    /// `agent.cancel`: asks the engine to end the turn in flight.
    fn cancel(&self, params: NodeParams) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: params.execution_id,
            node_id: params.node_id,
        };
        let (engine, session_id) = self
            .running()
            .get(&me)
            .map(|r| (r.engine.clone(), r.session_id.clone()))
            .ok_or_else(|| Self::not_running(&me))?;
        let args = json(engine::CancelArgs { session_id })?;
        self.ctx.call(&engine, engine::SESSION_CANCEL, args)?;
        Ok(Value::Null)
    }

    /// Nothing more leaves the box: waiting values are dropped, threads end after the
    /// turn they are in, which ends with the engine's sessions.
    fn shutdown(&self) {
        for (_, inbox) in lock(&self.inboxes).drain() {
            inbox.stop.store(true, Ordering::SeqCst);
        }
    }
}

impl Default for Agent {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Agent {
    fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        let inner = Arc::get_mut(&mut self.inner)
            .ok_or_else(|| ModuleError::Failed("the agent box is already running".into()))?;
        inner.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let inner = &self.inner;
        match name {
            method::NODE_START => inner.start(parse(params)?),
            method::NODE_STOP => inner.stop(parse(params)?),
            method::NODE_EVENT => inner.event(parse(params)?),
            PROMPT => inner.prompt(parse(params)?),
            SEND => inner.send(parse(params)?),
            HISTORY => inner.history(parse(params)?),
            CANCEL => inner.cancel(parse(params)?),
            participant::DESCRIBE => inner.describe(parse(params)?),
            participant::DELIVER => inner.deliver(parse(params)?),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.inner.shutdown();
        Ok(())
    }
}
