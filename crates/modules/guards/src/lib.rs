//! Guards (docs/13-blocks.md §3.2): wrappers that sit in a "then" wire and take and give
//! one capability, whichever the wire carries. `budget` stops a loop past a limit,
//! `retry` sends a failed step's value again with backoff, `timeout` reports a value
//! nobody acknowledged in time, `error` makes the explicit fail branch uniform, and
//! `watchdog` reports a busy step that stopped making progress.
//!
//! State is per node and execution. Nothing here blocks the bus thread: deferred work
//! runs on one timer thread (`timer`), started with the first timer.

mod timer;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{
    method, Direction, Manifest, Node, NodeCall, NodeEvent, NodeStart, PortRef, WireKind,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use timer::Timers;

const MANIFEST: &str = include_str!("../regulus-module.toml");

/// What the `error` node gives: [`Failure`].
pub const ERROR_CAPABILITY: &str = "error@1";

const BUDGET: &str = "budget";
const RETRY: &str = "retry";
const TIMEOUT: &str = "timeout";
const ERROR: &str = "error";
const WATCHDOG: &str = "watchdog";

/// The `select` value that switches a yes/no field on.
const YES: &str = "yes";

mod port {
    pub const IN: &str = "in";
    pub const OUT: &str = "out";
    pub const EXCEEDED: &str = "exceeded";
    pub const RESET: &str = "reset";
    pub const FAIL: &str = "fail";
    pub const GAVE_UP: &str = "gave_up";
    pub const ACK: &str = "ack";
    pub const TIMEOUT: &str = "timeout";
    pub const ERROR: &str = "error";
    pub const PROGRESS: &str = "progress";
    pub const DONE: &str = "done";
    pub const STUCK: &str = "stuck";
    pub const RESTART: &str = "restart";
}

/// The field of a failure (`error@1`) with the value that failed: what `retry` resends.
const VALUE_FIELD: &str = "value";

/// A node that is not between its `node/start` and `node/stop` takes nothing.
fn not_running(at: &At) -> ModuleError {
    ModuleError::Failed(format!("node `{}` is not running", at.node.node_id))
}

/// Fields of a value that may say what went wrong, in the order the `error` node tries them.
const MESSAGE_FIELDS: &[&str] = &["message", "error", "reason", "stderr"];

/// A failure as the `error` node gives it (`error@1`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Failure {
    /// The output it came from.
    pub from: PortRef,
    pub message: String,
    /// What arrived, untouched.
    pub value: Value,
    /// Milliseconds since the Unix epoch.
    pub at: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// What a budget has spent; also its checkpoint.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Spent {
    items: u64,
    cost: f64,
    /// When the first value came, ms since the epoch.
    first: Option<u64>,
    exceeded: bool,
}

#[derive(Debug)]
enum State {
    Budget(Spent),
    Retry {
        /// The last value on `in`: what a `fail` without its own `value` retries.
        last: Option<Value>,
        /// Retries so far per value, newest last, at most `max_pending` of them.
        tries: VecDeque<(Value, u64)>,
        /// Retries waiting for their backoff, by token.
        waiting: HashMap<u64, Value>,
    },
    Timeout {
        /// Running clocks, oldest first: token, the key `ack` matches, the value.
        clocks: VecDeque<(u64, Option<Value>, Value)>,
    },
    Watchdog {
        busy: Option<Value>,
        clock: u64,
        restarts: u64,
    },
}

/// A node in one execution and the "then" outputs it declares: it only emits on those.
#[derive(Debug, Clone)]
struct At {
    node: NodeRef,
    outputs: Vec<String>,
}

impl At {
    fn of(call: &NodeCall) -> Self {
        Self {
            node: NodeRef {
                execution_id: call.execution_id.clone(),
                node_id: call.node.id.clone(),
            },
            outputs: call
                .node
                .ports
                .iter()
                .filter(|p| p.direction == Direction::Output && p.kind == WireKind::Then)
                .map(|p| p.id.clone())
                .collect(),
        }
    }

    fn has(&self, port: &str) -> bool {
        self.outputs.iter().any(|p| p == port)
    }
}

/// Values to emit once the state lock is released.
type Out = Vec<(&'static str, Value)>;

struct Inner {
    ctx: OnceLock<ModuleCtx>,
    states: Mutex<HashMap<NodeRef, State>>,
    timers: Timers,
    tokens: AtomicU64,
}

impl Inner {
    fn states(&self) -> MutexGuard<'_, HashMap<NodeRef, State>> {
        self.states.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn token(&self) -> u64 {
        self.tokens.fetch_add(1, Ordering::Relaxed) + 1
    }

    /// Emits on the ports the node has. A stopped execution takes nothing, and that is
    /// fine: whoever stopped it no longer wants the value.
    fn emit(&self, at: &At, out: Out) {
        let Some(ctx) = self.ctx.get() else {
            return;
        };
        for (port, value) in out {
            if at.has(port) {
                let _ = ctx.emit(&at.node, port, value);
            }
        }
    }
}

pub struct Guards {
    manifest: Manifest,
    inner: Arc<Inner>,
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

/// The ports of each node type that carry the value of `in`, unchanged.
fn carried(node_type: &str) -> &'static [&'static str] {
    match node_type {
        BUDGET => &[port::OUT, port::EXCEEDED],
        RETRY => &[port::OUT, port::GAVE_UP],
        TIMEOUT => &[port::OUT, port::TIMEOUT],
        WATCHDOG => &[port::OUT, port::STUCK, port::RESTART],
        _ => &[],
    }
}

/// A wrapper gives what it takes: those ports, where declared, have the capability of `in`.
fn same_capability(node: &Node) -> Result<(), ModuleError> {
    let Some(input) = node.port(port::IN) else {
        return Ok(());
    };
    for id in carried(&node.node_type) {
        if let Some(port) = node.port(id).filter(|p| p.capability != input.capability) {
            return Err(ModuleError::InvalidParams(format!(
                "node `{}`: `{id}` is {} but `in` is {}; a guard gives what it takes",
                node.id, port.capability, input.capability
            )));
        }
    }
    Ok(())
}

/// What a failure says: a string as is, the first message-like field of an object, or
/// the value's JSON.
fn message_of(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.to_string();
    }
    MESSAGE_FIELDS
        .iter()
        .filter_map(|field| value.get(field)?.as_str())
        .find(|text| !text.trim().is_empty())
        .map_or_else(|| value.to_string(), str::to_string)
}

impl Guards {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("guards manifest is valid"),
            inner: Arc::new(Inner {
                ctx: OnceLock::new(),
                states: Mutex::default(),
                timers: Timers::default(),
                tokens: AtomicU64::new(0),
            }),
        }
    }

    fn number(&self, node: &Node, field: &str) -> f64 {
        self.manifest
            .setting(node, field)
            .and_then(Value::as_f64)
            .unwrap_or_default()
    }

    fn whole(&self, node: &Node, field: &str) -> u64 {
        self.manifest
            .setting(node, field)
            .and_then(Value::as_u64)
            .unwrap_or_default()
    }

    /// A wait in milliseconds, at least 1 ms.
    fn millis(&self, node: &Node, field: &str) -> Duration {
        Duration::from_millis(self.whole(node, field).max(1))
    }

    fn text<'a>(&'a self, node: &'a Node, field: &str) -> &'a str {
        self.manifest
            .setting(node, field)
            .and_then(Value::as_str)
            .unwrap_or_default()
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        same_capability(&call.node)?;
        let state = match call.node.node_type.as_str() {
            BUDGET => {
                let resumed: Option<NodeStart> = serde_json::from_value(call.args.clone()).ok();
                State::Budget(
                    resumed
                        .and_then(|start| start.checkpoint)
                        .and_then(|state| serde_json::from_value(state).ok())
                        .unwrap_or_default(),
                )
            }
            RETRY => State::Retry {
                last: None,
                tries: VecDeque::new(),
                waiting: HashMap::new(),
            },
            TIMEOUT => State::Timeout {
                clocks: VecDeque::new(),
            },
            WATCHDOG => State::Watchdog {
                busy: None,
                clock: 0,
                restarts: 0,
            },
            _ => return Ok(Value::Null),
        };
        self.inner.states().insert(At::of(&call).node, state);
        Ok(Value::Null)
    }

    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        // Timers still waiting find no state and do nothing.
        self.inner.states().remove(&At::of(&call).node);
        Ok(Value::Null)
    }

    fn event(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(call.args.clone())?;
        let at = At::of(&call);
        let out = match call.node.node_type.as_str() {
            BUDGET => self.budget(&call.node, &at, event)?,
            RETRY => self.retry(&call.node, &at, event)?,
            TIMEOUT => self.timeout(&call.node, &at, event)?,
            ERROR => vec![(port::ERROR, self.error(event))],
            WATCHDOG => self.watchdog(&call.node, &at, event)?,
            other => {
                return Err(ModuleError::InvalidParams(format!(
                    "`{other}` is not a node of this module"
                )))
            }
        };
        let ports: Vec<&str> = out.iter().map(|(port, _)| *port).collect();
        let answer = json!({ "emitted": ports });
        self.inner.emit(&at, out);
        Ok(answer)
    }

    fn budget(&self, node: &Node, at: &At, event: NodeEvent) -> Result<Out, ModuleError> {
        let mut states = self.inner.states();
        let Some(State::Budget(spent)) = states.get_mut(&at.node) else {
            return Err(not_running(at));
        };
        if event.port == port::RESET {
            *spent = Spent::default();
        } else {
            let max_items = self.whole(node, "max_items");
            let max_ms = self.whole(node, "max_ms");
            let max_cost = self.number(node, "max_cost");
            let pointer = self.text(node, "cost_path");
            let cost = if pointer.is_empty() {
                0.0
            } else {
                event
                    .value
                    .pointer(pointer)
                    .and_then(Value::as_f64)
                    .unwrap_or_default()
                    .max(0.0)
            };
            let now = now_ms();
            let first = *spent.first.get_or_insert(now);
            spent.exceeded = spent.exceeded
                || (max_items > 0 && spent.items >= max_items)
                || (max_ms > 0 && now.saturating_sub(first) > max_ms)
                || (max_cost > 0.0 && spent.cost + cost > max_cost);
            if !spent.exceeded {
                spent.items += 1;
                spent.cost += cost;
            }
        }
        let (exceeded, checkpoint) = (spent.exceeded, json!(spent));
        drop(states);
        // Best effort: without a store the budget still holds, only not across a restart.
        if let Some(ctx) = self.inner.ctx.get() {
            let _ = ctx.checkpoint(&at.node, checkpoint);
        }
        Ok(match (event.port.as_str(), exceeded) {
            (port::RESET, _) => Vec::new(),
            (_, true) => vec![(port::EXCEEDED, event.value)],
            (_, false) => vec![(port::OUT, event.value)],
        })
    }

    fn retry(&self, node: &Node, at: &At, event: NodeEvent) -> Result<Out, ModuleError> {
        let max_pending =
            usize::try_from(self.whole(node, "max_pending").max(1)).unwrap_or(usize::MAX);
        let mut states = self.inner.states();
        let Some(State::Retry {
            last,
            tries,
            waiting,
        }) = states.get_mut(&at.node)
        else {
            return Err(not_running(at));
        };
        if event.port != port::FAIL {
            // A new value starts over; a retry still waiting for another one goes ahead.
            *last = Some(event.value.clone());
            tries.retain(|(value, _)| *value != event.value);
            tries.push_back((event.value.clone(), 0));
            while tries.len() > max_pending {
                tries.pop_front();
            }
            return Ok(vec![(port::OUT, event.value)]);
        }
        // What failed: the `value` the failure carries (`error@1`), else the last value.
        let target = event
            .value
            .as_object()
            .and_then(|failure| failure.get(VALUE_FIELD))
            .cloned()
            .or_else(|| last.clone());
        // Nothing to retry, or this one already waits: the failure changes nothing.
        let Some(value) = target.filter(|v| !waiting.values().any(|w| w == v)) else {
            return Ok(Vec::new());
        };
        let retries = tries
            .iter()
            .find(|(v, _)| *v == value)
            .map_or(0, |(_, n)| *n);
        if retries >= self.whole(node, "max_retries") {
            tries.retain(|(v, _)| *v != value);
            if last.as_ref() == Some(&value) {
                *last = None;
            }
            return Ok(vec![(port::GAVE_UP, value)]);
        }
        if waiting.len() >= max_pending {
            return Err(ModuleError::Failed(format!(
                "retry `{}` already waits on {max_pending} values (`max_pending`)",
                at.node.node_id
            )));
        }
        tries.retain(|(v, _)| *v != value);
        tries.push_back((value.clone(), retries + 1));
        while tries.len() > max_pending {
            tries.pop_front();
        }
        let first = self.number(node, "backoff_ms");
        let factor = self.number(node, "backoff_factor");
        let longest = self.number(node, "max_backoff_ms");
        let exponent = i32::try_from(retries).unwrap_or(i32::MAX);
        let delay = (first * factor.powi(exponent)).min(longest).max(0.0);
        let delay = Duration::try_from_secs_f64(delay / 1000.0).unwrap_or(Duration::MAX);
        let token = self.inner.token();
        waiting.insert(token, value);
        drop(states);
        let (inner, at) = (self.inner.clone(), at.clone());
        self.inner.timers.after(delay, move || {
            let mut states = inner.states();
            let Some(State::Retry { waiting, .. }) = states.get_mut(&at.node) else {
                return;
            };
            let Some(value) = waiting.remove(&token) else {
                return;
            };
            drop(states);
            inner.emit(&at, vec![(port::OUT, value)]);
        });
        Ok(Vec::new())
    }

    fn timeout(&self, node: &Node, at: &At, event: NodeEvent) -> Result<Out, ModuleError> {
        let pointer = self.text(node, "key_path");
        let key = |value: &Value| {
            if pointer.is_empty() {
                Some(value.clone())
            } else {
                value.pointer(pointer).cloned()
            }
        };
        let mut states = self.inner.states();
        let Some(State::Timeout { clocks }) = states.get_mut(&at.node) else {
            return Err(not_running(at));
        };
        if event.port == port::ACK {
            // The clock of the value it names, if any; else the oldest.
            let named = key(&event.value);
            let index = clocks
                .iter()
                .position(|(_, k, _)| named.is_some() && *k == named)
                .unwrap_or(0);
            clocks.remove(index);
            return Ok(Vec::new());
        }
        let token = self.inner.token();
        clocks.push_back((token, key(&event.value), event.value.clone()));
        drop(states);
        let (inner, timed) = (self.inner.clone(), at.clone());
        self.inner
            .timers
            .after(self.millis(node, "timeout_ms"), move || {
                let mut states = inner.states();
                let Some(State::Timeout { clocks }) = states.get_mut(&timed.node) else {
                    return;
                };
                let Some(index) = clocks.iter().position(|(t, _, _)| *t == token) else {
                    return;
                };
                let late = clocks.remove(index).map(|(_, _, value)| value);
                drop(states);
                if let Some(value) = late {
                    inner.emit(&timed, vec![(port::TIMEOUT, value)]);
                }
            });
        Ok(vec![(port::OUT, event.value)])
    }

    fn error(&self, event: NodeEvent) -> Value {
        json!(Failure {
            from: event.from,
            message: message_of(&event.value),
            value: event.value,
            at: now_ms(),
        })
    }

    fn watchdog(&self, node: &Node, at: &At, event: NodeEvent) -> Result<Out, ModuleError> {
        let mut states = self.inner.states();
        let Some(State::Watchdog {
            busy,
            clock,
            restarts,
        }) = states.get_mut(&at.node)
        else {
            return Err(not_running(at));
        };
        let out = match event.port.as_str() {
            port::DONE => {
                *busy = None;
                return Ok(Vec::new());
            }
            port::PROGRESS if busy.is_none() => return Ok(Vec::new()),
            port::PROGRESS => Vec::new(),
            _ => {
                *busy = Some(event.value.clone());
                *restarts = 0;
                vec![(port::OUT, event.value)]
            }
        };
        *clock = self.inner.token();
        let token = *clock;
        drop(states);
        self.arm(node, at, token);
        Ok(out)
    }

    /// Starts the watchdog's clock `token`; a newer clock, `done` or a stop disarms it.
    fn arm(&self, node: &Node, at: &At, token: u64) {
        let stall = self.millis(node, "stall_ms");
        let restart = self.text(node, "restart") == YES && at.has(port::RESTART);
        let max_restarts = self.whole(node, "max_restarts");
        let (inner, at) = (self.inner.clone(), at.clone());
        self.inner.timers.after(stall, move || {
            watchdog_fired(&inner, &at, token, stall, restart, max_restarts)
        });
    }
}

/// The watchdog's clock ran out: report the step stuck and, if it may, restart it.
fn watchdog_fired(
    inner: &Arc<Inner>,
    at: &At,
    token: u64,
    stall: Duration,
    restart: bool,
    max_restarts: u64,
) {
    let mut states = inner.states();
    let Some(State::Watchdog {
        busy,
        clock,
        restarts,
    }) = states.get_mut(&at.node)
    else {
        return;
    };
    if *clock != token {
        return;
    }
    let Some(value) = busy.clone() else {
        return;
    };
    let mut out = vec![(port::STUCK, value.clone())];
    if restart && *restarts < max_restarts {
        *restarts += 1;
        *clock = inner.token();
        let next = *clock;
        out.push((port::RESTART, value));
        let (again, at_again) = (inner.clone(), at.clone());
        inner.timers.after(stall, move || {
            watchdog_fired(&again, &at_again, next, stall, restart, max_restarts)
        });
    } else {
        *busy = None;
    }
    drop(states);
    inner.emit(at, out);
}

impl Default for Guards {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Guards {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        let _ = self.inner.ctx.set(ctx.clone());
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            method::NODE_START => self.start(parse(params)?),
            method::NODE_EVENT => self.event(parse(params)?),
            method::NODE_STOP => self.stop(parse(params)?),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.inner.timers.stop();
        self.inner.states().clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(guards: &Guards, node: &Value, port: &str, value: Value) -> Value {
        let event = json!({ "port": port, "capability": "text@1", "value": value,
            "from": { "node_id": "src", "port_id": "out" }, "event": 1 });
        guards
            .call(
                method::NODE_EVENT,
                json!({ "execution_id": "e", "node": node, "args": event }),
            )
            .unwrap()
    }

    /// A started node: events reach only those between `node/start` and `node/stop`.
    fn node_in(guards: &Guards, node_type: &str, config: Value) -> Value {
        let node = json!({ "id": "g", "type": node_type, "config": config });
        guards
            .call(
                method::NODE_START,
                json!({ "execution_id": "e", "node": node, "args": null }),
            )
            .unwrap();
        node
    }

    #[test]
    fn a_budget_lets_through_what_fits_and_then_refuses() {
        let guards = Guards::new();
        let budget = node_in(&guards, BUDGET, json!({ "max_items": 2 }));
        let ports = |v: Value| v["emitted"].clone();
        assert_eq!(
            ports(call(&guards, &budget, "in", json!("a"))),
            json!(["out"])
        );
        assert_eq!(
            ports(call(&guards, &budget, "in", json!("b"))),
            json!(["out"])
        );
        assert_eq!(
            ports(call(&guards, &budget, "in", json!("c"))),
            json!(["exceeded"])
        );
        assert_eq!(ports(call(&guards, &budget, "reset", json!(0))), json!([]));
        assert_eq!(
            ports(call(&guards, &budget, "in", json!("d"))),
            json!(["out"])
        );
        let guards = Guards::new();
        let costly = node_in(
            &guards,
            BUDGET,
            json!({ "max_items": 0, "max_cost": 1.0, "cost_path": "/cost" }),
        );
        assert_eq!(
            ports(call(&guards, &costly, "in", json!({ "cost": 0.6 }))),
            json!(["out"])
        );
        assert_eq!(
            ports(call(&guards, &costly, "in", json!({ "cost": 0.6 }))),
            json!(["exceeded"])
        );
        // Once over, it stays over: even a free value is refused.
        assert_eq!(
            ports(call(&guards, &costly, "in", json!({}))),
            json!(["exceeded"])
        );
        // A negative cost buys no room: it counts as 0.
        let guards = Guards::new();
        let costly = node_in(
            &guards,
            BUDGET,
            json!({ "max_items": 0, "max_cost": 1.0,
            "cost_path": "/cost" }),
        );
        for (cost, port) in [(-5.0, "out"), (0.6, "out"), (0.6, "exceeded")] {
            let value = json!({ "cost": cost });
            assert_eq!(ports(call(&guards, &costly, "in", value)), json!([port]));
        }
    }

    #[test]
    fn a_retry_gives_up_after_its_retries() {
        let guards = Guards::new();
        let retry = node_in(&guards, RETRY, json!({ "max_retries": 0 }));
        assert_eq!(
            call(&guards, &retry, "fail", json!(1))["emitted"],
            json!([])
        );
        assert_eq!(
            call(&guards, &retry, "in", json!("x"))["emitted"],
            json!(["out"])
        );
        assert_eq!(
            call(&guards, &retry, "fail", json!(1))["emitted"],
            json!(["gave_up"])
        );
        // Given up: nothing left to retry.
        assert_eq!(
            call(&guards, &retry, "fail", json!(1))["emitted"],
            json!([])
        );
    }

    #[test]
    fn a_stopped_node_takes_nothing_and_keeps_no_state() {
        let guards = Guards::new();
        let retry = node_in(&guards, RETRY, json!({}));
        let stop = json!({ "execution_id": "e", "node": retry, "args": null });
        guards.call(method::NODE_STOP, stop).unwrap();
        let event = json!({ "port": "in", "capability": "text@1", "value": "late",
            "from": { "node_id": "src", "port_id": "out" }, "event": 1 });
        let late = guards.call(
            method::NODE_EVENT,
            json!({ "execution_id": "e", "node": retry, "args": event }),
        );
        assert!(late.unwrap_err().to_string().contains("not running"));
        assert!(guards.inner.states().is_empty());
    }

    #[test]
    fn a_huge_backoff_does_not_panic() {
        let guards = Guards::new();
        let retry = node_in(
            &guards,
            RETRY,
            json!({ "backoff_ms": 1e300, "max_backoff_ms": 1e300 }),
        );
        call(&guards, &retry, "in", json!("x"));
        assert_eq!(
            call(&guards, &retry, "fail", json!(1))["emitted"],
            json!([])
        );
    }

    #[test]
    fn the_error_node_finds_the_message() {
        let guards = Guards::new();
        let event = |value: Value| NodeEvent {
            port: "fail".into(),
            capability: "exit@1".into(),
            value,
            from: PortRef {
                node_id: "test".into(),
                port_id: "fail".into(),
            },
            event: 1,
        };
        let failure = guards.error(event(json!({ "code": 1, "stderr": "boom" })));
        assert_eq!(failure["message"], "boom");
        assert_eq!(failure["from"]["node_id"], "test");
        assert_eq!(guards.error(event(json!("plain")))["message"], "plain");
        assert_eq!(
            guards.error(event(json!({ "code": 2 })))["message"],
            r#"{"code":2}"#
        );
    }

    #[test]
    fn a_guard_must_give_what_it_takes() {
        let node: Node = serde_json::from_value(json!({ "id": "r", "type": "retry", "ports": [
            { "id": "in", "capability": "text@1", "direction": "input", "kind": "then" },
            { "id": "out", "capability": "text@1", "direction": "output", "kind": "then" },
            { "id": "gave_up", "capability": "error@1", "direction": "output", "kind": "then" },
        ]}))
        .unwrap();
        assert!(same_capability(&node).is_err());
        let manifest = Guards::new().manifest().clone();
        for node_type in [BUDGET, RETRY, TIMEOUT, ERROR, WATCHDOG] {
            assert!(manifest.node_type(node_type).is_some(), "{node_type}");
        }
        let error = manifest.node_type(ERROR).unwrap();
        assert!(error.ports.iter().any(|p| p.capability == ERROR_CAPABILITY));
    }
}
