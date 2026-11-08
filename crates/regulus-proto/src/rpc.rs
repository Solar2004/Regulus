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
/// `template.list`, `template.get` and `template.apply`.
pub const PROTOCOL: &str = "1.10";

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
    /// The profile the template would write already exists and `force` was not given, or
    /// another file defines a profile with that id.
    pub const PROFILE_EXISTS: i32 = -32007;
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
/// execution one of its nodes started (`ModuleCtx::call_workflow`). A child returns at
/// most once, then ends; one that fails or is stopped from outside only ends.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChildEvent {
    /// The child execution.
    pub execution_id: String,
    pub workflow_id: String,
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
    /// The child is no longer live: `stopped` after it returned or was stopped,
    /// `failed` if a node of it failed to start.
    Ended { state: crate::ExecutionState },
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
/// boolean is taken as its JSON text.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TemplateApplyParams {
    pub template: String,
    #[serde(default)]
    pub vars: serde_json::Map<String, serde_json::Value>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub force: bool,
}

/// The answer of `template.apply`: the profile now loaded, where it was written, and what
/// the check found short of an error (warnings and information).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateApplied {
    pub profile_id: String,
    pub path: String,
    pub diagnostics: Vec<Diagnostic>,
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
}
