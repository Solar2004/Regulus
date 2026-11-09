//! The native engine (docs/12-agents.md §8): an `agent.engine@1` that Regulus runs
//! itself, with no CLI behind it. Each prompt is a loop: ask the model wired into
//! `model`, run the tool calls it asks for through the `tool@1` nodes wired into `tools`
//! (directly, `tool.call`, no MCP), give it what they answered, and again, until it
//! answers without a tool call or the turn runs out of steps.
//!
//! What the loop does is decided by what is wired, not only by its fields: the `hook@1`
//! nodes in `context` add what the model sees (`session_start`, `prompt`), allow, ask or
//! refuse a tool call (`pre_tool`), comment on its result (`post_tool`) and keep the turn
//! going (`stop`). The fields are the defaults for when no hook says otherwise.
//!
//! Every session keeps its events (`engine.session.events`) like the ACP engines do, and
//! a call that must ask waits as a `permission_request` for `engine.session.decide`.

mod hooks;
mod model;

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regulus_contract_agent_engine as engine;
use regulus_contract_agent_engine::{
    PermissionOption, SessionEvent, SessionEventKind, SessionState, Usage,
};
use regulus_contract_llm_endpoint as llm;
use regulus_contract_tool as tool;
use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{hook_event, method, HookAnswer, HookRequest, Manifest, Node, NodeCall};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::hooks::Wired;
use crate::model::{Client, Msg, Reply, Request, ToolCall, ToolDef};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const MODEL_PORT: &str = "model";
const TOOLS_PORT: &str = "tools";
const CONTEXT_PORT: &str = "context";

/// Stop reasons, in ACP's words where ACP has one.
mod stop {
    pub const END_TURN: &str = "end_turn";
    pub const MAX_STEPS: &str = "max_steps";
    pub const CANCELLED: &str = "cancelled";
}

/// The answers a permission request offers.
const ALLOW_ONCE: &str = "allow_once";
const ALLOW_ALWAYS: &str = "allow_always";
const REJECT_ONCE: &str = "reject_once";

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn failed(error: impl ToString) -> ModuleError {
    ModuleError::Failed(error.to_string())
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, ModuleError> {
    serde_json::from_value(value).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn to_json(value: impl Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(failed)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Policy {
    Allow,
    Ask,
    Deny,
}

/// What a `pre_tool` hook's context means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextMeans {
    Note,
    Allow,
    Ask,
}

/// A node's settings, read once when its session starts.
#[derive(Debug, Clone)]
struct Settings {
    model: Option<String>,
    system: String,
    name: Option<String>,
    max_steps: u64,
    max_tool_calls: u64,
    policy: Policy,
    allow_tools: Vec<String>,
    deny_tools: Vec<String>,
    pre_tool_context: ContextMeans,
    stop_on_text: bool,
    temperature: Option<f64>,
    max_tokens: u64,
    history_turns: usize,
    observation_max_chars: usize,
    request_timeout: Duration,
    hook_timeout: Duration,
    events_keep: usize,
    permission_timeout: Duration,
}

impl Settings {
    fn read(manifest: &Manifest, node: &Node) -> Result<Self, ModuleError> {
        let value = |field: &str| manifest.setting(node, field);
        let text = |field: &str| {
            value(field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        let number =
            |field: &str, default: f64| value(field).and_then(Value::as_f64).unwrap_or(default);
        let count = |field: &str, default: f64| number(field, default).max(0.0) as u64;
        let list = |field: &str| -> Vec<String> {
            match value(field) {
                Some(Value::Array(items)) => items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                // A text field is read as names separated by `|` or `,`.
                Some(Value::String(names)) => names
                    .split(['|', ','])
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect(),
                _ => Vec::new(),
            }
        };
        let bad = |field: &str, got: &str, options: &str| {
            ModuleError::InvalidParams(format!(
                "native engine `{}`: `{field}` is {options}, not `{got}`",
                node.id
            ))
        };
        let policy = match text("tool_policy").as_deref().unwrap_or("ask") {
            "allow" => Policy::Allow,
            "ask" => Policy::Ask,
            "deny" => Policy::Deny,
            other => return Err(bad("tool_policy", other, "allow, ask or deny")),
        };
        let pre_tool_context = match text("pre_tool_context").as_deref().unwrap_or("note") {
            "note" => ContextMeans::Note,
            "allow" => ContextMeans::Allow,
            "ask" => ContextMeans::Ask,
            other => return Err(bad("pre_tool_context", other, "note, allow or ask")),
        };
        let stop_on_text = match text("stop_when").as_deref().unwrap_or("no_tool_call") {
            "no_tool_call" => false,
            "text" => true,
            other => return Err(bad("stop_when", other, "no_tool_call or text")),
        };
        let temperature = number("temperature", -1.0);
        Ok(Self {
            model: text("model"),
            system: value("system")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string(),
            name: text("name"),
            max_steps: count("max_steps", 16.0).max(1),
            max_tool_calls: count("max_tool_calls", 32.0),
            policy,
            allow_tools: list("allow_tools"),
            deny_tools: list("deny_tools"),
            pre_tool_context,
            stop_on_text,
            temperature: (temperature >= 0.0).then_some(temperature),
            max_tokens: count("max_tokens", 4096.0).max(1),
            history_turns: count("history_turns", 20.0) as usize,
            observation_max_chars: count("observation_max_chars", 20000.0) as usize,
            request_timeout: Duration::from_millis(count("request_timeout_ms", 120000.0).max(1)),
            hook_timeout: Duration::from_millis(count("hook_timeout_ms", 10000.0).max(1)),
            events_keep: count("events_keep", 2000.0).max(1) as usize,
            permission_timeout: Duration::from_secs_f64(
                number("permission_timeout_seconds", 600.0).max(0.0),
            ),
        })
    }
}

/// A session's events, bounded, oldest dropped first.
struct Events {
    keep: usize,
    state: Mutex<EventsState>,
}

#[derive(Default)]
struct EventsState {
    kept: VecDeque<SessionEvent>,
    seq: u64,
    turns: u64,
    turn: Option<u64>,
}

impl Events {
    fn new(keep: usize) -> Self {
        Self {
            keep: keep.max(1),
            state: Mutex::default(),
        }
    }

    fn push(&self, kind: SessionEventKind) {
        let mut state = lock(&self.state);
        state.seq += 1;
        let event = SessionEvent {
            seq: state.seq,
            at: now_ms(),
            turn: state.turn,
            kind,
        };
        state.kept.push_back(event);
        while state.kept.len() > self.keep {
            state.kept.pop_front();
        }
    }

    fn begin_turn(&self) {
        {
            let mut state = lock(&self.state);
            state.turns += 1;
            state.turn = Some(state.turns);
        }
        self.push(SessionEventKind::TurnStarted);
        self.push(SessionEventKind::State {
            state: SessionState::Busy,
        });
    }

    fn end_turn(&self, stop_reason: &str) {
        self.push(SessionEventKind::TurnEnded {
            stop_reason: stop_reason.to_string(),
        });
        lock(&self.state).turn = None;
        self.push(SessionEventKind::State {
            state: SessionState::Idle,
        });
    }

    fn since(&self, since: u64) -> (Vec<SessionEvent>, u64) {
        let state = lock(&self.state);
        let events: Vec<SessionEvent> = state
            .kept
            .iter()
            .filter(|e| e.seq > since)
            .cloned()
            .collect();
        let next = events.last().map_or(since, |e| e.seq);
        (events, next)
    }
}

/// What wakes a turn that waits: the model's answer, a decision or a cancel.
enum Wake {
    Model(Result<Reply, String>),
    Decided { id: String, option: Option<String> },
    Cancel,
}

/// The part of a session that changes from outside the turn.
#[derive(Default)]
struct Live {
    ended: bool,
    busy: bool,
    cancelled: bool,
    /// Where to wake the turn in flight.
    wake: Option<Sender<Wake>>,
    /// Permission requests waiting, with the options they offer.
    pending: HashMap<String, Vec<PermissionOption>>,
    next_request: u64,
    /// Tools allowed for the rest of the session (`allow_always`).
    always: HashSet<String>,
}

struct Session {
    me: NodeRef,
    settings: Settings,
    client: Client,
    /// The box's answer to permission requests: what `ask` becomes when it cannot ask.
    permissions: engine::Permissions,
    caller: String,
    cwd: PathBuf,
    hooks: Vec<Wired>,
    /// What `session_start` hooks added, for the whole session.
    session_context: String,
    events: Events,
    history: Mutex<VecDeque<Vec<Msg>>>,
    /// Held for a whole turn: prompts to one session queue up.
    turn: Mutex<()>,
    live: Mutex<Live>,
}

/// A turn that ended early.
enum Halt {
    Cancelled,
    Failed(String),
}

/// How a tool call was settled before it runs.
enum Verdict {
    Run,
    Refuse(String),
}

/// The tools of one turn: each name and the node behind it.
struct Toolbox {
    defs: Vec<ToolDef>,
    owners: HashMap<String, Endpoint>,
}

impl Session {
    fn hook_request(&self, event: &str) -> HookRequest {
        HookRequest {
            event: event.to_string(),
            cwd: Some(self.cwd.display().to_string()),
            ..HookRequest::default()
        }
    }

    fn cancelled(&self) -> bool {
        lock(&self.live).cancelled
    }

    /// Lists the tools of every node wired into `tools`. A name seen before is left out.
    fn toolbox(&self, ctx: &ModuleCtx) -> Toolbox {
        let mut toolbox = Toolbox {
            defs: Vec::new(),
            owners: HashMap::new(),
        };
        for endpoint in ctx.resolve(&self.me, TOOLS_PORT) {
            let specs = ctx
                .call(&endpoint, tool::LIST, Value::Null)
                .map_err(|e| e.to_string())
                .and_then(|v| {
                    serde_json::from_value::<Vec<tool::Spec>>(v).map_err(|e| e.to_string())
                });
            let specs = match specs {
                Ok(specs) => specs,
                Err(error) => {
                    eprintln!(
                        "regulus: native engine `{}`: tools of `{}` left out: {error}",
                        self.me.node_id,
                        endpoint.node().node_id
                    );
                    continue;
                }
            };
            for spec in specs {
                if toolbox.owners.contains_key(&spec.name) {
                    eprintln!(
                        "regulus: native engine `{}`: two tools are named `{}`: the one of `{}` is left out",
                        self.me.node_id,
                        spec.name,
                        endpoint.node().node_id
                    );
                    continue;
                }
                toolbox.owners.insert(spec.name.clone(), endpoint.clone());
                toolbox.defs.push(ToolDef {
                    schema: model::schema(&spec.input),
                    name: spec.name,
                    description: spec.description,
                });
            }
        }
        toolbox
    }

    /// Asks the model on a thread of its own, so a cancel ends the wait at once.
    fn ask_model(
        &self,
        rx: &Receiver<Wake>,
        tx: &Sender<Wake>,
        request: Request<'_>,
    ) -> Result<Reply, Halt> {
        let client = self.client.clone();
        let (system, messages, tools) = (
            request.system.to_string(),
            request.messages.to_vec(),
            request.tools.to_vec(),
        );
        let tx = tx.clone();
        std::thread::Builder::new()
            .name("regulus-native-model".into())
            .spawn(move || {
                let answer = client.ask(&Request {
                    system: &system,
                    messages: &messages,
                    tools: &tools,
                });
                let _ = tx.send(Wake::Model(answer));
            })
            .map_err(|e| Halt::Failed(e.to_string()))?;
        loop {
            match rx.recv() {
                Ok(Wake::Model(answer)) => return answer.map_err(Halt::Failed),
                Ok(Wake::Cancel) | Err(_) => return Err(Halt::Cancelled),
                Ok(Wake::Decided { .. }) => {}
            }
        }
    }

    /// Asks whoever decides for this session about `call`, and waits.
    fn ask_permission(
        &self,
        rx: &Receiver<Wake>,
        call: &ToolCall,
        note: Option<&str>,
    ) -> Result<Verdict, Halt> {
        let options = vec![
            PermissionOption {
                id: ALLOW_ONCE.into(),
                kind: ALLOW_ONCE.into(),
                name: "Allow".into(),
            },
            PermissionOption {
                id: ALLOW_ALWAYS.into(),
                kind: ALLOW_ALWAYS.into(),
                name: "Always allow this tool".into(),
            },
            PermissionOption {
                id: REJECT_ONCE.into(),
                kind: REJECT_ONCE.into(),
                name: "Reject".into(),
            },
        ];
        let id = {
            let mut live = lock(&self.live);
            live.next_request += 1;
            let id = format!("p{}", live.next_request);
            live.pending.insert(id.clone(), options.clone());
            id
        };
        let mut title = format!("{} {}", call.name, call.args);
        if let Some(note) = note {
            title = format!("{title}\n\n{note}");
        }
        self.events.push(SessionEventKind::PermissionRequest {
            id: id.clone(),
            tool: Some(call.name.clone()),
            title,
            options,
        });
        let deadline = Instant::now() + self.settings.permission_timeout;
        let answer = loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(Wake::Decided {
                    id: decided,
                    option,
                }) if decided == id => break Ok(option),
                Ok(Wake::Cancel) | Err(RecvTimeoutError::Disconnected) => {
                    break Err(Halt::Cancelled)
                }
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => {
                    lock(&self.live).pending.remove(&id);
                    break Ok(Some(REJECT_ONCE.to_string()));
                }
            }
        };
        let option = match answer {
            Ok(option) => option,
            Err(halt) => {
                lock(&self.live).pending.remove(&id);
                self.events
                    .push(SessionEventKind::PermissionResolved { id, option: None });
                return Err(halt);
            }
        };
        self.events.push(SessionEventKind::PermissionResolved {
            id,
            option: option.clone(),
        });
        match option.as_deref() {
            Some(ALLOW_ALWAYS) => {
                lock(&self.live).always.insert(call.name.clone());
                Ok(Verdict::Run)
            }
            Some(ALLOW_ONCE) => Ok(Verdict::Run),
            Some(_) => Ok(Verdict::Refuse("the user refused it".into())),
            None => Err(Halt::Cancelled),
        }
    }

    /// The fields' answer for `name`, and whether it must ask.
    fn policy(&self, name: &str) -> Policy {
        let settings = &self.settings;
        if settings.deny_tools.iter().any(|t| t == name) {
            return Policy::Deny;
        }
        if settings.allow_tools.iter().any(|t| t == name) || lock(&self.live).always.contains(name)
        {
            return Policy::Allow;
        }
        match settings.policy {
            // Nobody to ask: the box's own answer.
            Policy::Ask => match self.permissions {
                engine::Permissions::Ask => Policy::Ask,
                engine::Permissions::Allow => Policy::Allow,
                engine::Permissions::Reject => Policy::Deny,
            },
            other => other,
        }
    }

    /// Settles a tool call: the `pre_tool` hooks first, then the fields.
    fn settle(
        &self,
        ctx: &ModuleCtx,
        rx: &Receiver<Wake>,
        call: &ToolCall,
        note: &mut Option<String>,
    ) -> Result<Verdict, Halt> {
        let mut request = self.hook_request(hook_event::PRE_TOOL);
        request.tool = Some(call.name.clone());
        request.input = Some(call.args.clone());
        let policy = match hooks::dispatch(ctx, &self.hooks, &request, self.settings.hook_timeout) {
            HookAnswer::Veto { reason } => return Ok(Verdict::Refuse(reason)),
            HookAnswer::Context { text } => {
                let policy = match self.settings.pre_tool_context {
                    ContextMeans::Note => self.policy(&call.name),
                    ContextMeans::Allow => Policy::Allow,
                    ContextMeans::Ask => Policy::Ask,
                };
                *note = Some(text);
                policy
            }
            HookAnswer::Nothing => self.policy(&call.name),
        };
        match policy {
            Policy::Allow => Ok(Verdict::Run),
            Policy::Deny => Ok(Verdict::Refuse(format!(
                "`{}` is not allowed for this agent",
                call.name
            ))),
            Policy::Ask => self.ask_permission(rx, call, note.as_deref()),
        }
    }

    fn cut(&self, text: String) -> String {
        let max = self.settings.observation_max_chars;
        if max == 0 || text.chars().count() <= max {
            return text;
        }
        let mut cut: String = text.chars().take(max).collect();
        cut.push_str("\n[cut]");
        cut
    }

    /// Runs one tool call and returns the message the model gets back.
    fn run_tool(
        &self,
        ctx: &ModuleCtx,
        rx: &Receiver<Wake>,
        toolbox: &Toolbox,
        call: &ToolCall,
        spent: &mut u64,
    ) -> Result<Msg, Halt> {
        self.events.push(SessionEventKind::ToolCall {
            id: call.id.clone(),
            title: call.name.clone(),
            kind: None,
            status: "pending".into(),
        });
        let refuse = |reason: String| {
            self.events.push(SessionEventKind::ToolUpdate {
                id: call.id.clone(),
                status: "failed".into(),
                content: Some(json!(reason)),
            });
            Ok(Msg::Tool {
                id: call.id.clone(),
                content: reason,
                error: true,
            })
        };
        let Some(owner) = toolbox.owners.get(&call.name) else {
            return refuse(format!("there is no tool named `{}`", call.name));
        };
        if let Value::String(raw) = &call.args {
            return refuse(format!("the arguments are not JSON: {raw}"));
        }
        if *spent >= self.settings.max_tool_calls {
            return refuse(format!(
                "the {} tool calls of this turn are spent: answer with what you have",
                self.settings.max_tool_calls
            ));
        }
        let mut note = None;
        if let Verdict::Refuse(reason) = self.settle(ctx, rx, call, &mut note)? {
            return refuse(format!("refused: {reason}"));
        }
        if self.cancelled() {
            return Err(Halt::Cancelled);
        }
        *spent += 1;
        self.events.push(SessionEventKind::ToolUpdate {
            id: call.id.clone(),
            status: "in_progress".into(),
            content: None,
        });
        let args = tool::Call {
            name: call.name.clone(),
            args: call.args.clone(),
            caller: tool::Caller {
                name: self.caller.clone(),
            },
        };
        let outcome = to_json(&args)
            .and_then(|args| ctx.call(owner, tool::CALL, args))
            .and_then(parse::<tool::Outcome>);
        let (data, error) = match outcome {
            Ok(outcome) => (outcome.data, outcome.error),
            Err(error) => (json!(error.to_string()), true),
        };
        self.events.push(SessionEventKind::ToolUpdate {
            id: call.id.clone(),
            status: if error { "failed" } else { "completed" }.into(),
            content: Some(data.clone()),
        });
        let mut content = match &data {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        content = self.cut(content);
        let mut after = self.hook_request(hook_event::POST_TOOL);
        after.tool = Some(call.name.clone());
        after.input = Some(call.args.clone());
        after.text = Some(content.clone());
        let feedback = match hooks::dispatch(ctx, &self.hooks, &after, self.settings.hook_timeout) {
            HookAnswer::Veto { reason } => Some(reason),
            HookAnswer::Context { text } => Some(text),
            HookAnswer::Nothing => None,
        };
        for extra in [note, feedback].into_iter().flatten() {
            content = format!("{content}\n\n{extra}");
        }
        Ok(Msg::Tool {
            id: call.id.clone(),
            content,
            error,
        })
    }

    /// One prompt, to the end of its turn.
    fn prompt(&self, ctx: &ModuleCtx, text: &str) -> Result<engine::Turn, ModuleError> {
        let _turn = lock(&self.turn);
        let (tx, rx) = mpsc::channel();
        {
            let mut live = lock(&self.live);
            if live.ended {
                return Err(failed("the session is closed"));
            }
            live.busy = true;
            live.cancelled = false;
            live.wake = Some(tx.clone());
        }
        self.events.begin_turn();
        let result = self.turn(ctx, text, &rx, &tx);
        {
            let mut live = lock(&self.live);
            live.busy = false;
            live.wake = None;
            live.pending.clear();
        }
        match result {
            Ok(turn) => {
                self.events.end_turn(&turn.stop_reason);
                Ok(turn)
            }
            Err(Halt::Cancelled) => {
                self.events.end_turn(stop::CANCELLED);
                Ok(engine::Turn {
                    stop_reason: stop::CANCELLED.into(),
                    text: String::new(),
                    usage: None,
                })
            }
            Err(Halt::Failed(error)) => {
                self.events.end_turn("error");
                Err(failed(error))
            }
        }
    }

    fn turn(
        &self,
        ctx: &ModuleCtx,
        text: &str,
        rx: &Receiver<Wake>,
        tx: &Sender<Wake>,
    ) -> Result<engine::Turn, Halt> {
        let settings = &self.settings;
        let mut ask = self.hook_request(hook_event::PROMPT);
        ask.text = Some(text.to_string());
        let turn_context = match hooks::dispatch(ctx, &self.hooks, &ask, settings.hook_timeout) {
            HookAnswer::Veto { reason } => {
                return Err(Halt::Failed(format!("a hook refused the prompt: {reason}")))
            }
            HookAnswer::Context { text } => text,
            HookAnswer::Nothing => String::new(),
        };
        let system = [
            settings.system.as_str(),
            &self.session_context,
            &turn_context,
        ]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n");
        let earlier: Vec<Msg> = lock(&self.history).iter().flatten().cloned().collect();
        let mut messages = earlier;
        let first_new = messages.len();
        messages.push(Msg::User(text.to_string()));
        let toolbox = self.toolbox(ctx);
        let mut usage: Option<Usage> = None;
        let mut spent = 0;
        let mut last_text = String::new();
        let mut stop_reason = stop::MAX_STEPS;
        for step in 1..=settings.max_steps {
            if self.cancelled() {
                return Err(Halt::Cancelled);
            }
            let reply = self.ask_model(
                rx,
                tx,
                Request {
                    system: &system,
                    messages: &messages,
                    tools: &toolbox.defs,
                },
            )?;
            if let Some(more) = reply.usage {
                let total = usage.get_or_insert_with(Usage::default);
                total.input_tokens += more.input_tokens;
                total.output_tokens += more.output_tokens;
            }
            if !reply.text.is_empty() {
                self.events.push(SessionEventKind::Text {
                    text: reply.text.clone(),
                });
                last_text = reply.text.clone();
            }
            let ends = reply.calls.is_empty() || (settings.stop_on_text && !reply.text.is_empty());
            if ends {
                messages.push(Msg::Assistant {
                    text: reply.text.clone(),
                    calls: Vec::new(),
                });
                let mut done = self.hook_request(hook_event::STOP);
                done.text = Some(reply.text.clone());
                let more = match hooks::dispatch(ctx, &self.hooks, &done, settings.hook_timeout) {
                    HookAnswer::Veto { reason } => Some(reason),
                    HookAnswer::Context { text } => Some(text),
                    HookAnswer::Nothing => None,
                };
                match more {
                    // A stop hook keeps the turn going, with why as the next user message.
                    Some(why) if step < settings.max_steps => {
                        messages.push(Msg::User(why));
                        continue;
                    }
                    Some(_) => break,
                    None => {
                        stop_reason = stop::END_TURN;
                        break;
                    }
                }
            }
            messages.push(Msg::Assistant {
                text: reply.text.clone(),
                calls: reply.calls.clone(),
            });
            for call in &reply.calls {
                let answer = self.run_tool(ctx, rx, &toolbox, call, &mut spent)?;
                messages.push(answer);
            }
        }
        self.remember(messages.split_off(first_new));
        Ok(engine::Turn {
            stop_reason: stop_reason.to_string(),
            text: last_text,
            usage,
        })
    }

    /// Keeps a finished turn for the next ones, within `history_turns`.
    fn remember(&self, turn: Vec<Msg>) {
        let keep = self.settings.history_turns;
        let mut history = lock(&self.history);
        history.push_back(turn);
        while history.len() > keep {
            history.pop_front();
        }
    }

    fn cancel(&self) {
        let mut live = lock(&self.live);
        live.cancelled = true;
        if let Some(wake) = &live.wake {
            let _ = wake.send(Wake::Cancel);
        }
    }

    fn decide(&self, request_id: &str, option: Option<String>) -> Result<(), String> {
        let mut live = lock(&self.live);
        let options = live
            .pending
            .get(request_id)
            .ok_or_else(|| format!("no pending permission request `{request_id}`"))?;
        if let Some(chosen) = &option {
            if !options.iter().any(|o| &o.id == chosen) {
                return Err(format!("request `{request_id}` has no option `{chosen}`"));
            }
        }
        live.pending.remove(request_id);
        if let Some(wake) = &live.wake {
            let _ = wake.send(Wake::Decided {
                id: request_id.to_string(),
                option,
            });
        }
        Ok(())
    }

    fn end(&self) {
        let mut live = lock(&self.live);
        live.ended = true;
        live.cancelled = true;
        if let Some(wake) = &live.wake {
            let _ = wake.send(Wake::Cancel);
        }
        drop(live);
        self.events.push(SessionEventKind::State {
            state: SessionState::Ended,
        });
    }

    fn state(&self) -> SessionState {
        let live = lock(&self.live);
        if live.ended {
            SessionState::Ended
        } else if live.busy {
            SessionState::Busy
        } else {
            SessionState::Idle
        }
    }
}

struct Inner {
    manifest: Manifest,
    ctx: RwLock<ModuleCtx>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    /// Sessions by the engine node that opened them, `<execution>/<node>`.
    owners: Mutex<HashMap<String, Vec<String>>>,
    next: AtomicU64,
}

/// The module. Cheap to clone: clones share their sessions.
#[derive(Clone)]
pub struct NativeEngine {
    inner: Arc<Inner>,
}

impl Default for NativeEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl NativeEngine {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Inner {
                manifest: parse_manifest(MANIFEST).expect("engine-native manifest is valid"),
                ctx: RwLock::default(),
                sessions: Mutex::default(),
                owners: Mutex::default(),
                next: AtomicU64::new(0),
            }),
        }
    }

    fn ctx(&self) -> ModuleCtx {
        self.inner
            .ctx
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn session(&self, id: &str) -> Result<Arc<Session>, ModuleError> {
        lock(&self.inner.sessions)
            .get(id)
            .cloned()
            .ok_or_else(|| failed(format!("no live session `{id}`")))
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let args: engine::StartArgs = parse(call.args.clone())?;
        let ctx = self.ctx();
        let settings = Settings::read(&self.inner.manifest, &call.node)?;
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        let mut warnings = Vec::new();
        let model = ctx
            .resolve(&me, MODEL_PORT)
            .into_iter()
            .next()
            .ok_or_else(|| {
                failed(format!(
                    "no model wired into `{MODEL_PORT}` of `{}`: connect a provider",
                    me.node_id
                ))
            })?;
        let endpoint: llm::LlmEndpoint = parse(ctx.call(&model, llm::DESCRIBE, Value::Null)?)?;
        if !model::PROTOCOLS.contains(&endpoint.protocol.as_str()) {
            return Err(failed(format!(
                "the native engine speaks {}; its `{MODEL_PORT}` provider speaks `{}`: wire a translator between them",
                model::PROTOCOLS.join(", "),
                endpoint.protocol
            )));
        }
        let model_id = settings
            .model
            .clone()
            .or_else(|| endpoint.models.first().cloned())
            .ok_or_else(|| {
                failed("no model id: set `model` on the native engine or on its provider")
            })?;
        let ignored = [
            (args.provider.is_some(), "the box's `provider`: the native engine asks its own `model` input"),
            (!args.slots.is_empty(), "the box's provider slots"),
            (!args.skills.is_empty(), "skills: the native engine has no skills folder"),
            (!args.tools.is_empty(), "the box's tool mounts (MCP): wire `tool@1` nodes into the engine's `tools` instead"),
            (!args.hooks.is_empty(), "the box's hooks (a CLI bridge): wire them into the engine's `context` instead"),
            (args.resume.is_some(), "resume: the native engine cannot load an earlier session"),
        ];
        for (_, what) in ignored.iter().filter(|(wired, _)| *wired) {
            warnings.push(format!("left out {what}"));
        }
        if settings.policy == Policy::Ask && args.permissions != engine::Permissions::Ask {
            warnings.push(format!(
                "tool_policy = ask, but the box cannot ask: tool calls are {}",
                if args.permissions == engine::Permissions::Allow {
                    "allowed"
                } else {
                    "refused"
                }
            ));
        }
        let (wired, left_out) = hooks::describe(&ctx, ctx.resolve(&me, CONTEXT_PORT));
        warnings.extend(left_out);
        let mut first = HookRequest {
            event: hook_event::SESSION_START.into(),
            cwd: Some(args.cwd.display().to_string()),
            ..HookRequest::default()
        };
        first.raw = json!({ "source": "startup" });
        let session_context = match hooks::dispatch(&ctx, &wired, &first, settings.hook_timeout) {
            HookAnswer::Veto { reason } => {
                return Err(failed(format!("a hook refused the session: {reason}")))
            }
            HookAnswer::Context { text } => text,
            HookAnswer::Nothing => String::new(),
        };
        for warning in &warnings {
            eprintln!("regulus: node `{}`: {warning}", me.node_id);
        }
        let caller = settings.name.clone().unwrap_or_else(|| me.node_id.clone());
        let session = Session {
            client: Client {
                endpoint,
                model: model_id.clone(),
                temperature: settings.temperature,
                max_tokens: settings.max_tokens,
                timeout: settings.request_timeout,
            },
            events: Events::new(settings.events_keep),
            settings,
            permissions: args.permissions,
            caller,
            cwd: args.cwd,
            hooks: wired,
            session_context,
            history: Mutex::default(),
            turn: Mutex::default(),
            live: Mutex::default(),
            me,
        };
        session.events.push(SessionEventKind::State {
            state: SessionState::Idle,
        });
        let id = format!("n{}", self.inner.next.fetch_add(1, Ordering::Relaxed) + 1);
        lock(&self.inner.sessions).insert(id.clone(), Arc::new(session));
        lock(&self.inner.owners)
            .entry(format!("{}/{}", call.execution_id, call.node.id))
            .or_default()
            .push(id.clone());
        to_json(engine::Started {
            session_id: id,
            agent: engine::Agent {
                name: format!("regulus-native ({model_id})"),
                version: env!("CARGO_PKG_VERSION").into(),
            },
            warnings,
            capabilities: engine::EngineCapabilities {
                load_session: false,
                events: true,
            },
            native_session_id: None,
            resumed: false,
        })
    }

    fn stop_session(&self, id: &str) {
        if let Some(session) = lock(&self.inner.sessions).remove(id) {
            session.end();
        }
    }
}

impl Module for NativeEngine {
    fn manifest(&self) -> &Manifest {
        &self.inner.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        *self
            .inner
            .ctx
            .write()
            .unwrap_or_else(PoisonError::into_inner) = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            // Nothing to install: the engine is part of Regulus.
            engine::DETECT => to_json(engine::Detected::default()),
            engine::SESSION_START => self.start(parse(params)?),
            engine::SESSION_PROMPT => {
                let call: NodeCall = parse(params)?;
                let args: engine::PromptArgs = parse(call.args)?;
                let session = self.session(&args.session_id)?;
                to_json(session.prompt(&self.ctx(), &args.text)?)
            }
            engine::SESSION_STATUS => {
                let call: NodeCall = parse(params)?;
                let args: engine::StatusArgs = parse(call.args)?;
                let state = lock(&self.inner.sessions)
                    .get(&args.session_id)
                    .map_or(SessionState::Ended, |s| s.state());
                to_json(engine::Status { state })
            }
            engine::SESSION_CANCEL => {
                let call: NodeCall = parse(params)?;
                let args: engine::CancelArgs = parse(call.args)?;
                self.session(&args.session_id)?.cancel();
                Ok(Value::Null)
            }
            engine::SESSION_EVENTS => {
                let call: NodeCall = parse(params)?;
                let args: engine::EventsArgs = parse(call.args)?;
                let (events, next) = self.session(&args.session_id)?.events.since(args.since);
                to_json(engine::Events { events, next })
            }
            engine::SESSION_DECIDE => {
                let call: NodeCall = parse(params)?;
                let args: engine::DecideArgs = parse(call.args)?;
                self.session(&args.session_id)?
                    .decide(&args.request_id, args.option)
                    .map_err(ModuleError::InvalidParams)?;
                Ok(Value::Null)
            }
            engine::SESSION_STOP => {
                let call: NodeCall = parse(params)?;
                let args: engine::StopArgs = parse(call.args)?;
                self.stop_session(&args.session_id);
                Ok(Value::Null)
            }
            method::NODE_STOP => {
                let call: NodeCall = parse(params)?;
                let owned = lock(&self.inner.owners)
                    .remove(&format!("{}/{}", call.execution_id, call.node.id));
                for id in owned.into_iter().flatten() {
                    self.stop_session(&id);
                }
                Ok(Value::Null)
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        let sessions: Vec<Arc<Session>> =
            lock(&self.inner.sessions).drain().map(|(_, s)| s).collect();
        for session in sessions {
            session.end();
        }
        Ok(())
    }
}

/// A fresh instance, for the host's registry.
pub fn module() -> NativeEngine {
    NativeEngine::new()
}
