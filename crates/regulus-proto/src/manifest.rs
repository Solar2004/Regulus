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
    /// Where the module comes from if it derives from someone else's code
    /// (docs/plan.md §4.1). The UI shows it on the module's card.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub author: String,
    pub repo: String,
    pub license: String,
    /// Who the module honors, e.g. the original author.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub honors: Option<String>,
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

/// A field type the core checks: the id of a workflow of the same profile with at least one
/// "then" output, which the node runs as a child (`ModuleCtx::call_workflow`).
pub const FIELD_WORKFLOW: &str = "workflow";

/// An option of a node type. `field_type` is open (`text`, `number`, `select`, `code`…): a
/// client that does not know a type falls back to showing the raw value. The core only
/// checks [`FIELD_WORKFLOW`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub id: String,
    #[serde(rename = "type")]
    pub field_type: String,
    #[serde(default)]
    pub default: Option<Value>,
    /// Allowed values, for `select`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub options: Vec<Value>,
    /// For a [`FIELD_WORKFLOW`] field: the node's "then" ports are its own (a batch, a
    /// tool), not a mirror of the workflow's, so the core does not line them up.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub own_ports: bool,
    /// Every other key of the field, kept as written and handed to clients unchanged:
    /// presentation hints such as `multiline`, `language`, `label` or `description`. The
    /// core never reads them; a client that does not know a key ignores it. Adding one is
    /// an optional change and does not bump the protocol (docs/03-modules.md §1).
    #[serde(flatten, default)]
    pub extra: serde_json::Map<String, Value>,
}

impl NodeType {
    pub fn field(&self, id: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.id == id)
    }
}

impl Manifest {
    pub fn node_type(&self, node_type: &str) -> Option<&NodeType> {
        self.nodes.iter().find(|node| node.node_type == node_type)
    }

    /// A node's setting: its own value if the profile sets one, otherwise the default
    /// the manifest declares for that field.
    pub fn setting<'a>(&'a self, node: &'a crate::Node, field: &str) -> Option<&'a Value> {
        node.config.get(field).or_else(|| {
            self.node_type(&node.node_type)?
                .field(field)?
                .default
                .as_ref()
        })
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
    /// Not started on purpose, e.g. a local module of a project that is not trusted (1.14).
    Blocked { reason: String },
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
    /// For a local module (`[[modules.local]]`): the id of the project it belongs to, so
    /// a client can offer to trust that project when the module is `blocked` (1.15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn node(fields: Value) -> NodeType {
        serde_json::from_value(json!({ "type": "t", "name": "T", "field": fields })).unwrap()
    }

    #[test]
    fn field_keeps_keys_it_does_not_know() {
        let source = json!([{
            "id": "source", "type": "code", "default": "return value",
            "language": "python", "multiline": true, "label": "Source",
            "description": "Runs per event", "future": { "rows": 12 },
        }]);
        let node = node(source.clone());
        let field = node.field("source").unwrap();
        assert_eq!(field.field_type, "code");
        assert_eq!(field.default, Some(json!("return value")));
        assert_eq!(field.extra.get("language"), Some(&json!("python")));
        assert_eq!(field.extra.get("multiline"), Some(&json!(true)));
        assert_eq!(field.extra.get("future"), Some(&json!({ "rows": 12 })));
        assert!(!field.extra.contains_key("id") && !field.extra.contains_key("type"));
        // What clients receive is what the module wrote, flat, not nested under `extra`.
        assert_eq!(serde_json::to_value(&node).unwrap()["field"], source);
    }

    #[test]
    fn field_without_extra_keys_serializes_as_before() {
        let node = node(json!([{ "id": "path", "type": "text", "default": "out.md" }]));
        assert!(node.fields[0].extra.is_empty());
        assert_eq!(
            serde_json::to_value(&node).unwrap()["field"],
            json!([{ "id": "path", "type": "text", "default": "out.md" }])
        );
    }

    #[test]
    fn known_keys_are_not_duplicated_into_extra() {
        let node = node(json!([{
            "id": "flow", "type": "workflow", "own_ports": true, "options": ["a"], "hint": 1,
        }]));
        let field = &node.fields[0];
        assert!(field.own_ports);
        assert_eq!(field.options, vec![json!("a")]);
        assert_eq!(field.extra.keys().collect::<Vec<_>>(), vec!["hint"]);
    }
}
