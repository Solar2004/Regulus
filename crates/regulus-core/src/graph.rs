//! Checking a profile against the wiring rules (docs/02-architecture.md §4).
//!
//! The check reports instead of failing: a profile with problems still loads and the
//! client can show exactly what is wrong.

use std::collections::{HashMap, HashSet};

use regulus_proto::{
    Cardinality, Diagnostic, Direction, Node, Port, PortRef, Profile, Severity, Wire, WireKind,
    Workflow, BOX_SEPARATOR, WORKFLOW_SELF,
};

struct Report<'a> {
    list: Vec<Diagnostic>,
    workflow: Option<&'a str>,
}

impl Report<'_> {
    fn push(
        &mut self,
        severity: Severity,
        code: &str,
        message: String,
        node: Option<&str>,
        wire: Option<&str>,
    ) {
        self.list.push(Diagnostic {
            severity,
            code: code.into(),
            message,
            workflow_id: self.workflow.map(Into::into),
            node_id: node.map(Into::into),
            wire_id: wire.map(Into::into),
        });
    }
}

/// Checks every workflow of a profile. `is_installed` tells whether a module id is
/// loaded; a node from a missing module is reported as information, never as an error
/// (docs/04-client.md §3).
pub fn check_profile(profile: &Profile, is_installed: impl Fn(&str) -> bool) -> Vec<Diagnostic> {
    let mut report = Report {
        list: Vec::new(),
        workflow: None,
    };
    if profile.workflow(&profile.main_workflow_id).is_none() {
        let message = format!(
            "main workflow `{}` does not exist",
            profile.main_workflow_id
        );
        report.push(Severity::Error, "profile.main_missing", message, None, None);
    }
    let mut seen = HashSet::new();
    for workflow in &profile.workflows {
        if !seen.insert(workflow.id.as_str()) {
            let message = format!("workflow id `{}` is used more than once", workflow.id);
            report.push(Severity::Error, "workflow.duplicate", message, None, None);
        }
    }
    for workflow in &profile.workflows {
        report.workflow = Some(&workflow.id);
        check_workflow(profile, workflow, &is_installed, &mut report);
    }
    report.list
}

fn check_workflow(
    profile: &Profile,
    workflow: &Workflow,
    is_installed: &impl Fn(&str) -> bool,
    report: &mut Report,
) {
    let mut nodes = HashSet::new();
    for node in &workflow.nodes {
        let id = Some(node.id.as_str());
        if !nodes.insert(node.id.as_str()) {
            report.push(
                Severity::Error,
                "node.duplicate",
                format!("node id `{}` is used more than once", node.id),
                id,
                None,
            );
        }
        if let Some(module) = node
            .module_id
            .as_deref()
            .filter(|module| !is_installed(module))
        {
            report.push(
                Severity::Info,
                "node.module_not_installed",
                format!("module `{module}` is not installed"),
                id,
                None,
            );
        }
        if let Some(internal) = node
            .internal_workflow_id
            .as_deref()
            .filter(|w| profile.workflow(w).is_none())
        {
            let message = format!("internal workflow `{internal}` does not exist");
            report.push(
                Severity::Error,
                "node.internal_workflow_missing",
                message,
                id,
                None,
            );
        }
        if node.id == WORKFLOW_SELF || node.id.contains(BOX_SEPARATOR) {
            let message = format!(
                "node id `{}` is reserved: `{WORKFLOW_SELF}` names the workflow's own ports \
                 and `{BOX_SEPARATOR}` joins a box to the nodes inside it",
                node.id
            );
            report.push(Severity::Error, "node.reserved_id", message, id, None);
        }
        if let Some(inner) = node
            .internal_workflow_id
            .as_deref()
            .filter(|_| node.is_box())
            .and_then(|w| profile.workflow(w))
        {
            check_box(profile, workflow, node, inner, report);
        }
        let mut ports = HashSet::new();
        for port in &node.ports {
            if !ports.insert(port.id.as_str()) {
                report.push(
                    Severity::Error,
                    "port.duplicate",
                    format!("port id `{}` is used more than once", port.id),
                    id,
                    None,
                );
            }
        }
    }

    let mut own = HashSet::new();
    for port in &workflow.ports {
        if !own.insert(port.id.as_str()) {
            let message = format!("workflow port id `{}` is used more than once", port.id);
            report.push(
                Severity::Error,
                "workflow.port_duplicate",
                message,
                None,
                None,
            );
        }
        if port.kind == WireKind::Uses {
            let message = format!(
                "workflow port `{}` is a \"uses\" port: only \"then\" values cross a \
                 workflow's boundary for now",
                port.id
            );
            report.push(Severity::Warning, "workflow.port_uses", message, None, None);
        }
    }

    let mut wires = HashSet::new();
    let mut incoming: HashMap<(&str, &str), usize> = HashMap::new();
    for wire in &workflow.wires {
        let wire_id = Some(wire.id.as_str());
        if wire.id.contains(BOX_SEPARATOR) {
            let message = format!(
                "wire id `{}` is reserved: `{BOX_SEPARATOR}` joins a box to what is inside it",
                wire.id
            );
            report.push(Severity::Error, "wire.reserved_id", message, None, wire_id);
        }
        if !wires.insert(wire.id.as_str()) {
            report.push(
                Severity::Error,
                "wire.duplicate",
                format!("wire id `{}` is used more than once", wire.id),
                None,
                wire_id,
            );
        }
        let (Some((from, from_dir)), Some((to, to_dir))) = (
            endpoint(profile, workflow, &wire.from),
            endpoint(profile, workflow, &wire.to),
        ) else {
            let message = format!(
                "wire joins `{}.{}` to `{}.{}`, but one of those ports does not exist",
                wire.from.node_id, wire.from.port_id, wire.to.node_id, wire.to.port_id
            );
            report.push(
                Severity::Error,
                "wire.endpoint_missing",
                message,
                None,
                wire_id,
            );
            continue;
        };
        *incoming
            .entry((&wire.to.node_id, &wire.to.port_id))
            .or_default() += 1;
        if from_dir != Direction::Output || to_dir != Direction::Input {
            report.push(
                Severity::Error,
                "wire.direction",
                "a wire must go from an output to an input".into(),
                None,
                wire_id,
            );
        }
        if from.kind != wire.kind || to.kind != wire.kind {
            report.push(
                Severity::Error,
                "wire.kind_mismatch",
                "both ports must be of the same kind as the wire".into(),
                None,
                wire_id,
            );
        }
        if from.capability != to.capability {
            let message = format!(
                "`{}` does not fit into `{}`",
                from.capability, to.capability
            );
            report.push(
                Severity::Error,
                "wire.capability_mismatch",
                message,
                None,
                wire_id,
            );
        }
    }

    // Inside, the workflow's outputs take wires like inputs do.
    for port in workflow
        .ports
        .iter()
        .filter(|port| port.direction == Direction::Output)
    {
        let count = incoming
            .get(&(WORKFLOW_SELF, port.id.as_str()))
            .copied()
            .unwrap_or(0);
        if count > 1 && port.cardinality == Cardinality::One {
            let message = format!(
                "workflow output `{}` accepts one wire and has {count}",
                port.id
            );
            report.push(Severity::Error, "port.too_many_wires", message, None, None);
        }
    }
    for node in &workflow.nodes {
        for port in profile
            .ports_of(node)
            .iter()
            .filter(|port| port.direction == Direction::Input)
        {
            let count = incoming
                .get(&(node.id.as_str(), port.id.as_str()))
                .copied()
                .unwrap_or(0);
            if count > 1 && port.cardinality == Cardinality::One {
                let message = format!("input `{}` accepts one wire and has {count}", port.id);
                report.push(
                    Severity::Error,
                    "port.too_many_wires",
                    message,
                    Some(&node.id),
                    None,
                );
            }
            if count == 0 && port.required {
                let message = format!(
                    "required input `{}` ({}) has no wire; the node will be disabled",
                    port.id, port.capability
                );
                report.push(
                    Severity::Warning,
                    "port.required_unwired",
                    message,
                    Some(&node.id),
                    None,
                );
            }
        }
    }
}

/// One end of a wire in `workflow`: the port and the direction it has from inside the
/// workflow. The workflow's own ports (`self`) face the other way inside: an input feeds
/// nodes, an output takes values.
fn endpoint<'a>(
    profile: &'a Profile,
    workflow: &'a Workflow,
    end: &PortRef,
) -> Option<(&'a Port, Direction)> {
    if end.node_id == WORKFLOW_SELF {
        let port = workflow.port(&end.port_id)?;
        let inside = match port.direction {
            Direction::Input => Direction::Output,
            Direction::Output => Direction::Input,
        };
        return Some((port, inside));
    }
    let node = workflow.node(&end.node_id)?;
    let port = profile
        .ports_of(node)
        .iter()
        .find(|port| port.id == end.port_id)?;
    Some((port, port.direction))
}

/// A workflow placed as a node: the ports it declares, if any, are the workflow's, and
/// it never ends up inside itself.
fn check_box(
    profile: &Profile,
    workflow: &Workflow,
    node: &Node,
    inner: &Workflow,
    report: &mut Report,
) {
    let id = Some(node.id.as_str());
    let same = |a: &Port, b: &Port| {
        a.id == b.id
            && a.capability == b.capability
            && a.direction == b.direction
            && a.kind == b.kind
    };
    let matches = node.ports.len() == inner.ports.len()
        && node
            .ports
            .iter()
            .all(|port| inner.ports.iter().any(|other| same(port, other)));
    if !node.ports.is_empty() && !matches {
        let message = format!(
            "the ports of `{}` must be those of workflow `{}`, or none",
            node.id, inner.id
        );
        report.push(
            Severity::Error,
            "node.box_ports_mismatch",
            message,
            id,
            None,
        );
    }
    if inner.id == workflow.id || embeds(profile, inner, &workflow.id, &mut HashSet::new()) {
        let message = format!(
            "workflow `{}` ends up inside itself through `{}`",
            workflow.id, node.id
        );
        report.push(Severity::Error, "workflow.embeds_itself", message, id, None);
    }
}

/// Whether `workflow` places `target` as a box, directly or deeper down.
fn embeds<'a>(
    profile: &'a Profile,
    workflow: &'a Workflow,
    target: &str,
    seen: &mut HashSet<&'a str>,
) -> bool {
    workflow
        .nodes
        .iter()
        .filter(|node| node.is_box())
        .filter_map(|node| node.internal_workflow_id.as_deref())
        .any(|inner| {
            inner == target
                || (seen.insert(inner)
                    && profile
                        .workflow(inner)
                        .is_some_and(|w| embeds(profile, w, target, seen)))
        })
}

/// The copy of `workflow` an execution runs: every box node keeps its place, with the
/// ports of its workflow, and that workflow's nodes and wires join it under the box's id
/// (`box/node`), as deep as boxes go. Inner wires to `self` end on the box node; the top
/// workflow's own `self` stays as it is. A box whose workflow is missing stays empty.
pub fn flatten(profile: &Profile, workflow: &Workflow) -> Result<Workflow, String> {
    let mut plan = Workflow {
        nodes: Vec::with_capacity(workflow.nodes.len()),
        wires: Vec::with_capacity(workflow.wires.len()),
        ..workflow.clone()
    };
    let mut path = vec![workflow.id.as_str()];
    inline(profile, workflow, None, &mut path, &mut plan)?;
    // An id given twice in the copy is ambiguous: lookups find the first and the other
    // would start twice or take its events. Inlining can make one (`a/b` next to box `a`
    // with node `b`), and so can a plain duplicate.
    let mut nodes = HashSet::new();
    if let Some(node) = plan.nodes.iter().find(|n| !nodes.insert(n.id.as_str())) {
        return Err(format!("node id `{}` is used more than once", node.id));
    }
    let mut wires = HashSet::new();
    if let Some(wire) = plan.wires.iter().find(|w| !wires.insert(w.id.as_str())) {
        return Err(format!("wire id `{}` is used more than once", wire.id));
    }
    Ok(plan)
}

fn inline<'a>(
    profile: &'a Profile,
    workflow: &'a Workflow,
    prefix: Option<&str>,
    path: &mut Vec<&'a str>,
    plan: &mut Workflow,
) -> Result<(), String> {
    let join = |id: &str| match prefix {
        None => id.to_string(),
        Some(prefix) => format!("{prefix}{BOX_SEPARATOR}{id}"),
    };
    for wire in &workflow.wires {
        // `/` is how the copy tells which box a wire belongs to.
        if wire.id.contains(BOX_SEPARATOR) {
            return Err(format!(
                "wire id `{}` is reserved: `{BOX_SEPARATOR}` joins a box to what is inside it",
                wire.id
            ));
        }
        let end = |end: &PortRef| PortRef {
            node_id: match prefix {
                Some(prefix) if end.node_id == WORKFLOW_SELF => prefix.to_string(),
                _ => join(&end.node_id),
            },
            port_id: end.port_id.clone(),
        };
        plan.wires.push(Wire {
            id: join(&wire.id),
            kind: wire.kind,
            from: end(&wire.from),
            to: end(&wire.to),
        });
    }
    for node in &workflow.nodes {
        if node.id == WORKFLOW_SELF || node.id.contains(BOX_SEPARATOR) {
            return Err(format!(
                "node id `{}` is reserved: `{WORKFLOW_SELF}` names the workflow's own ports \
                 and `{BOX_SEPARATOR}` joins a box to the nodes inside it",
                node.id
            ));
        }
        let mut placed = node.clone();
        placed.id = join(&node.id);
        let inner = node
            .internal_workflow_id
            .as_deref()
            .filter(|_| node.is_box())
            .and_then(|id| profile.workflow(id));
        let Some(inner) = inner else {
            plan.nodes.push(placed);
            continue;
        };
        if path.contains(&inner.id.as_str()) {
            return Err(format!(
                "workflow `{}` ends up inside itself through `{}`",
                inner.id, placed.id
            ));
        }
        if placed.ports.is_empty() {
            placed.ports = inner.ports.clone();
        }
        let id = placed.id.clone();
        plan.nodes.push(placed);
        path.push(&inner.id);
        inline(profile, inner, Some(&id), path, plan)?;
        path.pop();
    }
    Ok(())
}

/// The boundary port at `end` of a running copy: a port of the workflow itself (`self`)
/// or of a box node. A value that reaches one is passed on from it, to whatever is wired
/// on its other side.
pub(crate) fn boundary<'a>(plan: &'a Workflow, end: &PortRef) -> Option<&'a Port> {
    if end.node_id == WORKFLOW_SELF {
        return plan.port(&end.port_id);
    }
    plan.node(&end.node_id)
        .filter(|node| node.is_box())
        .and_then(|node| node.port(&end.port_id))
}

/// One end of a wire.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum End {
    From,
    To,
}

/// The port at one end of a wire of a running copy, if it faces the way that end needs:
/// a value leaves from an output and arrives at an input. Boundary ports face the other
/// way from inside: the copy's own ports (`self`) and a box's ports for the wires of
/// that box, the ones whose id is `box/...`.
pub(crate) fn wire_end<'a>(plan: &'a Workflow, wire: &Wire, end: End) -> Option<&'a Port> {
    let at = match end {
        End::From => &wire.from,
        End::To => &wire.to,
    };
    let (port, inside) = if at.node_id == WORKFLOW_SELF {
        (plan.port(&at.port_id)?, true)
    } else {
        let node = plan.node(&at.node_id)?;
        let owner = wire.id.rsplit_once(BOX_SEPARATOR).map(|(owner, _)| owner);
        (
            node.port(&at.port_id)?,
            node.is_box() && owner == Some(node.id.as_str()),
        )
    };
    let wanted = match (end, inside) {
        (End::From, false) | (End::To, true) => Direction::Output,
        (End::To, false) | (End::From, true) => Direction::Input,
    };
    Some(port).filter(|port| port.direction == wanted)
}

/// The order in which a workflow's nodes start: whatever a node uses over a "uses" wire
/// starts before it, unless the wire ends in a lazy input. Nodes caught in a cycle keep
/// their declared order, after the rest.
pub fn start_order(workflow: &Workflow) -> Vec<&regulus_proto::Node> {
    let index: HashMap<&str, usize> = workflow
        .nodes
        .iter()
        .enumerate()
        .map(|(i, node)| (node.id.as_str(), i))
        .collect();
    let mut waiting = vec![0usize; workflow.nodes.len()];
    let mut users: Vec<Vec<usize>> = vec![Vec::new(); workflow.nodes.len()];
    for wire in workflow
        .wires
        .iter()
        .filter(|w| w.kind == regulus_proto::WireKind::Uses && !lazy_end(workflow, w))
    {
        let (Some(&from), Some(&to)) = (
            index.get(wire.from.node_id.as_str()),
            index.get(wire.to.node_id.as_str()),
        ) else {
            continue;
        };
        if from != to {
            waiting[to] += 1;
            users[from].push(to);
        }
    }
    let mut order = Vec::with_capacity(workflow.nodes.len());
    let mut done = vec![false; workflow.nodes.len()];
    let mut progress = true;
    while progress {
        progress = false;
        for i in 0..workflow.nodes.len() {
            if done[i] || waiting[i] > 0 {
                continue;
            }
            done[i] = true;
            progress = true;
            order.push(i);
            for &user in &users[i] {
                waiting[user] -= 1;
            }
        }
    }
    order.extend((0..workflow.nodes.len()).filter(|&i| !done[i]));
    order.into_iter().map(|i| &workflow.nodes[i]).collect()
}

/// Whether a wire ends in an input its node reads only while running.
fn lazy_end(workflow: &Workflow, wire: &regulus_proto::Wire) -> bool {
    workflow
        .nodes
        .iter()
        .find(|n| n.id == wire.to.node_id)
        .and_then(|n| n.ports.iter().find(|p| p.id == wire.to.port_id))
        .is_some_and(|p| p.lazy)
}
