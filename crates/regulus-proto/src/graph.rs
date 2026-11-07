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
    /// An input the node only reads while running, never when it starts: its wires do not
    /// order the start, so it can close a cycle (a channel whose participants use it).
    #[serde(default, skip_serializing_if = "is_false")]
    pub lazy: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
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

    /// A workflow placed as a node: no module runs it, its inside is the workflow it
    /// points at and its ports are that workflow's ports. A module node with an internal
    /// workflow is not one: its module runs it.
    pub fn is_box(&self) -> bool {
        self.module_id.is_none() && self.internal_workflow_id.is_some()
    }
}

/// The node id an inner wire uses for the ports of its own workflow: `self.ask` is the
/// input `ask` of the workflow the wire is in. Inside, a workflow input feeds nodes like
/// an output would, and a workflow output takes values like an input would.
pub const WORKFLOW_SELF: &str = "self";

/// Joins the id of a box node and the id of a node inside it in a running copy of a
/// workflow: `greet/hi` is node `hi` of the workflow placed as node `greet`.
pub const BOX_SEPARATOR: char = '/';

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
    /// The workflow's own inputs and outputs, as seen from outside: what it takes when it
    /// is placed as a node in another workflow or spoken to (`workflow.send`), and what it
    /// gives back. Inner wires reach them as [`WORKFLOW_SELF`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ports: Vec<Port>,
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

    pub fn port(&self, id: &str) -> Option<&Port> {
        self.ports.iter().find(|port| port.id == id)
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

    /// The ports of a node: its own, or, for a box that declares none, those of the
    /// workflow inside it.
    pub fn ports_of<'a>(&'a self, node: &'a Node) -> &'a [Port] {
        if !node.ports.is_empty() || !node.is_box() {
            return &node.ports;
        }
        node.internal_workflow_id
            .as_deref()
            .and_then(|id| self.workflow(id))
            .map_or(&node.ports, |workflow| &workflow.ports)
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
    /// The profile the workflow comes from: workflow ids are only unique inside one.
    #[serde(default)]
    pub profile_id: String,
    pub workflow_id: String,
    pub project_id: Option<String>,
    pub state: ExecutionState,
    /// Nodes left off because a required input has nothing usable wired into it. The
    /// rest of the execution runs without them (docs/03-modules.md §1.1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled: Vec<DisabledNode>,
    /// The execution that started this one as a child (`ModuleCtx::call_workflow`): it
    /// stops with it and its result goes back to the node that called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DisabledNode {
    pub node_id: String,
    /// The required input ports with nothing usable behind them.
    pub missing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum ExecutionState {
    Starting,
    Running,
    Stopped,
    Failed {
        reason: String,
    },
    /// It was running when the core went down and was not resumed on the next start
    /// (`[store] resume`).
    Interrupted {
        reason: String,
    },
}

impl ExecutionState {
    /// Starting or running: its nodes are, or are about to be, at work.
    pub fn is_live(&self) -> bool {
        matches!(self, Self::Starting | Self::Running)
    }
}
