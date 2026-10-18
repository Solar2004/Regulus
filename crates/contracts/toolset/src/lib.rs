//! How tools reach an agent (docs/11-composition.md §3.0). Two capabilities share one
//! shape, the [`Mount`]:
//!
//! - `toolset@1`: the tools box offers it to the Agent box, which asks for a mount in
//!   its own name and hands it to its engine.
//! - `tools.engine@1`: a protocol module (MCP today) offers it to the tools box, which
//!   asks it to mount a binding. Swapping the protocol is wiring another engine.
//!
//! The format belongs to the capabilities, not to any module. Adding an optional field
//! keeps `@1`; changing a shape means `@2`.

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "toolset@1";
pub const ENGINE_CAPABILITY: &str = "tools.engine@1";

/// A mount for one caller: `NodeCall` with [`MountArgs`] → [`Mount`].
pub const MOUNT: &str = "toolset.mount";
/// A mount for one binding: `NodeCall` with [`EngineMountArgs`] → [`Mount`].
pub const ENGINE_MOUNT: &str = "tools.engine.mount";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MountArgs {
    /// The caller's name, `@name`: every call through this mount is made in it.
    pub caller: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineMountArgs {
    /// What the agent sees the server as.
    pub name: String,
    /// The secret that ties calls back to one tools node and one caller.
    pub binding: String,
}

/// Something an agent's engine starts to get tools. `protocol` says how to talk to it
/// (`mcp-stdio`); an engine that does not speak it leaves the mount out with a warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Mount {
    pub protocol: String,
    pub name: String,
    pub command: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}
