//! Checking a profile against the wiring rules (docs/02-architecture.md §4).
//!
//! The check reports instead of failing: a profile with problems still loads and the
//! client can show exactly what is wrong.

use std::collections::{HashMap, HashSet};
use std::ops::Deref;

use regulus_proto::{
    shape, Cardinality, Diagnostic, Direction, Manifest, Node, Port, PortRef, Profile, Severity,
    Wire, WireKind, Workflow, BOX_SEPARATOR, FIELD_WORKFLOW, WORKFLOW_SELF,
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
        // A loop of "uses" wires shows only once boxes are opened: the running copy finds it.
        if let Err(error) = flatten(profile, workflow) {
            if error.starts_with(USES_CYCLE) {
                report.push(Severity::Error, "wire.uses_cycle", error, None, None);
            }
        }
    }
    report.list
}

/// The nodes of a profile whose module its project keeps from running (`[modules]
/// disable` or `enable` in its `regulus.toml`, docs/11-composition.md §13): each one is
/// reported as disabled, with the project as the reason. A warning, not an error: the
/// profile is still valid and can be saved; `execution.start` refuses to run them.
pub fn check_vetoed(
    profile: &Profile,
    project: &str,
    vetoes: impl Fn(&str) -> bool,
) -> Vec<Diagnostic> {
    let mut report = Report {
        list: Vec::new(),
        workflow: None,
    };
    for workflow in &profile.workflows {
        report.workflow = Some(&workflow.id);
        for node in &workflow.nodes {
            if let Some(module) = node.module_id.as_deref().filter(|m| vetoes(m)) {
                report.push(
                    Severity::Warning,
                    "node.module_disabled",
                    format!(
                        "disabled in project {project}: module `{module}` is off in its \
                         [modules] disable/enable"
                    ),
                    Some(&node.id),
                    None,
                );
            }
        }
    }
    report.list
}

/// Checks the fields of module nodes the core understands, from what their module's
/// manifest declares (`manifest_of`; a module that is not loaded is skipped): a
/// [`FIELD_WORKFLOW`] field must name a workflow of the profile with a "then" output. Unset
/// is a warning: the node has nothing to run. So is a node whose "then" ports do not line
/// up with that workflow's: the node feeds its inputs by id and gives back on the output
/// of the same id, so each of its "then" inputs must be one of the workflow's and each of
/// the workflow's "then" outputs one of its own, with the same capability; a field with
/// `own_ports` skips that. And so is a workflow with a "uses" input the node has no "uses"
/// input of the same id for: the node hands on what is wired into its own (C2 bindings),
/// so without one that input of the child stays empty.
pub fn check_fields<M: Deref<Target = Manifest>>(
    profile: &Profile,
    manifest_of: impl Fn(&str) -> Option<M>,
) -> Vec<Diagnostic> {
    let mut report = Report {
        list: Vec::new(),
        workflow: None,
    };
    for workflow in &profile.workflows {
        report.workflow = Some(&workflow.id);
        for node in &workflow.nodes {
            for (severity, code, message) in field_findings(profile, node, &manifest_of) {
                report.push(severity, code, message, Some(&node.id), None);
            }
        }
    }
    report.list
}

/// The code of a node whose ports differ from what its `managed_ports` type gives for its
/// config.
pub const PORTS_STALE: &str = "node.ports_stale";

/// The module nodes whose shape no longer fits what their module declares (protocol 1.17),
/// all warnings: a type the loaded module does not have (`node.type_missing`, e.g. after a
/// reload), and for a `managed_ports` type what of the config the shape cannot use
/// (`node.shape_problem`) and ports that differ from the shape ([`PORTS_STALE`]).
/// `manifest_of` gives what a module declared; a module that is not loaded, or never said
/// its manifest, is skipped.
pub fn check_shapes<M: Deref<Target = Manifest>>(
    profile: &Profile,
    manifest_of: impl Fn(&str) -> Option<M>,
) -> Vec<Diagnostic> {
    let mut report = Report {
        list: Vec::new(),
        workflow: None,
    };
    for workflow in &profile.workflows {
        report.workflow = Some(&workflow.id);
        for node in &workflow.nodes {
            let Some(manifest) = node.module_id.as_deref().and_then(&manifest_of) else {
                continue;
            };
            let id = Some(node.id.as_str());
            let Some(node_type) = manifest.node_type(&node.node_type) else {
                let message = format!(
                    "module `{}` has no node type `{}` (a reload may have removed it)",
                    manifest.module.id, node.node_type
                );
                report.push(Severity::Warning, "node.type_missing", message, id, None);
                continue;
            };
            if !node_type.managed_ports {
                continue;
            }
            let shaped = shape::shape(node_type, &node.config);
            for problem in shaped.problems {
                report.push(Severity::Warning, "node.shape_problem", problem, id, None);
            }
            let changes = shape::diff_ports(&node.ports, &shaped.ports);
            if !changes.is_empty() {
                let message = format!(
                    "ports differ from what `{}` declares for this config: {}; sync them \
                     (profile.sync)",
                    manifest.module.id,
                    changes.describe()
                );
                report.push(Severity::Warning, PORTS_STALE, message, id, None);
            }
        }
    }
    report.list
}

/// The first node of a running copy whose [`FIELD_WORKFLOW`] field names a workflow the
/// profile does not have: `execution.start` refuses it, the node could never run.
pub fn missing_workflow<M: Deref<Target = Manifest>>(
    profile: &Profile,
    plan: &Workflow,
    manifest_of: impl Fn(&str) -> Option<M>,
) -> Option<String> {
    plan.nodes.iter().find_map(|node| {
        field_findings(profile, node, &manifest_of)
            .into_iter()
            .find(|(_, code, _)| *code == FIELD_WORKFLOW_MISSING)
            .map(|(_, _, message)| format!("node `{}`: {message}", node.id))
    })
}

const FIELD_WORKFLOW_MISSING: &str = "field.workflow_missing";

fn field_findings<M: Deref<Target = Manifest>>(
    profile: &Profile,
    node: &Node,
    manifest_of: &impl Fn(&str) -> Option<M>,
) -> Vec<(Severity, &'static str, String)> {
    let mut found = Vec::new();
    let Some(manifest) = node.module_id.as_deref().and_then(manifest_of) else {
        return found;
    };
    let Some(node_type) = manifest.node_type(&node.node_type) else {
        return found;
    };
    for field in node_type
        .fields
        .iter()
        .filter(|f| f.field_type == FIELD_WORKFLOW)
    {
        let value = manifest.setting(node, &field.id).and_then(|v| v.as_str());
        let Some(target) = value.filter(|v| !v.is_empty()) else {
            let message = format!("`{}` names no workflow; the node runs none", field.id);
            found.push((Severity::Warning, "field.workflow_unset", message));
            continue;
        };
        let Some(called) = profile.workflow(target) else {
            let message = format!("`{}`: workflow `{target}` does not exist", field.id);
            found.push((Severity::Error, FIELD_WORKFLOW_MISSING, message));
            continue;
        };
        let then = |ports: &[Port], direction| -> Vec<(String, String)> {
            ports
                .iter()
                .filter(|p| p.direction == direction && p.kind == WireKind::Then)
                .map(|p| (p.id.clone(), p.capability.clone()))
                .collect()
        };
        let outputs = then(&called.ports, Direction::Output);
        if outputs.is_empty() {
            let message = format!(
                "`{}`: workflow `{target}` declares no \"then\" output, so nothing would \
                 come back from it",
                field.id
            );
            found.push((Severity::Error, "field.workflow_no_output", message));
            continue;
        }
        let uses_inputs = |ports: &[Port]| -> Vec<String> {
            ports
                .iter()
                .filter(|p| p.direction == Direction::Input && p.kind == WireKind::Uses)
                .map(|p| p.id.clone())
                .collect()
        };
        let exposed = uses_inputs(&node.ports);
        let unbound: Vec<String> = uses_inputs(&called.ports)
            .into_iter()
            .filter(|port| !exposed.contains(port))
            .map(|port| format!("`{port}`"))
            .collect();
        if !unbound.is_empty() {
            let message = format!(
                "`{}`: workflow `{target}` uses {} from whoever runs it, and the node has \
                 no \"uses\" input of that id to hand on",
                field.id,
                unbound.join(", ")
            );
            found.push((Severity::Warning, "field.workflow_uses_unbound", message));
        }
        if field.own_ports {
            continue;
        }
        let inputs = then(&called.ports, Direction::Input);
        let own_outputs = then(&node.ports, Direction::Output);
        let mut unmatched: Vec<String> = then(&node.ports, Direction::Input)
            .into_iter()
            .filter(|port| !inputs.contains(port))
            .map(|(id, capability)| format!("input `{id}` ({capability})"))
            .collect();
        unmatched.extend(
            outputs
                .iter()
                .filter(|port| !own_outputs.contains(port))
                .map(|(id, capability)| format!("output `{id}` ({capability})")),
        );
        if !unmatched.is_empty() {
            let message = format!(
                "the \"then\" ports of the node and of workflow `{target}` do not line up \
                 by id and capability: {}",
                unmatched.join(", ")
            );
            found.push((Severity::Warning, "field.workflow_ports_mismatch", message));
        }
    }
    found
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
        // Several services behind a boundary input cannot reach a consumer that takes
        // one: the running copy would wire them all into it.
        if wire.kind == WireKind::Uses
            && wire.from.node_id == WORKFLOW_SELF
            && from.cardinality == Cardinality::Many
            && to.cardinality == Cardinality::One
            && to_dir == Direction::Input
        {
            let message = format!(
                "workflow input `{}` takes many wires, but `{}.{}` accepts one",
                wire.from.port_id, wire.to.node_id, wire.to.port_id
            );
            report.push(
                Severity::Error,
                "wire.boundary_cardinality",
                message,
                None,
                wire_id,
            );
        }
    }

    // Inside, the workflow's outputs take wires like inputs do. A "uses" output hands out
    // one service, so it takes one wire whatever its cardinality.
    for port in workflow
        .ports
        .iter()
        .filter(|port| port.direction == Direction::Output)
    {
        let count = incoming
            .get(&(WORKFLOW_SELF, port.id.as_str()))
            .copied()
            .unwrap_or(0);
        if count > 1 && (port.cardinality == Cardinality::One || port.kind == WireKind::Uses) {
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
    // Everything but the display name: a box that takes one wire where its workflow takes
    // many, or reads lazily where it does not, is not the same box.
    let same = |a: &Port, b: &Port| {
        Port {
            name: String::new(),
            ..a.clone()
        } == Port {
            name: String::new(),
            ..b.clone()
        }
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
///
/// "Uses" wires do not stop at a box: each one that ends in a real input is joined with
/// what feeds the box port it starts from, through as many boxes as there are, into one
/// wire between the real ends (`collapse_uses`). "Then" wires keep the box ports: the bus
/// passes values across them.
pub fn flatten(profile: &Profile, workflow: &Workflow) -> Result<Workflow, String> {
    let mut plan = Workflow {
        nodes: Vec::with_capacity(workflow.nodes.len()),
        wires: Vec::with_capacity(workflow.wires.len()),
        ..workflow.clone()
    };
    let mut path = vec![workflow.id.as_str()];
    inline(profile, workflow, None, &mut path, &mut plan)?;
    // Collapsing follows wires by id: a wire id given twice must fail here, before it can
    // be folded away.
    unique_wire_ids(&plan)?;
    collapse_uses(&mut plan)?;
    // An id given twice in the copy is ambiguous: lookups find the first and the other
    // would start twice or take its events. Inlining can make one (`a/b` next to box `a`
    // with node `b`), and so can a plain duplicate.
    let mut nodes = HashSet::new();
    if let Some(node) = plan.nodes.iter().find(|n| !nodes.insert(n.id.as_str())) {
        return Err(format!("node id `{}` is used more than once", node.id));
    }
    unique_wire_ids(&plan)?;
    Ok(plan)
}

fn unique_wire_ids(plan: &Workflow) -> Result<(), String> {
    let mut wires = HashSet::new();
    match plan.wires.iter().find(|w| !wires.insert(w.id.as_str())) {
        Some(wire) => Err(format!("wire id `{}` is used more than once", wire.id)),
        None => Ok(()),
    }
}

/// How the error of a loop of "uses" wires through box ports begins: `check_profile`
/// reports it as `wire.uses_cycle`.
const USES_CYCLE: &str = "\"uses\" wires go round through box ports";

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

/// The box whose wire this is in a running copy: what comes before the last `/` of its
/// id, `None` for a wire of the top workflow.
fn wire_owner(wire: &Wire) -> Option<&str> {
    wire.id.rsplit_once(BOX_SEPARATOR).map(|(owner, _)| owner)
}

/// Whether a "uses" wire of a running copy touches a box port at either end.
fn touches_box(plan: &Workflow, wire: &Wire) -> bool {
    [&wire.from, &wire.to]
        .into_iter()
        .any(|end| plan.node(&end.node_id).is_some_and(Node::is_box))
}

/// Replaces every "uses" wire that touches a box port: one that ends in a real input (a
/// node's, or an output of the top workflow) becomes one wire per real output that feeds
/// it through the boxes, with the id of that last wire (`#<n>` added when there are
/// several); the rest go. An input whose box port nothing feeds is left unwired. The top
/// workflow's own `self` ports stay as they are: they are bound from outside. Wires that
/// go round through box ports without reaching a service are an error, never dropped.
fn collapse_uses(plan: &mut Workflow) -> Result<(), String> {
    let mut wires = Vec::with_capacity(plan.wires.len());
    for wire in &plan.wires {
        if wire.kind != WireKind::Uses || !touches_box(plan, wire) {
            wires.push(wire.clone());
            continue;
        }
        let mut path = vec![wire.id.as_str()];
        let sources = uses_sources(plan, &wire.from, wire_owner(wire), &mut path)?;
        // A wire into a box port is followed only to find loops: the wires inside, which
        // end in real inputs, carry it.
        if plan.node(&wire.to.node_id).is_some_and(Node::is_box) {
            continue;
        }
        let several = sources.len() > 1;
        for (n, from) in sources.into_iter().enumerate() {
            wires.push(Wire {
                id: if several {
                    format!("{}#{}", wire.id, n + 1)
                } else {
                    wire.id.clone()
                },
                kind: WireKind::Uses,
                from,
                to: wire.to.clone(),
            });
        }
    }
    plan.wires = wires;
    Ok(())
}

/// The real outputs behind `from`, the start of a "uses" wire of the box `owner` (or of
/// the top workflow): `from` itself when it is a node's or the top workflow's own input,
/// else what is wired into that box port on its other side, followed the same way.
/// `path` holds the wires followed so far: coming back to one is a loop of pass-through
/// wires, an error naming them.
fn uses_sources<'a>(
    plan: &'a Workflow,
    from: &PortRef,
    owner: Option<&str>,
    path: &mut Vec<&'a str>,
) -> Result<Vec<PortRef>, String> {
    if from.node_id == WORKFLOW_SELF {
        return Ok(vec![from.clone()]);
    }
    let Some(node) = plan.node(&from.node_id) else {
        return Ok(Vec::new());
    };
    if !node.is_box() {
        return Ok(vec![from.clone()]);
    }
    let Some(port) = node.port(&from.port_id) else {
        return Ok(Vec::new());
    };
    // From inside, a box input feeds: what feeds it comes from outside. From outside, a
    // box output gives: what gives it comes from inside.
    let inside = owner == Some(node.id.as_str());
    let wanted = if inside {
        Direction::Input
    } else {
        Direction::Output
    };
    if port.direction != wanted {
        return Ok(Vec::new());
    }
    let mut found = Vec::new();
    for feeder in plan.wires.iter().filter(|w| {
        w.kind == WireKind::Uses
            && w.to == *from
            && (wire_owner(w) == Some(node.id.as_str())) != inside
    }) {
        if let Some(start) = path.iter().position(|id| *id == feeder.id) {
            let mut round: Vec<&str> = path[start..].to_vec();
            round.push(&feeder.id);
            return Err(format!(
                "{USES_CYCLE} without reaching a service: {}",
                round
                    .iter()
                    .map(|id| format!("`{id}`"))
                    .collect::<Vec<_>>()
                    .join(" → ")
            ));
        }
        path.push(&feeder.id);
        let more = uses_sources(plan, &feeder.from, wire_owner(feeder), path);
        path.pop();
        for source in more? {
            if !found.contains(&source) {
                found.push(source);
            }
        }
    }
    Ok(found)
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
