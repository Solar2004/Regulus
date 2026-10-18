//! The `tool@1` capability (docs/11-composition.md §3.0): operations a module offers for
//! agents to call. It says *what* can be done, never how it reaches an agent: no MCP,
//! no wire format. Presenting tools to an agent is the tools box's job.
//!
//! The format belongs to the capability, not to any module. Adding an optional field
//! keeps `@1`; changing a shape means `tool@2`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CAPABILITY: &str = "tool@1";

/// The operations the node offers: `NodeCall` → `Vec<`[`Spec`]`>`.
pub const LIST: &str = "tool.list";
/// Runs one operation: `NodeCall` with [`Call`] args → [`Outcome`].
pub const CALL: &str = "tool.call";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    /// Unique among the operations of one node.
    pub name: String,
    /// What it does, for the agent to decide when to use it.
    pub description: String,
    /// JSON Schema of the arguments.
    pub input: Value,
}

/// Who is calling. Set by the box that presents the tools, never by the agent itself,
/// so an agent cannot speak in someone else's name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Caller {
    /// Its name in channels, `@name`.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub name: String,
    #[serde(default)]
    pub args: Value,
    pub caller: Caller,
}

/// What an operation gave back, as data: how it is written for the agent (JSON, TOON…)
/// is decided where the tools are presented.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Outcome {
    pub data: Value,
    /// The operation ran but failed; `data` says why.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub error: bool,
}
