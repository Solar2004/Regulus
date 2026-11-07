//! Flow logic (docs/13-blocks.md §3.2). Today one node, `call`: every value it receives
//! starts a child execution of the workflow in its `workflow` field, fed on the input of
//! the same id; the child's answer leaves on the output of the same id, and its end on
//! `done`. Switch, Loop, Join and the rest come later.

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, ChildEvent, ChildEventKind, Manifest, NodeCall, NodeEvent};
use serde_json::{json, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
/// The field with the id of the workflow a `call` node runs.
const WORKFLOW_FIELD: &str = "workflow";
/// The output that reports each child's end.
const DONE_PORT: &str = "done";

pub struct Flow {
    manifest: Manifest,
    ctx: ModuleCtx,
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

impl Flow {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("flow manifest is valid"),
            ctx: ModuleCtx::default(),
        }
    }

    /// One value in, one child: the workflow gets it on its input of the same id.
    fn event(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(call.args)?;
        let workflow = self
            .manifest
            .setting(&call.node, WORKFLOW_FIELD)
            .and_then(Value::as_str)
            .filter(|w| !w.is_empty())
            .ok_or_else(|| {
                ModuleError::InvalidParams(format!(
                    "node `{}` names no workflow in `{WORKFLOW_FIELD}`",
                    call.node.id
                ))
            })?
            .to_string();
        let node = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        let child = self
            .ctx
            .call_workflow(&node, &workflow, vec![(event.port, event.value)])?;
        Ok(json!({ "execution_id": child }))
    }

    /// The child's answer leaves on the output of the same id; its end, on `done` if the
    /// node has it.
    fn child(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let child: ChildEvent = parse(call.args)?;
        let node = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id.clone(),
        };
        match child.kind {
            ChildEventKind::Returned { port, value, .. } => {
                if call.node.port(&port).is_none() {
                    return Err(ModuleError::InvalidParams(format!(
                        "node `{}` has no output `{port}` for what `{}` returned",
                        call.node.id, child.execution_id
                    )));
                }
                self.ctx.emit(&node, &port, value)?;
            }
            ChildEventKind::Ended { state } => {
                if call.node.port(DONE_PORT).is_some() {
                    let end = json!({
                        "execution_id": child.execution_id,
                        "workflow_id": child.workflow_id,
                        "state": state,
                    });
                    self.ctx.emit(&node, DONE_PORT, end)?;
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
            method::NODE_START | method::NODE_STOP => Ok(Value::Null),
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
    use regulus_proto::FIELD_WORKFLOW;

    #[test]
    fn manifest_declares_call_with_a_workflow_field() {
        let manifest = Flow::new().manifest().clone();
        let call = manifest.node_type("call").unwrap();
        assert_eq!(
            call.field(WORKFLOW_FIELD).unwrap().field_type,
            FIELD_WORKFLOW
        );
        assert!(call.ports.iter().any(|p| p.id == DONE_PORT));
        assert!(manifest.module.methods.is_empty());
    }

    #[test]
    fn a_call_without_a_workflow_is_refused() {
        let flow = Flow::new();
        let event = json!({ "port": "in", "capability": "text@1", "value": "x",
            "from": { "node_id": "self", "port_id": "in" }, "event": 1 });
        let node = json!({ "id": "c", "type": "call" });
        let result = flow.call(
            method::NODE_EVENT,
            json!({ "execution_id": "e", "node": node, "args": event }),
        );
        assert!(matches!(result, Err(ModuleError::InvalidParams(_))));
    }
}
