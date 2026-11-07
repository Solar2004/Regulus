//! The event bus as clients and modules see it: what travels over "then" wires and what
//! the core reports about it (docs/02-architecture.md §6).

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::graph::PortRef;

/// One thing that happened on the bus of an execution. `seq` grows by one per event in a
/// core and is the cursor of `event.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BusEvent {
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub at: u64,
    pub execution_id: String,
    pub project_id: Option<String>,
    /// The `emitted` event this one is about; its own `seq` for `emitted` and `cut`.
    pub event: u64,
    /// The output port the value left from.
    pub from: PortRef,
    /// The capability of that port, e.g. `text@1`: the type of the value.
    pub capability: String,
    /// How many deliveries led to this emission: 0 when a node emits on its own, one
    /// more when it emits while handling an event.
    pub depth: u32,
    #[serde(flatten)]
    pub kind: BusEventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum BusEventKind {
    /// A node put a value on one of its "then" outputs.
    Emitted { value: Value },
    /// The node behind `to` took the value.
    Delivered { to: PortRef },
    /// The value did not reach `to`: a capability that does not fit, a node that is not
    /// running, or a module that failed or does not take events.
    Failed { to: PortRef, error: String },
    /// The chain went deeper than `max_depth` and the core dropped the value (a loop
    /// such as A → B → A).
    Cut { max_depth: u32 },
}

/// What a module receives with `node/event`, inside `NodeCall.args`: a value that came
/// over a "then" wire into `port` of its node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeEvent {
    /// The input port it arrives on.
    pub port: String,
    pub capability: String,
    pub value: Value,
    pub from: PortRef,
    /// The `seq` of the `emitted` event.
    pub event: u64,
}

/// `event.list`: events after `since` (0 = from the oldest kept), optionally of one
/// execution or project, at most `limit` of them.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventListParams {
    #[serde(default)]
    pub since: u64,
    #[serde(default)]
    pub execution_id: Option<String>,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

/// The answer of `event.list`. Pass `next` as `since` to follow. `pending` counts the
/// deliveries still queued for the executions asked about: 0 means the bus is idle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventPage {
    pub events: Vec<BusEvent>,
    pub next: u64,
    pub pending: usize,
}
