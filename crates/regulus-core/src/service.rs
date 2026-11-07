//! What the core answers, independent of the transport.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use regulus_proto::{
    code, method, BusEvent, BusEventKind, Direction, DisabledNode, EventListParams, EventPage,
    Execution, ExecutionStartParams, ExecutionState, IdParams, NameParams, Node, NodeCall,
    NodeEvent, NodeStart, PortRef, ProjectFilter, SecretSetParams, WireKind, Workflow,
    WorkflowSendParams, WorkflowSent, BOX_SEPARATOR, WORKFLOW_SELF,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::bus::{self, Journal, Listing, Pending};
use crate::config::{BusConfig, Resume};
use crate::graph::{boundary, check_profile, flatten, start_order};
use crate::module::{Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use crate::profiles::ProfileStore;
use crate::registry::{base_id, CallError, Registry};
use crate::secrets::{SecretError, Secrets};
use crate::store::{ModuleStore, Store, StoreError, StoredExecution};

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

impl From<SecretError> for RpcError {
    fn from(error: SecretError) -> Self {
        let code = match error {
            SecretError::InvalidName(_) | SecretError::Missing(_) => code::INVALID_PARAMS,
            SecretError::ReadOnly | SecretError::Store { .. } => code::MODULE_FAILED,
        };
        Self::new(code, error.to_string())
    }
}

/// What the core keeps on disk: the secrets it resolves for the modules, the root of their
/// per-node directories and the store. Without a data directory, `node_dir` fails; without
/// a store, executions and bus events live in memory only and `ModuleCtx::store` fails.
#[derive(Debug, Clone, Default)]
pub struct Storage {
    pub secrets: Secrets,
    pub data_dir: Option<PathBuf>,
    pub store: Option<Arc<Store>>,
}

/// Directories of nodes outside any project.
const GLOBAL_DATA: &str = "global";
/// Directories of nodes in a project, one per project id.
const PROJECTS_DATA: &str = "projects";

/// One plain path component: an id can never climb out of the data directory.
pub(crate) fn path_segment(id: &str) -> Result<&str, ModuleError> {
    let mut components = Path::new(id).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) if !id.contains('\\') => Ok(id),
        _ => Err(ModuleError::Failed(format!(
            "`{id}` cannot name a directory"
        ))),
    }
}

/// A running copy of a workflow. The workflow is copied when the execution starts, so
/// later edits to the profile do not reach it.
struct Live {
    /// The `N` of `exec-N`.
    number: u64,
    execution: Execution,
    workflow: Arc<Workflow>,
    /// Nodes whose module was asked to start them, in start order.
    started: Vec<NodeRef>,
    /// Nodes that take part: their module is loaded and their required inputs are met.
    /// Wires from any other node lead nowhere.
    active: HashSet<String>,
    /// Values on their way over "then" wires, delivered in order once the execution runs.
    queue: VecDeque<Pending>,
    /// A thread is delivering from `queue`.
    draining: bool,
}

/// The base: the module registry, the profiles and the executions. It has no functions of
/// its own; it only answers about the pieces and routes calls to the modules.
pub struct Core {
    /// This core, for the threads that drain the bus.
    me: Weak<Core>,
    registry: Registry,
    profiles: ProfileStore,
    storage: Storage,
    bus: BusConfig,
    /// Lock order: `executions` before `journal`, never the other way round.
    executions: Mutex<Vec<Live>>,
    journal: Mutex<Journal>,
    next_execution: AtomicU64,
    /// Executions the previous core left running, until `recover` takes them.
    recovering: Mutex<Vec<StoredExecution>>,
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
        Self::build_with(modules, without, profiles, Storage::default())
    }

    /// [`Core::build`] with secrets and a data directory for the modules.
    pub fn build_with(
        modules: Vec<Box<dyn Module>>,
        without: &[String],
        profiles: ProfileStore,
        storage: Storage,
    ) -> Arc<Self> {
        Self::build_with_bus(modules, without, profiles, storage, BusConfig::default())
    }

    /// [`Core::build_with`] with the settings of the event bus.
    pub fn build_with_bus(
        modules: Vec<Box<dyn Module>>,
        without: &[String],
        profiles: ProfileStore,
        storage: Storage,
        bus: BusConfig,
    ) -> Arc<Self> {
        // Numbers go on from the last run; what it left running waits for `recover`.
        let (next_execution, recovering) = match &storage.store {
            None => (1, Vec::new()),
            Some(store) => (store.first_execution(), store.left_running()),
        };
        let journal = Journal::new(bus.history, storage.store.clone());
        Arc::new_cyclic(|core| Self {
            me: core.clone(),
            registry: Registry::load_with(modules, without, &ModuleCtx::new(core.clone())),
            profiles,
            storage,
            executions: Mutex::default(),
            journal: Mutex::new(journal),
            bus,
            next_execution: AtomicU64::new(next_execution),
            recovering: Mutex::new(recovering),
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
                json(self.list_executions(filter.project_id.as_deref())?)
            }
            method::EXECUTION_START => json(self.start(parse(params)?)?),
            method::EXECUTION_STOP => {
                let params: IdParams = parse(params)?;
                // One still waiting for `recover` is stopped there. One that is not live
                // here (stopped or interrupted in an earlier run) is answered as the store
                // has it: there is nothing left to stop.
                let stopped = match self.stop_recovering(&params.id) {
                    Some(execution) => Some(execution),
                    None => match self.stop(&params.id, true) {
                        Some(execution) => Some(execution),
                        None => self.stored(&params.id)?.map(|stored| stored.execution),
                    },
                };
                json(stopped.ok_or_else(|| RpcError::invalid_params("Unknown execution"))?)
            }
            method::SECRET_SET => {
                let params: SecretSetParams = parse(params)?;
                self.storage.secrets.set(&params.name, &params.value)?;
                Ok(Value::Null)
            }
            method::SECRET_LIST => json(self.storage.secrets.names()?),
            method::SECRET_DELETE => {
                let params: NameParams = parse(params)?;
                if !self.storage.secrets.delete(&params.name)? {
                    return Err(RpcError::invalid_params(format!(
                        "secret `{}` is not set",
                        params.name
                    )));
                }
                Ok(Value::Null)
            }
            method::EVENT_LIST => json(self.events(&parse(params)?)?),
            method::WORKFLOW_SEND => json(self.send(parse(params)?)?),
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

    fn journal(&self) -> MutexGuard<'_, Journal> {
        self.journal
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Runs `f` on the store, if there is one. A store that fails does not stop the core:
    /// what it could not write is reported and stays in memory.
    fn persist(&self, f: impl FnOnce(&Store) -> Result<(), StoreError>) {
        if let Some(store) = &self.storage.store {
            if let Err(error) = f(store) {
                eprintln!("regulus: store: {error}");
            }
        }
    }

    fn stored(&self, id: &str) -> Result<Option<StoredExecution>, RpcError> {
        let Some(store) = &self.storage.store else {
            return Ok(None);
        };
        store
            .execution(id)
            .map_err(|e| RpcError::new(code::MODULE_FAILED, e.to_string()))
    }

    /// `execution.list`: what the store has, from every run, with what is live here on
    /// top; without a store, what this core started.
    fn list_executions(&self, project: Option<&str>) -> Result<Vec<Execution>, RpcError> {
        let mut all: Vec<(u64, Execution)> = match &self.storage.store {
            None => Vec::new(),
            Some(store) => store
                .executions(project)
                .map_err(|e| RpcError::new(code::MODULE_FAILED, e.to_string()))?
                .into_iter()
                .map(|stored| (stored.number, stored.execution))
                .collect(),
        };
        for live in self.executions().iter() {
            if project.is_some_and(|p| live.execution.project_id.as_deref() != Some(p)) {
                continue;
            }
            match all.iter_mut().find(|(_, e)| e.id == live.execution.id) {
                Some((_, execution)) => *execution = live.execution.clone(),
                None => all.push((live.number, live.execution.clone())),
            }
        }
        all.sort_by_key(|(number, _)| *number);
        Ok(all.into_iter().map(|(_, execution)| execution).collect())
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
        let execution = live.execution.clone();
        self.persist(|store| store.update_execution(&execution));
        Some(execution)
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
        // What runs is a copy with every box inlined: the bus only ever sees one graph.
        let workflow = Arc::new(
            flatten(
                profile,
                profile
                    .workflow(&workflow_id)
                    .ok_or_else(|| RpcError::invalid_params("Unknown workflow"))?,
            )
            .map_err(RpcError::invalid_params)?,
        );
        let number = self.next_execution.fetch_add(1, Ordering::Relaxed);
        let execution = Execution {
            id: format!("exec-{number}"),
            profile_id: profile.id.clone(),
            workflow_id,
            project_id: profile.project_id.clone(),
            state: ExecutionState::Starting,
            disabled: Vec::new(),
        };
        self.persist(|store| store.insert_execution(number, &execution, &workflow));
        let id = execution.id.clone();
        self.executions().push(Live {
            number,
            execution,
            workflow: workflow.clone(),
            started: Vec::new(),
            active: HashSet::new(),
            queue: VecDeque::new(),
            draining: false,
        });
        self.launch(&id, &workflow, &HashMap::new())
    }

    /// Takes the executions the previous core left running and, as `[store] resume`
    /// says, starts them again under the same id from the workflow copy they ran, or
    /// marks them `interrupted`. Nodes with a checkpoint get it in `node/start`
    /// (`NodeStart`); the rest start afresh. Returns what it did, one line each.
    pub fn recover(&self) -> Vec<String> {
        let Some(store) = self.storage.store.clone() else {
            return Vec::new();
        };
        let mut report = Vec::new();
        loop {
            // Each one leaves `recovering` and enters memory under the same lock, so a
            // stop always finds it in one or the other.
            let mut recovering = self.recovering();
            if recovering.is_empty() {
                break;
            }
            let StoredExecution {
                number,
                mut execution,
                workflow,
            } = recovering.remove(0);
            let checkpoints = store
                .checkpoints(execution.project_id.as_deref(), &execution.id)
                .unwrap_or_else(|e| {
                    report.push(format!("{}: {e}", execution.id));
                    HashMap::new()
                });
            let resume = match store.resume() {
                Resume::Always => true,
                Resume::Never => false,
                Resume::Checkpoint => !checkpoints.is_empty(),
            };
            if !resume {
                let reason = if store.resume() == Resume::Never {
                    "the core stopped and [store] resume = never"
                } else {
                    "the core stopped and it saved no checkpoint"
                };
                execution.state = ExecutionState::Interrupted {
                    reason: reason.into(),
                };
                self.persist(|store| store.update_execution(&execution));
                report.push(format!("{} interrupted: {reason}", execution.id));
                self.push_live(number, execution, Arc::new(workflow));
                continue;
            }
            let id = execution.id.clone();
            execution.state = ExecutionState::Starting;
            execution.disabled.clear();
            let workflow = Arc::new(workflow);
            self.push_live(number, execution, workflow.clone());
            drop(recovering);
            let line = match self.launch(&id, &workflow, &checkpoints) {
                Ok(Execution {
                    state: ExecutionState::Failed { reason },
                    ..
                }) => format!("{id} failed to resume: {reason}"),
                Ok(_) => format!("{id} resumed ({} checkpoints)", checkpoints.len()),
                Err(error) => format!("{id} failed to resume: {}", error.message),
            };
            report.push(line);
        }
        report
    }

    fn recovering(&self) -> MutexGuard<'_, Vec<StoredExecution>> {
        self.recovering
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Keeps an execution in memory, with nothing started yet.
    fn push_live(&self, number: u64, execution: Execution, workflow: Arc<Workflow>) {
        self.executions().push(Live {
            number,
            execution,
            workflow,
            started: Vec::new(),
            active: HashSet::new(),
            queue: VecDeque::new(),
            draining: false,
        });
    }

    /// Stops an execution `recover` has not reached yet: it is recorded stopped and
    /// `recover` no longer sees it.
    fn stop_recovering(&self, id: &str) -> Option<Execution> {
        let mut recovering = self.recovering();
        let position = recovering.iter().position(|s| s.execution.id == id)?;
        let StoredExecution {
            number,
            mut execution,
            workflow,
        } = recovering.remove(position);
        execution.state = ExecutionState::Stopped;
        self.persist(|store| store.update_execution(&execution));
        self.push_live(number, execution.clone(), Arc::new(workflow));
        Some(execution)
    }

    /// Starts the nodes of a live execution, what is used before what uses it. A node
    /// with a checkpoint gets it in `node/start`.
    fn launch(
        &self,
        id: &str,
        workflow: &Workflow,
        checkpoints: &HashMap<String, Value>,
    ) -> Result<Execution, RpcError> {
        let id = id.to_string();

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
        for node in start_order(workflow) {
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
            let args = match checkpoints.get(&node.id) {
                None => Value::Null,
                Some(state) => json(NodeStart {
                    checkpoint: Some(state.clone()),
                })?,
            };
            let started = match self.node_call(&id, node, args) {
                Err(error) => Err(error.to_string()),
                Ok(call) => {
                    match self
                        .registry
                        .call_module(module_id, method::NODE_START, json(call)?)
                    {
                        Ok(_) | Err(CallError::Module(ModuleError::UnknownMethod(_))) => Ok(()),
                        Err(error) => Err(error.to_string()),
                    }
                }
            };
            match started {
                Ok(()) => {}
                Err(error) => {
                    self.stop(&id, true);
                    let reason = format!("node `{}` failed to start: {error}", node.id);
                    return self
                        .set_state(&id, ExecutionState::Failed { reason })
                        .ok_or_else(|| RpcError::invalid_params("Unknown execution"));
                }
            }
        }
        let running = self
            .set_state(&id, ExecutionState::Running)
            .ok_or_else(|| RpcError::invalid_params("Unknown execution"))?;
        // What the nodes emitted while starting is delivered now that all of them run.
        self.kick(&id);
        Ok(running)
    }

    /// Stops an execution: `node/stop` on its started nodes, in reverse order. Stopping
    /// a stopped or failed execution changes nothing. Without `record`, the store keeps it
    /// as it was, so the next core finds it running and recovers it.
    fn stop(&self, execution_id: &str, record: bool) -> Option<Execution> {
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
                if record {
                    let execution = live.execution.clone();
                    self.persist(|store| store.update_execution(&execution));
                }
            }
            // Values still on their way are dropped: their targets are about to stop.
            live.queue.clear();
            (std::mem::take(&mut live.started), live.workflow.clone())
        };
        for node_ref in started.iter().rev() {
            let Some(node) = workflow.nodes.iter().find(|n| n.id == node_ref.node_id) else {
                continue;
            };
            let Some(module_id) = node.module_id.as_deref() else {
                continue;
            };
            // A secret gone since the start must not keep a node from stopping.
            let call = self
                .node_call(execution_id, node, Value::Null)
                .unwrap_or_else(|_| NodeCall {
                    execution_id: execution_id.into(),
                    node: node.clone(),
                    args: Value::Null,
                });
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

    /// Stops every execution, then shuts the modules down. The store still has them as
    /// they were: the next core recovers them (`recover`).
    pub fn shutdown(&self) {
        let running: Vec<String> = self
            .executions()
            .iter()
            .filter(|live| !live.started.is_empty())
            .map(|live| live.execution.id.clone())
            .collect();
        for id in running {
            self.stop(&id, false);
        }
        self.registry.shutdown();
        // A clean shutdown loses nothing that was queued for the store.
        if let Some(store) = &self.storage.store {
            store.flush();
        }
    }

    /// What a module gets for one of its nodes: the node with its secrets resolved.
    fn node_call(
        &self,
        execution_id: &str,
        node: &Node,
        args: Value,
    ) -> Result<NodeCall, SecretError> {
        let mut node = node.clone();
        for value in node.config.values_mut() {
            self.storage.secrets.resolve(value)?;
        }
        Ok(NodeCall {
            execution_id: execution_id.into(),
            node,
            args,
        })
    }

    /// `<data>/{global | projects/<project>}/<module>/<workflow>/<node>`
    /// (`ModuleCtx::node_dir`). The module id loses its version: an upgrade keeps the data.
    pub(crate) fn node_dir(&self, module_id: &str, node: &NodeRef) -> Result<PathBuf, ModuleError> {
        let root = self
            .storage
            .data_dir
            .as_ref()
            .ok_or_else(|| ModuleError::Failed("this core has no data directory".into()))?;
        let (workflow_id, project_id) = {
            let executions = self.executions();
            let live = executions
                .iter()
                .find(|live| live.execution.id == node.execution_id)
                .ok_or_else(|| {
                    ModuleError::Failed(format!("execution `{}` is gone", node.execution_id))
                })?;
            (
                live.execution.workflow_id.clone(),
                live.execution.project_id.clone(),
            )
        };
        let mut dir = match &project_id {
            None => root.join(GLOBAL_DATA),
            Some(project) => root.join(PROJECTS_DATA).join(path_segment(project)?),
        };
        let module = module_id.split('@').next().unwrap_or_default();
        if module.is_empty() {
            return Err(ModuleError::Failed("no module behind this context".into()));
        }
        for part in module.split('/') {
            dir.push(path_segment(part)?);
        }
        dir.push(path_segment(&workflow_id)?);
        // A node inside a box (`box/node`) gets a directory inside the box's.
        for part in node.node_id.split(BOX_SEPARATOR) {
            dir.push(path_segment(part)?);
        }
        std::fs::create_dir_all(&dir)
            .map_err(|e| ModuleError::Failed(format!("cannot create {}: {e}", dir.display())))?;
        Ok(dir)
    }

    /// The key-value space of a module (`ModuleCtx::store`): the namespace is the module id
    /// without its version, so an upgrade keeps the data; the database is that of the
    /// node's project, or the global one.
    pub(crate) fn module_store(
        &self,
        module_id: &str,
        node: Option<&NodeRef>,
    ) -> Result<ModuleStore, ModuleError> {
        let store = self
            .storage
            .store
            .as_ref()
            .ok_or_else(|| ModuleError::Failed("this core has no store".into()))?;
        let namespace = base_id(module_id);
        if namespace.is_empty() {
            return Err(ModuleError::Failed("no module behind this context".into()));
        }
        let project = match node {
            None => None,
            Some(node) => self.project_of_own(module_id, node, false)?,
        };
        Ok(store.module_store(project.as_deref(), namespace)?)
    }

    /// The project of a node's execution, if the node belongs to `module_id`: a module
    /// cannot reach a project, or a state, through a node that is not its own.
    fn project_of_own(
        &self,
        module_id: &str,
        node: &NodeRef,
        live_only: bool,
    ) -> Result<Option<String>, ModuleError> {
        let executions = self.executions();
        let live = executions
            .iter()
            .find(|live| {
                live.execution.id == node.execution_id
                    && (!live_only || live.execution.state.is_live())
            })
            .ok_or_else(|| {
                ModuleError::Failed(format!("execution `{}` is not running", node.execution_id))
            })?;
        let owned = live
            .workflow
            .node(&node.node_id)
            .and_then(|n| n.module_id.as_deref())
            .is_some_and(|m| base_id(m) == base_id(module_id));
        if !owned {
            return Err(ModuleError::InvalidParams(format!(
                "node `{}` does not belong to this module",
                node.node_id
            )));
        }
        Ok(live.execution.project_id.clone())
    }

    /// Saves the state of one of a module's nodes in a live execution
    /// (`ModuleCtx::checkpoint`), for `recover` to hand back after a restart.
    pub(crate) fn checkpoint(
        &self,
        module_id: &str,
        node: &NodeRef,
        state: &Value,
    ) -> Result<(), ModuleError> {
        let store = self
            .storage
            .store
            .as_ref()
            .ok_or_else(|| ModuleError::Failed("this core has no store".into()))?;
        let project = self.project_of_own(module_id, node, true)?;
        Ok(store.checkpoint(project.as_deref(), &node.execution_id, &node.node_id, state)?)
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
        let call = self
            .node_call(&to.node.execution_id, node, args)
            .map_err(|e| ModuleError::Failed(e.to_string()))?;
        let params = serde_json::to_value(call).map_err(|e| ModuleError::Failed(e.to_string()))?;
        match self.registry.call_module(&to.module_id, name, params) {
            Ok(value) => Ok(value),
            Err(CallError::Module(error)) => Err(error),
            Err(error) => Err(ModuleError::Failed(error.to_string())),
        }
    }

    // --- bus ----------------------------------------------------------------------------

    /// `event.list`: the kept bus events `params` asks for, and how many deliveries are
    /// still queued for the executions it names.
    fn events(&self, params: &EventListParams) -> Result<EventPage, RpcError> {
        let pending = self
            .executions()
            .iter()
            .filter(|live| {
                params
                    .execution_id
                    .as_deref()
                    .is_none_or(|id| live.execution.id == id)
                    && params
                        .project_id
                        .as_deref()
                        .is_none_or(|id| live.execution.project_id.as_deref() == Some(id))
            })
            .map(|live| live.queue.len() + usize::from(live.draining))
            .sum();
        let limit = match (params.limit, self.bus.max_page) {
            (limit, 0) => limit,
            (None, max) => Some(max),
            (Some(limit), max) => Some(limit.min(max)),
        };
        let listing = self.journal().list(params, limit);
        let (events, next) = match listing {
            Listing::Ready(events, next) => (events, next),
            Listing::Store { store, upto } => {
                // Everything up to `upto` was queued before; wait until it is written.
                store.flush();
                let events = store
                    .events(params, upto, limit)
                    .map_err(|e| RpcError::new(code::MODULE_FAILED, e.to_string()))?;
                let next = bus::next_cursor(&events, limit, params.since, upto);
                (events, next)
            }
        };
        Ok(EventPage {
            events,
            next,
            pending,
        })
    }

    /// Puts a value on a "then" output (`ModuleCtx::emit`) and queues it for every input
    /// wired to that output. A value that cannot reach an input is reported on the bus;
    /// a chain deeper than `bus.max_depth` is cut and reported.
    pub(crate) fn emit(
        &self,
        module_id: &str,
        node: &NodeRef,
        port: &str,
        value: Value,
    ) -> Result<(), ModuleError> {
        let depth = bus::handling_depth(&node.execution_id).map_or(0, |d| d + 1);
        let mut executions = self.executions();
        let live = executions
            .iter_mut()
            .find(|live| live.execution.id == node.execution_id)
            .ok_or_else(|| {
                ModuleError::Failed(format!("execution `{}` is gone", node.execution_id))
            })?;
        if !matches!(
            live.execution.state,
            ExecutionState::Starting | ExecutionState::Running
        ) {
            return Err(ModuleError::Failed(format!(
                "execution `{}` is not running",
                node.execution_id
            )));
        }
        let workflow = live.workflow.clone();
        let owner = workflow
            .node(&node.node_id)
            .filter(|n| n.module_id.as_deref().map(base_id) == Some(base_id(module_id)))
            .ok_or_else(|| {
                ModuleError::InvalidParams(format!(
                    "node `{}` does not belong to this module",
                    node.node_id
                ))
            })?;
        if !live.active.contains(&node.node_id) {
            return Err(ModuleError::Failed(format!(
                "node `{}` is not running",
                node.node_id
            )));
        }
        let output = owner
            .port(port)
            .filter(|p| p.direction == Direction::Output && p.kind == WireKind::Then)
            .ok_or_else(|| {
                ModuleError::InvalidParams(format!(
                    "`{port}` is not a \"then\" output of node `{}`",
                    node.node_id
                ))
            })?;
        let from = PortRef {
            node_id: node.node_id.clone(),
            port_id: port.into(),
        };
        let capability = output.capability.clone();
        self.publish(live, from, capability, value, depth);
        drop(executions);
        self.kick(&node.execution_id);
        Ok(())
    }

    /// Records `value` as emitted from `from` in `live` and queues it for every input
    /// wired to `from`; returns the `seq` of the `emitted` event, or of the `cut` when the
    /// chain is deeper than `bus.max_depth`. The caller holds `executions` and kicks the
    /// queue afterwards.
    fn publish(
        &self,
        live: &mut Live,
        from: PortRef,
        capability: String,
        value: Value,
        depth: u32,
    ) -> u64 {
        let workflow = live.workflow.clone();
        let head = |kind| BusEvent {
            seq: 0,
            at: 0,
            execution_id: live.execution.id.clone(),
            project_id: live.execution.project_id.clone(),
            event: 0,
            from: from.clone(),
            capability: capability.clone(),
            depth,
            kind,
        };

        let mut journal = self.journal();
        if depth > self.bus.max_depth {
            let max_depth = self.bus.max_depth;
            let seq = journal.record(head(BusEventKind::Cut { max_depth }));
            eprintln!(
                "regulus: bus cut a chain in `{}` at `{}.{}`: more than {max_depth} \
                 deliveries deep (event #{seq})",
                live.execution.id, from.node_id, from.port_id
            );
            return seq;
        }
        let event = journal.record(head(BusEventKind::Emitted {
            value: value.clone(),
        }));
        let mut queued = VecDeque::new();
        let max_queue = self.bus.max_queue;
        let mut room = if max_queue == 0 {
            usize::MAX
        } else {
            max_queue.saturating_sub(live.queue.len())
        };
        for wire in workflow
            .wires
            .iter()
            .filter(|w| w.kind == WireKind::Then && w.from == from)
        {
            // A boundary port takes a value from either side; a node only on its inputs.
            let input = boundary(&workflow, &wire.to)
                .or_else(|| {
                    workflow
                        .node(&wire.to.node_id)
                        .and_then(|n| n.port(&wire.to.port_id))
                        .filter(|p| p.direction == Direction::Input)
                })
                .filter(|p| p.kind == WireKind::Then);
            let error = match input {
                None => "no \"then\" input at the other end".to_string(),
                Some(p) if p.capability != capability => {
                    format!("`{capability}` does not fit into `{}`", p.capability)
                }
                Some(_) if room == 0 => {
                    format!("the execution's queue is full ({max_queue} values waiting)")
                }
                Some(_) => {
                    room -= 1;
                    queued.push_back(Pending {
                        event,
                        depth,
                        from: from.clone(),
                        capability: capability.clone(),
                        value: value.clone(),
                        to: wire.to.clone(),
                    });
                    continue;
                }
            };
            journal.record(BusEvent {
                event,
                ..head(BusEventKind::Failed {
                    to: wire.to.clone(),
                    error,
                })
            });
        }
        drop(journal);
        live.queue.append(&mut queued);
        event
    }

    /// `workflow.send`: a value from outside on an input of the workflow a live execution
    /// runs. It starts a chain of its own, at depth 0; what reaches the workflow's outputs
    /// shows up in `event.list` as emitted from `self`.
    fn send(&self, params: WorkflowSendParams) -> Result<WorkflowSent, RpcError> {
        let mut executions = self.executions();
        let live = executions
            .iter_mut()
            .find(|live| live.execution.id == params.execution_id)
            .ok_or_else(|| RpcError::invalid_params("Unknown execution"))?;
        if !live.execution.state.is_live() {
            return Err(RpcError::invalid_params(format!(
                "execution `{}` is not running",
                params.execution_id
            )));
        }
        let capability = live
            .workflow
            .port(&params.port)
            .filter(|p| p.direction == Direction::Input && p.kind == WireKind::Then)
            .map(|p| p.capability.clone())
            .ok_or_else(|| {
                RpcError::invalid_params(format!(
                    "`{}` is not a \"then\" input of workflow `{}`",
                    params.port, live.execution.workflow_id
                ))
            })?;
        let from = PortRef {
            node_id: WORKFLOW_SELF.into(),
            port_id: params.port,
        };
        let event = self.publish(live, from, capability, params.value, 0);
        drop(executions);
        self.kick(&params.execution_id);
        Ok(WorkflowSent { event })
    }

    /// Starts draining the queue of a running execution unless a thread already does.
    fn kick(&self, execution_id: &str) {
        {
            let mut executions = self.executions();
            let Some(live) = executions
                .iter_mut()
                .find(|live| live.execution.id == execution_id)
            else {
                return;
            };
            if live.execution.state != ExecutionState::Running
                || live.draining
                || live.queue.is_empty()
            {
                return;
            }
            live.draining = true;
        }
        let core = self.me.clone();
        let id = execution_id.to_string();
        let spawned = std::thread::Builder::new()
            .name(bus::THREAD_NAME.into())
            .spawn(move || Self::drain(core, &id));
        if let Err(error) = spawned {
            eprintln!("regulus: cannot start the bus of `{execution_id}`: {error}");
            if let Some(live) = self
                .executions()
                .iter_mut()
                .find(|live| live.execution.id == execution_id)
            {
                live.draining = false;
            }
        }
    }

    /// Delivers an execution's queue in order until it is empty or the execution stops.
    fn drain(core: Weak<Core>, execution_id: &str) {
        while let Some(core) = core.upgrade() {
            let Some(next) = core.next_delivery(execution_id) else {
                return;
            };
            core.deliver(execution_id, next);
        }
    }

    fn next_delivery(&self, execution_id: &str) -> Option<Pending> {
        let mut executions = self.executions();
        let live = executions
            .iter_mut()
            .find(|live| live.execution.id == execution_id)?;
        let next = if live.execution.state == ExecutionState::Running {
            live.queue.pop_front()
        } else {
            live.queue.clear();
            None
        };
        live.draining = next.is_some();
        next
    }

    /// Hands one value to the module of its target node as `node/event`, and reports
    /// whether it took it.
    fn deliver(&self, execution_id: &str, pending: Pending) {
        let found = {
            let executions = self.executions();
            executions
                .iter()
                .find(|live| live.execution.id == execution_id)
                .map(|live| {
                    (
                        live.workflow.clone(),
                        live.execution.project_id.clone(),
                        live.active.contains(&pending.to.node_id),
                    )
                })
        };
        // The execution vanished meanwhile: nothing left to report into.
        let Some((workflow, project_id, active)) = found else {
            return;
        };
        if boundary(&workflow, &pending.to).is_some() {
            return self.cross(execution_id, project_id, pending);
        }
        let node = workflow
            .node(&pending.to.node_id)
            .filter(|n| active && n.module_id.is_some())
            .cloned();
        let result = match node {
            None => Err(format!("node `{}` is not running", pending.to.node_id)),
            Some(node) => {
                let args = NodeEvent {
                    port: pending.to.port_id.clone(),
                    capability: pending.capability.clone(),
                    value: pending.value,
                    from: pending.from.clone(),
                    event: pending.event,
                };
                let module_id = node.module_id.clone().unwrap_or_default();
                serde_json::to_value(args)
                    .map_err(|e| e.to_string())
                    .and_then(|args| {
                        self.node_call(execution_id, &node, args)
                            .map_err(|e| e.to_string())
                    })
                    .and_then(|call| serde_json::to_value(call).map_err(|e| e.to_string()))
                    .and_then(|params| {
                        bus::while_handling(execution_id, pending.depth, || {
                            self.registry
                                .call_module(&module_id, method::NODE_EVENT, params)
                        })
                        .map_err(|e| e.to_string())
                    })
            }
        };
        let to = pending.to;
        let kind = match result {
            Ok(_) => BusEventKind::Delivered { to },
            Err(error) => BusEventKind::Failed { to, error },
        };
        self.journal().record(BusEvent {
            seq: 0,
            at: 0,
            execution_id: execution_id.into(),
            project_id,
            event: pending.event,
            from: pending.from,
            capability: pending.capability,
            depth: pending.depth,
            kind,
        });
    }

    /// A value reached a boundary port: the box or the workflow takes it, and it goes on
    /// from that same port to whatever is wired on the other side, one delivery deeper,
    /// so the depth cut still sees loops through boxes. Outputs of the top workflow have
    /// nothing on the other side: their `emitted` event is the answer a client reads.
    fn cross(&self, execution_id: &str, project_id: Option<String>, pending: Pending) {
        let event = BusEvent {
            seq: 0,
            at: 0,
            execution_id: execution_id.into(),
            project_id,
            event: pending.event,
            from: pending.from,
            capability: pending.capability.clone(),
            depth: pending.depth,
            kind: BusEventKind::Delivered {
                to: pending.to.clone(),
            },
        };
        let mut executions = self.executions();
        let Some(live) = executions
            .iter_mut()
            .find(|live| live.execution.id == execution_id)
            .filter(|live| live.execution.state.is_live())
        else {
            return;
        };
        self.journal().record(event);
        self.publish(
            live,
            pending.to,
            pending.capability,
            pending.value,
            pending.depth + 1,
        );
    }
}
