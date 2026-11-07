//! Flow logic (docs/13-blocks.md §3.2): the nodes that decide where a value goes next.
//! `call` runs a workflow as a child execution per value; `switch` routes by rules;
//! `loop` repeats a body until a rule or a cap; `join` waits for several branches;
//! `fanout` copies or splits; `wait` delays without holding the bus; `variable` keeps a
//! value in the store; `convert` changes what a value is between capabilities.
//!
//! None of them is a flow engine: each takes values over "then" wires and emits on its
//! outputs, and the bus does the rest. Ports are declared on each node in the profile;
//! nothing here assumes a capability except `convert`, whose job it is.

mod convert;
mod join;
mod repeat;
mod route;
mod rule;
mod variable;
mod wait;

use std::collections::HashMap;
use std::sync::{Mutex, MutexGuard, PoisonError};

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{
    method, ChildEvent, ChildEventKind, Direction, Manifest, NodeCall, NodeEvent, WireKind,
};
use serde_json::{json, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
/// The field with the id of the workflow a `call` node runs.
const WORKFLOW_FIELD: &str = "workflow";
/// The output that reports each child's end.
const DONE_PORT: &str = "done";

/// What a node of one type does with a value that arrives on one of its inputs.
type OnEvent = fn(&Here, NodeEvent) -> Result<Value, ModuleError>;

/// Every node type that takes values, by type. A new node is a row here.
const EVENTS: &[(&str, OnEvent)] = &[
    ("call", call_event),
    ("switch", route::switch),
    ("fanout", route::fanout),
    ("loop", repeat::event),
    ("join", join::event),
    ("wait", wait::event),
    ("variable", variable::event),
    ("convert", convert::event),
];

pub struct Flow {
    manifest: Manifest,
    ctx: ModuleCtx,
    /// The turn each running `loop` is on.
    loops: Mutex<HashMap<NodeRef, u64>>,
    /// What each `join` holds until its barrier opens.
    joins: Mutex<HashMap<NodeRef, join::Gate>>,
    /// The values each `wait` holds.
    timers: wait::Timers,
    /// `variable`s of a core without a store live here, for as long as the core does.
    memory: Mutex<HashMap<String, Value>>,
}

pub(crate) fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

/// A lock that a panic elsewhere does not poison for good: the state it guards is
/// always whole between statements.
pub(crate) fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// One call to one node: the node with its settings, and how to act as it.
pub(crate) struct Here<'a> {
    pub flow: &'a Flow,
    pub call: NodeCall,
    pub me: NodeRef,
}

impl Here<'_> {
    /// The node's own value for `field`, or the manifest's default.
    pub fn setting(&self, field: &str) -> Option<&Value> {
        self.flow.manifest.setting(&self.call.node, field)
    }

    /// A setting as text: a string as is, anything else as JSON, nothing as empty.
    pub fn text(&self, field: &str) -> String {
        self.setting(field).map(rule::text).unwrap_or_default()
    }

    /// A setting as a whole number; text that reads as one counts.
    pub fn number(&self, field: &str) -> u64 {
        match self.setting(field) {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    pub fn emit(&self, port: &str, value: Value) -> Result<(), ModuleError> {
        self.flow.ctx.emit(&self.me, port, value)
    }

    /// Emits each `(port, value)` in order and answers which ports got one.
    pub fn send(&self, out: Vec<(String, Value)>) -> Result<Value, ModuleError> {
        let mut ports = Vec::with_capacity(out.len());
        for (port, value) in out {
            self.emit(&port, value)?;
            ports.push(port);
        }
        Ok(json!({ "sent": ports }))
    }

    /// The ids of the node's "then" ports facing `direction`, in declared order.
    pub fn then_ports(&self, direction: Direction) -> Vec<String> {
        self.call
            .node
            .ports
            .iter()
            .filter(|p| p.kind == WireKind::Then && p.direction == direction)
            .map(|p| p.id.clone())
            .collect()
    }

    pub fn has_output(&self, port: &str) -> bool {
        self.call
            .node
            .port(port)
            .is_some_and(|p| p.kind == WireKind::Then && p.direction == Direction::Output)
    }

    /// The capability of one of the node's ports, if it has it.
    pub fn capability(&self, port: &str) -> Option<&str> {
        self.call.node.port(port).map(|p| p.capability.as_str())
    }

    pub fn ctx(&self) -> &ModuleCtx {
        &self.flow.ctx
    }
}

/// One value in, one child: the workflow gets it on its input of the same id.
fn call_event(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    let workflow = here
        .setting(WORKFLOW_FIELD)
        .and_then(Value::as_str)
        .filter(|w| !w.is_empty())
        .ok_or_else(|| {
            ModuleError::InvalidParams(format!(
                "node `{}` names no workflow in `{WORKFLOW_FIELD}`",
                here.me.node_id
            ))
        })?
        .to_string();
    let child = here
        .ctx()
        .call_workflow(&here.me, &workflow, vec![(event.port, event.value)])?;
    Ok(json!({ "execution_id": child }))
}

impl Flow {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("flow manifest is valid"),
            ctx: ModuleCtx::default(),
            loops: Mutex::default(),
            joins: Mutex::default(),
            timers: wait::Timers::default(),
            memory: Mutex::default(),
        }
    }

    fn here(&self, call: NodeCall) -> Here<'_> {
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        Here {
            flow: self,
            call,
            me,
        }
    }

    fn event(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        let (_, handle) = EVENTS
            .iter()
            .find(|(node_type, _)| *node_type == call.node.node_type)
            .ok_or_else(|| {
                ModuleError::InvalidParams(format!(
                    "node type `{}` takes no events",
                    call.node.node_type
                ))
            })?;
        handle(&self.here(call), event)
    }

    /// Checks what a node can only check whole, and sets up what it keeps.
    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let here = self.here(call);
        match here.call.node.node_type.as_str() {
            "wait" => wait::start(&here),
            "join" => join::start(&here),
            _ => Ok(Value::Null),
        }
    }

    /// Whatever a node held dies with it: turns, half-open barriers, delayed values.
    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let me = self.here(call).me;
        lock(&self.loops).remove(&me);
        lock(&self.joins).remove(&me);
        self.timers.cancel(&me);
        Ok(Value::Null)
    }

    /// The child's answer leaves on the output of the same id; its end, on `done` if the
    /// node has it.
    fn child(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let child: ChildEvent = parse(call.args.clone())?;
        let here = self.here(call);
        match child.kind {
            ChildEventKind::Returned { port, value, .. } => {
                if here.call.node.port(&port).is_none() {
                    return Err(ModuleError::InvalidParams(format!(
                        "node `{}` has no output `{port}` for what `{}` returned",
                        here.me.node_id, child.execution_id
                    )));
                }
                here.emit(&port, value)?;
            }
            ChildEventKind::Ended { state } => {
                if here.call.node.port(DONE_PORT).is_some() {
                    let end = json!({
                        "execution_id": child.execution_id,
                        "workflow_id": child.workflow_id,
                        "state": state,
                    });
                    here.emit(DONE_PORT, end)?;
                }
            }
        }
        Ok(Value::Null)
    }
}

impl Default for Flow {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Flow {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError> {
        match method {
            method::NODE_EVENT => self.event(parse(params)?),
            method::NODE_CHILD => self.child(parse(params)?),
            method::NODE_STOP => self.stop(parse(params)?),
            method::NODE_START => self.start(parse(params)?),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.timers.cancel_all();
        Ok(())
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use regulus_proto::FIELD_WORKFLOW;

    /// A `NodeCall` for a node of `node_type` with `config` and `ports`, as JSON.
    pub fn node_call(node_type: &str, config: Value, ports: Value, args: Value) -> Value {
        let node = json!({ "id": "n", "type": node_type, "module_id": "regulus/flow@0.1.0",
                           "config": config, "ports": ports });
        json!({ "execution_id": "e", "node": node, "args": args })
    }

    #[test]
    fn manifest_declares_every_node_that_takes_events() {
        let manifest = Flow::new().manifest().clone();
        let call = manifest.node_type("call").unwrap();
        assert_eq!(
            call.field(WORKFLOW_FIELD).unwrap().field_type,
            FIELD_WORKFLOW
        );
        assert!(call.ports.iter().any(|p| p.id == DONE_PORT));
        assert!(manifest.module.methods.is_empty());
        for (node_type, _) in EVENTS {
            assert!(manifest.node_type(node_type).is_some(), "{node_type}");
        }
        assert_eq!(manifest.nodes.len(), EVENTS.len());
    }

    #[test]
    fn a_call_without_a_workflow_is_refused() {
        let flow = Flow::new();
        let event = json!({ "port": "in", "capability": "text@1", "value": "x",
            "from": { "node_id": "self", "port_id": "in" }, "event": 1 });
        let result = flow.call(
            method::NODE_EVENT,
            node_call("call", json!({}), json!([]), event),
        );
        assert!(matches!(result, Err(ModuleError::InvalidParams(_))));
    }
}
