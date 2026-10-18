//! Checking a profile against the wiring rules (docs/02-architecture.md §4).
//!
//! The check reports instead of failing: a profile with problems still loads and the
//! client can show exactly what is wrong.

use std::collections::{HashMap, HashSet};

use regulus_proto::{Cardinality, Diagnostic, Direction, Profile, Severity, Workflow};

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

    let mut wires = HashSet::new();
    let mut incoming: HashMap<(&str, &str), usize> = HashMap::new();
    for wire in &workflow.wires {
        let wire_id = Some(wire.id.as_str());
        if !wires.insert(wire.id.as_str()) {
            report.push(
                Severity::Error,
                "wire.duplicate",
                format!("wire id `{}` is used more than once", wire.id),
                None,
                wire_id,
            );
        }
        let endpoint = |end: &regulus_proto::PortRef| {
            workflow
                .node(&end.node_id)
                .and_then(|node| node.port(&end.port_id))
        };
        let (Some(from), Some(to)) = (endpoint(&wire.from), endpoint(&wire.to)) else {
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
        if from.direction != Direction::Output || to.direction != Direction::Input {
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

    for node in &workflow.nodes {
        for port in node
            .ports
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
