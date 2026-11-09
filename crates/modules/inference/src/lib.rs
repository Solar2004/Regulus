//! Inference (docs/13-blocks.md §3.3): `complete` sends each value to the model behind
//! the `llm.endpoint@1` wired into `model` and emits its text, its usage, or an
//! `error@1`. No agent, no tools, no session: one request per value.
//!
//! Every value runs on a thread of its own, so the bus that delivered it goes on at once.
//! What the node emits from there starts a new chain on the bus (depth 0).

pub mod client;

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_contract_llm_endpoint::{LlmEndpoint, DESCRIBE};
use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, Node, NodeCall, NodeEvent, PortRef};
use serde_json::{json, Value};

pub use client::{Answer, Client, Failure, Request};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const COMPLETE: &str = "complete";
const MODEL: &str = "model";
const OUT: &str = "out";
const USAGE: &str = "usage";
const ERROR: &str = "error";

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// One value on its way: what arrived and where from.
struct Item {
    value: Value,
    from: PortRef,
}

/// A node of one execution, as a thread sees it.
struct Job {
    manifest: Arc<Manifest>,
    ctx: ModuleCtx,
    node: Node,
    me: NodeRef,
    stop: Arc<AtomicBool>,
    agent: ureq::Agent,
}

impl Job {
    fn setting(&self, field: &str) -> Option<&Value> {
        self.manifest.setting(&self.node, field)
    }

    fn text(&self, field: &str) -> String {
        match self.setting(field) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    fn number(&self, field: &str) -> f64 {
        self.setting(field).and_then(Value::as_f64).unwrap_or(0.0)
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    fn emit(&self, port: &str, value: Value) {
        if self.node.port(port).is_none() {
            return;
        }
        if let Err(e) = self.ctx.emit(&self.me, port, value) {
            eprintln!(
                "regulus: inference: {} `{}`: {e}",
                self.node.node_type, self.node.id
            );
        }
    }

    /// A failure as `error@1`.
    fn fail(&self, item: &Item, message: String) {
        let message = format!("{} `{}`: {message}", self.node.node_type, self.node.id);
        eprintln!("regulus: inference: {message}");
        self.emit(
            ERROR,
            json!({ "from": item.from, "message": message, "value": item.value, "at": now_ms() }),
        );
    }

    fn request(&self, value: &Value) -> Result<Request, String> {
        let mut request = Request {
            system: self.text("system"),
            prompt: String::new(),
            max_tokens: self.number("max_tokens").max(1.0) as u64,
            temperature: self.number("temperature"),
        };
        match value {
            Value::Object(o) => {
                request.prompt = o
                    .get("prompt")
                    .map(text_of)
                    .ok_or("a JSON value needs `prompt`")?;
                if let Some(system) = o.get("system").and_then(Value::as_str) {
                    request.system = system.into();
                }
                if let Some(max) = o.get("max_tokens").and_then(Value::as_u64) {
                    request.max_tokens = max.max(1);
                }
            }
            other => request.prompt = text_of(other),
        }
        if request.prompt.trim().is_empty() {
            return Err("an empty prompt is not sent".into());
        }
        Ok(request)
    }

    fn client(&self) -> Result<Client, String> {
        let endpoint = self
            .ctx
            .resolve(&self.me, MODEL)
            .into_iter()
            .next()
            .ok_or("no provider wired into `model`")?;
        let described = self
            .ctx
            .call(&endpoint, DESCRIBE, Value::Null)
            .map_err(|e| format!("its provider: {e}"))?;
        let described: LlmEndpoint =
            serde_json::from_value(described).map_err(|e| format!("its provider: {e}"))?;
        Client::new(described, &self.text("model"), self.agent.clone())
    }

    /// One value: asked until it passes, fails for good or the attempts run out.
    fn run(&self, item: &Item) {
        let began = Instant::now();
        let outcome = self.request(&item.value).and_then(|request| {
            let client = self.client()?;
            let attempts = self.number("retries").max(0.0) as u32 + 1;
            let backoff = Duration::from_millis(self.number("backoff_ms").max(0.0) as u64);
            let mut last = String::new();
            for attempt in 1..=attempts {
                if self.stopped() {
                    return Err("the node stopped".into());
                }
                if attempt > 1 {
                    std::thread::sleep(backoff);
                }
                match client.complete(&request) {
                    Ok(answer) => return Ok((client, answer, attempt)),
                    Err(failure) if failure.retry => last = failure.message,
                    Err(failure) => return Err(failure.message),
                }
            }
            Err(format!("no answer in {attempts} attempt(s): {last}"))
        });
        if self.stopped() {
            return;
        }
        match outcome {
            Ok((client, answer, attempts)) => {
                let total = match (answer.input_tokens, answer.output_tokens) {
                    (Some(i), Some(o)) => Some(i + o),
                    _ => None,
                };
                self.emit(
                    USAGE,
                    json!({ "protocol": client.protocol(), "model": client.model,
                            "input_tokens": answer.input_tokens,
                            "output_tokens": answer.output_tokens, "total_tokens": total,
                            "attempts": attempts,
                            "ms": began.elapsed().as_millis() as u64 }),
                );
                self.emit(OUT, json!(answer.text));
            }
            Err(error) => self.fail(item, error),
        }
    }
}

/// What one node of one execution has going on.
struct NodeState {
    running: usize,
    queue: VecDeque<Item>,
    stop: Arc<AtomicBool>,
    agent: ureq::Agent,
}

type Key = (String, String);

#[derive(Default)]
struct Shared {
    nodes: Mutex<HashMap<Key, NodeState>>,
}

impl Shared {
    fn nodes(&self) -> std::sync::MutexGuard<'_, HashMap<Key, NodeState>> {
        self.nodes.lock().unwrap_or_else(|e| e.into_inner())
    }
}

pub struct Inference {
    manifest: Arc<Manifest>,
    ctx: ModuleCtx,
    shared: Arc<Shared>,
}

impl Inference {
    pub fn new() -> Self {
        Self {
            manifest: Arc::new(parse_manifest(MANIFEST).expect("inference manifest is valid")),
            ctx: ModuleCtx::default(),
            shared: Arc::default(),
        }
    }

    fn job(&self, call: NodeCall, stop: Arc<AtomicBool>, agent: ureq::Agent) -> Job {
        Job {
            manifest: self.manifest.clone(),
            ctx: self.ctx.clone(),
            me: NodeRef {
                execution_id: call.execution_id,
                node_id: call.node.id.clone(),
            },
            node: call.node,
            stop,
            agent,
        }
    }

    fn event(&self, call: NodeCall) -> Result<Value, ModuleError> {
        if call.node.node_type != COMPLETE {
            return Err(ModuleError::InvalidParams(format!(
                "`{}` is not a node of this module",
                call.node.node_type
            )));
        }
        let event: NodeEvent = parse(call.args.clone())?;
        let item = Item {
            value: event.value,
            from: event.from,
        };
        let key = (call.execution_id.clone(), call.node.id.clone());
        let setting = |field: &str, floor: f64| {
            self.manifest
                .setting(&call.node, field)
                .and_then(Value::as_f64)
                .map_or(floor, |n| n.max(floor))
        };
        let (max, max_queue) = (
            setting("max_in_flight", 1.0) as usize,
            setting("max_queue", 0.0) as usize,
        );
        let timeout = Duration::from_millis(setting("timeout_ms", 1.0) as u64);
        let (stop, agent) = {
            let mut nodes = self.shared.nodes();
            let state = nodes.entry(key.clone()).or_insert_with(|| NodeState {
                running: 0,
                queue: VecDeque::new(),
                stop: Arc::default(),
                agent: client::agent(timeout),
            });
            if state.running >= max {
                if state.queue.len() < max_queue {
                    state.queue.push_back(item);
                    return Ok(json!({ "queued": state.queue.len() }));
                }
                drop(nodes);
                let job = self.job(call, Arc::default(), client::agent(timeout));
                let error = format!(
                    "{max} values running and {max_queue} waiting (`max_in_flight`, \
                     `max_queue`): this one is dropped"
                );
                job.fail(&item, error.clone());
                return Err(ModuleError::Failed(error));
            }
            state.running += 1;
            (state.stop.clone(), state.agent.clone())
        };
        let job = self.job(call, stop, agent);
        let shared = self.shared.clone();
        let mut item = item;
        std::thread::Builder::new()
            .name("regulus-inference".into())
            .spawn(move || loop {
                if job.stopped() {
                    return;
                }
                job.run(&item);
                let mut nodes = shared.nodes();
                let Some(state) = nodes.get_mut(&key) else {
                    return;
                };
                match state.queue.pop_front() {
                    Some(next) => item = next,
                    None => {
                        state.running = state.running.saturating_sub(1);
                        return;
                    }
                }
            })
            .map_err(|e| ModuleError::Failed(format!("cannot start an inference thread: {e}")))?;
        Ok(json!({ "running": true }))
    }

    /// Waiting values are dropped; running ones end after the request they are in, and
    /// emit nothing.
    fn stop(&self, call: NodeCall) {
        let key = (call.execution_id, call.node.id);
        if let Some(state) = self.shared.nodes().remove(&key) {
            state.stop.store(true, Ordering::SeqCst);
        }
    }
}

impl Default for Inference {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Inference {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            method::NODE_EVENT => self.event(parse(params)?),
            method::NODE_STOP => {
                self.stop(parse(params)?);
                Ok(Value::Null)
            }
            method::NODE_START => Ok(Value::Null),
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
    fn manifest_requires_a_model_and_routes_errors_as_error_at_1() {
        let inference = Inference::new();
        let node = inference.manifest().node_type(COMPLETE).unwrap();
        let port = |id: &str| node.ports.iter().find(|p| p.id == id).unwrap();
        assert_eq!(port(MODEL).capability, "llm.endpoint@1");
        assert!(port(MODEL).required);
        assert_eq!(port(ERROR).capability, "error@1");
        for field in [
            "system",
            "max_tokens",
            "temperature",
            "timeout_ms",
            "retries",
        ] {
            assert!(node.field(field).unwrap().default.is_some(), "{field}");
        }
    }
}
