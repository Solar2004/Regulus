//! The profile the Edit view changes, as data. Every change is a function of the profile
//! and its arguments, with no I/O, so it is tested without a terminal. The same operations
//! and wire rules as the app's editor (`frontend/src/composition/editor/document.ts` and
//! `connect.ts`); `profile.check` (`regulus_core::graph::check_profile`) has the last word.

use std::collections::HashSet;

use regulus_proto::{
    Cardinality, Direction, Node, Port, PortRef, Position, Profile, Wire, WireKind, Workflow,
    WorkflowKind, WORKFLOW_SELF,
};
use serde_json::Value;

/// Only "then" values cross a workflow's boundary: the core refuses a "uses" workflow port.
pub const WORKFLOW_PORT_KIND: WireKind = WireKind::Then;
/// The capability a new workflow port starts with.
pub const DEFAULT_PORT_CAPABILITY: &str = "text@1";

/// A module id without its version: `regulus/hello@0.1.0` → `regulus/hello`.
pub fn base_module_id(id: &str) -> &str {
    id.split('@').next().unwrap_or(id)
}

/// A name turned into an id the core accepts as a file or node name.
pub fn slugify(text: &str) -> String {
    let mut out = String::new();
    let mut gap = false;
    for c in text.trim().to_lowercase().chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
            if gap && !out.is_empty() {
                out.push('-');
            }
            gap = false;
            out.push(c);
        } else {
            gap = true;
        }
    }
    let out = out.trim_start_matches(['-', '.']);
    out.trim_end_matches('-').to_string()
}

/// `base`, or `base-2`, `base-3`… whichever is free; never `self` or empty.
pub fn unique_id<'a>(
    base: &str,
    taken: impl IntoIterator<Item = &'a str>,
    fallback: &str,
) -> String {
    let used: HashSet<&str> = taken.into_iter().collect();
    let root = match slugify(base) {
        root if root.is_empty() => fallback.to_string(),
        root => root,
    };
    let free = |id: &str| !used.contains(id) && id != WORKFLOW_SELF;
    if free(&root) {
        return root;
    }
    (2..)
        .map(|n| format!("{root}-{n}"))
        .find(|id| free(id))
        .expect("some suffix is free")
}

/// Why an id cannot name a new profile.
pub fn profile_id_problem(id: &str, taken: &[String]) -> Option<&'static str> {
    if id.is_empty() {
        Some("the id is empty")
    } else if slugify(id) != id {
        Some("an id is lowercase letters, digits, `.`, `_` and `-`")
    } else if taken.iter().any(|t| t == id) {
        Some("a profile with that id exists")
    } else {
        None
    }
}

fn empty_workflow(id: &str, name: &str, kind: WorkflowKind) -> Workflow {
    Workflow {
        id: id.into(),
        name: name.into(),
        kind,
        module_id: None,
        ports: Vec::new(),
        nodes: Vec::new(),
        wires: Vec::new(),
        layout: Default::default(),
    }
}

/// A new profile: one empty main workflow.
pub fn new_profile(id: &str, name: &str, main_name: &str) -> Profile {
    let main = empty_workflow("main", main_name, WorkflowKind::Main);
    Profile {
        id: id.into(),
        name: name.into(),
        project_id: None,
        extends: None,
        main_workflow_id: main.id.clone(),
        workflows: vec![main],
    }
}

pub fn workflow_mut<'a>(profile: &'a mut Profile, id: &str) -> Option<&'a mut Workflow> {
    profile.workflows.iter_mut().find(|w| w.id == id)
}

pub fn node_mut<'a>(profile: &'a mut Profile, workflow: &str, node: &str) -> Option<&'a mut Node> {
    workflow_mut(profile, workflow)?
        .nodes
        .iter_mut()
        .find(|n| n.id == node)
}

/// Adds `node` unless one with its id is there. Returns whether it was added.
pub fn add_node(profile: &mut Profile, workflow: &str, node: Node) -> bool {
    let Some(workflow) = workflow_mut(profile, workflow) else {
        return false;
    };
    if workflow.node(&node.id).is_some() {
        return false;
    }
    workflow.nodes.push(node);
    true
}

/// Removes a node and every wire that touches it.
pub fn remove_node(profile: &mut Profile, workflow: &str, node: &str) {
    if let Some(workflow) = workflow_mut(profile, workflow) {
        workflow.nodes.retain(|n| n.id != node);
        workflow
            .wires
            .retain(|w| w.from.node_id != node && w.to.node_id != node);
    }
}

pub fn remove_wire(profile: &mut Profile, workflow: &str, wire: &str) {
    if let Some(workflow) = workflow_mut(profile, workflow) {
        workflow.wires.retain(|w| w.id != wire);
    }
}

/// Adds a wire with a fresh id and returns it. Whether it may exist is [`can_connect`]'s
/// question.
pub fn add_wire(
    profile: &mut Profile,
    workflow: &str,
    kind: WireKind,
    from: PortRef,
    to: PortRef,
) -> Option<String> {
    let workflow = workflow_mut(profile, workflow)?;
    let base = format!(
        "{}-{}-{}-{}",
        from.node_id, from.port_id, to.node_id, to.port_id
    );
    let id = unique_id(&base, workflow.wires.iter().map(|w| w.id.as_str()), "wire");
    workflow.wires.push(Wire {
        id: id.clone(),
        kind,
        from,
        to,
    });
    Some(id)
}

pub fn rename_node(profile: &mut Profile, workflow: &str, node: &str, name: &str) {
    if let Some(node) = node_mut(profile, workflow, node) {
        node.name = name.into();
    }
}

/// Sets one config value; `None` removes the key, so the module's default applies.
pub fn set_config(
    profile: &mut Profile,
    workflow: &str,
    node: &str,
    key: &str,
    value: Option<Value>,
) {
    if let Some(node) = node_mut(profile, workflow, node) {
        match value {
            Some(value) => {
                node.config.insert(key.into(), value);
            }
            None => {
                node.config.remove(key);
            }
        }
    }
}

pub fn set_internal_workflow(
    profile: &mut Profile,
    workflow: &str,
    node: &str,
    internal: Option<&str>,
) {
    if let Some(node) = node_mut(profile, workflow, node) {
        node.internal_workflow_id = internal.filter(|id| !id.is_empty()).map(Into::into);
    }
}

/// A new extra workflow; returns its id.
pub fn add_workflow(profile: &mut Profile, name: &str) -> String {
    let id = unique_id(
        name,
        profile.workflows.iter().map(|w| w.id.as_str()),
        "workflow",
    );
    profile
        .workflows
        .push(empty_workflow(&id, name, WorkflowKind::Extra));
    id
}

pub fn rename_workflow(profile: &mut Profile, id: &str, name: &str) {
    if let Some(workflow) = workflow_mut(profile, id) {
        workflow.name = name.into();
    }
}

fn places(node: &Node, workflow: &str) -> bool {
    node.is_box() && node.internal_workflow_id.as_deref() == Some(workflow)
}

/// Removes a workflow other than the main one. The boxes that placed it go too, with
/// their wires; a module node that named it as its inner workflow only stops naming it.
pub fn remove_workflow(profile: &mut Profile, id: &str) -> bool {
    if id == profile.main_workflow_id || profile.workflow(id).is_none() {
        return false;
    }
    profile.workflows.retain(|w| w.id != id);
    for workflow in &mut profile.workflows {
        let boxes: Vec<String> = workflow
            .nodes
            .iter()
            .filter(|n| places(n, id))
            .map(|n| n.id.clone())
            .collect();
        workflow.nodes.retain(|n| !boxes.contains(&n.id));
        workflow
            .wires
            .retain(|w| !boxes.contains(&w.from.node_id) && !boxes.contains(&w.to.node_id));
        for node in &mut workflow.nodes {
            if node.internal_workflow_id.as_deref() == Some(id) {
                node.internal_workflow_id = None;
            }
        }
    }
    true
}

/// Adds a port of the workflow itself (always "then") and returns its id.
pub fn add_workflow_port(
    profile: &mut Profile,
    workflow: &str,
    id: &str,
    direction: Direction,
    capability: &str,
) -> Option<String> {
    let workflow = workflow_mut(profile, workflow)?;
    let fallback = match direction {
        Direction::Input => "in",
        Direction::Output => "out",
    };
    let id = unique_id(
        if id.is_empty() { fallback } else { id },
        workflow.ports.iter().map(|p| p.id.as_str()),
        "port",
    );
    workflow.ports.push(Port {
        id: id.clone(),
        name: id.clone(),
        capability: if capability.is_empty() {
            DEFAULT_PORT_CAPABILITY.into()
        } else {
            capability.into()
        },
        direction,
        kind: WORKFLOW_PORT_KIND,
        required: false,
        cardinality: Cardinality::One,
        lazy: false,
    });
    Some(id)
}

/// Removes a port of the workflow and every wire that reaches it: its own `self` wires
/// and those to the boxes that place it elsewhere.
pub fn remove_workflow_port(profile: &mut Profile, workflow: &str, port: &str) {
    let Some(owner) = workflow_mut(profile, workflow) else {
        return;
    };
    if !owner.ports.iter().any(|p| p.id == port) {
        return;
    }
    owner.ports.retain(|p| p.id != port);
    for each in &mut profile.workflows {
        let inside = each.id == workflow;
        let boxes: HashSet<String> = each
            .nodes
            .iter()
            .filter(|n| n.ports.is_empty() && places(n, workflow))
            .map(|n| n.id.clone())
            .collect();
        let reaches = |end: &PortRef| {
            end.port_id == port
                && if inside {
                    end.node_id == WORKFLOW_SELF
                } else {
                    boxes.contains(&end.node_id)
                }
        };
        each.wires.retain(|w| !reaches(&w.from) && !reaches(&w.to));
    }
}

/// One end of a wire as seen from inside the workflow: a `self` input feeds nodes, so
/// inside it is an output.
pub fn endpoint(profile: &Profile, workflow: &str, end: &PortRef) -> Option<(Port, Direction)> {
    let workflow = profile.workflow(workflow)?;
    if end.node_id == WORKFLOW_SELF {
        let port = workflow.port(&end.port_id)?;
        let inside = match port.direction {
            Direction::Input => Direction::Output,
            Direction::Output => Direction::Input,
        };
        return Some((port.clone(), inside));
    }
    let node = workflow.node(&end.node_id)?;
    let port = profile
        .ports_of(node)
        .iter()
        .find(|p| p.id == end.port_id)?;
    Some((port.clone(), port.direction))
}

/// Why a wire may not join two ports: the reasons the app gives (`connect.ts`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    SameNode,
    MissingPort,
    Direction,
    Kind,
    Capability { from: String, to: String },
    Taken,
    Duplicate,
}

impl Refusal {
    pub fn reason(&self) -> String {
        match self {
            Self::SameNode => "a node cannot wire to itself".into(),
            Self::MissingPort => "one of those ports does not exist".into(),
            Self::Direction => "a wire goes from an output to an input".into(),
            Self::Kind => "\"uses\" joins \"uses\" and \"then\" joins \"then\"".into(),
            Self::Capability { from, to } => format!("the capabilities differ: {from} → {to}"),
            Self::Taken => "the input takes one wire and has it".into(),
            Self::Duplicate => "that wire is already there".into(),
        }
    }
}

/// May a wire go from `from` (an output) to `to` (an input) in this workflow? The kind
/// of the wire it would be, or why not.
pub fn can_connect(
    profile: &Profile,
    workflow: &str,
    from: &PortRef,
    to: &PortRef,
) -> Result<WireKind, Refusal> {
    if from.node_id == to.node_id && from.node_id != WORKFLOW_SELF {
        return Err(Refusal::SameNode);
    }
    let (Some((source, source_dir)), Some((target, target_dir))) = (
        endpoint(profile, workflow, from),
        endpoint(profile, workflow, to),
    ) else {
        return Err(Refusal::MissingPort);
    };
    if source_dir != Direction::Output || target_dir != Direction::Input {
        return Err(Refusal::Direction);
    }
    if source.kind != target.kind {
        return Err(Refusal::Kind);
    }
    if source.capability != target.capability {
        return Err(Refusal::Capability {
            from: source.capability,
            to: target.capability,
        });
    }
    let wires: Vec<&Wire> = profile
        .workflow(workflow)
        .map(|w| w.wires.iter().filter(|wire| wire.to == *to).collect())
        .unwrap_or_default();
    if wires.iter().any(|wire| wire.from == *from) {
        return Err(Refusal::Duplicate);
    }
    if !wires.is_empty() && target.cardinality != Cardinality::Many {
        return Err(Refusal::Taken);
    }
    Ok(source.kind)
}

/// Every end inside the workflow that sends (`Output`) or takes (`Input`) a value, in
/// order: the workflow's own ports first, then each node's.
pub fn ends(profile: &Profile, workflow: &str, direction: Direction) -> Vec<(PortRef, Port)> {
    let Some(flow) = profile.workflow(workflow) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for port in &flow.ports {
        let end = PortRef {
            node_id: WORKFLOW_SELF.into(),
            port_id: port.id.clone(),
        };
        if endpoint(profile, workflow, &end).is_some_and(|(_, d)| d == direction) {
            out.push((end, port.clone()));
        }
    }
    for node in &flow.nodes {
        for port in profile.ports_of(node) {
            if port.direction == direction {
                let end = PortRef {
                    node_id: node.id.clone(),
                    port_id: port.id.clone(),
                };
                out.push((end, port.clone()));
            }
        }
    }
    out
}

/// `point` moved right by `size.x` until a node placed there with footprint `size`
/// touches none of the workflow's (the app's `freePosition`).
pub fn free_position(workflow: &Workflow, point: Position, size: Position) -> Position {
    let mut x = point.x;
    let y = point.y;
    let touches = |x: f64, node: &Node| {
        x < node.position.x + size.x
            && node.position.x < x + size.x
            && y < node.position.y + size.y
            && node.position.y < y + size.y
    };
    for _ in 0..100 {
        if !workflow.nodes.iter().any(|n| touches(x, n)) {
            break;
        }
        x += size.x;
    }
    Position { x, y }
}

/// Where the next node goes: one `step` down and right of the last one placed, moved on
/// until it overlaps nothing the app would draw.
pub fn cascade_position(workflow: &Workflow, step: f64, width: f64, height: f64) -> Position {
    let n = workflow.nodes.len() as f64;
    let start = Position {
        x: n * step,
        y: n * step,
    };
    free_position(
        workflow,
        start,
        Position {
            x: width + step,
            y: height + step,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use regulus_core::graph::check_profile;
    use regulus_proto::Severity;

    fn port(id: &str, capability: &str, direction: Direction, kind: WireKind) -> Port {
        Port {
            id: id.into(),
            name: id.into(),
            capability: capability.into(),
            direction,
            kind,
            required: false,
            cardinality: Cardinality::One,
            lazy: false,
        }
    }

    fn node(id: &str, ports: Vec<Port>) -> Node {
        Node {
            id: id.into(),
            node_type: id.into(),
            name: id.into(),
            module_id: Some("test/mod@0.1.0".into()),
            resource: None,
            ports,
            config: Default::default(),
            position: Position::default(),
            internal_workflow_id: None,
            exposed_parameters: Vec::new(),
        }
    }

    fn end(node: &str, port: &str) -> PortRef {
        PortRef {
            node_id: node.into(),
            port_id: port.into(),
        }
    }

    fn sample() -> Profile {
        let mut profile = new_profile("demo", "Demo", "Main");
        add_node(
            &mut profile,
            "main",
            node(
                "say",
                vec![port("out", "text@1", Direction::Output, WireKind::Then)],
            ),
        );
        add_node(
            &mut profile,
            "main",
            node(
                "write",
                vec![
                    port("in", "text@1", Direction::Input, WireKind::Then),
                    port("path", "path@1", Direction::Input, WireKind::Then),
                    port("llm", "llm.chat@1", Direction::Input, WireKind::Uses),
                ],
            ),
        );
        profile
    }

    fn errors(profile: &Profile) -> Vec<String> {
        check_profile(profile, |_| true)
            .into_iter()
            .filter(|d| d.severity == Severity::Error)
            .map(|d| d.code)
            .collect()
    }

    #[test]
    fn ids_are_slugs_and_free() {
        assert_eq!(slugify("  Hello World! "), "hello-world");
        assert_eq!(slugify("-.a__b-"), "a__b");
        assert_eq!(unique_id("Say", ["say", "say-2"], "node"), "say-3");
        assert_eq!(unique_id("self", [], "node"), "self-2");
        assert_eq!(unique_id("!!", [], "node"), "node");
        assert_eq!(
            profile_id_problem("demo", &["demo".into()]),
            Some("a profile with that id exists")
        );
        assert!(profile_id_problem("Demo", &[]).is_some());
        assert_eq!(profile_id_problem("demo-2", &[]), None);
    }

    #[test]
    fn a_new_profile_passes_the_core_check() {
        assert!(errors(&new_profile("x", "X", "Main")).is_empty());
    }

    #[test]
    fn wires_follow_the_rules_of_the_core() {
        let mut profile = sample();
        assert_eq!(
            can_connect(&profile, "main", &end("say", "out"), &end("write", "in")),
            Ok(WireKind::Then)
        );
        assert_eq!(
            can_connect(&profile, "main", &end("say", "out"), &end("say", "out")),
            Err(Refusal::SameNode)
        );
        assert_eq!(
            can_connect(&profile, "main", &end("say", "nope"), &end("write", "in")),
            Err(Refusal::MissingPort)
        );
        assert_eq!(
            can_connect(&profile, "main", &end("write", "in"), &end("say", "out")),
            Err(Refusal::Direction)
        );
        assert_eq!(
            can_connect(&profile, "main", &end("say", "out"), &end("write", "llm")),
            Err(Refusal::Kind)
        );
        assert!(matches!(
            can_connect(&profile, "main", &end("say", "out"), &end("write", "path")),
            Err(Refusal::Capability { .. })
        ));
        add_wire(
            &mut profile,
            "main",
            WireKind::Then,
            end("say", "out"),
            end("write", "in"),
        );
        assert!(errors(&profile).is_empty(), "{:?}", errors(&profile));
        assert_eq!(
            can_connect(&profile, "main", &end("say", "out"), &end("write", "in")),
            Err(Refusal::Duplicate)
        );
        add_node(
            &mut profile,
            "main",
            node(
                "other",
                vec![port("out", "text@1", Direction::Output, WireKind::Then)],
            ),
        );
        assert_eq!(
            can_connect(&profile, "main", &end("other", "out"), &end("write", "in")),
            Err(Refusal::Taken)
        );
    }

    #[test]
    fn what_can_connect_is_what_the_core_accepts() {
        // Every pair the rules allow passes the core's check, and every pair they refuse
        // (when it is a real pair of ports) fails it.
        let base = sample();
        let outputs = ends(&base, "main", Direction::Output);
        let inputs = ends(&base, "main", Direction::Input);
        for (from, _) in &outputs {
            for (to, _) in &inputs {
                let mut profile = base.clone();
                let verdict = can_connect(&profile, "main", from, to);
                let kind = verdict.clone().unwrap_or(WireKind::Then);
                add_wire(&mut profile, "main", kind, from.clone(), to.clone());
                assert_eq!(
                    verdict.is_ok(),
                    errors(&profile).is_empty(),
                    "{from:?} → {to:?}: {verdict:?} vs {:?}",
                    errors(&profile)
                );
            }
        }
    }

    #[test]
    fn removing_a_node_takes_its_wires() {
        let mut profile = sample();
        add_wire(
            &mut profile,
            "main",
            WireKind::Then,
            end("say", "out"),
            end("write", "in"),
        );
        remove_node(&mut profile, "main", "say");
        let main = profile.workflow("main").unwrap();
        assert_eq!(main.nodes.len(), 1);
        assert!(main.wires.is_empty());
        assert!(errors(&profile).is_empty());
    }

    #[test]
    fn workflow_ports_are_self_inside_and_go_with_their_wires() {
        let mut profile = sample();
        let ask = add_workflow_port(&mut profile, "main", "ask", Direction::Input, "").unwrap();
        assert_eq!(ask, "ask");
        let outputs = ends(&profile, "main", Direction::Output);
        assert!(outputs.iter().any(|(e, _)| e.node_id == WORKFLOW_SELF));
        let to = end("write", "in");
        let from = end(WORKFLOW_SELF, "ask");
        assert_eq!(
            can_connect(&profile, "main", &from, &to),
            Ok(WireKind::Then)
        );
        add_wire(&mut profile, "main", WireKind::Then, from, to);
        assert!(errors(&profile).is_empty(), "{:?}", errors(&profile));
        remove_workflow_port(&mut profile, "main", "ask");
        let main = profile.workflow("main").unwrap();
        assert!(main.ports.is_empty() && main.wires.is_empty());
    }

    #[test]
    fn extra_workflows_come_and_go_with_their_boxes() {
        let mut profile = sample();
        let id = add_workflow(&mut profile, "Helper");
        assert_eq!(id, "helper");
        add_workflow_port(&mut profile, "helper", "in", Direction::Input, "text@1");
        let mut placed = node("box", Vec::new());
        placed.module_id = None;
        placed.internal_workflow_id = Some(id.clone());
        add_node(&mut profile, "main", placed);
        add_wire(
            &mut profile,
            "main",
            WireKind::Then,
            end("say", "out"),
            end("box", "in"),
        );
        assert!(errors(&profile).is_empty(), "{:?}", errors(&profile));
        assert!(!remove_workflow(&mut profile, "main"));
        assert!(remove_workflow(&mut profile, "helper"));
        let main = profile.workflow("main").unwrap();
        assert!(main.node("box").is_none() && main.wires.is_empty());
        assert!(errors(&profile).is_empty());
    }

    #[test]
    fn config_sets_and_resets() {
        let mut profile = sample();
        set_config(&mut profile, "main", "say", "who", Some("Ana".into()));
        assert_eq!(
            profile.workflow("main").unwrap().nodes[0].config["who"],
            "Ana"
        );
        set_config(&mut profile, "main", "say", "who", None);
        assert!(profile.workflow("main").unwrap().nodes[0].config.is_empty());
        rename_node(&mut profile, "main", "say", "Greeter");
        assert_eq!(profile.workflow("main").unwrap().nodes[0].name, "Greeter");
    }

    #[test]
    fn new_nodes_cascade_without_overlapping() {
        let mut profile = new_profile("x", "X", "Main");
        let mut seen: Vec<Position> = Vec::new();
        for i in 0..5 {
            let main = profile.workflow("main").unwrap();
            let at = cascade_position(main, 40.0, 250.0, 160.0);
            for other in &seen {
                let apart = (at.x - other.x).abs() >= 290.0 || (at.y - other.y).abs() >= 200.0;
                assert!(apart, "{at:?} overlaps {other:?}");
            }
            seen.push(at);
            let mut n = node(&format!("n{i}"), Vec::new());
            n.position = at;
            add_node(&mut profile, "main", n);
        }
    }
}
