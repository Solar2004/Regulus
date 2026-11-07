//! Triggers (docs/13-blocks.md §3.2): Cron, Webhook, Bus event, Chat, Idle and
//! Threshold. Each starts work in a workflow by emitting on a "then" output. Timers and
//! listeners run on threads of their own, so the bus never waits on them, and stop with
//! their node (`node/stop`) or with the module.
//!
//! "File changes" is not here: watching files belongs to the `files` module.

pub mod timer;
pub mod webhook;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::Duration;

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, Node, NodeCall, NodeEvent, NodeStart};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use timer::Timer;
use webhook::{Route, Webhooks, BODY_PORT, JSON_PORT};

const MANIFEST: &str = include_str!("../regulus-module.toml");
/// Where each live Webhook listens: `{}` → `[{ execution_id, node_id, url, token }]`.
pub const WEBHOOKS: &str = "triggers.webhooks";
/// A Chat's turns: `{ execution_id, node_id }` → `[{ role, text, at }]`.
pub const TRANSCRIPT: &str = "triggers.transcript";

const TICK_PORT: &str = "tick";
const IDLE_PORT: &str = "idle";
const FIRED_PORT: &str = "fired";
const REACHED_PORT: &str = "reached";
/// Chat: what the user says comes in on `user` and leaves on `message`; what comes back
/// on `reply` leaves on `out`.
const USER_PORT: &str = "user";
const MESSAGE_PORT: &str = "message";
const REPLY_PORT: &str = "reply";
const OUT_PORT: &str = "out";
const TRANSCRIPT_PORT: &str = "transcript";
const YES: &str = "yes";

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// One turn of a chat.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Turn {
    /// `user` or `assistant`.
    pub role: String,
    pub text: String,
    /// Milliseconds since the Unix epoch.
    pub at: u64,
}

/// A Threshold's progress, also its checkpoint.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Count {
    count: u64,
    /// Fired already and does not reset.
    done: bool,
}

type Key = (String, String);

fn key(node: &NodeRef) -> Key {
    (node.execution_id.clone(), node.node_id.clone())
}

fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

pub struct Triggers {
    manifest: Manifest,
    ctx: ModuleCtx,
    timers: Mutex<HashMap<Key, Timer>>,
    webhooks: Webhooks,
    counts: Mutex<HashMap<Key, Count>>,
    /// Chat transcripts when the core has no store.
    transcripts: Mutex<HashMap<Key, Vec<Turn>>>,
}

impl Triggers {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("triggers manifest is valid"),
            ctx: ModuleCtx::default(),
            timers: Mutex::default(),
            webhooks: Webhooks::default(),
            counts: Mutex::default(),
            transcripts: Mutex::default(),
        }
    }

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

    fn me(call: &NodeCall) -> NodeRef {
        NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        }
    }

    fn failed(call: &NodeCall, message: impl std::fmt::Display) -> ModuleError {
        ModuleError::InvalidParams(format!(
            "{} `{}`: {message}",
            call.node.node_type, call.node.id
        ))
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let me = Self::me(&call);
        match call.node.node_type.as_str() {
            "cron" => {
                let cron = timer::schedule(&self.text(&call.node, "schedule"))
                    .map_err(|e| Self::failed(&call, e))?;
                let utc = self.text(&call.node, "timezone") == "utc";
                let (ctx, node) = (self.ctx.clone(), me.clone());
                self.add_timer(&call, me, move |shared| {
                    timer::run_cron(shared, cron, utc, |at| {
                        if let Err(error) = ctx.emit(&node, TICK_PORT, json!(at)) {
                            eprintln!("regulus: triggers: cron `{}`: {error}", node.node_id);
                        }
                    })
                })?;
            }
            "idle" => {
                let after = Duration::from_millis(self.number(&call.node, "after_ms").max(1));
                let repeat = self.text(&call.node, "repeat") == YES;
                let (ctx, node) = (self.ctx.clone(), me.clone());
                self.add_timer(&call, me, move |shared| {
                    timer::run_idle(shared, after, repeat, || {
                        let at = chrono::Local::now().to_rfc3339();
                        if let Err(error) = ctx.emit(&node, IDLE_PORT, json!(at)) {
                            eprintln!("regulus: triggers: idle `{}`: {error}", node.node_id);
                        }
                    })
                })?;
            }
            "webhook" => {
                let addr = self.listen(&call, me)?;
                return Ok(json!({ "listening": addr.to_string() }));
            }
            "threshold" => {
                let start: NodeStart = serde_json::from_value(call.args).unwrap_or_default();
                let count = start
                    .checkpoint
                    .and_then(|c| serde_json::from_value(c).ok())
                    .unwrap_or_default();
                lock(&self.counts).insert(key(&me), count);
            }
            _ => {}
        }
        Ok(Value::Null)
    }

    fn add_timer(
        &self,
        call: &NodeCall,
        me: NodeRef,
        run: impl FnOnce(timer::Shared) + Send + 'static,
    ) -> Result<(), ModuleError> {
        let name = format!("regulus-{}", call.node.node_type);
        let timer = Timer::spawn(&name, timer::Shared::default(), run)
            .map_err(|e| Self::failed(call, e))?;
        if let Some(old) = lock(&self.timers).insert(key(&me), timer) {
            old.stop();
        }
        Ok(())
    }

    fn listen(&self, call: &NodeCall, me: NodeRef) -> Result<SocketAddr, ModuleError> {
        let bind = self.text(&call.node, "bind");
        let token = Some(self.text(&call.node, "token")).filter(|t| !t.is_empty());
        let ip = bind
            .parse::<SocketAddr>()
            .map_err(|e| Self::failed(call, format!("`bind` = `{bind}`: {e}")))?
            .ip();
        if token.is_none() && !ip.is_loopback() {
            return Err(Self::failed(
                call,
                format!("{bind} is not loopback: set a `token` to listen beyond this machine"),
            ));
        }
        let mut path = self.text(&call.node, "path");
        if path.is_empty() {
            path = call.node.id.clone();
        }
        if !path.starts_with('/') {
            path.insert(0, '/');
        }
        let route = Route {
            node: me.clone(),
            token,
            max_body: self.number(&call.node, "max_body_bytes") as usize,
            body_port: call.node.port(BODY_PORT).is_some(),
            json_port: call.node.port(JSON_PORT).is_some(),
        };
        let addr = self
            .webhooks
            .add(&self.ctx, &bind, &path, route)
            .map_err(|e| Self::failed(call, e))?;
        eprintln!(
            "regulus: triggers: webhook `{}` of `{}` listens on http://{addr}{path}",
            me.node_id, me.execution_id
        );
        Ok(addr)
    }

    fn stop(&self, call: NodeCall) {
        let me = Self::me(&call);
        if let Some(timer) = lock(&self.timers).remove(&key(&me)) {
            timer.stop();
        }
        self.webhooks.remove(&me);
        lock(&self.counts).remove(&key(&me));
        lock(&self.transcripts).remove(&key(&me));
    }

    fn event(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(call.args.clone())?;
        let me = Self::me(&call);
        match call.node.node_type.as_str() {
            "idle" => {
                if let Some(timer) = lock(&self.timers).get(&key(&me)) {
                    timer.touch();
                }
            }
            "threshold" => self.count(&call, &me, event.value)?,
            "bus_event" => {
                let wanted = |field: &str, got: &str| {
                    let want = self.text(&call.node, field);
                    want.is_empty() || want == got
                };
                let contains = self.text(&call.node, "contains");
                if wanted("from_node", &event.from.node_id)
                    && wanted("from_port", &event.from.port_id)
                    && wanted("capability", &event.capability)
                    && (contains.is_empty() || as_text(&event.value).contains(&contains))
                {
                    let fired = json!({ "port": event.port, "from": event.from,
                        "capability": event.capability, "value": event.value,
                        "event": event.event });
                    self.ctx.emit(&me, FIRED_PORT, fired)?;
                    return Ok(json!({ "fired": true }));
                }
                return Ok(json!({ "fired": false }));
            }
            "chat" => self.chat(&call, &me, &event)?,
            other => return Err(Self::failed(&call, format!("`{other}` takes no events"))),
        }
        Ok(Value::Null)
    }

    fn count(&self, call: &NodeCall, me: &NodeRef, value: Value) -> Result<(), ModuleError> {
        let target = self.number(&call.node, "count").max(1);
        let reset = self.text(&call.node, "reset") == YES;
        let (fire, state) = {
            let mut counts = lock(&self.counts);
            let state = counts.entry(key(me)).or_default();
            if state.done {
                return Ok(());
            }
            state.count += 1;
            let fire = state.count >= target;
            let reached = state.count;
            if fire {
                if reset {
                    state.count = 0;
                } else {
                    state.done = true;
                }
            }
            (fire.then_some(reached), *state)
        };
        // A core without a store still counts; only a restart loses the count.
        let _ = self.ctx.checkpoint(me, json!(state));
        if let Some(count) = fire {
            self.ctx
                .emit(me, REACHED_PORT, json!({ "count": count, "value": value }))?;
        }
        Ok(())
    }

    fn transcript_key(me: &NodeRef) -> String {
        format!("transcript/{}/{}", me.execution_id, me.node_id)
    }

    /// The turns of a chat: from the store when the core has one, else from memory.
    fn turns(&self, me: &NodeRef) -> Vec<Turn> {
        match self.ctx.store(Some(me)) {
            Ok(store) => store
                .get_json(&Self::transcript_key(me))
                .ok()
                .flatten()
                .unwrap_or_default(),
            Err(_) => lock(&self.transcripts)
                .get(&key(me))
                .cloned()
                .unwrap_or_default(),
        }
    }

    fn save_turns(&self, me: &NodeRef, turns: &[Turn]) -> Result<(), ModuleError> {
        match self.ctx.store(Some(me)) {
            Ok(store) => store.set_json(&Self::transcript_key(me), &turns),
            Err(_) => {
                lock(&self.transcripts).insert(key(me), turns.to_vec());
                Ok(())
            }
        }
    }

    fn chat(&self, call: &NodeCall, me: &NodeRef, event: &NodeEvent) -> Result<(), ModuleError> {
        let (role, out) = match event.port.as_str() {
            USER_PORT => ("user", MESSAGE_PORT),
            REPLY_PORT => ("assistant", OUT_PORT),
            other => return Err(Self::failed(call, format!("`{other}` takes no events"))),
        };
        let text = as_text(&event.value);
        let mut turns = self.turns(me);
        let history = self.number(&call.node, "history_turns") as usize;
        let message = if role == "user" && history > 0 && !turns.is_empty() {
            let earlier: Vec<String> = turns
                .iter()
                .rev()
                .take(history)
                .rev()
                .map(|t| format!("{}: {}", t.role, t.text))
                .collect();
            format!("{}\nuser: {text}", earlier.join("\n"))
        } else {
            text.clone()
        };
        turns.push(Turn {
            role: role.into(),
            text,
            at: now_ms(),
        });
        let max = (self.number(&call.node, "max_turns") as usize).max(1);
        if turns.len() > max {
            turns.drain(..turns.len() - max);
        }
        self.save_turns(me, &turns)?;
        if call.node.port(out).is_some() {
            self.ctx.emit(me, out, json!(message))?;
        }
        if call.node.port(TRANSCRIPT_PORT).is_some() {
            self.ctx.emit(me, TRANSCRIPT_PORT, json!(turns))?;
        }
        Ok(())
    }
}

impl Default for Triggers {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct TranscriptParams {
    execution_id: String,
    node_id: String,
}

impl Module for Triggers {
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
            WEBHOOKS => Ok(Value::Array(self.webhooks.list())),
            TRANSCRIPT => {
                let params: TranscriptParams = parse(params)?;
                let me = NodeRef {
                    execution_id: params.execution_id,
                    node_id: params.node_id,
                };
                Ok(json!(self.turns(&me)))
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        for (_, timer) in lock(&self.timers).drain() {
            timer.stop();
        }
        self.webhooks.close_all();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_the_nodes_and_its_methods() {
        let triggers = Triggers::new();
        let manifest = triggers.manifest();
        for kind in ["cron", "webhook", "bus_event", "chat", "idle", "threshold"] {
            assert!(manifest.node_type(kind).is_some(), "{kind}");
        }
        assert_eq!(manifest.module.methods, [WEBHOOKS, TRANSCRIPT]);
    }

    #[test]
    fn a_webhook_without_token_refuses_a_public_bind() {
        let triggers = Triggers::new();
        let node = json!({ "id": "w", "type": "webhook",
            "config": { "bind": "0.0.0.0:0" } });
        let error = triggers
            .call(
                method::NODE_START,
                json!({ "execution_id": "e", "node": node }),
            )
            .unwrap_err()
            .to_string();
        assert!(error.contains("not loopback"), "{error}");
    }

    #[test]
    fn a_bad_schedule_fails_the_start() {
        let triggers = Triggers::new();
        let node = json!({ "id": "c", "type": "cron", "config": { "schedule": "soon" } });
        assert!(triggers
            .call(
                method::NODE_START,
                json!({ "execution_id": "e", "node": node })
            )
            .is_err());
    }
}
