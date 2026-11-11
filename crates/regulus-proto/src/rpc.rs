//! The external boundary: JSON-RPC 2.0 methods, parameters and versioning.
//!
//! Versioning rules (docs/06-stack.md §5.1): same major = compatible; a new method bumps
//! the minor; removing or changing a method bumps the major.

use serde::{Deserialize, Serialize};

/// Protocol spoken by this core. 1.0 = the mock methods; 1.1 adds `profile.check`;
/// 1.2 adds `execution.start` and `execution.stop`; 1.3 adds `secret.set`, `secret.list`
/// and `secret.delete`; 1.4 adds `profile_id` to executions; 1.5 adds `event.list`; 1.6
/// adds the `interrupted` state and `NodeStart` for nodes resumed from a checkpoint; 1.7
/// adds workflow ports (`Workflow.ports`, inner wires to `self`) and `workflow.send`; 1.8
/// adds child executions: `Execution.parent` and `node/child` (`ChildEvent`); 1.9 adds
/// the `hook@1` relay: `hook.dispatch` and `node/hook`; 1.10 adds templates:
/// `template.list`, `template.get` and `template.apply`; 1.11 adds what the app needs to
/// edit: `module.manifest`, `profile.save` and `profile.delete`; 1.12 adds projects:
/// `project.list`, `project.get`, `project.open`, `project.close`, `project_id` on
/// templates and `template.apply`; 1.13 adds `config.client`; 1.14 adds `project.trust`
/// and the `blocked` module state; 1.15 adds `project` to the entries of `module.list`;
/// 1.16 adds children a node keeps (`ChildEventKind::Output`, `ChildEvent.tag`,
/// `Execution.caller` and `Execution.tag`), `execution.tree` and `template.save`; 1.17 adds
/// node shapes (`dynamic_port`, `when`, `managed_ports`, `module.shape`, `profile.sync`),
/// `project.reload`, the source of local modules (`source.get`, `source.save`,
/// `source.check`, `node.init`, `ProjectDetails.sources`) and `data` on errors.
pub const PROTOCOL: &str = "1.17";

pub mod method {
    pub const HELLO: &str = "hello";
    pub const PROFILE_LIST: &str = "profile.list";
    pub const PROFILE_GET: &str = "profile.get";
    pub const PROFILE_CHECK: &str = "profile.check";
    pub const MODULE_LIST: &str = "module.list";
    pub const EXECUTION_LIST: &str = "execution.list";
    pub const EXECUTION_START: &str = "execution.start";
    pub const EXECUTION_STOP: &str = "execution.stop";
    pub const SECRET_SET: &str = "secret.set";
    pub const SECRET_LIST: &str = "secret.list";
    pub const SECRET_DELETE: &str = "secret.delete";
    pub const EVENT_LIST: &str = "event.list";
    pub const WORKFLOW_SEND: &str = "workflow.send";
    /// Relays a `HookRequest` to the `hook@1` nodes wired into a node and returns their
    /// answers (`HookDispatchParams` → `HookDispatched`).
    pub const HOOK_DISPATCH: &str = "hook.dispatch";
    /// Templates found in `[templates] paths` (→ `Vec<TemplateSummary>`).
    pub const TEMPLATE_LIST: &str = "template.list";
    /// The raw text of one template (`TemplateParams` → `TemplateText`).
    pub const TEMPLATE_GET: &str = "template.get";
    /// Renders a template into a profile file and loads it (`TemplateApplyParams` →
    /// `TemplateApplied`).
    pub const TEMPLATE_APPLY: &str = "template.apply";
    /// Writes a profile, or one workflow of it with the workflows it places as boxes, as
    /// a template file (`TemplateSaveParams` → `TemplateSaved`, 1.16).
    pub const TEMPLATE_SAVE: &str = "template.save";
    /// The manifest of one loaded module, or of all of them (`ModuleManifestParams` →
    /// `Manifest` or `Vec<Manifest>`), whatever their state.
    pub const MODULE_MANIFEST: &str = "module.manifest";
    /// Writes a whole profile to the first `[profiles] paths` entry and loads it
    /// (`ProfileSaveParams` → `ProfileSaved`).
    pub const PROFILE_SAVE: &str = "profile.save";
    /// Removes a profile's file and unloads it (`IdParams` → `ProfileDeleted`).
    pub const PROFILE_DELETE: &str = "profile.delete";
    /// The open projects (→ `Vec<ProjectSummary>`).
    pub const PROJECT_LIST: &str = "project.list";
    /// One open project and its `regulus.toml` (`IdParams` → `ProjectDetails`).
    pub const PROJECT_GET: &str = "project.get";
    /// Opens a folder as a project and remembers it in `config.toml`
    /// (`ProjectOpenParams` → `ProjectSummary`).
    pub const PROJECT_OPEN: &str = "project.open";
    /// Forgets an open project; nothing on disk is deleted (`IdParams` → `ProjectSummary`).
    pub const PROJECT_CLOSE: &str = "project.close";
    /// Trusts a project folder, or stops trusting it (`ProjectTrustParams` →
    /// `ProjectTrusted`): only a trusted project's local modules run (1.14).
    pub const PROJECT_TRUST: &str = "project.trust";
    /// `[client]` of `config.toml` with a project's own `[client]` on top, for the clients
    /// (`ProjectFilter` → `ClientSettings`). Read-only.
    pub const CONFIG_CLIENT: &str = "config.client";
    /// An execution and its children, as deep as they go (`ExecutionTreeParams` →
    /// `ExecutionTree`). Read-only (1.16).
    pub const EXECUTION_TREE: &str = "execution.tree";
    /// The ports a node of a module's type has for a config (`ModuleShapeParams` →
    /// `shape::NodeShape`). Read-only (1.17).
    pub const MODULE_SHAPE: &str = "module.shape";
    /// A profile with its nodes' ports synced with their shape, and what that drops
    /// (`ProfileSyncParams` → `shape::ProfileSynced`). Never writes (1.17).
    pub const PROFILE_SYNC: &str = "profile.sync";
    /// Starts again the local modules of an open project, or one of them, without the rest
    /// of the core (`ProjectReloadParams` → `ProjectReloaded`, 1.17).
    pub const PROJECT_RELOAD: &str = "project.reload";
    /// The Python file of a local module (`SourceParams` → `SourceText`, 1.17).
    pub const SOURCE_GET: &str = "source.get";
    /// Writes it, unless it changed since the revision read (`SourceSaveParams` →
    /// `SourceSaved`, 1.17). Never runs it.
    pub const SOURCE_SAVE: &str = "source.save";
    /// Reads it, or a text for it, without running it (`SourceCheckParams` →
    /// `SourceCheck`, 1.17).
    pub const SOURCE_CHECK: &str = "source.check";
    /// Writes a new Python node into a project and loads it (`NodeInitParams` →
    /// `NodeInitialized`, 1.17).
    pub const NODE_INIT: &str = "node.init";

    /// Methods served by the core itself. A module may not claim any of them.
    pub const CORE: &[&str] = &[
        HELLO,
        PROFILE_LIST,
        PROFILE_GET,
        PROFILE_CHECK,
        MODULE_LIST,
        EXECUTION_LIST,
        EXECUTION_START,
        EXECUTION_STOP,
        SECRET_SET,
        SECRET_LIST,
        SECRET_DELETE,
        EVENT_LIST,
        WORKFLOW_SEND,
        HOOK_DISPATCH,
        TEMPLATE_LIST,
        TEMPLATE_GET,
        TEMPLATE_APPLY,
        TEMPLATE_SAVE,
        MODULE_MANIFEST,
        PROFILE_SAVE,
        PROFILE_DELETE,
        PROJECT_LIST,
        PROJECT_GET,
        PROJECT_OPEN,
        PROJECT_CLOSE,
        PROJECT_TRUST,
        CONFIG_CLIENT,
        EXECUTION_TREE,
        MODULE_SHAPE,
        PROFILE_SYNC,
        PROJECT_RELOAD,
        SOURCE_GET,
        SOURCE_SAVE,
        SOURCE_CHECK,
        NODE_INIT,
    ];

    /// Sent by the core to the module that owns a node, never by a client. A module may
    /// not declare them; one that does not implement them has no lifecycle.
    pub const NODE_START: &str = "node/start";
    pub const NODE_STOP: &str = "node/stop";
    /// A value arrived over a "then" wire; `args` is a `NodeEvent`.
    pub const NODE_EVENT: &str = "node/event";
    /// A child execution the node started returned or ended; `args` is a `ChildEvent`.
    pub const NODE_CHILD: &str = "node/child";
    /// A hook node is asked whether to go on; `args` is a `HookRequest`, the answer a
    /// `HookAnswer`. The core waits for it only so long (`hook.dispatch`).
    pub const NODE_HOOK: &str = "node/hook";
    /// Every method the core sends to the owner of a node.
    pub const NODE: &[&str] = &[NODE_START, NODE_STOP, NODE_EVENT, NODE_CHILD, NODE_HOOK];
}

/// Error codes beyond the JSON-RPC 2.0 standard ones.
pub mod code {
    pub const INVALID_PARAMS: i32 = -32602;
    /// Any method other than `hello` before a compatible `hello` on this connection.
    pub const HELLO_REQUIRED: i32 = -32001;
    /// The module serving the method is broken, disabled or incompatible.
    pub const MODULE_UNAVAILABLE: i32 = -32002;
    /// The module returned an error or panicked.
    pub const MODULE_FAILED: i32 = -32003;
    /// No template has that name, or the path holds no readable template.
    pub const TEMPLATE_NOT_FOUND: i32 = -32004;
    /// The template is malformed: its `[template]` table, a hole it does not declare, a
    /// rendered text that is not a profile, or a profile the check finds errors in.
    pub const TEMPLATE_INVALID: i32 = -32005;
    /// A module the template `requires` is not installed, or not at a version it accepts.
    pub const TEMPLATE_UNMET: i32 = -32006;
    /// The profile `template.apply` or `profile.save` would write already exists and
    /// `force` was not given, or it lives where the core may not replace or remove it:
    /// another file, a file shared with other profiles, or no file at all.
    pub const PROFILE_EXISTS: i32 = -32007;
    /// `profile.save`: the profile has errors (`profile.check`); nothing is written.
    pub const PROFILE_INVALID: i32 = -32008;
    /// `profile.delete`: an execution of the profile is starting or running.
    pub const PROFILE_IN_USE: i32 = -32009;
    /// No open project has that id, or the path to open is not a folder.
    pub const PROJECT_NOT_FOUND: i32 = -32010;
    /// The project cannot open: its `regulus.toml` cannot be read or is not TOML, or its id
    /// cannot be used (it must be a plain name, safe as a file name).
    pub const PROJECT_INVALID: i32 = -32011;
    /// Another open project has that id.
    pub const PROJECT_EXISTS: i32 = -32012;
    /// `execution.start`: a node's module is disabled in the profile's project
    /// (`[modules] disable` or `enable` in its `regulus.toml`).
    pub const MODULE_DISABLED: i32 = -32013;
    /// `project.reload`: live or resuming executions run nodes of the module; nothing was
    /// changed. `data.executions` lists them (`InUseExecution`); stop them, or ask again
    /// with `stop_executions` (1.17).
    pub const MODULE_IN_USE: i32 = -32014;
    /// `source.save`: the file changed since the revision the client read (`data.revision`
    /// is the current one, `null` if there is no file); `node.init`: its folder or entry
    /// exists. Nothing was written (1.17).
    pub const SOURCE_CONFLICT: i32 = -32015;
    /// `source.*`: no local module of that id in the project, or it has no Python file the
    /// core may edit (outside the project, a link, missing, not UTF-8, too large) (1.17).
    pub const SOURCE_UNAVAILABLE: i32 = -32016;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub proto: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    pub core: String,
    pub proto: String,
    pub compatible: bool,
}

/// Parameters of read methods: no `project_id` means every project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFilter {
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdParams {
    pub id: String,
}

/// `execution.start`: no `workflow_id` means the profile's main workflow.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionStartParams {
    pub profile_id: String,
    #[serde(default)]
    pub workflow_id: Option<String>,
}

/// `workflow.send`: puts `value` on the input `port` of the workflow a live execution
/// runs, as if it came from outside. Its type is the port's capability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowSendParams {
    pub execution_id: String,
    pub port: String,
    pub value: serde_json::Value,
}

/// The answer of `workflow.send`: the `seq` of the `emitted` event, the one every event
/// about this value points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowSent {
    pub event: u64,
}

/// `secret.set`. The value never comes back: `secret.list` answers names only.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SecretSetParams {
    pub name: String,
    pub value: String,
}

/// `secret.delete`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameParams {
    pub name: String,
}

/// What a module receives when a call is addressed to one of its nodes: `node/start`,
/// `node/stop`, `node/event`, `node/hook`, or a capability method reached through a wire. `node` carries the node's
/// configuration as it was when the execution started, with every `secret:<name>`
/// already swapped for the secret.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeCall {
    pub execution_id: String,
    pub node: crate::Node,
    #[serde(default)]
    pub args: serde_json::Value,
}

/// `NodeCall.args` of a `node/start` that resumes an execution after a restart of the
/// core, when the node saved a checkpoint (`ModuleCtx::checkpoint`). A fresh start sends
/// `null`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeStart {
    /// The last state the node saved.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<serde_json::Value>,
}

/// What a module receives with `node/child`, inside `NodeCall.args`: news from a child
/// execution one of its nodes started (`ModuleCtx::call_workflow` or `start_child`). A
/// child it calls returns at most once, then ends; one it keeps sends every output, until
/// it ends. One that fails or is stopped from outside only ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildEvent {
    /// The child execution.
    pub execution_id: String,
    pub workflow_id: String,
    /// What the caller tagged the child with when it started it (1.16).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>,
    #[serde(flatten)]
    pub kind: ChildEventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum ChildEventKind {
    /// The first value that reached an output of the child's workflow: its result. The
    /// core stops the child right after.
    Returned {
        /// The output port of the child's workflow.
        port: String,
        capability: String,
        value: serde_json::Value,
        /// The `seq` of the child's `emitted` event from `self.<port>`.
        event: u64,
    },
    /// A value reached an output of a child the node keeps (1.16): the child goes on.
    Output {
        /// The output port of the child's workflow.
        port: String,
        capability: String,
        value: serde_json::Value,
        /// The `seq` of the child's `emitted` event from `self.<port>`.
        event: u64,
    },
    /// The child is no longer live: `stopped` after it returned or was stopped,
    /// `failed` if a node of it failed to start.
    Ended { state: crate::ExecutionState },
}

/// `execution.tree`: an execution, the one at the root of the answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionTreeParams {
    pub execution_id: String,
}

/// The answer of `execution.tree`: the execution asked about and its children, each with
/// its own, in the order they started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionTree {
    pub execution: crate::Execution,
    pub children: Vec<ExecutionTree>,
}

/// A hole a template declares (`[template.vars]`). Without a default, applying it needs a
/// value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateVar {
    pub name: String,
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// A module a template needs: its id (`author/module`) and a semver requirement on the
/// installed version, e.g. `>=0.1`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateRequire {
    pub module: String,
    pub version: String,
}

/// One entry of `template.list`. A file that cannot be read as a template is listed by
/// its file name with `error` set, so the user sees why it is not usable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateSummary {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub path: String,
    #[serde(default)]
    pub vars: Vec<TemplateVar>,
    #[serde(default)]
    pub requires: Vec<TemplateRequire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// The open project the template belongs to; `None` for a global one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// `template.get`: a template by name, or by path when it contains `/` or ends in
/// `.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateParams {
    pub template: String,
}

/// The answer of `template.get`: the file as it is on disk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateText {
    pub name: String,
    pub path: String,
    pub text: String,
}

/// `template.apply`: renders `template` with `vars` and writes the profile to the first
/// `[profiles] paths` entry as `<id>.toml`. `id` defaults to the template's profile id; an
/// existing profile is only replaced with `force`. Var values are text; a number or a
/// boolean is taken as its JSON text. With `project_id` (an open project), its templates
/// are looked up first and the profile is written to its profiles folder with that
/// `project_id`; without it, a template of a project goes to that project.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TemplateApplyParams {
    pub template: String,
    #[serde(default)]
    pub vars: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

/// `template.save`: writes the profile `profile_id` as the template `name`. With
/// `workflow_id`, only that workflow, as the main one, and the workflows it runs (placed
/// as boxes or named by a node's `workflow` field), as deep as they go. `vars` declares
/// holes (`{{name}}`) the profile's text already has. The file goes to `path` (a file, or
/// a folder for `<name>.toml`; relative to the core's home) or to the first templates
/// path of the profile's project, else of `[templates] paths`. An existing file is only
/// replaced with `force`, and only if it is a template.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateSaveParams {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub profile_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vars: Option<std::collections::BTreeMap<String, TemplateSaveVar>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default)]
    pub force: bool,
}

/// A hole `template.save` declares.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateSaveVar {
    #[serde(default)]
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// The answer of `template.save`: where the template was written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateSaved {
    pub path: String,
}

/// The answer of `template.apply`: the profile now loaded, where it was written, and what
/// the check found short of an error (warnings and information).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateApplied {
    pub profile_id: String,
    pub path: String,
    pub diagnostics: Vec<Diagnostic>,
}

/// `module.manifest`: one module by id (with or without the version), or every loaded
/// module without it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleManifestParams {
    #[serde(default)]
    pub id: Option<String>,
}

/// `profile.save`: the whole profile, as `profile.get` returns it. A profile that is not
/// loaded yet is written as is; one that is loaded is only replaced with `force`, and only
/// in the file that holds it alone in the first `[profiles] paths` entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSaveParams {
    pub profile: crate::Profile,
    #[serde(default)]
    pub force: bool,
}

/// The answer of `profile.save`: the same as `template.apply`'s.
pub type ProfileSaved = TemplateApplied;

/// The answer of `profile.delete`: the profile no longer loaded and the file removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileDeleted {
    pub profile_id: String,
    pub path: String,
}

/// `project.open`: a folder (or its `regulus.toml`). A relative path is resolved against
/// the core's home; clients send absolute ones.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectOpenParams {
    pub path: String,
    /// Also trust it, so its local modules run (`project.trust`, 1.14).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub trust: bool,
}

/// `project.trust`: a project folder (or its `regulus.toml`), relative to the core's home
/// like `project.open`. `remove` stops trusting it. It need not be open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTrustParams {
    pub path: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remove: bool,
}

/// The answer of `project.trust`: the folder as the core resolved it and whether it is
/// trusted now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectTrusted {
    pub path: String,
    pub trusted: bool,
}

/// One entry of `project.list`, and the answer of `project.open` and `project.close`.
/// `warnings` says what in its `regulus.toml` the core did not understand or use; they
/// never stop it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectSummary {
    pub id: String,
    pub name: String,
    /// The folder.
    pub path: String,
    #[serde(default)]
    pub warnings: Vec<String>,
    /// Set when a folder of `[projects] paths` did not open on start (an unreadable
    /// `regulus.toml`, an invalid id, a folder that is gone): it is listed so a client can
    /// show it, but nothing of it is loaded. `project.open` tries again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The answer of `project.get`: the project's `regulus.toml` as the core resolved it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectDetails {
    pub id: String,
    pub name: String,
    /// The folder.
    pub path: String,
    /// Its `regulus.toml`.
    pub file: String,
    /// Where its profiles and templates are read from, absolute.
    pub profiles: Vec<String>,
    pub templates: Vec<String>,
    /// `[modules] disable` and `enable`: module ids whose nodes do not run in it.
    #[serde(default)]
    pub disable: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable: Option<Vec<String>>,
    /// `[[modules.local]]` as written: kept, not loaded yet.
    #[serde(default)]
    pub local: Vec<serde_json::Value>,
    /// `[client]`, as written, for the clients.
    #[serde(default)]
    pub client: serde_json::Map<String, serde_json::Value>,
    /// The whole file as read, unknown keys included where they were.
    #[serde(default)]
    pub content: serde_json::Value,
    #[serde(default)]
    pub warnings: Vec<String>,
    /// One per `[[modules.local]]` with an id: where its editable Python file is, or why it
    /// has none (1.17).
    #[serde(default)]
    pub sources: Vec<LocalSource>,
}

/// Where the editable file of a local module is (`source.*`), or why it has none.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalSource {
    /// Its id in `[[modules.local]]`.
    pub module: String,
    /// Relative to the project.
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

/// `module.shape`: a node type of a loaded module (any version if the exact one is not
/// loaded) and a node's config; the field defaults fill what it does not set.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleShapeParams {
    pub module_id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub config: serde_json::Map<String, serde_json::Value>,
}

/// `profile.sync`: a profile (`profile`, e.g. a client's unsaved one) or a loaded one
/// (`profile_id`). With `node_ids`, those nodes; without, every stale node of a
/// `managed_ports` type. Nothing is written.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProfileSyncParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<crate::Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node_ids: Option<Vec<String>>,
}

/// `project.reload`: an open project, and one of its local modules (its id in
/// `[[modules.local]]`, or `<project>/<id>[@version]`) or all of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectReloadParams {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<String>,
    /// Stop the live executions that run nodes of a module being replaced, instead of
    /// refusing (`MODULE_IN_USE`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stop_executions: bool,
}

/// What a reload did to one local module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReloadOutcome {
    /// Nothing usable ran before; `state` says whether it runs now.
    Started,
    /// A new process took the place of the old one.
    Replaced,
    /// The new one failed (`error`); the old one still runs.
    Kept,
    /// The new one failed and nothing usable ran before either.
    Failed,
    /// The project is not trusted: no process.
    Blocked,
    /// Its entry left `regulus.toml`: the old one is stopped.
    Removed,
    /// Its entry cannot run or `--without` leaves it out (`error`).
    Skipped,
}

/// Node types a reload added, removed or changed, by type.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeTypeChanges {
    #[serde(default)]
    pub added: Vec<String>,
    #[serde(default)]
    pub removed: Vec<String>,
    #[serde(default)]
    pub changed: Vec<String>,
}

/// One local module after `project.reload` (or `node.init`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReloadedModule {
    pub local_id: String,
    /// The module id listed now (`<project>/<id>@<version>`).
    pub id: String,
    /// The one listed before, if there was one.
    #[serde(default)]
    pub previous: Option<String>,
    pub outcome: ReloadOutcome,
    pub state: crate::ModuleState,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub node_types: NodeTypeChanges,
}

/// A node of a loaded profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeLocation {
    pub profile_id: String,
    pub workflow_id: String,
    pub node_id: String,
}

/// The answer of `project.reload`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectReloaded {
    pub project: ProjectSummary,
    pub modules: Vec<ReloadedModule>,
    /// Executions stopped because `stop_executions` was given.
    #[serde(default)]
    pub stopped_executions: Vec<String>,
    /// Nodes of loaded profiles whose ports now differ from their shape (`profile.sync`).
    #[serde(default)]
    pub stale: Vec<NodeLocation>,
}

/// An execution that keeps a reload from happening (`MODULE_IN_USE` data).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InUseExecution {
    pub id: String,
    pub profile_id: String,
    pub workflow_id: String,
    /// Its nodes of the module.
    pub nodes: Vec<String>,
}

/// `source.get`: a local module of an open project, by its id in `[[modules.local]]` (or
/// `<project>/<id>[@version]`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceParams {
    pub project_id: String,
    pub module: String,
}

/// The answer of `source.get`. `revision` is opaque: compare it, never parse it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceText {
    pub project_id: String,
    pub module: String,
    /// Relative to the project.
    pub path: String,
    /// Absolute.
    pub file: String,
    pub language: String,
    pub text: String,
    pub revision: String,
}

/// `source.save`: `revision` is what the client read (`None`: it expects no file). A file
/// that changed since is only replaced with `force`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSaveParams {
    pub project_id: String,
    pub module: String,
    pub text: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub force: bool,
}

/// The answer of `source.save`: the new revision and what a check of the saved text found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceSaved {
    pub path: String,
    pub file: String,
    pub revision: String,
    pub check: SourceCheck,
}

/// `source.check`: `text`, or the file on disk without it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCheckParams {
    pub project_id: String,
    pub module: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// A problem in a source, at a place in it: 1-based lines, and columns in Unicode code
/// points, 1-based too.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDiagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_line: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_column: Option<u32>,
}

/// What reading a source without running it found: its problems and the node types it
/// declares, as far as they can be read (`complete`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceCheck {
    pub language: String,
    /// `false` if the checker could not run (a `check.unavailable` diagnostic says why).
    pub checked: bool,
    #[serde(default)]
    pub interpreter: Option<String>,
    #[serde(default)]
    pub diagnostics: Vec<SourceDiagnostic>,
    #[serde(default)]
    pub nodes: Vec<crate::NodeType>,
    /// Every node declaration was read without running the file.
    #[serde(default)]
    pub complete: bool,
    /// Whether `nodes` are the loaded module's; `None` if it is not loaded or `complete` is
    /// false.
    #[serde(default)]
    pub matches_loaded: Option<bool>,
}

/// `node.init`: a new Python node in an open project. `python` is the program its entry
/// runs it with (`[local_modules] python` by default).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInitParams {
    pub project_id: String,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
}

/// The answer of `node.init`: what was written and how the module loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeInitialized {
    pub project_id: String,
    /// `<project>/<id>`, what a node's `module_id` names.
    pub module_id: String,
    pub node_type: String,
    /// The Python file to edit, relative to the project.
    pub source: String,
    /// Every file written, relative to the project.
    pub files: Vec<String>,
    /// `regulus.toml` was written again as a whole (its comments are lost).
    pub rewritten: bool,
    #[serde(default)]
    pub notes: Vec<String>,
    pub module: ReloadedModule,
}

/// `config.client`: the settings the clients read, only as far as they are written down;
/// what is not there, each client takes from its own defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ClientSettings {
    /// `[client]` of `config.toml` with the project's `[client]` on top, key by key; a table
    /// on both sides (`keys`, `tui`) is merged key by key too. Keys the core does not know
    /// are passed as they are: what a client understands is up to the client.
    #[serde(default)]
    pub client: serde_json::Map<String, serde_json::Value>,
    /// The file each value came from (`config.toml` or `regulus.toml`), by `key` or, inside
    /// a table, `table.key`.
    #[serde(default)]
    pub sources: std::collections::BTreeMap<String, String>,
    /// What `config.toml` has in `[client]` that this core does not know.
    #[serde(default)]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A problem found when checking a profile. `code` is stable and machine readable;
/// `message` is for humans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
    pub wire_id: Option<String>,
}

fn parts(version: &str) -> Option<(u64, Option<u64>)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = match parts.next() {
        Some(minor) => Some(minor.parse().ok()?),
        None => None,
    };
    Some((major, minor))
}

/// Same major = compatible.
pub fn compatible(core: &str, client: &str) -> bool {
    match (parts(core), parts(client)) {
        (Some((a, _)), Some((b, _))) => a == b,
        _ => false,
    }
}

/// Whether `have` satisfies a requirement such as `^1`, `^1.1` or `1`: same major and,
/// if the requirement names a minor, at least that minor.
pub fn satisfies(requirement: &str, have: &str) -> bool {
    let requirement = requirement.trim().trim_start_matches('^');
    match (parts(requirement), parts(have)) {
        (Some((major, minor)), Some((have_major, have_minor))) => {
            major == have_major && minor.is_none_or(|minor| have_minor.unwrap_or(0) >= minor)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_major_is_compatible() {
        assert!(compatible("1.1", "1.0"));
        assert!(compatible("1.0", "1.9"));
        assert!(!compatible("1.1", "2.0"));
        assert!(!compatible("1.1", "garbage"));
        assert!(!compatible("1.1", ""));
    }

    #[test]
    fn requirements_check_major_and_minimum_minor() {
        assert!(satisfies("^1", "1.1"));
        assert!(satisfies("^1.1", "1.1"));
        assert!(satisfies("1", "1.0"));
        assert!(!satisfies("^1.2", "1.1"));
        assert!(!satisfies("^2", "1.1"));
        assert!(!satisfies("latest", "1.1"));
    }

    #[test]
    fn a_module_with_dynamic_ports_is_refused_by_a_core_that_would_drop_them() {
        // What the Python SDK declares when a node has dynamic ports (1.17).
        assert!(!satisfies("^1.17", "1.16"));
        assert!(satisfies("^1.17", "1.17"));
        assert!(satisfies("^1.17", PROTOCOL));
        // And without: every 1.x core takes it, this one too.
        assert!(satisfies("^1", "1.16") && satisfies("^1", PROTOCOL));
    }
}
