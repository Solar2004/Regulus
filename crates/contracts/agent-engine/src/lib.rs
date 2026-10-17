//! The `agent.engine@1` capability (docs/12-agents.md §3): the methods the Agent box
//! calls on whatever engine is wired into it, and their arguments.
//!
//! The format belongs to the capability, not to any module, so the box and every engine
//! depend on this crate and on nothing of each other. Changing a shape here means a new
//! capability version (`agent.engine@2`), not a silent break.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "agent.engine@1";

/// Installations of the engine's CLI found on this machine → [`Detected`].
pub const DETECT: &str = "engine.detect";
/// Launches the CLI and opens a session: [`StartArgs`] → [`Started`].
pub const SESSION_START: &str = "engine.session.start";
/// One turn: [`PromptArgs`] → [`Turn`].
pub const SESSION_PROMPT: &str = "engine.session.prompt";
/// Ends a session and its process: [`StopArgs`] → `null`.
pub const SESSION_STOP: &str = "engine.session.stop";

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
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub session_id: String,
    pub agent: Agent,
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
