//! Code (docs/14-code.md): a folder an agent works in, as boxes. `workspace` is the
//! folder and what may be done in it: its operations reach an agent as tools (`tool@1`)
//! and other boxes through `code.workspace@1`; `tools` offers some of those operations
//! (what a read-only worker gets); `instructions` gives an agent the repository's own
//! instructions as a hook (`hook@1`).
//!
//! The module runs no model and keeps no conversation: the Agent box, its engine, the
//! provider, whoever decides a permission and the workers are other modules. It owns
//! what happens in the folder: every path resolved and kept inside the root, every
//! answer within a limit, commands with a time limit and nothing left running, an edit
//! that only lands on text that matches exactly, and git that runs nothing a repository
//! configures. The shell starts in the root and is not a sandbox.

pub mod contract;
mod fsops;
mod git;
mod hook;
mod look;
mod place;
mod run;
mod tools;

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use regulus_contract_tool as tool;
use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{
    method, Direction, Manifest, Node, NodeCall, NodeEvent, NodeStart, WireKind, HOOK_DESCRIBE,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

use crate::contract::{tool_name, Access, Description, InspectParams, Look, Operation};
use crate::git::Git;
use crate::place::Root;
use crate::run::{Key, Runs};

pub use crate::contract::{CAPABILITY, INSPECT};

const MANIFEST: &str = include_str!("../regulus-module.toml");

/// The node types.
const WORKSPACE: &str = "workspace";
const VIEW: &str = "tools";
const INSTRUCTIONS: &str = "instructions";

/// The ports the code names.
const WORKSPACE_PORT: &str = "workspace";
const ROOT_PORT: &str = "root";
const CHANGED_PORT: &str = "changed";
const RAN_PORT: &str = "ran";

/// The fields of each node type, as the manifest declares them: every one the code reads
/// is here, and a test holds the two lists together.
pub(crate) mod field {
    pub const ROOT: &str = "root";
    pub const TOOLS: &str = "tools";
    pub const PROTECT: &str = "protect";
    pub const SKIP: &str = "skip";
    pub const MAX_READ_BYTES: &str = "max_read_bytes";
    pub const MAX_LIST_ENTRIES: &str = "max_list_entries";
    pub const MAX_SEARCH_MATCHES: &str = "max_search_matches";
    pub const MAX_DIFF_BYTES: &str = "max_diff_bytes";
    pub const MAX_OUTPUT_BYTES: &str = "max_output_bytes";
    pub const MAX_LINE_CHARS: &str = "max_line_chars";
    pub const MAX_SEARCH_FILES: &str = "max_search_files";
    pub const MAX_SEARCH_FILE_BYTES: &str = "max_search_file_bytes";
    pub const SEARCH_TIMEOUT_MS: &str = "search_timeout_ms";
    pub const MAX_SEARCH_DEPTH: &str = "max_search_depth";
    pub const MAX_FOLDER_ENTRIES: &str = "max_folder_entries";
    pub const MAX_WRITE_BYTES: &str = "max_write_bytes";
    pub const INSPECT_MAX_BYTES: &str = "inspect_max_bytes";
    pub const INSPECT_MAX_ENTRIES: &str = "inspect_max_entries";
    pub const SHELL: &str = "shell";
    pub const COMMAND_TIMEOUT_MS: &str = "command_timeout_ms";
    pub const MAX_COMMAND_TIMEOUT_MS: &str = "max_command_timeout_ms";
    pub const KILL_GRACE_MS: &str = "kill_grace_ms";
    pub const PIPE_GRACE_MS: &str = "pipe_grace_ms";
    pub const MAX_RUNNING: &str = "max_running";
    pub const ENV_PASSTHROUGH: &str = "env_passthrough";
    pub const ENV: &str = "env";
    pub const GIT: &str = "git";
    pub const GIT_TIMEOUT_MS: &str = "git_timeout_ms";
    pub const GIT_DIR: &str = "git_dir";
    /// The fields of `workspace`.
    #[cfg(test)]
    pub const WORKSPACE: &[&str] = &[
        ROOT,
        TOOLS,
        PROTECT,
        SKIP,
        MAX_READ_BYTES,
        MAX_LIST_ENTRIES,
        MAX_SEARCH_MATCHES,
        MAX_DIFF_BYTES,
        MAX_OUTPUT_BYTES,
        MAX_LINE_CHARS,
        MAX_SEARCH_FILES,
        MAX_SEARCH_FILE_BYTES,
        SEARCH_TIMEOUT_MS,
        MAX_SEARCH_DEPTH,
        MAX_FOLDER_ENTRIES,
        MAX_WRITE_BYTES,
        INSPECT_MAX_BYTES,
        INSPECT_MAX_ENTRIES,
        SHELL,
        COMMAND_TIMEOUT_MS,
        MAX_COMMAND_TIMEOUT_MS,
        KILL_GRACE_MS,
        PIPE_GRACE_MS,
        MAX_RUNNING,
        ENV_PASSTHROUGH,
        ENV,
        GIT,
        GIT_TIMEOUT_MS,
        GIT_DIR,
    ];

    pub const ACCESS: &str = "access";
    /// The fields of `tools`.
    #[cfg(test)]
    pub const VIEW: &[&str] = &[ACCESS, TOOLS];

    pub const FILES: &str = "files";
    pub const EVENT: &str = "event";
    pub const HEADER: &str = "header";
    pub const MAX_CHARS: &str = "max_chars";
    /// The fields of `instructions`.
    #[cfg(test)]
    pub const INSTRUCTIONS: &[&str] = &[FILES, EVENT, HEADER, MAX_CHARS];
}

/// Why an operation did not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Fault {
    /// What was asked for cannot be given: a path outside the root, a file that is not
    /// there, text that does not match. Asking differently may work.
    Request(String),
    /// The workspace itself failed: no root, a disk error.
    Failed(String),
}

impl Fault {
    pub(crate) fn text(&self) -> &str {
        match self {
            Self::Request(text) | Self::Failed(text) => text,
        }
    }

    /// As the error of a call: a bad request is the caller's, the rest is the module's.
    fn into_error(self) -> ModuleError {
        match self {
            Self::Request(text) => ModuleError::InvalidParams(text),
            Self::Failed(text) => ModuleError::Failed(text),
        }
    }

    /// What another module's call failed with, read the same way.
    fn from_error(error: ModuleError) -> Self {
        match error {
            ModuleError::InvalidParams(text) => Self::Request(text),
            other => Self::Failed(other.to_string()),
        }
    }
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn to_json(value: impl Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(|e| ModuleError::Failed(e.to_string()))
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// A node's fields: its own value, or the manifest's default.
pub(crate) struct Fields<'a> {
    manifest: &'a Manifest,
    node: &'a Node,
}

impl<'a> Fields<'a> {
    fn value(&self, id: &str) -> Option<&'a Value> {
        self.manifest.setting(self.node, id)
    }

    pub(crate) fn text(&self, id: &str) -> String {
        match self.value(id) {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    pub(crate) fn count(&self, id: &str) -> u64 {
        match self.value(id) {
            Some(Value::Number(n)) => n.as_f64().map_or(0, |n| n.max(0.0) as u64),
            Some(Value::String(text)) => text.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn size(&self, id: &str) -> usize {
        usize::try_from(self.count(id)).unwrap_or(usize::MAX)
    }

    fn millis(&self, id: &str) -> Duration {
        Duration::from_millis(self.count(id))
    }

    /// A list, or a text with its items between `,` or `|`: how a template fills one.
    pub(crate) fn list(&self, id: &str) -> Vec<String> {
        let clean = |item: &str| Some(item.trim().to_string()).filter(|item| !item.is_empty());
        match self.value(id) {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .filter_map(clean)
                .collect(),
            Some(Value::String(items)) => items.split([',', '|']).filter_map(clean).collect(),
            _ => Vec::new(),
        }
    }

    fn map(&self, id: &str) -> Vec<(String, String)> {
        match self.value(id) {
            Some(Value::Object(entries)) => entries
                .iter()
                .map(|(name, value)| {
                    let value = match value {
                        Value::String(text) => text.clone(),
                        other => other.to_string(),
                    };
                    (name.clone(), value)
                })
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// A workspace node's settings, read on each call: an edit of the profile is a new
/// execution, never a surprise in a running one.
#[derive(Debug, Clone)]
pub(crate) struct Settings {
    pub root: String,
    pub tools: Vec<String>,
    pub protect: Vec<String>,
    pub skip: Vec<String>,
    pub max_read_bytes: usize,
    pub max_list_entries: usize,
    pub max_search_matches: usize,
    pub max_diff_bytes: usize,
    pub max_output_bytes: usize,
    pub max_line_chars: usize,
    pub max_search_files: usize,
    pub max_search_file_bytes: u64,
    pub search_timeout: Duration,
    pub max_search_depth: usize,
    pub max_folder_entries: usize,
    pub max_write_bytes: usize,
    pub inspect_max_bytes: usize,
    pub inspect_max_entries: usize,
    pub shell: Vec<String>,
    pub command_timeout: Duration,
    pub max_command_timeout: Duration,
    pub kill_grace: Duration,
    pub pipe_grace: Duration,
    pub max_running: usize,
    pub env_passthrough: Vec<String>,
    pub env: Vec<(String, String)>,
    pub git: String,
    pub git_timeout: Duration,
    pub git_inside_only: bool,
}

impl Settings {
    fn read(manifest: &Manifest, node: &Node) -> Result<Self, String> {
        use field::*;
        let fields = Fields { manifest, node };
        let bad = |what: String| format!("code workspace `{}`: {what}", node.id);
        let tools = fields.list(TOOLS);
        if let Some(unknown) = tools.iter().find(|t| !tool_name::ALL.contains(&t.as_str())) {
            return Err(bad(format!(
                "`tools` names `{unknown}`, which is not one of {}",
                tool_name::ALL.join(", ")
            )));
        }
        let shell = fields.list(SHELL);
        if shell.is_empty() {
            return Err(bad(
                "`shell` is empty: it is the program that runs a command line".into(),
            ));
        }
        let git_inside_only = match fields.text(GIT_DIR).trim() {
            "any" | "" => false,
            "inside" => true,
            other => return Err(bad(format!("`git_dir` is any or inside, not `{other}`"))),
        };
        let git = match fields.text(GIT).trim() {
            "" => return Err(bad("`git` is empty: it is the git program to run".into())),
            program => program.to_string(),
        };
        Ok(Self {
            root: fields.text(ROOT).trim().to_string(),
            tools,
            protect: fields.list(PROTECT),
            skip: fields.list(SKIP),
            max_read_bytes: fields.size(MAX_READ_BYTES).max(1),
            max_list_entries: fields.size(MAX_LIST_ENTRIES).max(1),
            max_search_matches: fields.size(MAX_SEARCH_MATCHES).max(1),
            max_diff_bytes: fields.size(MAX_DIFF_BYTES).max(1),
            max_output_bytes: fields.size(MAX_OUTPUT_BYTES).max(2),
            max_line_chars: fields.size(MAX_LINE_CHARS).max(1),
            max_search_files: fields.size(MAX_SEARCH_FILES).max(1),
            max_search_file_bytes: fields.count(MAX_SEARCH_FILE_BYTES).max(1),
            search_timeout: fields
                .millis(SEARCH_TIMEOUT_MS)
                .max(Duration::from_millis(1)),
            max_search_depth: fields.size(MAX_SEARCH_DEPTH),
            max_folder_entries: fields.size(MAX_FOLDER_ENTRIES).max(1),
            max_write_bytes: fields.size(MAX_WRITE_BYTES).max(1),
            inspect_max_bytes: fields.size(INSPECT_MAX_BYTES).max(1),
            inspect_max_entries: fields.size(INSPECT_MAX_ENTRIES).max(1),
            shell,
            command_timeout: fields
                .millis(COMMAND_TIMEOUT_MS)
                .max(Duration::from_millis(1)),
            max_command_timeout: fields
                .millis(MAX_COMMAND_TIMEOUT_MS)
                .max(Duration::from_millis(1)),
            kill_grace: fields.millis(KILL_GRACE_MS),
            pipe_grace: fields.millis(PIPE_GRACE_MS),
            max_running: fields.size(MAX_RUNNING),
            env_passthrough: fields.list(ENV_PASSTHROUGH),
            env: fields.map(ENV),
            git,
            git_timeout: fields.millis(GIT_TIMEOUT_MS).max(Duration::from_millis(1)),
            git_inside_only,
        })
    }
}

/// What a `tools` node lets through.
#[derive(Debug, Clone)]
struct View {
    /// The widest kind of operation it offers.
    access: Access,
    /// Of those, only these names; empty: all.
    only: Vec<String>,
}

impl View {
    fn read(manifest: &Manifest, node: &Node) -> Result<Self, String> {
        let fields = Fields { manifest, node };
        let access = match fields.text(field::ACCESS).trim() {
            "read" | "" => Access::Read,
            "write" => Access::Write,
            "all" => Access::Execute,
            other => {
                return Err(format!(
                    "code tools `{}`: `access` is read, write or all, not `{other}`",
                    node.id
                ))
            }
        };
        Ok(Self {
            access,
            only: fields.list(field::TOOLS),
        })
    }

    fn allows(&self, operation: &Operation) -> bool {
        let within = match self.access {
            Access::Read => operation.access == Access::Read,
            Access::Write => operation.access != Access::Execute,
            Access::Execute => true,
        };
        within && (self.only.is_empty() || self.only.contains(&operation.name))
    }
}

/// A node of this module while its execution runs.
struct Live {
    /// As it arrived in `node/start`: its settings, secrets resolved.
    node: Node,
    /// A workspace's root as it last arrived on `root`; none: the `root` field.
    moved: Option<String>,
}

/// The module.
pub struct Code {
    manifest: Manifest,
    ctx: ModuleCtx,
    /// The nodes of this module in running executions.
    live: Mutex<HashMap<Key, Live>>,
    /// The commands its workspaces have running.
    runs: Arc<Runs>,
}

/// One workspace node, for one call.
pub(crate) struct Workspace<'a> {
    code: &'a Code,
    key: Key,
    node: Node,
    pub(crate) settings: Settings,
}

impl Workspace<'_> {
    /// The folder the workspace works in now.
    pub(crate) fn root(&self) -> Result<Root, Fault> {
        let moved = lock(&self.code.live)
            .get(&self.key)
            .and_then(|live| live.moved.clone());
        let folder = moved.unwrap_or_else(|| self.settings.root.clone());
        Root::open(&folder, self.settings.protect.clone())
    }

    /// What of the core's environment a program of this workspace gets.
    pub(crate) fn inherited(&self) -> Vec<(String, String)> {
        self.settings
            .env_passthrough
            .iter()
            .filter_map(|name| Some((name.clone(), std::env::var(name).ok()?)))
            .collect()
    }

    pub(crate) fn git(&self) -> Git {
        Git {
            program: self.settings.git.clone(),
            timeout: self.settings.git_timeout,
            env: self.inherited(),
            inside_only: self.settings.git_inside_only,
            pipe_grace: self.settings.pipe_grace,
        }
    }

    pub(crate) fn runs(&self) -> &Runs {
        &self.code.runs
    }

    pub(crate) fn key(&self) -> &Key {
        &self.key
    }

    /// The operations this workspace has, in the catalog's order.
    pub(crate) fn operations(&self) -> Vec<Operation> {
        tools::catalog(&self.settings)
            .into_iter()
            .filter(|operation| self.settings.tools.contains(&operation.name))
            .collect()
    }

    /// Runs one operation for an agent. What went wrong is an answer for it to read, not
    /// a broken call.
    fn outcome(&self, call: &tool::Call) -> tool::Outcome {
        if !self.settings.tools.contains(&call.name) {
            return tool::Outcome {
                data: json!(format!("this workspace has no tool `{}`", call.name)),
                error: true,
            };
        }
        match self.perform(call) {
            Ok((data, failed)) => tool::Outcome {
                data,
                error: failed,
            },
            Err(fault) => tool::Outcome {
                data: json!(fault.text()),
                error: true,
            },
        }
    }

    /// Puts `value` on the node's output `port`, if the node declares it.
    pub(crate) fn emit(&self, port: &str, value: Value) {
        let declared = self
            .node
            .port(port)
            .is_some_and(|p| p.kind == WireKind::Then && p.direction == Direction::Output);
        if !declared {
            return;
        }
        let me = NodeRef {
            execution_id: self.key.0.clone(),
            node_id: self.key.1.clone(),
        };
        if let Err(error) = self.code.ctx.emit(&me, port, value) {
            eprintln!(
                "regulus: code workspace `{}`: nothing left on `{port}`: {error}",
                self.key.1
            );
        }
    }
}

/// The workspace behind a `tools` or an `instructions` node: one of this module's, reached
/// directly, or whatever else offers `code.workspace@1`, reached over the wire.
pub(crate) enum Backing<'a> {
    Own(Box<Workspace<'a>>),
    Other(&'a Code, Endpoint),
}

impl Backing<'_> {
    fn operations(&self) -> Result<Vec<Operation>, Fault> {
        match self {
            Self::Own(workspace) => Ok(workspace.operations()),
            Self::Other(code, endpoint) => {
                let described = code
                    .ctx
                    .call(endpoint, contract::DESCRIBE, Value::Null)
                    .map_err(Fault::from_error)?;
                let description: Description = serde_json::from_value(described)
                    .map_err(|e| Fault::Failed(format!("not a workspace's description: {e}")))?;
                Ok(description.operations)
            }
        }
    }

    fn call(&self, call: &tool::Call) -> Result<tool::Outcome, Fault> {
        match self {
            Self::Own(workspace) => Ok(workspace.outcome(call)),
            Self::Other(code, endpoint) => {
                let args = serde_json::to_value(call).map_err(|e| Fault::Failed(e.to_string()))?;
                let answer = code
                    .ctx
                    .call(endpoint, contract::CALL, args)
                    .map_err(Fault::from_error)?;
                serde_json::from_value(answer)
                    .map_err(|e| Fault::Failed(format!("not an operation's outcome: {e}")))
            }
        }
    }

    pub(crate) fn look(&self, look: &Look) -> Result<Value, Fault> {
        match self {
            Self::Own(workspace) => workspace.look(look),
            Self::Other(code, endpoint) => {
                let args = serde_json::to_value(look).map_err(|e| Fault::Failed(e.to_string()))?;
                code.ctx
                    .call(endpoint, contract::LOOK, args)
                    .map_err(Fault::from_error)
            }
        }
    }
}

fn node_ref(call: &NodeCall) -> NodeRef {
    NodeRef {
        execution_id: call.execution_id.clone(),
        node_id: call.node.id.clone(),
    }
}

fn specs(operations: Vec<Operation>) -> Vec<tool::Spec> {
    operations
        .into_iter()
        .map(|operation| tool::Spec {
            name: operation.name,
            description: operation.description,
            input: operation.input,
        })
        .collect()
}

impl Code {
    fn workspace(&self, key: Key, node: Node) -> Result<Workspace<'_>, ModuleError> {
        let settings = Settings::read(&self.manifest, &node).map_err(ModuleError::InvalidParams)?;
        Ok(Workspace {
            code: self,
            key,
            node,
            settings,
        })
    }

    /// The workspace node a call is for.
    fn workspace_of(&self, call: NodeCall) -> Result<Workspace<'_>, ModuleError> {
        if call.node.node_type != WORKSPACE {
            return Err(ModuleError::InvalidParams(format!(
                "`{}` is a `{}` node, not a code workspace",
                call.node.id, call.node.node_type
            )));
        }
        self.workspace((call.execution_id, call.node.id.clone()), call.node)
    }

    /// What is wired into the `workspace` input of `me`.
    pub(crate) fn backing(&self, me: &NodeRef) -> Result<Backing<'_>, ModuleError> {
        let endpoint = self
            .ctx
            .resolve(me, WORKSPACE_PORT)
            .into_iter()
            .next()
            .ok_or_else(|| {
                ModuleError::Failed(format!(
                    "nothing is wired into `{WORKSPACE_PORT}` of `{}`: connect a code workspace",
                    me.node_id
                ))
            })?;
        let key = (
            endpoint.node().execution_id.clone(),
            endpoint.node().node_id.clone(),
        );
        let own = lock(&self.live)
            .get(&key)
            .filter(|live| live.node.node_type == WORKSPACE)
            .map(|live| live.node.clone());
        match own {
            Some(node) => Ok(Backing::Own(Box::new(self.workspace(key, node)?))),
            None => Ok(Backing::Other(self, endpoint)),
        }
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let key = (call.execution_id.clone(), call.node.id.clone());
        let mut moved = None;
        match call.node.node_type.as_str() {
            WORKSPACE => {
                let settings = Settings::read(&self.manifest, &call.node)
                    .map_err(ModuleError::InvalidParams)?;
                let start: NodeStart =
                    serde_json::from_value(call.args.clone()).unwrap_or_default();
                moved = start
                    .checkpoint
                    .as_ref()
                    .and_then(|saved| saved.get(ROOT_PORT))
                    .and_then(Value::as_str)
                    .map(String::from);
                // A root named in the profile must be there now. One that arrives on
                // `root` is checked when it arrives.
                if moved.is_none() && !settings.root.is_empty() {
                    Root::open(&settings.root, Vec::new()).map_err(|fault| {
                        ModuleError::Failed(format!(
                            "code workspace `{}`: {}",
                            call.node.id,
                            fault.text()
                        ))
                    })?;
                }
                self.runs.start(&key);
            }
            VIEW => {
                View::read(&self.manifest, &call.node).map_err(ModuleError::InvalidParams)?;
            }
            _ => {}
        }
        lock(&self.live).insert(
            key,
            Live {
                node: call.node,
                moved,
            },
        );
        Ok(Value::Null)
    }

    fn stop(&self, call: NodeCall) {
        let key = (call.execution_id, call.node.id);
        lock(&self.live).remove(&key);
        self.runs.stop(&key);
    }

    /// A value on a workspace's `root`: the folder it works in from now on.
    fn event(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        if call.node.node_type != WORKSPACE || event.port != ROOT_PORT {
            return Err(ModuleError::InvalidParams(format!(
                "`{}` takes nothing on `{}`",
                call.node.node_type, event.port
            )));
        }
        let folder = match &event.value {
            Value::String(path) => Some(path.as_str()),
            Value::Object(info) => info.get("path").and_then(Value::as_str),
            _ => None,
        }
        .ok_or_else(|| {
            ModuleError::InvalidParams(format!(
                "code workspace `{}`: `{ROOT_PORT}` takes a folder as text, or an object with `path`",
                call.node.id
            ))
        })?
        .trim()
        .to_string();
        let me = node_ref(&call);
        let moved = (!folder.is_empty()).then_some(folder);
        {
            let mut live = lock(&self.live);
            let node = live
                .get_mut(&(me.execution_id.clone(), me.node_id.clone()))
                .ok_or_else(|| {
                    ModuleError::Failed(format!("code workspace `{}` is not running", me.node_id))
                })?;
            node.moved = moved.clone();
        }
        // Without a store there is no restart to survive.
        let _ = self.ctx.checkpoint(&me, json!({ ROOT_PORT: moved }));
        // Kept even when it is no folder: what comes next fails here, it does not go on
        // working in the folder before.
        if let Some(folder) = &moved {
            Root::open(folder, Vec::new()).map_err(|fault| {
                ModuleError::InvalidParams(format!(
                    "code workspace `{}`: {}",
                    me.node_id,
                    fault.text()
                ))
            })?;
        }
        Ok(json!({ ROOT_PORT: moved }))
    }

    fn tool_list(&self, call: NodeCall) -> Result<Value, ModuleError> {
        match call.node.node_type.as_str() {
            WORKSPACE => to_json(specs(self.workspace_of(call)?.operations())),
            VIEW => {
                let view =
                    View::read(&self.manifest, &call.node).map_err(ModuleError::InvalidParams)?;
                let operations = self
                    .backing(&node_ref(&call))?
                    .operations()
                    .map_err(Fault::into_error)?;
                to_json(specs(
                    operations
                        .into_iter()
                        .filter(|operation| view.allows(operation))
                        .collect(),
                ))
            }
            _ => Err(ModuleError::UnknownMethod(tool::LIST.into())),
        }
    }

    fn tool_call(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let request: tool::Call = parse(std::mem::take(&mut call.args))?;
        match call.node.node_type.as_str() {
            WORKSPACE => to_json(self.workspace_of(call)?.outcome(&request)),
            VIEW => {
                let view =
                    View::read(&self.manifest, &call.node).map_err(ModuleError::InvalidParams)?;
                let backing = self.backing(&node_ref(&call))?;
                let refused = |why: String| {
                    to_json(tool::Outcome {
                        data: json!(why),
                        error: true,
                    })
                };
                let operations = match backing.operations() {
                    Ok(operations) => operations,
                    Err(fault) => return refused(fault.text().to_string()),
                };
                let offered = operations
                    .iter()
                    .any(|operation| operation.name == request.name && view.allows(operation));
                if !offered {
                    return refused(format!("there is no tool `{}` here", request.name));
                }
                match backing.call(&request) {
                    Ok(outcome) => to_json(outcome),
                    Err(fault) => refused(fault.text().to_string()),
                }
            }
            _ => Err(ModuleError::UnknownMethod(tool::CALL.into())),
        }
    }

    /// `code.workspace.describe`.
    fn describe(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let workspace = self.workspace_of(call)?;
        to_json(Description {
            // No root yet is not a failure of the description: its operations will say so.
            root: workspace.root().map(|root| root.text()).unwrap_or_default(),
            operations: workspace.operations(),
        })
    }

    /// `code.inspect`: a look at a live workspace node, from the socket.
    fn inspect(&self, params: Value) -> Result<Value, ModuleError> {
        let params: InspectParams = parse(params)?;
        let key = (params.execution_id, params.node_id);
        let node = lock(&self.live)
            .get(&key)
            .filter(|live| live.node.node_type == WORKSPACE)
            .map(|live| live.node.clone())
            .ok_or_else(|| {
                ModuleError::InvalidParams(format!(
                    "no code workspace `{}` is running in `{}`",
                    key.1, key.0
                ))
            })?;
        self.workspace(key, node)?
            .look(&params.look)
            .map_err(Fault::into_error)
    }
}

impl Code {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("code manifest is valid"),
            ctx: ModuleCtx::default(),
            live: Mutex::default(),
            runs: Arc::default(),
        }
    }
}

impl Default for Code {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Code {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let code = self;
        match name {
            method::NODE_START => code.start(parse(params)?),
            method::NODE_STOP => {
                code.stop(parse(params)?);
                Ok(Value::Null)
            }
            method::NODE_EVENT => code.event(parse(params)?),
            tool::LIST => code.tool_list(parse(params)?),
            tool::CALL => code.tool_call(parse(params)?),
            contract::DESCRIBE => code.describe(parse(params)?),
            contract::CALL => {
                let mut call: NodeCall = parse(params)?;
                let request: tool::Call = parse(std::mem::take(&mut call.args))?;
                to_json(code.workspace_of(call)?.outcome(&request))
            }
            contract::LOOK => {
                let mut call: NodeCall = parse(params)?;
                let look: Look = parse(std::mem::take(&mut call.args))?;
                code.workspace_of(call)?
                    .look(&look)
                    .map_err(Fault::into_error)
            }
            HOOK_DESCRIBE => code.describe_hook(parse(params)?),
            method::NODE_HOOK => code.answer_hook(parse(params)?),
            contract::INSPECT => code.inspect(params),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.runs.stop_all();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use regulus_proto::NodeType;

    fn node_type(kind: &str) -> NodeType {
        Code::new()
            .manifest()
            .node_type(kind)
            .unwrap_or_else(|| panic!("no node type `{kind}`"))
            .clone()
    }

    /// `(id, capability, direction, kind)` of each declared port.
    fn ports(kind: &str) -> Vec<(String, String, Direction, WireKind)> {
        node_type(kind)
            .ports
            .into_iter()
            .map(|p| (p.id, p.capability, p.direction, p.kind))
            .collect()
    }

    #[test]
    fn the_manifest_declares_exactly_the_fields_the_code_reads() {
        for (kind, read) in [
            (WORKSPACE, field::WORKSPACE),
            (VIEW, field::VIEW),
            (INSTRUCTIONS, field::INSTRUCTIONS),
        ] {
            let declared = node_type(kind).fields;
            let mut names: Vec<&str> = declared.iter().map(|f| f.id.as_str()).collect();
            let mut wanted = read.to_vec();
            names.sort_unstable();
            wanted.sort_unstable();
            assert_eq!(names, wanted, "fields of `{kind}`");
            for field in &declared {
                assert!(
                    field.default.is_some(),
                    "`{kind}.{}` has no default",
                    field.id
                );
                if field.field_type == "select" {
                    let default = field.default.as_ref().unwrap();
                    assert!(
                        field.options.contains(default),
                        "`{kind}.{}`: its default is not one of its options",
                        field.id
                    );
                }
            }
        }
    }

    #[test]
    fn the_manifest_declares_the_ports_and_the_method() {
        use Direction::{Input, Output};
        use WireKind::{Then, Uses};
        let port = |id: &str, capability: &str, direction, kind| {
            (id.to_string(), capability.to_string(), direction, kind)
        };
        assert_eq!(
            ports(WORKSPACE),
            [
                port(WORKSPACE_PORT, CAPABILITY, Output, Uses),
                port("tools", tool::CAPABILITY, Output, Uses),
                port(ROOT_PORT, "path@1", Input, Then),
                port(CHANGED_PORT, "json@1", Output, Then),
                port(RAN_PORT, "json@1", Output, Then),
            ]
        );
        assert_eq!(
            ports(VIEW),
            [
                port(WORKSPACE_PORT, CAPABILITY, Input, Uses),
                port("tools", tool::CAPABILITY, Output, Uses),
            ]
        );
        assert_eq!(
            ports(INSTRUCTIONS),
            [
                port(WORKSPACE_PORT, CAPABILITY, Input, Uses),
                port("hook", regulus_proto::HOOK_CAPABILITY, Output, Uses),
            ]
        );
        for kind in [VIEW, INSTRUCTIONS] {
            let input = &node_type(kind).ports[0];
            assert!(input.required, "`{kind}` is nothing without a workspace");
        }
        assert_eq!(Code::new().manifest().module.methods, [INSPECT]);
    }

    #[test]
    fn the_default_tools_are_the_whole_catalog_in_the_contract_order() {
        let code = Code::new();
        let node: Node = serde_json::from_value(json!({ "id": "w", "type": WORKSPACE })).unwrap();
        let settings = Settings::read(code.manifest(), &node).unwrap();
        assert_eq!(settings.tools, tool_name::ALL);
        let catalog: Vec<String> = tools::catalog(&settings)
            .into_iter()
            .map(|operation| operation.name)
            .collect();
        assert_eq!(catalog, tool_name::ALL);
    }

    #[test]
    fn settings_refuse_what_they_cannot_mean() {
        let code = Code::new();
        let read = |config: Value| {
            let node: Node =
                serde_json::from_value(json!({ "id": "w", "type": WORKSPACE, "config": config }))
                    .unwrap();
            Settings::read(code.manifest(), &node).map_err(|e| e.to_string())
        };
        assert!(read(json!({ "tools": ["code_fly"] }))
            .unwrap_err()
            .contains("`code_fly`"));
        assert!(read(json!({ "shell": [] }))
            .unwrap_err()
            .contains("`shell`"));
        assert!(read(json!({ "git_dir": "nowhere" }))
            .unwrap_err()
            .contains("`git_dir`"));
        assert!(read(json!({ "git": " " })).unwrap_err().contains("`git`"));
        // A template fills a list with one text.
        let narrowed = read(json!({ "tools": "code_read_file, code_search" })).unwrap();
        assert_eq!(narrowed.tools, ["code_read_file", "code_search"]);
    }

    #[test]
    fn a_view_lets_through_what_its_access_covers() {
        let code = Code::new();
        let view = |config: Value| {
            let node: Node =
                serde_json::from_value(json!({ "id": "v", "type": VIEW, "config": config }))
                    .unwrap();
            View::read(code.manifest(), &node)
        };
        let operation = |name: &str, access| Operation {
            name: name.into(),
            description: String::new(),
            input: json!({}),
            access,
        };
        let (read, write, run) = (
            operation("r", Access::Read),
            operation("w", Access::Write),
            operation("x", Access::Execute),
        );
        let allowed = |view: &View| [&read, &write, &run].map(|operation| view.allows(operation));
        assert_eq!(allowed(&view(json!({})).unwrap()), [true, false, false]);
        assert_eq!(
            allowed(&view(json!({ "access": "write" })).unwrap()),
            [true, true, false]
        );
        assert_eq!(
            allowed(&view(json!({ "access": "all" })).unwrap()),
            [true, true, true]
        );
        assert_eq!(
            allowed(&view(json!({ "access": "all", "tools": ["w"] })).unwrap()),
            [false, true, false]
        );
        assert!(view(json!({ "access": "root" })).is_err());
    }
}
