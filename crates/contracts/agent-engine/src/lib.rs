//! The `agent.engine@1` capability (docs/12-agents.md §3): the methods the Agent box
//! calls on whatever engine is wired into it, and their arguments.
//!
//! The format belongs to the capability, not to any module, so the box and every engine
//! depend on this crate and on nothing of each other. Changing a shape here means a new
//! capability version (`agent.engine@2`), not a silent break.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use regulus_contract_llm_endpoint::LlmEndpoint;
pub use regulus_contract_skill::Skill;
pub use regulus_contract_toolset::Mount;
pub use regulus_proto::Hook;
use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "agent.engine@1";

/// Installations of the engine's CLI found on this machine → [`Detected`].
pub const DETECT: &str = "engine.detect";
/// Launches the CLI and opens a session: [`StartArgs`] → [`Started`].
pub const SESSION_START: &str = "engine.session.start";
/// One turn: [`PromptArgs`] → [`Turn`].
pub const SESSION_PROMPT: &str = "engine.session.prompt";
/// Where a session stands: [`StatusArgs`] → [`Status`]. Optional: an engine without it
/// answers `UnknownMethod` and the box reports the state as unknown.
pub const SESSION_STATUS: &str = "engine.session.status";
/// Ends a session and its process: [`StopArgs`] → `null`.
pub const SESSION_STOP: &str = "engine.session.stop";
/// Asks the turn in flight to end (ACP `session/cancel`): [`CancelArgs`] → `null`. The
/// turn still answers, usually with stop reason `cancelled`. Optional: an engine without
/// it answers `UnknownMethod` and the box drops the session instead.
pub const SESSION_CANCEL: &str = "engine.session.cancel";

/// The Agent box's provider slots besides the main one (docs/12-agents.md §6.5): each
/// takes a provider for one purpose. A slot an engine does not know is left out with a
/// warning, never a failure.
pub mod slot {
    /// Background work: titles, summaries, quick checks.
    pub const FAST: &str = "fast";
    /// The subagents the CLI spawns on its own.
    pub const SUBAGENT: &str = "subagent";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installation {
    pub path: PathBuf,
    /// What `--version` printed, if it answered.
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detected {
    pub installations: Vec<Installation>,
}

/// What the session answers when the agent asks for permission.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permissions {
    Allow,
    #[default]
    Reject,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartArgs {
    pub cwd: PathBuf,
    #[serde(default)]
    pub permissions: Permissions,
    /// The model endpoint wired into the box, if any (docs/12-agents.md §6). Optional, so
    /// it extends `@1` without breaking engines that ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<LlmEndpoint>,
    /// Providers for other purposes, keyed by [`slot`]. Optional, so it extends `@1`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slots: BTreeMap<String, LlmEndpoint>,
    /// The skills wired into the box, for the engine to place in the profile. Optional,
    /// so it extends `@1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<Skill>,
    /// Tool servers to start with the session, each in the protocol it says. One the
    /// engine does not speak is left out with a warning. Optional, so it extends `@1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Mount>,
    /// The hooks wired into the box (docs/13-blocks.md §4), for the engine to write into
    /// its CLI's hook settings. One the engine cannot place is left out with a warning.
    /// Optional, so it extends `@1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookMount>,
}

/// One hook for the engine's CLI: when to run the bridge and how. The engine appends
/// [`HOOK_FORMAT_FLAG`] and the name of its own hook protocol to `args`, so the bridge
/// reads the CLI's payload and answers in kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookMount {
    /// The hook node, for messages.
    pub node: String,
    #[serde(flatten)]
    pub hook: Hook,
    pub command: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// The bridge's flag naming the hook protocol of the CLI that runs it.
pub const HOOK_FORMAT_FLAG: &str = "--format";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub session_id: String,
    pub agent: Agent,
    /// What was wired in but left out, and why. Optional, so it extends `@1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptArgs {
    pub session_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub stop_reason: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// Waiting for a prompt.
    Idle,
    /// In a turn.
    Busy,
    /// The session or its process is gone.
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub state: SessionState,
}
