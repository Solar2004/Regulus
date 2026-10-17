//! What the core answers, independent of the transport.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use regulus_proto::{
    code, method, Direction, DisabledNode, Execution, ExecutionStartParams, ExecutionState,
    IdParams, NodeCall, ProjectFilter, WireKind, Workflow,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::graph::{check_profile, start_order};
use crate::module::{Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use crate::profiles::ProfileStore;
use crate::registry::{CallError, Registry};

/// JSON-RPC "method not found".
const METHOD_NOT_FOUND: i32 = -32601;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_PARAMS, message)
    }
}

impl From<CallError> for RpcError {
    fn from(error: CallError) -> Self {
        let code = match &error {
            CallError::NotFound | CallError::Module(ModuleError::UnknownMethod(_)) => {
                METHOD_NOT_FOUND
            }
            CallError::Module(ModuleError::InvalidParams(_)) => code::INVALID_PARAMS,
            CallError::Unavailable { .. } => code::MODULE_UNAVAILABLE,
            CallError::Module(ModuleError::Failed(_)) | CallError::Panicked(_) => {
                code::MODULE_FAILED
            }
        };
        let message = match error {
            CallError::NotFound => "Method not found".to_string(),
            other => other.to_string(),
        };
        Self::new(code, message)
    }
}

/// A running copy of a workflow. The workflow is copied when the execution starts, so
/// later edits to the profile do not reach it.
struct Live {
    execution: Execution,
    workflow: Arc<Workflow>,
    /// Nodes whose module was asked to start them, in start order.
    started: Vec<NodeRef>,
    /// Nodes that take part: their module is loaded and their required inputs are met.
    /// Wires from any other node lead nowhere.
    active: HashSet<String>,
}

/// The base: the module registry, the profiles and the executions. It has no functions of
/// its own; it only answers about the pieces and routes calls to the modules.
pub struct Core {
    registry: Registry,
    profiles: ProfileStore,
    executions: Mutex<Vec<Live>>,
    next_execution: AtomicU64,
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("Invalid params: {e}")))
}

fn json(value: impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::new(code::MODULE_FAILED, e.to_string()))
}

impl Core {
    /// Loads the modules and builds the core. Every module gets a `ModuleCtx` that points
    /// back here; calls through it fail until `build` returns.
    pub fn build(
        modules: Vec<Box<dyn Module>>,
        without: &[String],
        profiles: ProfileStore,
    ) -> Arc<Self> {
        Arc::new_cyclic(|core| Self {
            registry: Registry::load_with(modules, without, &ModuleCtx::new(core.clone())),
            profiles,
            executions: Mutex::default(),
            next_execution: AtomicU64::new(1),
        })
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Answers every method except `hello`, which belongs to the connection.
    pub fn dispatch(&self, name: &str, params: Value) -> Result<Value, RpcError> {
        match name {
            method::PROFILE_LIST => {
                let filter: ProjectFilter = parse(params)?;
                json(
                    self.profiles
                        .list(filter.project_id.as_deref())
                        .map(|p| p.summary())
                        .collect::<Vec<_>>(),
                )
            }
            method::PROFILE_GET => json(self.profile(parse(params)?)?),
            method::PROFILE_CHECK => {
                let profile = self.profile(parse(params)?)?;
                json(check_profile(profile, |id| self.registry.is_installed(id)))
            }
            method::MODULE_LIST => json(self.registry.summaries()),
            method::EXECUTION_LIST => {
                let filter: ProjectFilter = parse(params)?;
                let wanted = filter.project_id.as_deref();
                let matches =
                    |e: &&Execution| wanted.is_none_or(|w| e.project_id.as_deref() == Some(w));
                let executions = self.executions();
                json(
                    executions
                        .iter()
                        .map(|live| &live.execution)
                        .filter(matches)
                        .collect::<Vec<_>>(),
                )
            }
            method::EXECUTION_START => json(self.start(parse(params)?)?),
            method::EXECUTION_STOP => {
                let params: IdParams = parse(params)?;
                json(
                    self.stop(&params.id)
                        .ok_or_else(|| RpcError::invalid_params("Unknown execution"))?,
                )
            }
            method::HELLO => Err(RpcError::new(
                METHOD_NOT_FOUND,
                "hello is answered by the connection",
            )),
            other => Ok(self.registry.call(other, params)?),
        }
    }

    fn profile(&self, params: IdParams) -> Result<&regulus_proto::Profile, RpcError> {
        self.profiles
            .get(&params.id)
            .ok_or_else(|| RpcError::invalid_params("Unknown profile"))
    }

    fn executions(&self) -> MutexGuard<'_, Vec<Live>> {
        self.executions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn workflow_of(&self, execution_id: &str) -> Option<Arc<Workflow>> {
        let executions = self.executions();
        let live = executions
            .iter()
            .find(|live| live.execution.id == execution_id)?;
        Some(live.workflow.clone())
    }

    fn set_state(&self, execution_id: &str, state: ExecutionState) -> Option<Execution> {
        let mut executions = self.executions();
        let live = executions
            .iter_mut()
            .find(|live| live.execution.id == execution_id)?;
        live.execution.state = state;
        Some(live.execution.clone())
    }

    /// Starts a copy of a workflow: `node/start` on every node whose module is loaded,
    /// what is used before what uses it (docs/03-modules.md §2). A node that fails to
    /// start fails the execution and stops the nodes already started.
    fn start(&self, params: ExecutionStartParams) -> Result<Execution, RpcError> {
        let profile = self
            .profiles
            .get(&params.profile_id)
            .ok_or_else(|| RpcError::invalid_params("Unknown profile"))?;
        let workflow_id = params
            .workflow_id
            .unwrap_or_else(|| profile.main_workflow_id.clone());
        let workflow = Arc::new(
            profile
                .workflow(&workflow_id)
                .ok_or_else(|| RpcError::invalid_params("Unknown workflow"))?
                .clone(),
        );
        let execution = Execution {
            id: format!(
                "exec-{}",
                self.next_execution.fetch_add(1, Ordering::Relaxed)
            ),
            workflow_id,
            project_id: profile.project_id.clone(),
            state: ExecutionState::Starting,
            disabled: Vec::new(),
        };
        let id = execution.id.clone();
        self.executions().push(Live {
            execution,
            workflow: workflow.clone(),
            started: Vec::new(),
            active: HashSet::new(),
        });

        // Resources (no module) take part as they are; a module node needs its module.
        let mut active: HashSet<&str> = workflow
            .nodes
            .iter()
            .filter(|n| {
                n.module_id
                    .as_deref()
                    .is_none_or(|m| self.registry.is_installed(m))
            })
            .map(|n| n.id.as_str())
            .collect();
        for node in start_order(&workflow) {
            let Some(module_id) = node
                .module_id
                .as_deref()
                .filter(|_| active.contains(node.id.as_str()))
            else {
                continue;
            };
            // A required input with nothing usable behind it disables the node, and in turn
            // whatever requires it, instead of failing the whole execution.
            let missing: Vec<String> = node
                .ports
                .iter()
                .filter(|p| p.required && p.direction == Direction::Input)
                .filter(|p| {
                    !workflow.wires.iter().any(|w| {
                        w.kind == WireKind::Uses
                            && w.to.node_id == node.id
                            && w.to.port_id == p.id
                            && active.contains(w.from.node_id.as_str())
                    })
                })
                .map(|p| p.id.clone())
                .collect();
            let node_ref = NodeRef {
                execution_id: id.clone(),
                node_id: node.id.clone(),
            };
            {
                let mut executions = self.executions();
                let Some(live) = executions.iter_mut().find(|live| live.execution.id == id) else {
                    break;
                };
                if !missing.is_empty() {
                    live.execution.disabled.push(DisabledNode {
                        node_id: node.id.clone(),
                        missing,
                    });
                    active.remove(node.id.as_str());
                    continue;
                }
                live.active.insert(node.id.clone());
                live.started.push(node_ref);
            }
            let call = NodeCall {
                execution_id: id.clone(),
                node: node.clone(),
                args: Value::Null,
            };
            match self
                .registry
                .call_module(module_id, method::NODE_START, json(call)?)
            {
                Ok(_) | Err(CallError::Module(ModuleError::UnknownMethod(_))) => {}
                Err(error) => {
                    self.stop(&id);
                    let reason = format!("node `{}` failed to start: {error}", node.id);
                    return self
                        .set_state(&id, ExecutionState::Failed { reason })
                        .ok_or_else(|| RpcError::invalid_params("Unknown execution"));
                }
            }
        }
        self.set_state(&id, ExecutionState::Running)
            .ok_or_else(|| RpcError::invalid_params("Unknown execution"))
    }

    /// Stops an execution: `node/stop` on its started nodes, in reverse order. Stopping
    /// a stopped or failed execution changes nothing.
    fn stop(&self, execution_id: &str) -> Option<Execution> {
        let (started, workflow) = {
            let mut executions = self.executions();
            let live = executions
                .iter_mut()
                .find(|live| live.execution.id == execution_id)?;
            if matches!(
                live.execution.state,
                ExecutionState::Starting | ExecutionState::Running
            ) {
                live.execution.state = ExecutionState::Stopped;
            }
            (std::mem::take(&mut live.started), live.workflow.clone())
        };
        for node_ref in started.iter().rev() {
            let Some(node) = workflow.nodes.iter().find(|n| n.id == node_ref.node_id) else {
                continue;
            };
            let Some(module_id) = node.module_id.as_deref() else {
                continue;
            };
            let call = NodeCall {
                execution_id: execution_id.into(),
                node: node.clone(),
                args: Value::Null,
            };
            let Ok(params) = serde_json::to_value(call) else {
                continue;
            };
            match self
                .registry
                .call_module(module_id, method::NODE_STOP, params)
            {
                Ok(_) | Err(CallError::Module(ModuleError::UnknownMethod(_))) => {}
                Err(error) => eprintln!("regulus: node `{}` failed to stop: {error}", node.id),
            }
        }
        let executions = self.executions();
        executions
            .iter()
            .find(|live| live.execution.id == execution_id)
            .map(|live| live.execution.clone())
    }

    /// Stops every execution, then shuts the modules down.
    pub fn shutdown(&self) {
        let running: Vec<String> = self
            .executions()
            .iter()
            .filter(|live| !live.started.is_empty())
            .map(|live| live.execution.id.clone())
            .collect();
        for id in running {
            self.stop(&id);
        }
        self.registry.shutdown();
    }

    /// What is wired over "uses" into `port` of a node (`ModuleCtx::resolve`).
    pub(crate) fn resolve(&self, node: &NodeRef, port: &str) -> Vec<Endpoint> {
        let (workflow, active) = {
            let executions = self.executions();
            let Some(live) = executions
                .iter()
                .find(|live| live.execution.id == node.execution_id)
            else {
                return Vec::new();
            };
            (live.workflow.clone(), live.active.clone())
        };
        workflow
            .wires
            .iter()
            .filter(|w| {
                w.kind == WireKind::Uses && w.to.node_id == node.node_id && w.to.port_id == port
            })
            .filter(|w| active.contains(&w.from.node_id))
            .filter_map(|w| {
                let from = workflow.nodes.iter().find(|n| n.id == w.from.node_id)?;
                Some(Endpoint {
                    node: NodeRef {
                        execution_id: node.execution_id.clone(),
                        node_id: from.id.clone(),
                    },
                    module_id: from.module_id.clone()?,
                })
            })
            .collect()
    }

    /// Calls the module behind an endpoint with a `NodeCall` (`ModuleCtx::call`).
    pub(crate) fn call_node(
        &self,
        to: &Endpoint,
        name: &str,
        args: Value,
    ) -> Result<Value, ModuleError> {
        let workflow = self.workflow_of(&to.node.execution_id).ok_or_else(|| {
            ModuleError::Failed(format!("execution `{}` is gone", to.node.execution_id))
        })?;
        let node = workflow
            .nodes
            .iter()
            .find(|n| n.id == to.node.node_id)
            .ok_or_else(|| ModuleError::Failed(format!("node `{}` is gone", to.node.node_id)))?;
        let call = NodeCall {
            execution_id: to.node.execution_id.clone(),
            node: node.clone(),
            args,
        };
        let params = serde_json::to_value(call).map_err(|e| ModuleError::Failed(e.to_string()))?;
        match self.registry.call_module(&to.module_id, name, params) {
            Ok(value) => Ok(value),
            Err(CallError::Module(error)) => Err(error),
            Err(error) => Err(ModuleError::Failed(error.to_string())),
        }
    }
}
