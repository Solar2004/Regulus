//! What a module declares about itself (`regulus-module.toml`) and how the registry
//! reports it.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::graph::Port;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub module: ModuleInfo,
    #[serde(default, rename = "node")]
    pub nodes: Vec<NodeType>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModuleInfo {
    /// `author/module@version`.
    pub id: String,
    pub name: String,
    /// Protocol requirement checked before loading, e.g. `^1` or `^1.1`.
    #[serde(default)]
    pub requires_proto: Option<String>,
    /// RPC methods this module serves. Same names on both sides of the core.
    #[serde(default)]
    pub methods: Vec<String>,
    #[serde(default)]
    pub workflow_id: Option<String>,
}

/// A kind of node a module brings: its ports and the fields the UI draws by itself.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeType {
    #[serde(rename = "type")]
    pub node_type: String,
    pub name: String,
    #[serde(default, rename = "port")]
    pub ports: Vec<Port>,
    #[serde(default, rename = "field")]
    pub fields: Vec<Field>,
}

/// An option of a node type. `field_type` is open (`text`, `number`, `select`…): a client
/// that does not know a type falls back to showing the raw value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub id: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub default: Option<Value>,
}

impl NodeType {
    pub fn field(&self, id: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.id == id)
    }
}

/// Real module state, not just "installed" (docs/03-modules.md §3).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ModuleState {
    /// Loaded and used. `since` is the first use, in milliseconds since the Unix epoch.
    Active { since: u64 },
    /// Loaded, never used (`last_used = None`) or not used for a while.
    Idle { last_used: Option<u64> },
    /// A required input has no wire.
    Disabled { missing: Vec<String> },
    /// Panicked or failed in `init`.
    Broken { reason: String },
    /// `requires_proto` does not match the core protocol.
    Incompatible { requires: String, have: String },
}

impl ModuleState {
    pub fn is_usable(&self) -> bool {
        matches!(self, ModuleState::Active { .. } | ModuleState::Idle { .. })
    }
}

/// One entry of `module.list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleSummary {
    pub id: String,
    pub name: String,
    pub node_types: Vec<String>,
    pub workflow_id: Option<String>,
    pub state: ModuleState,
}
