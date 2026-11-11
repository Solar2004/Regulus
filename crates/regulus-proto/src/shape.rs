//! The shape of a node: the ports a node of a manifest type has for its config. One
//! resolver, here, used by the core (`module.shape`, `profile.check`, `profile.sync`); the
//! CLI and the app ask the core for it. A second resolver in a client would drift.
//!
//! A node type's `ports` are always there. Its `dynamic_ports` depend on the config: one is
//! there only `when` a field says so, and one that `repeat`s a `list` field becomes one port
//! per item, with `{item}` in its id and name replaced (protocol 1.17).

use std::collections::HashSet;
use std::ops::Deref;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::graph::{Port, Profile, Wire, Workflow};
use crate::manifest::{DynamicPort, Manifest, NodeType, When, FIELD_LIST};

/// What a repeating port's `id` and `name` hold for the item.
pub const ITEM: &str = "{item}";
/// The longest item of a repeated field: it ends up in a port id.
pub const MAX_ITEM_LEN: usize = 64;

/// The ports a node has for its config, the fields its config hides, and what in the config
/// could not be used (an item that is not a valid id, a value of the wrong type).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeShape {
    pub ports: Vec<Port>,
    #[serde(default)]
    pub hidden_fields: Vec<String>,
    #[serde(default)]
    pub problems: Vec<String>,
}

/// A field's value for a node: its config, else the field's default, else `null`.
fn value<'a>(node_type: &'a NodeType, config: &'a Map<String, Value>, field: &str) -> &'a Value {
    static NULL: Value = Value::Null;
    config
        .get(field)
        .or_else(|| node_type.field(field)?.default.as_ref())
        .unwrap_or(&NULL)
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Whether `when` holds for a node with this config.
pub fn holds(when: &When, node_type: &NodeType, config: &Map<String, Value>) -> bool {
    let value = value(node_type, config, &when.field);
    match (&when.equals, &when.one_of) {
        (Some(expected), _) => value == expected,
        (None, Some(list)) => list.contains(value),
        (None, None) => truthy(value),
    }
}

/// A port as a profile keeps it: without a name, it is named after its id (what the core
/// does to every profile it loads or saves).
fn named(mut port: Port) -> Port {
    if port.name.is_empty() {
        port.name = port.id.clone();
    }
    port
}

/// Whether `item` can stand in a port id.
pub fn item_ok(item: &str) -> bool {
    (1..=MAX_ITEM_LEN).contains(&item.len())
        && item
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

/// The ports, in order, that a node of `node_type` with `config` has: the fixed ones, then
/// the dynamic ones that apply, a repeating one expanded in the order of its items. A port
/// without a name is named after its id, as in a saved profile.
pub fn shape(node_type: &NodeType, config: &Map<String, Value>) -> NodeShape {
    let mut shape = NodeShape::default();
    let mut taken: HashSet<String> = HashSet::new();
    for port in &node_type.ports {
        taken.insert(port.id.clone());
        shape.ports.push(named(port.clone()));
    }
    for dynamic in &node_type.dynamic_ports {
        if dynamic
            .when
            .as_ref()
            .is_some_and(|w| !holds(w, node_type, config))
        {
            continue;
        }
        let concrete = match &dynamic.repeat {
            None => vec![dynamic.port.clone()],
            Some(field) => repeat(dynamic, field, node_type, config, &mut shape.problems),
        };
        for port in concrete {
            if taken.insert(port.id.clone()) {
                shape.ports.push(named(port));
            } else {
                shape.problems.push(format!(
                    "port `{}` is given twice for this config; the first one stays",
                    port.id
                ));
            }
        }
    }
    for field in &node_type.fields {
        if field
            .when
            .as_ref()
            .is_some_and(|w| !holds(w, node_type, config))
        {
            shape.hidden_fields.push(field.id.clone());
        }
    }
    shape
}

fn repeat(
    dynamic: &DynamicPort,
    field: &str,
    node_type: &NodeType,
    config: &Map<String, Value>,
    problems: &mut Vec<String>,
) -> Vec<Port> {
    let items = match value(node_type, config, field) {
        Value::Null => return Vec::new(),
        Value::Array(items) => items,
        other => {
            problems.push(format!("`{field}` must be a list of text, not {other}"));
            return Vec::new();
        }
    };
    let mut seen = HashSet::new();
    let mut ports = Vec::new();
    for item in items {
        let Some(text) = item.as_str() else {
            problems.push(format!("`{field}`: {item} is not text; skipped"));
            continue;
        };
        if !item_ok(text) {
            problems.push(format!(
                "`{field}`: `{text}` must be 1 to {MAX_ITEM_LEN} characters of A-Z, a-z, 0-9, \
                 `_` or `-`; skipped"
            ));
            continue;
        }
        if !seen.insert(text) {
            problems.push(format!("`{field}`: `{text}` is listed twice; skipped"));
            continue;
        }
        let mut port = dynamic.port.clone();
        port.id = port.id.replace(ITEM, text);
        port.name = if port.name.is_empty() {
            text.to_string()
        } else {
            port.name.replace(ITEM, text)
        };
        ports.push(port);
    }
    ports
}

/// What is wrong in how a node type declares its fields and ports, one sentence each: the
/// same messages for a manifest the core loads and for a source the core reads statically.
pub fn validate(node_type: &NodeType) -> Vec<String> {
    let owner = format!("node type `{}`", node_type.node_type);
    let mut problems = Vec::new();
    let mut fields = HashSet::new();
    for field in &node_type.fields {
        if !fields.insert(field.id.as_str()) {
            problems.push(format!("{owner}: field `{}` is declared twice", field.id));
        }
    }
    let mut ports = HashSet::new();
    for port in &node_type.ports {
        if !ports.insert(port.id.as_str()) {
            problems.push(format!("{owner}: port `{}` is declared twice", port.id));
        }
        if port.id.contains(ITEM) {
            problems.push(format!(
                "{owner}: port `{}` has `{ITEM}` in its id; only a dynamic port that repeats \
                 a list field may",
                port.id
            ));
        }
    }
    let check_when = |what: &str, when: &When, problems: &mut Vec<String>| {
        if node_type.field(&when.field).is_none() {
            problems.push(format!(
                "{owner}: {what}: `when` names field `{}`, which the node type does not declare",
                when.field
            ));
        }
        if when.equals.is_some() && when.one_of.is_some() {
            problems.push(format!(
                "{owner}: {what}: `when` has both `equals` and `in`; give one"
            ));
        }
    };
    for field in &node_type.fields {
        if let Some(when) = &field.when {
            check_when(&format!("field `{}`", field.id), when, &mut problems);
        }
    }
    for dynamic in &node_type.dynamic_ports {
        let id = &dynamic.port.id;
        let what = format!("dynamic port `{id}`");
        if let Some(when) = &dynamic.when {
            check_when(&what, when, &mut problems);
        }
        match &dynamic.repeat {
            Some(field) => {
                match node_type.field(field) {
                    None => problems.push(format!(
                        "{owner}: {what}: `repeat` names field `{field}`, which the node type \
                         does not declare"
                    )),
                    Some(f) if f.field_type != FIELD_LIST => problems.push(format!(
                        "{owner}: {what}: `repeat` needs a `{FIELD_LIST}` field; `{field}` is \
                         `{}`",
                        f.field_type
                    )),
                    Some(_) => {}
                }
                if !id.contains(ITEM) {
                    problems.push(format!(
                        "{owner}: {what} repeats `{field}` but its id has no `{ITEM}`, so every \
                         port would have the same id"
                    ));
                }
            }
            None => {
                if id.contains(ITEM) {
                    problems.push(format!(
                        "{owner}: {what} has `{ITEM}` in its id but repeats nothing; add \
                         `repeat`"
                    ));
                }
                if ports.contains(id.as_str()) {
                    problems.push(format!(
                        "{owner}: {what} has the id of a fixed port; a port that depends on \
                         the config must not also be fixed"
                    ));
                }
            }
        }
    }
    problems
}

/// [`validate`] for every node type of a manifest, plus a node type declared twice.
pub fn validate_manifest(manifest: &Manifest) -> Vec<String> {
    let mut problems = Vec::new();
    let mut types = HashSet::new();
    for node_type in &manifest.nodes {
        if !types.insert(node_type.node_type.as_str()) {
            problems.push(format!(
                "node type `{}` is declared twice",
                node_type.node_type
            ));
        }
        problems.extend(validate(node_type));
    }
    problems
}

/// A port whose id stayed and whose definition did not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortChange {
    pub before: Port,
    pub after: Port,
}

impl PortChange {
    /// Whether wires on it can stay: only its name, `required`, `cardinality` or `lazy`
    /// changed, not where values go, how, or of what type.
    pub fn keeps_wires(&self) -> bool {
        self.before.direction == self.after.direction
            && self.before.kind == self.after.kind
            && self.before.capability == self.after.capability
    }

    /// What changed, e.g. `capability text@1 → json@1, required`.
    pub fn describe(&self) -> String {
        let (a, b) = (&self.before, &self.after);
        let mut parts = Vec::new();
        if a.capability != b.capability {
            parts.push(format!("capability {} → {}", a.capability, b.capability));
        }
        let named = [
            ("direction", a.direction != b.direction),
            ("kind", a.kind != b.kind),
            ("required", a.required != b.required),
            ("cardinality", a.cardinality != b.cardinality),
            ("lazy", a.lazy != b.lazy),
            ("name", named(a.clone()).name != named(b.clone()).name),
        ];
        parts.extend(
            named
                .into_iter()
                .filter(|(_, differs)| *differs)
                .map(|(what, _)| what.to_string()),
        );
        parts.join(", ")
    }
}

/// How a node's ports differ from the ones it should have, by port id.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PortChanges {
    pub added: Vec<Port>,
    pub removed: Vec<Port>,
    pub changed: Vec<PortChange>,
}

impl PortChanges {
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty()
    }

    /// One line for a person: `missing `out2`; extra `old`; changed `in` (capability …)`.
    pub fn describe(&self) -> String {
        let ids = |ports: &[Port]| {
            ports
                .iter()
                .map(|p| format!("`{}`", p.id))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let mut parts = Vec::new();
        if !self.added.is_empty() {
            parts.push(format!("missing {}", ids(&self.added)));
        }
        if !self.removed.is_empty() {
            parts.push(format!("extra {}", ids(&self.removed)));
        }
        for change in &self.changed {
            parts.push(format!(
                "changed `{}` ({})",
                change.before.id,
                change.describe()
            ));
        }
        parts.join("; ")
    }
}

/// `current` against `wanted`, by id; order does not count, nor a name left empty where
/// the other side names the port after its id.
pub fn diff_ports(current: &[Port], wanted: &[Port]) -> PortChanges {
    let mut changes = PortChanges::default();
    for want in wanted {
        match current.iter().find(|p| p.id == want.id) {
            None => changes.added.push(want.clone()),
            Some(have) if named(have.clone()) != named(want.clone()) => {
                changes.changed.push(PortChange {
                    before: have.clone(),
                    after: want.clone(),
                })
            }
            Some(_) => {}
        }
    }
    for have in current {
        if !wanted.iter().any(|p| p.id == have.id) {
            changes.removed.push(have.clone());
        }
    }
    changes
}

/// A wire a sync takes out, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RemovedWire {
    pub wire: Wire,
    pub reason: String,
}

/// What syncing one node changes, or would change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeSync {
    pub workflow_id: String,
    pub node_id: String,
    #[serde(default)]
    pub module_id: Option<String>,
    pub node_type: String,
    #[serde(default)]
    pub added: Vec<Port>,
    #[serde(default)]
    pub removed: Vec<Port>,
    #[serde(default)]
    pub changed: Vec<PortChange>,
    #[serde(default)]
    pub wires_removed: Vec<RemovedWire>,
    /// Why the node was left as it is, or what of its config the shape could not use.
    #[serde(default)]
    pub problems: Vec<String>,
}

/// Gives node `node_id` of `workflow` the ports `wanted` and takes out the wires that would
/// no longer fit: those on a port that goes, or on one whose direction, kind or capability
/// changes. Other wires stay. `None` if there is no such node.
pub fn sync_node(workflow: &mut Workflow, node_id: &str, wanted: Vec<Port>) -> Option<NodeSync> {
    let node = workflow.nodes.iter_mut().find(|n| n.id == node_id)?;
    let changes = diff_ports(&node.ports, &wanted);
    let (module_id, node_type) = (node.module_id.clone(), node.node_type.clone());
    node.ports = wanted;
    let why = |port: &str| -> Option<String> {
        if changes.removed.iter().any(|p| p.id == port) {
            return Some(format!("port `{port}` of `{node_id}` is gone"));
        }
        changes
            .changed
            .iter()
            .find(|c| c.before.id == port && !c.keeps_wires())
            .map(|c| format!("port `{port}` of `{node_id}` changed: {}", c.describe()))
    };
    let mut wires_removed = Vec::new();
    workflow.wires.retain(|wire| {
        let ends = [&wire.from, &wire.to];
        let reason = ends
            .into_iter()
            .filter(|end| end.node_id == node_id)
            .find_map(|end| why(&end.port_id));
        match reason {
            Some(reason) => {
                wires_removed.push(RemovedWire {
                    wire: wire.clone(),
                    reason,
                });
                false
            }
            None => true,
        }
    });
    Some(NodeSync {
        workflow_id: workflow.id.clone(),
        node_id: node_id.to_string(),
        module_id,
        node_type,
        added: changes.added,
        removed: changes.removed,
        changed: changes.changed,
        wires_removed,
        problems: Vec::new(),
    })
}

/// A profile after a sync: every node changed, and whether any wire went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfileSynced {
    pub profile: Profile,
    pub drops_wires: bool,
    pub nodes: Vec<NodeSync>,
    /// Nodes asked for that the profile does not have.
    #[serde(default)]
    pub problems: Vec<String>,
}

/// Syncs the ports of a profile's nodes with their shape. With `node_ids`, those nodes
/// (managed or not, in `workflow_id` or any workflow); without, every node of a
/// `managed_ports` type whose ports differ. A node whose module is not loaded or has no such
/// type is listed with why and left as it is. Nodes already in shape are not listed.
pub fn sync_profile<M: Deref<Target = Manifest>>(
    mut profile: Profile,
    manifest_of: impl Fn(&str) -> Option<M>,
    workflow_id: Option<&str>,
    node_ids: Option<&[String]>,
) -> ProfileSynced {
    let mut nodes = Vec::new();
    let mut found = HashSet::new();
    for workflow in profile
        .workflows
        .iter_mut()
        .filter(|w| workflow_id.is_none_or(|id| w.id == id))
    {
        let mut plans = Vec::new();
        for node in &workflow.nodes {
            let asked = node_ids.is_some_and(|ids| ids.contains(&node.id));
            if node_ids.is_some() && !asked {
                continue;
            }
            if asked {
                found.insert(node.id.clone());
            }
            let left = |why: String| NodeSync {
                workflow_id: workflow.id.clone(),
                node_id: node.id.clone(),
                module_id: node.module_id.clone(),
                node_type: node.node_type.clone(),
                added: Vec::new(),
                removed: Vec::new(),
                changed: Vec::new(),
                wires_removed: Vec::new(),
                problems: vec![why],
            };
            let Some(module_id) = node.module_id.as_deref() else {
                if asked {
                    nodes.push(left(format!("`{}` is not a module's node", node.id)));
                }
                continue;
            };
            let Some(manifest) = manifest_of(module_id) else {
                if asked {
                    nodes.push(left(format!("module `{module_id}` is not loaded")));
                }
                continue;
            };
            let Some(node_type) = manifest.node_type(&node.node_type) else {
                if asked {
                    nodes.push(left(format!(
                        "module `{module_id}` has no node type `{}`",
                        node.node_type
                    )));
                }
                continue;
            };
            if !asked && !node_type.managed_ports {
                continue;
            }
            let shaped = shape(node_type, &node.config);
            if diff_ports(&node.ports, &shaped.ports).is_empty() {
                continue;
            }
            plans.push((node.id.clone(), shaped));
        }
        for (node_id, shaped) in plans {
            if let Some(mut synced) = sync_node(workflow, &node_id, shaped.ports) {
                synced.problems = shaped.problems;
                nodes.push(synced);
            }
        }
    }
    let problems = node_ids
        .unwrap_or_default()
        .iter()
        .filter(|id| !found.contains(*id))
        .map(|id| match workflow_id {
            Some(workflow) => format!("workflow `{workflow}` has no node `{id}`"),
            None => format!("the profile has no node `{id}`"),
        })
        .collect();
    ProfileSynced {
        drops_wires: nodes.iter().any(|n| !n.wires_removed.is_empty()),
        profile,
        nodes,
        problems,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Direction, Node, PortRef, WireKind, WorkflowKind};
    use serde_json::json;

    fn node_type(value: Value) -> NodeType {
        serde_json::from_value(value).unwrap()
    }

    fn switch() -> NodeType {
        node_type(json!({
            "type": "switch", "name": "Switch", "managed_ports": true,
            "port": [{ "id": "in", "capability": "text@1", "direction": "input", "kind": "then" }],
            "dynamic_port": [
                { "id": "case_{item}", "name": "{item}", "capability": "text@1",
                  "direction": "output", "kind": "then", "repeat": "cases" },
                { "id": "other", "capability": "text@1", "direction": "output", "kind": "then",
                  "when": { "field": "fallback" } },
                { "id": "out", "capability": "json@1", "direction": "output", "kind": "then",
                  "when": { "field": "format", "equals": "json" } },
                { "id": "out", "capability": "text@1", "direction": "output", "kind": "then",
                  "when": { "field": "format", "in": ["text", "plain"] } },
            ],
            "field": [
                { "id": "cases", "type": "list", "default": ["a", "b"] },
                { "id": "fallback", "type": "boolean", "default": false },
                { "id": "format", "type": "select", "default": "text", "options": ["json", "text", "plain"] },
                { "id": "pattern", "type": "text", "when": { "field": "format", "equals": "text" } },
            ],
        }))
    }

    fn ids(shape: &NodeShape) -> Vec<&str> {
        shape.ports.iter().map(|p| p.id.as_str()).collect()
    }

    fn config(value: Value) -> Map<String, Value> {
        value.as_object().unwrap().clone()
    }

    #[test]
    fn defaults_give_fixed_ports_then_dynamic_ones_in_order() {
        let shaped = shape(&switch(), &Map::new());
        assert_eq!(ids(&shaped), ["in", "case_a", "case_b", "out"]);
        assert_eq!(shaped.ports[1].name, "a");
        assert_eq!(
            shaped.ports[0].name, "in",
            "named after its id, as profiles keep it"
        );
        assert_eq!(shaped.ports[3].capability, "text@1");
        assert!(shaped.problems.is_empty(), "{:?}", shaped.problems);
        assert!(shaped.hidden_fields.is_empty());
        assert!(validate(&switch()).is_empty(), "{:?}", validate(&switch()));
    }

    #[test]
    fn config_drives_repeat_when_and_hidden_fields() {
        let shaped = shape(
            &switch(),
            &config(
                json!({ "cases": ["x", "bad id", "x", 3], "fallback": true, "format": "json" }),
            ),
        );
        assert_eq!(ids(&shaped), ["in", "case_x", "other", "out"]);
        assert_eq!(shaped.ports[3].capability, "json@1");
        assert_eq!(shaped.hidden_fields, ["pattern"]);
        assert_eq!(shaped.problems.len(), 3, "{:?}", shaped.problems);
        let wrong = shape(&switch(), &config(json!({ "cases": "a" })));
        assert!(
            wrong.problems[0].contains("must be a list of text"),
            "{wrong:?}"
        );
    }

    #[test]
    fn declarations_are_validated_with_one_message_each() {
        let bad = node_type(json!({
            "type": "t", "name": "T",
            "port": [
                { "id": "a", "capability": "x@1", "direction": "input", "kind": "then" },
                { "id": "a", "capability": "x@1", "direction": "input", "kind": "then" },
                { "id": "p_{item}", "capability": "x@1", "direction": "input", "kind": "then" },
            ],
            "dynamic_port": [
                { "id": "r", "capability": "x@1", "direction": "output", "kind": "then", "repeat": "name" },
                { "id": "s_{item}", "capability": "x@1", "direction": "output", "kind": "then", "repeat": "nope" },
                { "id": "w_{item}", "capability": "x@1", "direction": "output", "kind": "then",
                  "when": { "field": "ghost", "equals": 1, "in": [1] } },
                { "id": "a", "capability": "x@1", "direction": "output", "kind": "then", "when": { "field": "name" } },
            ],
            "field": [{ "id": "name", "type": "text" }, { "id": "name", "type": "text" }],
        }));
        let problems = validate(&bad);
        for expected in [
            "field `name` is declared twice",
            "port `a` is declared twice",
            "port `p_{item}` has `{item}`",
            "`repeat` needs a `list` field; `name` is `text`",
            "repeats `name` but its id has no `{item}`",
            "`repeat` names field `nope`",
            "`when` names field `ghost`",
            "both `equals` and `in`",
            "has `{item}` in its id but repeats nothing",
            "has the id of a fixed port",
        ] {
            assert!(
                problems.iter().any(|p| p.contains(expected)),
                "{expected:?} in {problems:#?}"
            );
        }
        assert_eq!(problems.len(), 10, "{problems:#?}");
    }

    fn port(id: &str, capability: &str, direction: Direction) -> Port {
        Port {
            id: id.into(),
            name: String::new(),
            capability: capability.into(),
            direction,
            kind: WireKind::Then,
            required: false,
            cardinality: Default::default(),
            lazy: false,
        }
    }

    fn wire(id: &str, from: (&str, &str), to: (&str, &str)) -> Wire {
        let end = |(node_id, port_id): (&str, &str)| PortRef {
            node_id: node_id.into(),
            port_id: port_id.into(),
        };
        Wire {
            id: id.into(),
            kind: WireKind::Then,
            from: end(from),
            to: end(to),
        }
    }

    fn workflow(ports: Vec<Port>) -> Workflow {
        Workflow {
            id: "main".into(),
            name: String::new(),
            kind: WorkflowKind::Main,
            module_id: None,
            ports: Vec::new(),
            nodes: vec![
                Node {
                    id: "s".into(),
                    node_type: "switch".into(),
                    name: String::new(),
                    module_id: Some("p/sw".into()),
                    resource: None,
                    ports,
                    config: Map::new(),
                    position: Default::default(),
                    internal_workflow_id: None,
                    exposed_parameters: Vec::new(),
                },
                Node {
                    id: "sink".into(),
                    node_type: "sink".into(),
                    name: String::new(),
                    module_id: None,
                    resource: None,
                    ports: vec![
                        port("in", "text@1", Direction::Input),
                        port("out", "text@1", Direction::Output),
                    ],
                    config: Map::new(),
                    position: Default::default(),
                    internal_workflow_id: None,
                    exposed_parameters: Vec::new(),
                },
            ],
            wires: vec![
                wire("w-gone", ("s", "case_old"), ("sink", "in")),
                wire("w-kept", ("s", "case_a"), ("sink", "in")),
                wire("w-retyped", ("sink", "out"), ("s", "in")),
            ],
            layout: Default::default(),
        }
    }

    #[test]
    fn sync_drops_only_wires_that_no_longer_fit_and_says_why() {
        let current = vec![
            port("in", "json@1", Direction::Input),
            port("case_a", "text@1", Direction::Output),
            port("case_old", "text@1", Direction::Output),
        ];
        let mut flow = workflow(current);
        let wanted = shape(&switch(), &Map::new()).ports;
        // An empty name and one equal to the id are the same port.
        let mut named_in = port("x", "text@1", Direction::Input);
        named_in.name = "x".into();
        assert!(diff_ports(&[port("x", "text@1", Direction::Input)], &[named_in]).is_empty());
        let synced = sync_node(&mut flow, "s", wanted.clone()).unwrap();
        assert_eq!(flow.nodes[0].ports, wanted);
        let added: Vec<_> = synced.added.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(added, ["case_b", "out"]);
        assert_eq!(synced.removed[0].id, "case_old");
        assert_eq!(synced.changed[0].describe(), "capability json@1 → text@1");
        let gone: Vec<_> = synced
            .wires_removed
            .iter()
            .map(|w| w.wire.id.as_str())
            .collect();
        assert_eq!(gone, ["w-gone", "w-retyped"]);
        assert!(synced.wires_removed[1].reason.contains("json@1 → text@1"));
        let left: Vec<_> = flow.wires.iter().map(|w| w.id.as_str()).collect();
        assert_eq!(left, ["w-kept"]);
    }

    #[test]
    fn a_name_or_cardinality_change_keeps_wires() {
        let mut renamed = port("case_a", "text@1", Direction::Output);
        renamed.name = "old name".into();
        let change = PortChange {
            before: renamed,
            after: port("case_a", "text@1", Direction::Output),
        };
        assert!(change.keeps_wires());
        assert_eq!(change.describe(), "name");
    }

    fn profile(ports: Vec<Port>) -> Profile {
        Profile {
            id: "p".into(),
            name: String::new(),
            project_id: None,
            extends: None,
            main_workflow_id: "main".into(),
            workflows: vec![workflow(ports)],
        }
    }

    fn manifest(managed: bool) -> Manifest {
        let mut node = switch();
        node.managed_ports = managed;
        serde_json::from_value(json!({
            "module": { "id": "p/sw@0.1.0", "name": "sw" },
            "node": [node],
        }))
        .unwrap()
    }

    #[test]
    fn profile_sync_takes_managed_stale_nodes_or_the_ones_asked() {
        let stale = vec![
            port("in", "text@1", Direction::Input),
            port("case_old", "text@1", Direction::Output),
        ];
        let managed = manifest(true);
        let synced = sync_profile(profile(stale.clone()), |_| Some(&managed), None, None);
        assert_eq!(synced.nodes.len(), 1);
        assert!(synced.drops_wires, "case_old has a wire");
        // In shape already: nothing to list.
        let again = sync_profile(synced.profile, |_| Some(&managed), None, None);
        assert!(again.nodes.is_empty());

        // Not managed: only when asked for by id.
        let free = manifest(false);
        let untouched = sync_profile(profile(stale.clone()), |_| Some(&free), None, None);
        assert!(untouched.nodes.is_empty());
        let asked = ["s".to_string(), "sink".to_string(), "ghost".to_string()];
        let reset = sync_profile(profile(stale), |_| Some(&free), Some("main"), Some(&asked));
        assert_eq!(reset.nodes.len(), 2, "{:#?}", reset.nodes);
        assert!(reset
            .nodes
            .iter()
            .any(|n| n.node_id == "sink" && n.problems[0].contains("not a module's node")));
        assert_eq!(reset.problems, ["workflow `main` has no node `ghost`"]);

        let none: Option<&Manifest> = None;
        let missing = sync_profile(profile(Vec::new()), |_| none, None, Some(&asked[..1]));
        assert!(missing.nodes[0].problems[0].contains("not loaded"));
    }

    #[test]
    fn dynamic_ports_round_trip_and_old_manifests_serialize_as_before() {
        let value = serde_json::to_value(switch()).unwrap();
        assert_eq!(value["dynamic_port"][0]["repeat"], "cases");
        assert_eq!(
            value["dynamic_port"][1]["when"],
            json!({ "field": "fallback" })
        );
        assert_eq!(
            value["dynamic_port"][3]["when"]["in"],
            json!(["text", "plain"])
        );
        assert_eq!(value["field"][3]["when"]["equals"], "text");
        let back: NodeType = serde_json::from_value(value).unwrap();
        assert_eq!(back, switch());
        let plain = node_type(json!({ "type": "t", "name": "T", "port": [], "field": [] }));
        let text = serde_json::to_value(&plain).unwrap();
        assert!(text.get("dynamic_port").is_none() && text.get("managed_ports").is_none());
    }
}
