//! The design-time model: what pieces exist and how they are wired.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Input,
    Output,
}

/// `Uses` is a permanent service link; `Then` is a subscription on the event bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WireKind {
    Uses,
    Then,
}

/// How many wires an input accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Cardinality {
    #[default]
    One,
    Many,
}

impl Cardinality {
    fn is_one(&self) -> bool {
        *self == Cardinality::One
    }
}

/// An input or output of a node. `capability` is a versioned port type such as
/// `llm.chat@1`; its payload format is fixed by the first module that uses it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Port {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub capability: String,
    pub direction: Direction,
    pub kind: WireKind,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Cardinality::is_one")]
    pub cardinality: Cardinality,
}

#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Position {
    pub x: f64,
    pub y: f64,
}

/// One piece on a canvas. Its shape (ports and config) is data declared by a module or a
/// resource, never a type known to the core.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    #[serde(rename = "type")]
    pub node_type: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub module_id: Option<String>,
    #[serde(default)]
    pub resource: Option<String>,
    #[serde(default)]
    pub ports: Vec<Port>,
    /// Secrets are only stored as references (`secret:<name>`), never as values.
    #[serde(default)]
    pub config: Map<String, Value>,
    /// Client layout data: the core stores it but never interprets it.
    #[serde(default)]
    pub position: Position,
    #[serde(default)]
    pub internal_workflow_id: Option<String>,
    #[serde(default)]
    pub exposed_parameters: Vec<String>,
}

impl Node {
    pub fn port(&self, id: &str) -> Option<&Port> {
        self.ports.iter().find(|port| port.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PortRef {
    pub node_id: String,
    pub port_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Wire {
    pub id: String,
    pub kind: WireKind,
    pub from: PortRef,
    pub to: PortRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WorkflowKind {
    Main,
    Extra,
    Module,
}

/// Client layout data, like [`Position`].
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Layout {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub kind: WorkflowKind,
    #[serde(default)]
    pub module_id: Option<String>,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub wires: Vec<Wire>,
    #[serde(default)]
    pub layout: Layout,
}

impl Workflow {
    pub fn node(&self, id: &str) -> Option<&Node> {
        self.nodes.iter().find(|node| node.id == id)
    }
}

/// Every workflow and setting a project works with. `project_id = None` is the global
/// profile.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub project_id: Option<String>,
    /// Reserved: profile inheritance is outside v1.
    #[serde(default)]
    pub extends: Option<String>,
    pub main_workflow_id: String,
    #[serde(default)]
    pub workflows: Vec<Workflow>,
}

impl Profile {
    pub fn summary(&self) -> ProfileSummary {
        ProfileSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            project_id: self.project_id.clone(),
        }
    }

    pub fn workflow(&self, id: &str) -> Option<&Workflow> {
        self.workflows.iter().find(|workflow| workflow.id == id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileSummary {
    pub id: String,
    pub name: String,
    pub project_id: Option<String>,
}

/// A live copy of a workflow. Several can run from the same workflow at once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Execution {
    pub id: String,
    pub workflow_id: String,
    pub project_id: Option<String>,
    pub state: ExecutionState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ExecutionState {
    Starting,
    Running,
    Stopped,
    Failed { reason: String },
}
