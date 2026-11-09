//! Children that keep the node's "uses" inputs (docs/02-architecture.md §4 rule 5): a
//! node that runs a workflow hands on whatever is wired into its own "uses" inputs to the
//! child's "uses" inputs of the same id, as bindings. The node declares those inputs in
//! the profile (ports are data); the module never names one.

use regulus_core::{ChildOptions, Endpoint, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{Direction, Node, WireKind};
use serde_json::Value;

/// What is wired into each "uses" input of `node`, by port id; inputs with nothing behind
/// them are left out.
pub(crate) fn of(ctx: &ModuleCtx, me: &NodeRef, node: &Node) -> Vec<(String, Vec<Endpoint>)> {
    node.ports
        .iter()
        .filter(|p| p.direction == Direction::Input && p.kind == WireKind::Uses)
        .filter_map(|p| {
            let endpoints = ctx.resolve(me, &p.id);
            (!endpoints.is_empty()).then(|| (p.id.clone(), endpoints))
        })
        .collect()
}

/// Starts a child of `workflow` with `opts`, its bindings first narrowed to the "uses"
/// inputs the workflow declares: the core refuses a binding to any other, and the node
/// cannot see the workflow's ports, so each refused one is dropped from `opts.bindings`
/// (which the caller may keep for the next child) and the start is tried again.
pub(crate) fn start(
    ctx: &ModuleCtx,
    me: &NodeRef,
    workflow: &str,
    inputs: Vec<(String, Value)>,
    opts: &mut ChildOptions,
) -> Result<String, ModuleError> {
    loop {
        match ctx.start_child(me, workflow, inputs.clone(), opts.clone()) {
            Err(ModuleError::InvalidParams(message)) => {
                let refused = opts.bindings.iter().position(|(port, _)| {
                    message == format!("`{port}` is not a \"uses\" input of workflow `{workflow}`")
                });
                match refused {
                    Some(index) => {
                        opts.bindings.remove(index);
                    }
                    None => return Err(ModuleError::InvalidParams(message)),
                }
            }
            other => return other,
        }
    }
}
