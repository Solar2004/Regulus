//! What the core answers, independent of the transport.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, RwLock, RwLockReadGuard, Weak};

use regulus_proto::ModuleState;
use regulus_proto::{
    code, method, BusEvent, BusEventKind, ChildEvent, ChildEventKind, ClientSettings, Direction,
    DisabledNode, EventListParams, EventPage, Execution, ExecutionStartParams, ExecutionState,
    ExecutionTree, ExecutionTreeParams, HookAnswer, HookDispatchParams, HookDispatched, HookReply,
    IdParams, ModuleManifestParams, NameParams, Node, NodeCall, NodeEvent, NodeStart, PortRef,
    Profile, ProfileDeleted, ProfileSaveParams, ProfileSaved, ProjectDetails, ProjectFilter,
    ProjectOpenParams, ProjectSummary, ProjectTrustParams, ProjectTrusted, SecretSetParams,
    Severity, TemplateApplied, TemplateApplyParams, TemplateParams, TemplateText, WireKind,
    Workflow, WorkflowSendParams, WorkflowSent, BOX_SEPARATOR, HOOK_CAPABILITY, WORKFLOW_SELF,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::bus::{self, ChildNews, Journal, Listing, News, Pending};
use crate::config::{BusConfig, CallConfig, HooksConfig, LocalModulesConfig, Resume};
use crate::graph::{
    boundary, check_fields, check_profile, check_vetoed, flatten, missing_workflow, start_order,
    wire_end, End,
};
use crate::module::{ChildMode, ChildOptions, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use crate::process::{LocalSpec, ProcessModule};
use crate::profiles::ProfileStore;
use crate::projects::{self, Project, ProjectError, Projects};
use crate::registry::{base_id, CallError, Local, Registry};
use crate::secrets::{SecretError, Secrets};
use crate::store::{ModuleStore, Store, StoreError, StoredExecution};
use crate::templates::{self, Places, TemplateError};

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

impl From<TemplateError> for RpcError {
    fn from(error: TemplateError) -> Self {
        let code = match error {
            TemplateError::NotFound(_) => code::TEMPLATE_NOT_FOUND,
            TemplateError::Invalid(_) => code::TEMPLATE_INVALID,
            TemplateError::Params(_) => code::INVALID_PARAMS,
            TemplateError::Unmet(_) => code::TEMPLATE_UNMET,
            TemplateError::Exists(_) => code::PROFILE_EXISTS,
            TemplateError::Io(_) => code::MODULE_FAILED,
        };
        Self::new(code, error.to_string())
    }
}

impl From<ProjectError> for RpcError {
    fn from(error: ProjectError) -> Self {
        let code = match error {
            ProjectError::NotFound(_) => code::PROJECT_NOT_FOUND,
            ProjectError::Invalid(_) => code::PROJECT_INVALID,
            ProjectError::Exists(_) => code::PROJECT_EXISTS,
            ProjectError::Io(_) => code::MODULE_FAILED,
        };
        Self::new(code, error.to_string())
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

/// Where `project.open` and `project.close` remember the open folders: `config.toml`, and
/// the home its relative paths are resolved against.
#[derive(Debug, Clone, Default)]
pub struct ProjectHome {
    pub config: PathBuf,
    pub home: PathBuf,
}

/// `[client]` of `config.toml`, kept for `config.client`.
#[derive(Debug, Clone, Default)]
struct GlobalClient {
    table: toml::Table,
    warnings: Vec<String>,
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
    /// How many parents it has: 0 for an execution a client started.
    nesting: u32,
    /// For a child: where its result goes. Taken when it ends, so the caller hears that once.
    link: Option<CallLink>,
    /// The depth cut dropped a chain of this child before it returned: it is failed.
    cut: bool,
    /// For a child: the endpoints its caller bound to "uses" inputs of its workflow, what
    /// its `self.<port>` "uses" wires lead to (`ChildOptions::bindings`).
    bindings: HashMap<String, Vec<Endpoint>>,
}

impl Live {
    fn new(number: u64, execution: Execution, workflow: Arc<Workflow>) -> Self {
        Self {
            number,
            execution,
            workflow,
            started: Vec::new(),
            active: HashSet::new(),
            queue: VecDeque::new(),
            draining: false,
            nesting: 0,
            link: None,
            cut: false,
            bindings: HashMap::new(),
        }
    }
}

/// The node a child execution answers to (`ModuleCtx::start_child`).
struct CallLink {
    parent: String,
    node: String,
    /// The depth of the call in the parent's chain: news from the child is delivered at
    /// it, so a loop through calls is still cut by `bus.max_depth`. A send to a kept child
    /// deeper in the parent's chain moves it down.
    depth: u32,
    /// It already returned its one value.
    returned: bool,
    /// Kept (`ChildMode::Keep`): every output goes to the caller and the child goes on.
    keep: bool,
    tag: Option<String>,
}

/// Why a local module of a project that is not trusted does not run.
const UNTRUSTED: &str = "untrusted project: reopen with --trust";

/// The threads that ask hook nodes (`hook.dispatch`).
const HOOK_THREAD: &str = "regulus-hook";

/// The thread that starts the nodes of a child execution.
const CALL_THREAD: &str = "regulus-call";

/// The base: the module registry, the profiles and the executions. It has no functions of
/// its own; it only answers about the pieces and routes calls to the modules.
pub struct Core {
    /// This core, for the threads that drain the bus.
    me: Weak<Core>,
    registry: Registry,
    /// Written only by `template.apply`, `profile.save` and `profile.delete`; executions
    /// copy what they run.
    profiles: RwLock<ProfileStore>,
    /// Held by whoever writes or removes a profile file, so only one does at a time while
    /// `profiles` stays free for readers. Lock order: `recovering`, `executions`, then
    /// `profiles`.
    profile_writes: Mutex<()>,
    /// Where templates are read from and applied ones written (`set_template_places`).
    places: RwLock<Places>,
    /// The open projects. Lock order: `profile_writes`, then `projects`, then `profiles`;
    /// `executions` before `projects`.
    projects: RwLock<Projects>,
    /// Without it, `project.open` and `project.close` change nothing on disk.
    project_home: RwLock<Option<ProjectHome>>,
    /// `[client]` of `config.toml` as written, and what it has that the core does not know
    /// (`set_client`).
    client: RwLock<GlobalClient>,
    /// How the processes of local modules are run (`set_local_modules`).
    local_modules: RwLock<LocalModulesConfig>,
    storage: Storage,
    bus: BusConfig,
    call: CallConfig,
    hooks: HooksConfig,
    /// Unanswered `node/hook` requests per hook node (`[hooks] max_in_flight`).
    hooks_in_flight: Mutex<HashMap<NodeRef, usize>>,
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
        let call = CallConfig::default();
        Self::build_with_limits(modules, without, profiles, storage, bus, call)
    }

    /// [`Core::build_with_bus`] with the limits of child executions.
    pub fn build_with_limits(
        modules: Vec<Box<dyn Module>>,
        without: &[String],
        profiles: ProfileStore,
        storage: Storage,
        bus: BusConfig,
        call: CallConfig,
    ) -> Arc<Self> {
        let hooks = HooksConfig::default();
        Self::build_with_hooks(modules, without, profiles, storage, bus, call, hooks)
    }

    /// [`Core::build_with_limits`] with how long hooks may take.
    pub fn build_with_hooks(
        modules: Vec<Box<dyn Module>>,
        without: &[String],
        profiles: ProfileStore,
        storage: Storage,
        bus: BusConfig,
        call: CallConfig,
        hooks: HooksConfig,
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
            profiles: RwLock::new(profiles),
            profile_writes: Mutex::default(),
            places: RwLock::default(),
            projects: RwLock::default(),
            project_home: RwLock::default(),
            client: RwLock::default(),
            local_modules: RwLock::default(),
            storage,
            executions: Mutex::default(),
            journal: Mutex::new(journal),
            bus,
            call,
            hooks,
            hooks_in_flight: Mutex::default(),
            next_execution: AtomicU64::new(next_execution),
            recovering: Mutex::new(recovering),
        })
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Where `template.*` reads templates and writes applied ones. Without it, the list is
    /// empty and applying is refused.
    pub fn set_template_places(&self, places: Places) {
        *self.places.write().unwrap_or_else(|p| p.into_inner()) = places;
    }

    /// The open projects, and where opening or closing one is remembered.
    pub fn set_projects(&self, projects: Projects, home: ProjectHome) {
        *self.projects.write().unwrap_or_else(|p| p.into_inner()) = projects;
        *self.project_home.write().unwrap_or_else(|p| p.into_inner()) = Some(home);
    }

    /// `[local_modules]` of `config.toml`: how the processes of local modules are run.
    pub fn set_local_modules(&self, config: LocalModulesConfig) {
        *self
            .local_modules
            .write()
            .unwrap_or_else(|p| p.into_inner()) = config;
    }

    /// Starts the local modules (`[[modules.local]]`) of every open project. A module that
    /// cannot start is listed broken with why; nothing else is affected.
    pub fn start_local_modules(&self) {
        let open: Vec<Arc<Project>> = self
            .projects()
            .iter()
            .filter_map(|p| self.projects().get(&p.id))
            .collect();
        for project in open {
            self.start_locals(&project);
        }
    }

    /// Starts the local modules of one project, unless `--without` leaves them out.
    fn start_locals(&self, project: &Project) {
        let config = self
            .local_modules
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let specs: Vec<LocalSpec> = project
            .file
            .modules
            .local
            .iter()
            .filter(|local| local.unusable().is_none())
            .filter_map(|local| {
                Some(LocalSpec {
                    project_id: project.id.clone(),
                    root: project.root.clone(),
                    id: local.id.clone()?,
                    command: local.command.clone(),
                    cwd: local.cwd.clone(),
                    env: local.env.clone(),
                    version: local.version.clone(),
                    manifest: local.manifest.clone(),
                })
            })
            .filter(|spec| !self.registry.excludes(&spec.base_id()))
            .collect();
        let trusted = self.is_trusted(&project.root);
        // Decided before anything runs: what may not start never gets a process.
        let mut seen = HashSet::new();
        let presets: Vec<Option<ModuleState>> = specs
            .iter()
            .map(|spec| {
                let id = spec.base_id();
                if !seen.insert(id.clone()) || self.registry.is_installed(&id) {
                    Some(ModuleState::Broken {
                        reason: format!("duplicate id: a module `{id}` is already loaded"),
                    })
                } else if !trusted {
                    Some(ModuleState::Blocked {
                        reason: UNTRUSTED.into(),
                    })
                } else {
                    None
                }
            })
            .collect();
        // In parallel: starting them all takes as long as the slowest, not their sum.
        let started: Vec<Local> = std::thread::scope(|scope| {
            let spawning: Vec<_> = specs
                .iter()
                .zip(presets)
                .map(|(spec, preset)| {
                    let spawn = preset
                        .is_none()
                        .then(|| scope.spawn(|| ProcessModule::spawn(spec, &config)));
                    (spec, preset, spawn)
                })
                .collect();
            spawning
                .into_iter()
                .map(|(spec, preset, spawn)| {
                    let spawned = match (preset, spawn) {
                        (_, Some(handle)) => handle
                            .join()
                            .unwrap_or_else(|_| Err("starting it panicked".into()))
                            .map_err(|reason| ModuleState::Broken { reason }),
                        (Some(preset), None) => Err(preset),
                        (None, None) => Err(ModuleState::Broken {
                            reason: "not started".into(),
                        }),
                    };
                    match spawned {
                        Ok(module) => Local {
                            manifest: module.manifest().clone(),
                            health: Some(module.health()),
                            module: Box::new(module),
                            preset: None,
                        },
                        Err(state) => Local {
                            manifest: spec.placeholder(),
                            module: Box::new(Unstarted(spec.placeholder())),
                            preset: Some(state),
                            health: None,
                        },
                    }
                })
                .collect()
        });
        self.registry.add_local(&project.id, started);
    }

    /// Whether `[projects] trusted` of `config.toml` names this folder. Without a home,
    /// nothing is trusted.
    fn is_trusted(&self, root: &Path) -> bool {
        self.project_home
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .is_some_and(|home| projects::is_trusted(&home.config, &home.home, root))
    }

    /// Trusts a folder or stops trusting it, in `config.toml`. If that changed and it is an
    /// open project, its local modules start again under the new decision; returns whether
    /// they did.
    fn set_trust(&self, root: &Path, trusted: bool) -> Result<bool, RpcError> {
        let home = self
            .project_home
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| {
                RpcError::from(ProjectError::Io(
                    "this core has no config.toml to remember trust in".into(),
                ))
            })?;
        let before = projects::is_trusted(&home.config, &home.home, root);
        projects::trust(&home.config, &home.home, root, trusted)?;
        let open = self.projects().by_root(root);
        match open {
            Some(project) if before != trusted => {
                self.registry.remove_local(&project.id);
                self.start_locals(&project);
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// `project.trust`: the folder need not be open.
    fn trust_project(&self, params: ProjectTrustParams) -> Result<ProjectTrusted, RpcError> {
        let _writing = self.writing();
        let base = self
            .project_home
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map_or(PathBuf::new(), |h| h.home.clone());
        let root = projects::folder(&base, Path::new(&params.path))?;
        self.set_trust(&root, !params.remove)?;
        Ok(ProjectTrusted {
            path: root.display().to_string(),
            trusted: !params.remove,
        })
    }

    /// `[client]` of `config.toml` as written, and the warnings about it, for
    /// `config.client`. Without it, a client gets only what a project sets.
    pub fn set_client(&self, table: toml::Table, warnings: Vec<String>) {
        *self.client.write().unwrap_or_else(|p| p.into_inner()) = GlobalClient { table, warnings };
    }

    /// `config.client`: the global `[client]` with the project's on top.
    fn client_settings(&self, project_id: Option<&str>) -> Result<ClientSettings, RpcError> {
        let project = project_id.map(|id| self.project(id)).transpose()?;
        let global = self.client.read().unwrap_or_else(|p| p.into_inner());
        let (table, sources) =
            crate::config::layer_client(&global.table, project.as_ref().map(|p| &p.file.client));
        let client = match serde_json::to_value(table) {
            Ok(Value::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        Ok(ClientSettings {
            client,
            sources,
            warnings: global.warnings.clone(),
        })
    }

    /// Reads again the `regulus.toml` files edited since they were read: a look at each
    /// file's time, and the write lock only if one changed.
    fn refresh_projects(&self) {
        if self.projects().iter().any(Project::is_stale) {
            self.projects
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .refresh();
        }
    }

    fn projects(&self) -> RwLockReadGuard<'_, Projects> {
        self.projects.read().unwrap_or_else(|p| p.into_inner())
    }

    fn project(&self, id: &str) -> Result<Arc<Project>, RpcError> {
        self.projects()
            .get(id)
            .ok_or_else(|| ProjectError::NotFound(format!("no open project `{id}`")).into())
    }

    fn project_warnings(&self, project: &Project) -> Vec<String> {
        project.warnings(|id| self.registry.is_installed(id))
    }

    fn project_summary(&self, project: &Project) -> ProjectSummary {
        project.summary(self.project_warnings(project))
    }

    /// `project.open`: makes a folder a project (a minimal `regulus.toml` if it has none),
    /// remembers it in `config.toml` and serves it. Opening an open folder again changes
    /// nothing.
    fn open_project(&self, params: ProjectOpenParams) -> Result<ProjectSummary, RpcError> {
        let _writing = self.writing();
        let home = self
            .project_home
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let base = home.as_ref().map_or(PathBuf::new(), |h| h.home.clone());
        let root = projects::folder(&base, Path::new(&params.path))?;
        if let Some(open) = self.projects().by_root(&root) {
            if params.trust {
                self.set_trust(&root, true)?;
            }
            return Ok(self.project_summary(&open));
        }
        // config.toml must take the folder before the folder gets a file.
        if let Some(home) = &home {
            projects::read_config(&home.config)?;
        }
        projects::ensure_file(&root)?;
        let project = Project::load(&root)?;
        if let Some(other) = self.projects().get(&project.id) {
            return Err(ProjectError::Exists(format!(
                "project `{}` is already open at {}; give this one another `[project] id`",
                other.id,
                other.root.display()
            ))
            .into());
        }
        if let Some(home) = &home {
            projects::remember(&home.config, &home.home, &root)?;
        }
        self.projects
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove_failed(None, Some(&root));
        let (id, dirs) = (project.id.clone(), project.profiles.clone());
        self.projects
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .insert(project)?;
        let found = self
            .profiles
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .load_project(&id, &dirs);
        let mut projects = self.projects.write().unwrap_or_else(|p| p.into_inner());
        projects.note(&id, found);
        let project = projects.get(&id).ok_or_else(|| {
            RpcError::from(ProjectError::NotFound(format!("no open project `{id}`")))
        })?;
        drop(projects);
        // Trusting it for the first time already starts its local modules.
        if !(params.trust && self.set_trust(&project.root, true)?) {
            self.start_locals(&project);
        }
        Ok(self.project_summary(&project))
    }

    /// `project.close`: stops serving a project and forgets it in `config.toml`. Nothing
    /// in its folder is touched.
    fn close_project(&self, params: IdParams) -> Result<ProjectSummary, RpcError> {
        let _writing = self.writing();
        let home = self
            .project_home
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        // One that did not open on start is only forgotten.
        let failed = self
            .projects()
            .failed()
            .iter()
            .find(|f| f.id == params.id)
            .cloned();
        if let Some(failed) = failed {
            if let Some(home) = &home {
                projects::forget(&home.config, &home.home, &failed.root)?;
            }
            self.projects
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .remove_failed(Some(&failed.id), None);
            return Ok(failed.summary());
        }
        let project = self.project(&params.id)?;
        if let Some(home) = &home {
            projects::forget(&home.config, &home.home, &project.root)?;
        }
        self.projects
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&project.id);
        self.profiles
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .remove_project(&project.id, &project.profiles);
        self.registry.remove_local(&project.id);
        Ok(self.project_summary(&project))
    }

    fn profile_store(&self) -> RwLockReadGuard<'_, ProfileStore> {
        self.profiles.read().unwrap_or_else(|p| p.into_inner())
    }

    fn template_places(&self) -> Places {
        self.places
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    /// Where templates are looked up, with the project each place belongs to: `first`'s
    /// templates (an open project) and the global ones; without it, the global ones, then
    /// every open project's.
    fn template_sources(&self, first: Option<&str>) -> Vec<(Places, Option<String>)> {
        let global = self.template_places();
        let project_places = |project: &Project| Places {
            templates: project.templates.clone(),
            ..global.clone()
        };
        let projects = self.projects();
        let mut sources = Vec::new();
        if let Some(project) = first.and_then(|id| projects.get(id)) {
            sources.push((project_places(&project), Some(project.id.clone())));
        }
        sources.push((global.clone(), None));
        if first.is_none() {
            for project in projects.iter() {
                sources.push((project_places(project), Some(project.id.clone())));
            }
        }
        sources
    }

    /// A template by name, in the order of [`Core::template_sources`], or by path; with the
    /// project it belongs to.
    fn find_template(
        &self,
        wanted: &str,
        project: Option<&str>,
    ) -> Result<(templates::Template, Option<String>), RpcError> {
        let mut searched = Vec::new();
        for (places, owner) in self.template_sources(project) {
            match templates::find(&places, wanted) {
                Ok(template) => return Ok((template, owner.or(project.map(Into::into)))),
                Err(TemplateError::NotFound(_)) => {
                    searched.extend(places.templates.iter().map(|p| p.display().to_string()))
                }
                Err(other) => return Err(other.into()),
            }
        }
        Err(TemplateError::NotFound(format!(
            "no template named `{wanted}` in {}",
            searched.join(", ")
        ))
        .into())
    }

    /// `template.list`: the global templates, then each open project's, tagged with it.
    fn list_templates(&self) -> Vec<regulus_proto::TemplateSummary> {
        self.template_sources(None)
            .into_iter()
            .flat_map(|(places, owner)| {
                templates::list(&places).into_iter().map(move |mut t| {
                    t.project_id = owner.clone();
                    t
                })
            })
            .collect()
    }

    /// Where a profile of `project_id` is written: the first profiles path of its project
    /// if it is open, else the first `[profiles] paths` entry.
    fn profiles_dir(&self, project_id: Option<&str>) -> Result<PathBuf, RpcError> {
        if let Some(project) = project_id.and_then(|id| self.projects().get(id)) {
            return project.profiles.first().cloned().ok_or_else(|| {
                TemplateError::Io(format!(
                    "project `{}` has no profiles path to write to",
                    project.id
                ))
                .into()
            });
        }
        self.template_places().profiles_dir.ok_or_else(|| {
            TemplateError::Io("no [profiles] paths entry to write the profile to".into()).into()
        })
    }

    /// `template.apply`: renders, checks the modules it requires and the profile it makes,
    /// then writes the copy and loads it. Nothing is written unless every check passes, and
    /// an existing profile is only replaced with `force`.
    fn apply_template(&self, params: TemplateApplyParams) -> Result<TemplateApplied, RpcError> {
        if let Some(id) = &params.project_id {
            self.project(id)?;
        }
        let (template, project) =
            self.find_template(&params.template, params.project_id.as_deref())?;
        let mut values = std::collections::BTreeMap::new();
        for (name, value) in params.vars {
            let text = match value {
                Value::String(text) => text,
                Value::Number(_) | Value::Bool(_) => value.to_string(),
                other => {
                    return Err(RpcError::invalid_params(format!(
                        "var `{name}` must be text, a number or a boolean, not {other}"
                    )))
                }
            };
            values.insert(name, text);
        }
        let installed = self.registry.ids();
        templates::check_requires(&template.requires, installed.iter().map(String::as_str))?;
        let mut text = templates::render(&template, &values)?;
        if let Some(project) = &project {
            text = templates::set_project(&text, project);
        }
        let mut profile = templates::profile_of(&template, &text)?;
        if let Some(id) = params.id.filter(|id| *id != profile.id) {
            templates::file_name(&id)?;
            text = templates::rename(&text, &id)?;
            profile = templates::profile_of(&template, &text)?;
        }
        let id = profile.id.clone();
        templates::file_name(&id)
            .map_err(|e| TemplateError::Invalid(format!("template `{}`: {e}", template.name)))?;
        crate::profiles::normalize(&mut profile);
        let (errors, diagnostics) = self.diagnose(&profile);
        if !errors.is_empty() {
            return Err(TemplateError::Invalid(format!(
                "template `{}` renders a profile with errors: {}",
                template.name,
                errors.join("; ")
            ))
            .into());
        }
        let dir = self.profiles_dir(project.as_deref())?;
        let text = templates::provenance(&template) + &text;
        let written = self.write_profile(&dir, profile, &text, params.force)?;
        Ok(TemplateApplied {
            profile_id: id,
            path: written.display().to_string(),
            diagnostics,
        })
    }

    /// Everything `profile.check` reports: the wiring rules, the fields the core
    /// understands and the nodes the profile's project keeps from running.
    fn check(&self, profile: &Profile) -> Vec<regulus_proto::Diagnostic> {
        let mut found = check_profile(profile, |id| self.registry.is_installed(id));
        found.extend(check_fields(profile, |id| self.registry.manifest(id)));
        if let Some(project) = self.project_of(profile.project_id.as_deref()) {
            found.extend(check_vetoed(profile, &project.id, |m| project.vetoes(m)));
        }
        found
    }

    /// The open project with this id, if any.
    fn project_of(&self, project_id: Option<&str>) -> Option<Arc<Project>> {
        project_id.and_then(|id| self.projects().get(id))
    }

    /// Why a workflow about to run cannot: the nodes of it whose module its project keeps
    /// from running. `None` if it may run.
    fn vetoed(&self, project_id: Option<&str>, workflow: &Workflow) -> Option<String> {
        let project = self.project_of(project_id)?;
        let nodes: Vec<String> = workflow
            .nodes
            .iter()
            .filter_map(|n| {
                let module = n.module_id.as_deref().filter(|m| project.vetoes(m))?;
                Some(format!("`{}` ({module})", n.id))
            })
            .collect();
        (!nodes.is_empty()).then(|| {
            format!(
                "disabled in project {}: node {} (its [modules] disable/enable)",
                project.id,
                nodes.join(", ")
            )
        })
    }

    /// What `profile.check` finds: the errors, as `message (code)`, and the rest.
    fn diagnose(&self, profile: &Profile) -> (Vec<String>, Vec<regulus_proto::Diagnostic>) {
        let found = self.check(profile);
        let (errors, rest): (Vec<_>, Vec<_>) = found
            .into_iter()
            .partition(|d| d.severity == Severity::Error);
        let errors = errors
            .iter()
            .map(|d| format!("{} ({})", d.message, d.code))
            .collect();
        (errors, rest)
    }

    fn writing(&self) -> MutexGuard<'_, ()> {
        self.profile_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Writes `text`, the file of `profile`, as `<dir>/<id>.toml` and loads it in place of
    /// the profile with that id. A profile that lives in another file, or in none, is never
    /// replaced; one that lives here only with `force`. One writer at a time: the store is
    /// locked to look and to swap, never while the disk is written.
    fn write_profile(
        &self,
        dir: &Path,
        profile: Profile,
        text: &str,
        force: bool,
    ) -> Result<PathBuf, RpcError> {
        let id = profile.id.clone();
        let dest = dir.join(templates::file_name(&id)?);
        let _writing = self.writing();
        {
            let store = self.profile_store();
            let loaded = store.get(&id).is_some();
            match store.origin(&id) {
                Some(origin) if origin != dest => {
                    return Err(TemplateError::Exists(format!(
                        "profile `{id}` is already defined in {}, not in {}; edit that file or \
                         choose another id",
                        origin.display(),
                        dir.display()
                    ))
                    .into())
                }
                None if loaded => {
                    return Err(TemplateError::Exists(format!(
                        "profile `{id}` is already loaded, without a file; choose another id"
                    ))
                    .into())
                }
                _ => {}
            }
            if !force && (loaded || dest.exists()) {
                return Err(TemplateError::Exists(format!(
                    "profile `{id}` already exists at {}; use force to replace it",
                    dest.display()
                ))
                .into());
            }
        }
        let written = templates::write(dir, &id, text, force)?;
        self.profiles
            .write()
            .unwrap_or_else(|p| p.into_inner())
            .replace(profile, written.clone());
        Ok(written)
    }

    /// `profile.save`: checks the whole profile a client sends, writes it as TOML to the
    /// first `[profiles] paths` entry and loads it. Running executions keep the workflow
    /// they started with.
    fn save_profile(&self, params: ProfileSaveParams) -> Result<ProfileSaved, RpcError> {
        let mut profile = params.profile;
        let id = profile.id.clone();
        templates::file_name(&id)?;
        crate::profiles::normalize(&mut profile);
        let (errors, diagnostics) = self.diagnose(&profile);
        if !errors.is_empty() {
            return Err(RpcError::new(
                code::PROFILE_INVALID,
                format!("profile `{id}` has errors: {}", errors.join("; ")),
            ));
        }
        let text = crate::profiles::to_toml(&profile).map_err(|e| {
            RpcError::invalid_params(format!("profile `{id}` cannot be written as TOML: {e}"))
        })?;
        let dir = self.profiles_dir(profile.project_id.as_deref())?;
        let written = self.write_profile(&dir, profile, &text, params.force)?;
        Ok(ProfileSaved {
            profile_id: id,
            path: written.display().to_string(),
            diagnostics,
        })
    }

    /// `profile.delete`: unloads a profile and removes its file, if the file sits in a
    /// `[profiles] paths` directory and holds that profile alone, and nothing of it is
    /// running or waiting to resume.
    fn delete_profile(&self, params: IdParams) -> Result<ProfileDeleted, RpcError> {
        let id = params.id;
        let _writing = self.writing();
        let origin = {
            let store = self.profile_store();
            if store.get(&id).is_none() {
                return Err(RpcError::invalid_params("Unknown profile"));
            }
            store.origin(&id).map(Path::to_path_buf)
        };
        let origin = origin.ok_or_else(|| {
            TemplateError::Exists(format!(
                "profile `{id}` is loaded without a file; there is nothing to delete"
            ))
        })?;
        let project_paths: Vec<PathBuf> = self
            .projects()
            .iter()
            .flat_map(|p| p.profiles.clone())
            .collect();
        let inside = self
            .template_places()
            .profile_paths
            .iter()
            .chain(&project_paths)
            .any(|dir| origin.parent() == Some(dir.as_path()) && dir.is_dir());
        if !inside {
            return Err(TemplateError::Exists(format!(
                "profile `{id}` lives in {}, which is not a file inside a [profiles] paths \
                 directory; it is left as it is",
                origin.display()
            ))
            .into());
        }
        templates::check_replace(&origin, &id)?;
        // Checked and unloaded under the locks `start`, `recover` and `call_workflow` hold
        // while they read the profile and register what runs it: an execution either shows
        // up here or finds the profile gone.
        let removed = {
            let recovering = self.recovering();
            let executions = self.executions();
            let busy: Vec<&str> = recovering
                .iter()
                .map(|stored| &stored.execution)
                .chain(
                    executions
                        .iter()
                        .map(|live| &live.execution)
                        .filter(|e| e.state.is_live()),
                )
                .filter(|e| e.profile_id == id)
                .map(|e| e.id.as_str())
                .collect();
            if !busy.is_empty() {
                return Err(RpcError::new(
                    code::PROFILE_IN_USE,
                    format!(
                        "profile `{id}` is in use by {}; stop it first",
                        busy.join(", ")
                    ),
                ));
            }
            self.profiles
                .write()
                .unwrap_or_else(|p| p.into_inner())
                .remove(&id)
        };
        match std::fs::remove_file(&origin) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                if let Some((profile, _)) = removed {
                    self.profiles
                        .write()
                        .unwrap_or_else(|p| p.into_inner())
                        .replace(Profile::clone(&profile), origin.clone());
                }
                return Err(
                    TemplateError::Io(format!("cannot remove {}: {e}", origin.display())).into(),
                );
            }
        }
        Ok(ProfileDeleted {
            profile_id: id,
            path: origin.display().to_string(),
        })
    }

    /// Answers every method except `hello`, which belongs to the connection.
    pub fn dispatch(&self, name: &str, params: Value) -> Result<Value, RpcError> {
        self.refresh_projects();
        match name {
            method::PROFILE_LIST => {
                let filter: ProjectFilter = parse(params)?;
                json(
                    self.profile_store()
                        .list(filter.project_id.as_deref())
                        .map(|p| p.summary())
                        .collect::<Vec<_>>(),
                )
            }
            method::PROFILE_GET => json(&*self.profile(parse(params)?)?),
            method::PROFILE_CHECK => {
                let profile = self.profile(parse(params)?)?;
                json(self.check(&profile))
            }
            method::PROJECT_LIST => {
                let projects = self.projects();
                let open = projects.iter().map(|p| self.project_summary(p));
                let failed = projects.failed().iter().map(|f| f.summary());
                json(open.chain(failed).collect::<Vec<_>>())
            }
            method::PROJECT_GET => {
                let params: IdParams = parse(params)?;
                let project = self.project(&params.id)?;
                let details: ProjectDetails = project.details(self.project_warnings(&project));
                json(details)
            }
            method::PROJECT_OPEN => json(self.open_project(parse(params)?)?),
            method::PROJECT_TRUST => json(self.trust_project(parse(params)?)?),
            method::PROJECT_CLOSE => json(self.close_project(parse(params)?)?),
            method::CONFIG_CLIENT => {
                let filter: ProjectFilter = parse(params)?;
                json(self.client_settings(filter.project_id.as_deref())?)
            }
            method::TEMPLATE_LIST => json(self.list_templates()),
            method::TEMPLATE_GET => {
                let params: TemplateParams = parse(params)?;
                let (template, _) = self.find_template(&params.template, None)?;
                json(TemplateText {
                    name: template.name,
                    path: template.path.display().to_string(),
                    text: template.text,
                })
            }
            method::TEMPLATE_APPLY => json(self.apply_template(parse(params)?)?),
            method::MODULE_LIST => json(self.registry.summaries()),
            method::MODULE_MANIFEST => {
                let params: ModuleManifestParams = parse(params)?;
                match params.id {
                    Some(id) => json(&*self.registry.manifest(&id).ok_or_else(|| {
                        RpcError::invalid_params(format!("Unknown module `{id}`"))
                    })?),
                    None => json(
                        self.registry
                            .manifests()
                            .iter()
                            .map(|m| &**m)
                            .collect::<Vec<_>>(),
                    ),
                }
            }
            method::PROFILE_SAVE => json(self.save_profile(parse(params)?)?),
            method::PROFILE_DELETE => json(self.delete_profile(parse(params)?)?),
            method::EXECUTION_LIST => {
                let filter: ProjectFilter = parse(params)?;
                json(self.list_executions(filter.project_id.as_deref())?)
            }
            method::EXECUTION_START => json(self.start(parse(params)?)?),
            method::EXECUTION_TREE => json(self.tree(parse(params)?)?),
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
            method::HOOK_DISPATCH => json(self.hook(parse(params)?)?),
            method::HELLO => Err(RpcError::new(
                METHOD_NOT_FOUND,
                "hello is answered by the connection",
            )),
            other => Ok(self.registry.call(other, params)?),
        }
    }

    fn profile(&self, params: IdParams) -> Result<Arc<Profile>, RpcError> {
        self.profile_store()
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

    /// Moves an execution to `state` only if `from` holds for the state it is in, and
    /// returns it as it ends up.
    fn settle(
        &self,
        execution_id: &str,
        from: fn(&ExecutionState) -> bool,
        state: ExecutionState,
    ) -> Option<Execution> {
        let mut executions = self.executions();
        let live = executions
            .iter_mut()
            .find(|live| live.execution.id == execution_id)?;
        if from(&live.execution.state) {
            live.execution.state = state;
            let execution = live.execution.clone();
            self.persist(|store| store.update_execution(&execution));
        }
        Some(live.execution.clone())
    }

    /// Starts a copy of a workflow: `node/start` on every node whose module is loaded,
    /// what is used before what uses it (docs/03-modules.md §2). A node that fails to
    /// start fails the execution and stops the nodes already started.
    ///
    /// It refuses only what would make the running copy ambiguous or endless, which
    /// `flatten` rejects: an id twice, a reserved id, a workflow inside itself. Anything
    /// else `profile.check` reports still runs, as it always did: a wire that does not fit
    /// fails visibly on the bus and the rest keeps working.
    fn start(&self, params: ExecutionStartParams) -> Result<Execution, RpcError> {
        // The profile is read and the execution registered under the `executions` lock
        // (taken before `profiles`, as `profile.delete` does), so a delete either sees the
        // execution or has already unloaded the profile. Nothing here touches the disk or
        // the bus.
        let (number, execution, workflow) = {
            let mut executions = self.executions();
            let profile = self
                .profile_store()
                .get(&params.profile_id)
                .ok_or_else(|| RpcError::invalid_params("Unknown profile"))?;
            let profile = profile.as_ref();
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
            // A node that runs a workflow the profile does not have could never work.
            if let Some(error) =
                missing_workflow(profile, &workflow, |id| self.registry.manifest(id))
            {
                return Err(RpcError::invalid_params(error));
            }
            if let Some(error) = self.vetoed(profile.project_id.as_deref(), &workflow) {
                return Err(RpcError::new(code::MODULE_DISABLED, error));
            }
            let number = self.next_execution.fetch_add(1, Ordering::Relaxed);
            let execution = Execution {
                id: format!("exec-{number}"),
                profile_id: profile.id.clone(),
                workflow_id,
                project_id: profile.project_id.clone(),
                state: ExecutionState::Starting,
                disabled: Vec::new(),
                parent: None,
                caller: None,
                tag: None,
            };
            executions.push(Live::new(number, execution.clone(), workflow.clone()));
            (number, execution, workflow)
        };
        self.persist(|store| store.insert_execution(number, &execution, &workflow));
        self.launch(&execution.id, &workflow, &HashMap::new())
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
                caller,
            } = recovering.remove(0);
            let checkpoints = store
                .checkpoints(execution.project_id.as_deref(), &execution.id)
                .unwrap_or_else(|e| {
                    report.push(format!("{}: {e}", execution.id));
                    HashMap::new()
                });
            // A child is never resumed: the node it answered to lived in memory only.
            let resume = execution.parent.is_none()
                && match store.resume() {
                    Resume::Always => true,
                    Resume::Never => false,
                    Resume::Checkpoint => !checkpoints.is_empty(),
                };
            if !resume {
                let reason = if execution.parent.is_some() {
                    "the core stopped while it ran as a child; nothing waits for it any more"
                } else if store.resume() == Resume::Never {
                    "the core stopped and [store] resume = never"
                } else {
                    "the core stopped and it saved no checkpoint"
                };
                execution.state = ExecutionState::Interrupted {
                    reason: reason.into(),
                };
                self.persist(|store| store.update_execution(&execution));
                report.push(format!("{} interrupted: {reason}", execution.id));
                // A parent resumed before it (lower number) hears its wait is over.
                let news = match (&execution.parent, caller) {
                    (Some(parent), Some(node)) => Some((parent.clone(), node)),
                    _ => None,
                };
                let ended = ChildNews {
                    execution_id: execution.id.clone(),
                    workflow_id: execution.workflow_id.clone(),
                    tag: execution.tag.clone(),
                    kind: News::Ended(execution.state.clone()),
                };
                self.push_live(number, execution, Arc::new(workflow));
                if let Some((parent, node)) = news {
                    self.tell_parent(&parent, end_news(ended, node, 0));
                }
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
        self.executions()
            .push(Live::new(number, execution, workflow));
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
            ..
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
        // What a caller bound to the "uses" inputs of this child: its `self` wires lead
        // there.
        let bound: HashSet<String> = self
            .executions()
            .iter()
            .find(|live| live.execution.id == id)
            .map(|live| {
                live.bindings
                    .iter()
                    .filter(|(_, endpoints)| !endpoints.is_empty())
                    .map(|(port, _)| port.clone())
                    .collect()
            })
            .unwrap_or_default();

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
                            && (active.contains(w.from.node_id.as_str())
                                || (w.from.node_id == WORKFLOW_SELF
                                    && bound.contains(&w.from.port_id)))
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
                // Gone or stopped meanwhile (a parent stopping takes its children along).
                let Some(live) = executions
                    .iter_mut()
                    .find(|live| live.execution.id == id)
                    .filter(|live| live.execution.state.is_live())
                else {
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
            let started = (|| -> Result<(), String> {
                let args = match checkpoints.get(&node.id) {
                    None => Value::Null,
                    Some(state) => serde_json::to_value(NodeStart {
                        checkpoint: Some(state.clone()),
                    })
                    .map_err(|e| e.to_string())?,
                };
                let call = self.node_call(&id, node, args).map_err(|e| e.to_string())?;
                let params = serde_json::to_value(call).map_err(|e| e.to_string())?;
                match self
                    .registry
                    .call_module(module_id, method::NODE_START, params)
                {
                    Ok(_) | Err(CallError::Module(ModuleError::UnknownMethod(_))) => Ok(()),
                    Err(error) => Err(error.to_string()),
                }
            })();
            if let Err(error) = started {
                // Failed before stopping, so a caller waiting on it hears `failed`; unless
                // it was stopped meanwhile, which stays.
                let reason = format!("node `{}` failed to start: {error}", node.id);
                let failed = self.settle(&id, starting, ExecutionState::Failed { reason });
                self.stop(&id, true);
                return failed.ok_or_else(|| RpcError::invalid_params("Unknown execution"));
            }
        }
        // Stopped while starting: it stays so.
        let running = self
            .settle(&id, starting, ExecutionState::Running)
            .ok_or_else(|| RpcError::invalid_params("Unknown execution"))?;
        // What the nodes emitted while starting is delivered now that all of them run.
        self.kick(&id);
        Ok(running)
    }

    /// Stops an execution: `node/stop` on its started nodes, in reverse order. Stopping
    /// a stopped or failed execution changes nothing. Without `record`, the store keeps it
    /// as it was, so the next core finds it running and recovers it.
    ///
    /// Its live children stop first, with the same `record`. A child that stops tells the
    /// node that called it (`node/child`, `ended`).
    fn stop(&self, execution_id: &str, record: bool) -> Option<Execution> {
        let (started, workflow, children) = {
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
            let started = std::mem::take(&mut live.started);
            let workflow = live.workflow.clone();
            let children: Vec<String> = executions
                .iter()
                .filter(|child| {
                    child.execution.parent.as_deref() == Some(execution_id)
                        && child.execution.state.is_live()
                })
                .map(|child| child.execution.id.clone())
                .collect();
            (started, workflow, children)
        };
        for child in children {
            self.stop(&child, record);
        }
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
        self.ended(execution_id);
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

    /// What is wired over "uses" into `port` of a node (`ModuleCtx::resolve`). A wire from
    /// the workflow's own input (`self.<port>`) leads to what the caller of a child bound
    /// to it, as long as that is still live.
    pub(crate) fn resolve(&self, node: &NodeRef, port: &str) -> Vec<Endpoint> {
        let executions = self.executions();
        let Some(live) = executions
            .iter()
            .find(|live| live.execution.id == node.execution_id)
        else {
            return Vec::new();
        };
        uses_endpoints(live, &executions, |w| {
            w.to.node_id == node.node_id && w.to.port_id == port
        })
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

    // --- hooks ----------------------------------------------------------------------

    /// Relays a hook request to the `hook@1` nodes wired over "uses" into a node of a
    /// running execution (`hook.dispatch`). The core knows the capability, not what a hook
    /// decides: it asks each one with `node/hook` on a thread of its own and waits at most
    /// the time limit. One that does not answer in time, fails or does not take hooks
    /// counts as no answer, with the reason; its thread is left to finish on its own.
    fn hook(&self, params: HookDispatchParams) -> Result<HookDispatched, RpcError> {
        let targets: Vec<Endpoint> = {
            let executions = self.executions();
            let live = executions
                .iter()
                .find(|live| live.execution.id == params.execution_id)
                .filter(|live| live.execution.state.is_live())
                .ok_or_else(|| RpcError::invalid_params("Unknown or stopped execution"))?;
            if !live.active.contains(&params.node_id) {
                return Err(RpcError::invalid_params(format!(
                    "node `{}` is not running in `{}`",
                    params.node_id, params.execution_id
                )));
            }
            let workflow = live.workflow.clone();
            let is_hook = |wire: &regulus_proto::Wire| {
                [End::From, End::To].into_iter().any(|end| {
                    wire_end(&workflow, wire, end).is_some_and(|p| p.capability == HOOK_CAPABILITY)
                })
            };
            uses_endpoints(live, &executions, |w| {
                w.to.node_id == params.node_id && is_hook(w)
            })
            .into_iter()
            .filter(|target| {
                params
                    .hook_id
                    .as_ref()
                    .is_none_or(|id| *id == target.node.node_id)
            })
            .collect()
        };
        if let Some(id) = params.hook_id.as_ref().filter(|_| targets.is_empty()) {
            return Err(RpcError::invalid_params(format!(
                "no running hook `{id}` is wired into `{}`",
                params.node_id
            )));
        }
        let limit = params
            .timeout_ms
            .unwrap_or(self.hooks.timeout_ms)
            .clamp(1, self.hooks.max_timeout_ms.max(1));
        let limit = std::time::Duration::from_millis(limit);
        let request = json(&params.request)?;
        let (sender, answers) = std::sync::mpsc::channel();
        let mut got: Vec<Option<Result<Value, ModuleError>>> =
            targets.iter().map(|_| None).collect();
        for (index, target) in targets.iter().enumerate() {
            if let Err(busy) = self.hook_enter(&target.node) {
                got[index] = Some(Err(busy));
                continue;
            }
            let (core, tx, target) = (self.me.clone(), sender.clone(), target.clone());
            let request = request.clone();
            let node = target.node.clone();
            let spawned = std::thread::Builder::new()
                .name(HOOK_THREAD.into())
                .spawn(move || {
                    let answer = match core.upgrade() {
                        Some(core) => {
                            let answer = core.call_node(&target, method::NODE_HOOK, request);
                            core.hook_leave(&target.node);
                            answer
                        }
                        None => Err(ModuleError::Failed("the core is not running".into())),
                    };
                    let _ = tx.send((index, answer));
                });
            if let Err(error) = spawned {
                self.hook_leave(&node);
                got[index] = Some(Err(ModuleError::Failed(error.to_string())));
            }
        }
        drop(sender);
        let deadline = std::time::Instant::now() + limit;
        while got.iter().any(Option::is_none) {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            match answers.recv_timeout(left) {
                Ok((index, answer)) => got[index] = Some(answer),
                Err(_) => break,
            }
        }
        let replies = targets
            .iter()
            .zip(got)
            .map(|(target, answer)| {
                let (answer, error) = match answer {
                    None => (
                        HookAnswer::Nothing,
                        Some(format!("no answer within {} ms", limit.as_millis())),
                    ),
                    Some(Err(error)) => (HookAnswer::Nothing, Some(error.to_string())),
                    Some(Ok(value)) => match HookAnswer::from_value(value) {
                        Ok(answer) => match answer
                            .effect()
                            .filter(|e| !params.returns.is_empty() && !params.returns.contains(e))
                        {
                            None => (answer, None),
                            Some(undeclared) => {
                                let declared: Vec<&str> =
                                    params.returns.iter().map(|e| e.name()).collect();
                                let error = format!(
                                    "answered `{}`, which it does not declare ({}): dropped",
                                    undeclared.name(),
                                    declared.join(", ")
                                );
                                (HookAnswer::Nothing, Some(error))
                            }
                        },
                        Err(error) => (HookAnswer::Nothing, Some(format!("bad answer: {error}"))),
                    },
                };
                HookReply {
                    node_id: target.node.node_id.clone(),
                    answer,
                    error,
                }
            })
            .collect();
        Ok(HookDispatched { replies })
    }

    /// Counts one more request in flight to a hook node, unless it is at
    /// `[hooks] max_in_flight`.
    fn hook_enter(&self, node: &NodeRef) -> Result<(), ModuleError> {
        let mut in_flight = self
            .hooks_in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let count = in_flight.entry(node.clone()).or_default();
        let max = self.hooks.max_in_flight;
        if max > 0 && *count >= max {
            return Err(ModuleError::Failed(format!(
                "skipped: {max} requests to it are still unanswered"
            )));
        }
        *count += 1;
        Ok(())
    }

    fn hook_leave(&self, node: &NodeRef) {
        let mut in_flight = self
            .hooks_in_flight
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(count) = in_flight.get_mut(node) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(node);
            }
        }
    }

    // --- child executions -----------------------------------------------------------

    /// Starts a child execution of `workflow_id`, from the profile of `caller`'s
    /// execution, with `inputs` put on its workflow's "then" inputs (`ModuleCtx::start_child`).
    /// Returns its id at once: its nodes start on a thread of their own, and what it
    /// returns or puts out reaches `caller` as `node/child`, through the parent's queue.
    ///
    /// Refused: a caller that is not a running node of `module_id`, a workflow that is
    /// missing or declares no "then" output (nothing would come back), an input that is
    /// not one of its "then" inputs, a binding to a port that is not one of its "uses"
    /// inputs, a call deeper than `bus.max_depth` in the caller's chain, and the `[call]`
    /// limits.
    pub(crate) fn start_child(
        &self,
        module_id: &str,
        caller: &NodeRef,
        workflow_id: &str,
        inputs: Vec<(String, Value)>,
        opts: ChildOptions,
    ) -> Result<String, ModuleError> {
        let depth = bus::handling_depth(&caller.execution_id).map_or(0, |d| d + 1);
        if depth > self.bus.max_depth {
            return Err(ModuleError::Failed(format!(
                "the call is more than {} deliveries deep",
                self.bus.max_depth
            )));
        }
        let mut executions = self.executions();
        let parent = executions
            .iter()
            .find(|live| live.execution.id == caller.execution_id)
            .filter(|live| live.execution.state.is_live())
            .ok_or_else(|| {
                ModuleError::Failed(format!(
                    "execution `{}` is not running",
                    caller.execution_id
                ))
            })?;
        let owned = parent
            .workflow
            .node(&caller.node_id)
            .and_then(|n| n.module_id.as_deref())
            .is_some_and(|m| base_id(m) == base_id(module_id));
        if !owned {
            return Err(ModuleError::InvalidParams(format!(
                "node `{}` does not belong to this module",
                caller.node_id
            )));
        }
        if !parent.active.contains(&caller.node_id) {
            return Err(ModuleError::Failed(format!(
                "node `{}` is not running",
                caller.node_id
            )));
        }
        let nesting = parent.nesting + 1;
        if nesting > self.call.max_nesting {
            return Err(ModuleError::Failed(format!(
                "children nest more than {} deep ([call] max_nesting)",
                self.call.max_nesting
            )));
        }
        let children = executions
            .iter()
            .filter(|live| {
                live.execution.parent.as_deref() == Some(caller.execution_id.as_str())
                    && live.execution.state.is_live()
                    && live.link.as_ref().is_some_and(|link| !link.returned)
            })
            .count();
        if self.call.max_children > 0 && children >= self.call.max_children {
            return Err(ModuleError::Failed(format!(
                "execution `{}` already has {children} live children ([call] max_children)",
                caller.execution_id
            )));
        }
        let profile = self
            .profile_store()
            .get(&parent.execution.profile_id)
            .ok_or_else(|| {
                ModuleError::Failed(format!("profile `{}` is gone", parent.execution.profile_id))
            })?;
        let profile = profile.as_ref();
        let target = profile.workflow(workflow_id).ok_or_else(|| {
            ModuleError::InvalidParams(format!(
                "profile `{}` has no workflow `{workflow_id}`",
                profile.id
            ))
        })?;
        let then = |direction| {
            move |p: &&regulus_proto::Port| p.direction == direction && p.kind == WireKind::Then
        };
        if !target.ports.iter().any(|p| then(Direction::Output)(&p)) {
            return Err(ModuleError::InvalidParams(format!(
                "workflow `{workflow_id}` declares no \"then\" output: nothing would come back"
            )));
        }
        let mut fed = Vec::with_capacity(inputs.len());
        for (port, value) in inputs {
            let capability = target
                .port(&port)
                .filter(then(Direction::Input))
                .map(|p| p.capability.clone())
                .ok_or_else(|| {
                    ModuleError::InvalidParams(format!(
                        "`{port}` is not a \"then\" input of workflow `{workflow_id}`"
                    ))
                })?;
            fed.push((port, capability, value));
        }
        let mut bindings: HashMap<String, Vec<Endpoint>> = HashMap::new();
        for (port, endpoints) in opts.bindings {
            let declared = target
                .port(&port)
                .is_some_and(|p| p.direction == Direction::Input && p.kind == WireKind::Uses);
            if !declared {
                return Err(ModuleError::InvalidParams(format!(
                    "`{port}` is not a \"uses\" input of workflow `{workflow_id}`"
                )));
            }
            bindings.entry(port).or_default().extend(endpoints);
        }
        let workflow = Arc::new(flatten(profile, target).map_err(ModuleError::InvalidParams)?);
        if let Some(error) = missing_workflow(profile, &workflow, |id| self.registry.manifest(id)) {
            return Err(ModuleError::InvalidParams(error));
        }
        if let Some(error) = self.vetoed(profile.project_id.as_deref(), &workflow) {
            return Err(ModuleError::Failed(error));
        }
        let number = self.next_execution.fetch_add(1, Ordering::Relaxed);
        let execution = Execution {
            id: format!("exec-{number}"),
            profile_id: profile.id.clone(),
            workflow_id: workflow_id.into(),
            project_id: profile.project_id.clone(),
            state: ExecutionState::Starting,
            disabled: Vec::new(),
            parent: Some(caller.execution_id.clone()),
            caller: Some(caller.node_id.clone()),
            tag: opts.tag.clone(),
        };
        self.persist(|store| {
            store.insert_child(number, &execution, &workflow, Some(&caller.node_id))
        });
        let id = execution.id.clone();
        let mut child = Live::new(number, execution, workflow.clone());
        child.nesting = nesting;
        child.bindings = bindings;
        child.link = Some(CallLink {
            parent: caller.execution_id.clone(),
            node: caller.node_id.clone(),
            depth,
            returned: false,
            keep: opts.mode == ChildMode::Keep,
            tag: opts.tag,
        });
        // Like `workflow.send`: each input starts a chain of the child's own, delivered
        // once its nodes run.
        for (port, capability, value) in fed {
            let from = PortRef {
                node_id: WORKFLOW_SELF.into(),
                port_id: port,
            };
            self.publish(&mut child, from, capability, value, 0);
        }
        executions.push(child);
        drop(executions);

        // Starting nodes can take long and may call back into the caller's module: never
        // on the caller's thread.
        let core = self.me.clone();
        let child_id = id.clone();
        let spawned = std::thread::Builder::new()
            .name(CALL_THREAD.into())
            .spawn(move || {
                if let Some(core) = core.upgrade() {
                    let _ = core.launch(&child_id, &workflow, &HashMap::new());
                }
            });
        if let Err(error) = spawned {
            let reason = format!("cannot start its nodes: {error}");
            self.set_state(&id, ExecutionState::Failed { reason });
            self.stop(&id, true);
        }
        Ok(id)
    }

    /// Tells the node that called a child, once, that the child is no longer live.
    fn ended(&self, child_id: &str) {
        let news = {
            let mut executions = self.executions();
            let Some(child) = executions
                .iter_mut()
                .find(|live| live.execution.id == child_id)
                .filter(|live| !live.execution.state.is_live())
            else {
                return;
            };
            let Some(link) = child.link.take() else {
                return;
            };
            let news = ChildNews {
                execution_id: child_id.into(),
                workflow_id: child.execution.workflow_id.clone(),
                tag: link.tag,
                kind: News::Ended(child.execution.state.clone()),
            };
            (link.parent, end_news(news, link.node, link.depth))
        };
        self.tell_parent(&news.0, news.1);
    }

    /// Queues news from a child for its parent, if the parent is still live: a parent that
    /// stopped meanwhile has no one left to tell.
    fn tell_parent(&self, parent: &str, news: Pending) {
        {
            let mut executions = self.executions();
            let Some(live) = executions
                .iter_mut()
                .find(|live| live.execution.id == parent)
                .filter(|live| live.execution.state.is_live())
            else {
                return;
            };
            live.queue.push_back(news);
        }
        self.kick(parent);
    }

    /// A child's chain was cut past `bus.max_depth` before it returned: it fails, and its
    /// caller hears so. The stop runs on a thread of its own: this may be inside `emit`,
    /// and stopping calls back into the emitting module.
    fn fail_cut(&self, child_id: &str) {
        let reason = format!(
            "a chain was cut past [bus] max_depth ({}) before it returned",
            self.bus.max_depth
        );
        self.settle(
            child_id,
            ExecutionState::is_live,
            ExecutionState::Failed { reason },
        );
        let core = self.me.clone();
        let id = child_id.to_string();
        let spawned = std::thread::Builder::new()
            .name(CALL_THREAD.into())
            .spawn(move || {
                if let Some(core) = core.upgrade() {
                    core.stop(&id, true);
                }
            });
        if spawned.is_err() {
            self.stop(child_id, true);
        }
    }

    /// The index in `executions` of `child`, if it is a child that `caller`, a node of
    /// `module_id`, started; `None` if it is not in memory (an earlier run's). Refused: a
    /// caller that is not this module's node, a child of another node.
    fn own_child(
        executions: &[Live],
        module_id: &str,
        caller: &NodeRef,
        child: &str,
    ) -> Result<Option<usize>, ModuleError> {
        let parent = executions
            .iter()
            .find(|live| live.execution.id == caller.execution_id)
            .ok_or_else(|| {
                ModuleError::Failed(format!("execution `{}` is gone", caller.execution_id))
            })?;
        let owned = parent
            .workflow
            .node(&caller.node_id)
            .and_then(|n| n.module_id.as_deref())
            .is_some_and(|m| base_id(m) == base_id(module_id));
        if !owned {
            return Err(ModuleError::InvalidParams(format!(
                "node `{}` does not belong to this module",
                caller.node_id
            )));
        }
        let not_child = || {
            ModuleError::InvalidParams(format!(
                "execution `{child}` is not a child of node `{}`",
                caller.node_id
            ))
        };
        match executions
            .iter()
            .position(|live| live.execution.id == child)
        {
            None => Ok(None),
            Some(index) => {
                let execution = &executions[index].execution;
                if execution.parent.as_deref() == Some(caller.execution_id.as_str())
                    && execution.caller.as_deref() == Some(caller.node_id.as_str())
                {
                    Ok(Some(index))
                } else {
                    Err(not_child())
                }
            }
        }
    }

    /// `ModuleCtx::send_child`: like `workflow.send` on a live child of `caller`.
    pub(crate) fn send_child(
        &self,
        module_id: &str,
        caller: &NodeRef,
        child: &str,
        port: &str,
        value: Value,
    ) -> Result<(), ModuleError> {
        let depth = bus::handling_depth(&caller.execution_id).map_or(0, |d| d + 1);
        if depth > self.bus.max_depth {
            return Err(ModuleError::Failed(format!(
                "the send is more than {} deliveries deep",
                self.bus.max_depth
            )));
        }
        let mut executions = self.executions();
        let index = Self::own_child(&executions, module_id, caller, child)?.ok_or_else(|| {
            ModuleError::InvalidParams(format!(
                "execution `{child}` is not a child of node `{}`",
                caller.node_id
            ))
        })?;
        let live = &mut executions[index];
        if !live.execution.state.is_live() {
            return Err(ModuleError::Failed(format!(
                "execution `{child}` is not running"
            )));
        }
        let capability = live
            .workflow
            .port(port)
            .filter(|p| p.direction == Direction::Input && p.kind == WireKind::Then)
            .map(|p| p.capability.clone())
            .ok_or_else(|| {
                ModuleError::InvalidParams(format!(
                    "`{port}` is not a \"then\" input of workflow `{}`",
                    live.execution.workflow_id
                ))
            })?;
        // What comes back from this send is news at least as deep as the send.
        if let Some(link) = live.link.as_mut() {
            link.depth = link.depth.max(depth);
        }
        let from = PortRef {
            node_id: WORKFLOW_SELF.into(),
            port_id: port.into(),
        };
        self.publish(live, from, capability, value, 0);
        drop(executions);
        self.kick(child);
        Ok(())
    }

    /// `ModuleCtx::stop_child`: stops a child of `caller`, which then hears `ended`.
    pub(crate) fn stop_child(
        &self,
        module_id: &str,
        caller: &NodeRef,
        child: &str,
    ) -> Result<Execution, ModuleError> {
        self.child_status(module_id, caller, child)?;
        self.stop(child, true)
            .ok_or_else(|| ModuleError::Failed(format!("execution `{child}` is gone")))
    }

    /// `ModuleCtx::child_status`: a child of `caller` as it is now; one from an earlier
    /// run of the core, as the store has it.
    pub(crate) fn child_status(
        &self,
        module_id: &str,
        caller: &NodeRef,
        child: &str,
    ) -> Result<Execution, ModuleError> {
        let not_child = || {
            ModuleError::InvalidParams(format!(
                "execution `{child}` is not a child of node `{}`",
                caller.node_id
            ))
        };
        {
            let executions = self.executions();
            if let Some(index) = Self::own_child(&executions, module_id, caller, child)? {
                return Ok(executions[index].execution.clone());
            }
        }
        let stored = self
            .stored(child)
            .map_err(|e| ModuleError::Failed(e.message))?
            .ok_or_else(not_child)?;
        let execution = stored.execution;
        if execution.parent.as_deref() == Some(caller.execution_id.as_str())
            && execution.caller.as_deref() == Some(caller.node_id.as_str())
        {
            Ok(execution)
        } else {
            Err(not_child())
        }
    }

    /// `execution.tree`: an execution and its children, from what `execution.list` has.
    fn tree(&self, params: ExecutionTreeParams) -> Result<ExecutionTree, RpcError> {
        let all = self.list_executions(None)?;
        let root = all
            .iter()
            .find(|e| e.id == params.execution_id)
            .cloned()
            .ok_or_else(|| RpcError::invalid_params("Unknown execution"))?;
        fn grow(all: &[Execution], execution: Execution) -> ExecutionTree {
            let children = all
                .iter()
                .filter(|e| e.parent.as_deref() == Some(execution.id.as_str()))
                .map(|e| grow(all, e.clone()))
                .collect();
            ExecutionTree {
                execution,
                children,
            }
        }
        Ok(grow(&all, root))
    }

    // --- bus ----------------------------------------------------------------------------

    /// `event.list`: the kept bus events `params` asks for, and how many deliveries are
    /// still queued for the executions it names. A child that has not yet told its caller
    /// it ended counts as one more: the caller is still waiting for it.
    fn events(&self, params: &EventListParams) -> Result<EventPage, RpcError> {
        let executions = self.executions();
        let waiting = |parent: &str| {
            executions
                .iter()
                .filter(|child| child.link.as_ref().is_some_and(|l| l.parent == parent))
                .count()
        };
        let pending = executions
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
            .map(|live| live.queue.len() + usize::from(live.draining) + waiting(&live.execution.id))
            .sum();
        drop(executions);
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
        let cut = std::mem::take(&mut live.cut);
        drop(executions);
        self.kick(&node.execution_id);
        if cut {
            self.fail_cut(&node.execution_id);
        }
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
            if live.link.as_ref().is_some_and(|link| !link.returned) {
                live.cut = true;
            }
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
        // Only wires that leave `from` the way it faces: a boundary port passes a value on
        // to the side it did not come from.
        for wire in workflow.wires.iter().filter(|w| {
            w.kind == WireKind::Then
                && w.from == from
                && wire_end(&workflow, w, End::From).is_some()
        }) {
            let input = wire_end(&workflow, wire, End::To).filter(|p| p.kind == WireKind::Then);
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
                        child: None,
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
        if pending.child.is_some() {
            return self.deliver_child(execution_id, project_id, &workflow, active, pending);
        }
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

    /// Hands news from a child to the node that called it, as `node/child`. A returned
    /// value is reported on the bus like any delivery, from `<child>/self.<output>`; an
    /// end the node does not take is only logged, like a failed `node/stop`.
    fn deliver_child(
        &self,
        execution_id: &str,
        project_id: Option<String>,
        workflow: &Workflow,
        active: bool,
        pending: Pending,
    ) {
        let Some(news) = pending.child else {
            return;
        };
        let (port, capability) = (pending.from.port_id.clone(), pending.capability.clone());
        let (value, event) = (pending.value, pending.event);
        let kind = match news.kind {
            News::Ended(state) => ChildEventKind::Ended { state },
            News::Returned => ChildEventKind::Returned {
                port,
                capability,
                value,
                event,
            },
            News::Output => ChildEventKind::Output {
                port,
                capability,
                value,
                event,
            },
        };
        // A value from the child is reported on the bus like a delivery; an end is not.
        let returned = !matches!(kind, ChildEventKind::Ended { .. });
        let args = ChildEvent {
            execution_id: news.execution_id,
            workflow_id: news.workflow_id,
            tag: news.tag,
            kind,
        };
        let node = workflow
            .node(&pending.to.node_id)
            .filter(|n| active && n.module_id.is_some());
        let result = match node {
            None => Err(format!("node `{}` is not running", pending.to.node_id)),
            Some(node) => {
                let module_id = node.module_id.clone().unwrap_or_default();
                serde_json::to_value(args)
                    .map_err(|e| e.to_string())
                    .and_then(|args| {
                        self.node_call(execution_id, node, args)
                            .map_err(|e| e.to_string())
                    })
                    .and_then(|call| serde_json::to_value(call).map_err(|e| e.to_string()))
                    .and_then(|params| {
                        bus::while_handling(execution_id, pending.depth, || {
                            self.registry
                                .call_module(&module_id, method::NODE_CHILD, params)
                        })
                        .map_err(|e| e.to_string())
                    })
            }
        };
        if !returned {
            if let Err(error) = result {
                eprintln!(
                    "regulus: node `{}` did not take the end of `{}`: {error}",
                    pending.to.node_id, pending.from.node_id
                );
            }
            return;
        }
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
        let Some(index) = executions
            .iter()
            .position(|live| live.execution.id == execution_id && live.execution.state.is_live())
        else {
            return;
        };
        self.journal().record(event);
        let live = &mut executions[index];
        let depth = pending.depth + 1;
        // A child returns the first value that reaches one of its own outputs, unless the
        // depth cut drops it.
        let output = pending.to.node_id == WORKFLOW_SELF
            && depth <= self.bus.max_depth
            && live
                .workflow
                .port(&pending.to.port_id)
                .is_some_and(|p| p.direction == Direction::Output);
        let link = live
            .link
            .as_mut()
            .filter(|link| output && !link.returned)
            .map(|link| {
                link.returned = !link.keep;
                let news = if link.keep {
                    News::Output
                } else {
                    News::Returned
                };
                let tag = link.tag.clone();
                (
                    link.parent.clone(),
                    link.node.clone(),
                    link.depth,
                    news,
                    tag,
                )
            });
        let workflow_id = live.execution.workflow_id.clone();
        let value = link.as_ref().map(|_| pending.value.clone());
        let port = pending.to.port_id.clone();
        let capability = pending.capability.clone();
        let event = self.publish(live, pending.to, pending.capability, pending.value, depth);
        if std::mem::take(&mut live.cut) {
            drop(executions);
            return self.fail_cut(execution_id);
        }
        let (Some((parent, node, depth, news, tag)), Some(value)) = (link, value) else {
            return;
        };
        let keep = matches!(news, News::Output);
        if let Some(parent_live) = executions
            .iter_mut()
            .find(|live| live.execution.id == parent && live.execution.state.is_live())
        {
            // News from a child is not bounded by `bus.max_queue`: `[call] max_children`
            // bounds it, and an end that is dropped would leave the caller waiting.
            parent_live.queue.push_back(Pending {
                event,
                depth,
                from: child_self(execution_id, &port),
                capability,
                value,
                to: PortRef {
                    node_id: node,
                    port_id: port,
                },
                child: Some(ChildNews {
                    execution_id: execution_id.into(),
                    workflow_id,
                    tag,
                    kind: news,
                }),
            });
        }
        drop(executions);
        self.kick(&parent);
        // It returned: what is still on its way inside is dropped with it. A kept child
        // goes on.
        if !keep {
            self.stop(execution_id, true);
        }
    }
}

/// The news that a child ended (`news`), for the node `node` of its parent.
fn end_news(news: ChildNews, node: String, depth: u32) -> Pending {
    Pending {
        event: 0,
        depth,
        from: child_self(&news.execution_id, ""),
        capability: String::new(),
        value: Value::Null,
        to: PortRef {
            node_id: node,
            port_id: String::new(),
        },
        child: Some(news),
    }
}

/// The endpoints behind the "uses" wires of `live` that `wanted` picks: the nodes at
/// their other end that take part, and for a wire from the workflow's own input
/// (`self.<port>`), what its caller bound there, while that execution is live.
fn uses_endpoints(
    live: &Live,
    executions: &[Live],
    wanted: impl Fn(&regulus_proto::Wire) -> bool,
) -> Vec<Endpoint> {
    let workflow = &live.workflow;
    let mut found = Vec::new();
    for wire in workflow
        .wires
        .iter()
        .filter(|w| w.kind == WireKind::Uses && wanted(w))
    {
        if wire.from.node_id == WORKFLOW_SELF {
            let bound = live.bindings.get(&wire.from.port_id).into_iter().flatten();
            found.extend(
                bound
                    .filter(|endpoint| {
                        executions.iter().any(|other| {
                            other.execution.id == endpoint.node.execution_id
                                && other.execution.state.is_live()
                        })
                    })
                    .cloned(),
            );
            continue;
        }
        if !live.active.contains(&wire.from.node_id) {
            continue;
        }
        let Some(from) = workflow.node(&wire.from.node_id) else {
            continue;
        };
        let Some(module_id) = from.module_id.clone() else {
            continue;
        };
        found.push(Endpoint {
            node: NodeRef {
                execution_id: live.execution.id.clone(),
                node_id: from.id.clone(),
            },
            module_id,
        });
    }
    found
}

fn starting(state: &ExecutionState) -> bool {
    *state == ExecutionState::Starting
}

/// How the parent's bus names the outputs of a child: `<child>/self.<port>`. `/` and
/// `self` are reserved in node ids, so it never names a node of the parent.
fn child_self(child_id: &str, port: &str) -> PortRef {
    PortRef {
        node_id: format!("{child_id}{BOX_SEPARATOR}{WORKFLOW_SELF}"),
        port_id: port.into(),
    }
}

/// The module behind a local module whose process could not start: it is listed broken
/// with why, and never called.
struct Unstarted(regulus_proto::Manifest);

impl Module for Unstarted {
    fn manifest(&self) -> &regulus_proto::Manifest {
        &self.0
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, method: &str, _: Value) -> Result<Value, ModuleError> {
        Err(ModuleError::Failed(format!(
            "`{method}`: the local module did not start"
        )))
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}
