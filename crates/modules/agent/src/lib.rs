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

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::time::Duration;

use regulus_contract_agent_engine as engine;
use regulus_contract_llm_endpoint as llm;
use regulus_contract_participant as participant;
use regulus_contract_skill as skill;
use regulus_contract_toolset as toolset;
use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Direction, Manifest, Node, NodeCall, NodeEvent};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::{json, Map, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const PROMPT: &str = "agent.prompt";
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
}

/// The values waiting on a node's `prompt` input.
#[derive(Default)]
struct Inbox {
    /// A thread is prompting this node's values.
    busy: bool,
    queue: VecDeque<String>,
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
}

#[derive(Deserialize)]
struct PromptParams {
    execution_id: String,
    node_id: String,
    text: String,
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
        let cwd = self.cwd(&call.node, None)?;
        let (engine, started) = self.open(&me, &call.node, &cwd)?;
        let answer = json(&started)?;
        self.running().insert(
            me,
            Running {
                engine,
                session_id: started.session_id,
                node: call.node,
                used: false,
                cwd,
                workdir: None,
                dropped: false,
                turn: Arc::default(),
            },
        );
        Ok(answer)
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
        let permissions = match self
            .manifest
            .setting(node, "permissions")
            .and_then(Value::as_str)
        {
            Some("allow") => engine::Permissions::Allow,
            _ => engine::Permissions::Reject,
        };
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
    fn deliver(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let delivery: participant::Delivery = parse(call.args)?;
        let text = format!(
            "[message from @{} in channel `{}`]\n{}",
            delivery.from, delivery.channel, delivery.text
        );
        let turn: engine::Turn = parse(self.prompt(PromptParams {
            execution_id: call.execution_id,
            node_id: call.node.id,
            text,
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
    fn prompt(&self, params: PromptParams) -> Result<Value, ModuleError> {
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
            )
        };
        let (engine, session_id, node, renew) = renewed;
        let (engine, session_id) = match renew {
            Some((node, cwd)) => self.renew(&me, &node, &cwd, engine, session_id)?,
            None => (engine, session_id),
        };
        self.timed_turn(&me, &node, engine, session_id, params.text)
    }

    /// The engine's turn, bounded by `turn_timeout_ms`. When it runs out, the turn is
    /// cancelled (`engine.session.cancel`) and given `cancel_grace_ms` to end; an engine
    /// without cancel, or a turn that still does not end, loses its session, and the next
    /// turn opens another. Either way the turn fails.
    fn timed_turn(
        &self,
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
        if limit == 0 {
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
        match answered.recv_timeout(Duration::from_millis(limit)) {
            Ok(outcome) => outcome,
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ModuleError::Failed(
                "the turn ended without an answer".into(),
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => {
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
        }
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
    ) -> Result<(Endpoint, String), ModuleError> {
        self.ctx.call(
            &engine,
            engine::SESSION_STOP,
            json(engine::StopArgs { session_id })?,
        )?;
        let (engine, started) = self.open(me, node, cwd)?;
        let session_id = started.session_id;
        let mut running = self.running();
        match running.get_mut(me) {
            Some(found) => {
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
        let text = as_text(&event.value);
        let max_queue = self.count(&call.node, MAX_QUEUE_FIELD);
        let stop = {
            let mut inboxes = lock(&self.inboxes);
            let inbox = inboxes.entry(me.clone()).or_default();
            if inbox.busy {
                if (inbox.queue.len() as u64) < max_queue {
                    inbox.queue.push_back(text);
                    return Ok(json!({ "queued": inbox.queue.len() }));
                }
                drop(inboxes);
                // One signal, with the value, so whoever listens on `failed` can resend it.
                let echo =
                    usize::try_from(self.count(&call.node, ECHO_FIELD)).unwrap_or(usize::MAX);
                let error = format!(
                    "agent `{}`: a prompt is running and {max_queue} wait (`{MAX_QUEUE_FIELD}`): \
                     this one is dropped: {}",
                    me.node_id,
                    truncated(&text, echo)
                );
                eprintln!("regulus: {error}");
                self.emit(&me, &call.node, FAILED_PORT, json!(error));
                return Ok(json!({ "dropped": true }));
            }
            inbox.busy = true;
            inbox.stop.clone()
        };
        let inner = self.clone();
        let node = call.node;
        std::thread::Builder::new()
            .name(THREAD.into())
            .spawn(move || inner.drain(me, node, stop, text))
            .map_err(|e| ModuleError::Failed(format!("cannot start an agent thread: {e}")))?;
        Ok(json!({ "prompting": true }))
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
    fn drain(&self, me: NodeRef, node: Node, stop: Arc<AtomicBool>, mut text: String) {
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
                Some(next) => text = next,
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
