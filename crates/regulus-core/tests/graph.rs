//! The wiring rules of docs/02-architecture.md §4, checked by `check_profile`.

use regulus_core::graph::check_profile;
use regulus_proto::{
    Cardinality, Diagnostic, Direction, Node, Port, PortRef, Profile, Severity, Wire, WireKind,
    Workflow, WorkflowKind,
};
use serde_json::Value;

const FIXTURE: &str = include_str!("../../regulus-tauri/frontend/mock/profile.json");

fn port(id: &str, capability: &str, direction: Direction, kind: WireKind) -> Port {
    Port {
        id: id.into(),
        name: id.into(),
        capability: capability.into(),
        direction,
        kind,
        required: false,
        cardinality: Cardinality::One,
    }
}

fn node(id: &str, ports: Vec<Port>) -> Node {
    serde_json::from_value(serde_json::json!({ "id": id, "type": "test", "ports": ports })).unwrap()
}

fn wire(id: &str, kind: WireKind, from: (&str, &str), to: (&str, &str)) -> Wire {
    let end = |(node_id, port_id): (&str, &str)| PortRef {
        node_id: node_id.into(),
        port_id: port_id.into(),
    };
    Wire {
        id: id.into(),
        kind,
        from: end(from),
        to: end(to),
    }
}

fn profile(nodes: Vec<Node>, wires: Vec<Wire>) -> Profile {
    let workflow = Workflow {
        id: "main".into(),
        name: "main".into(),
        kind: WorkflowKind::Main,
        module_id: None,
        nodes,
        wires,
        layout: Default::default(),
    };
    Profile {
        id: "p".into(),
        name: "p".into(),
        project_id: None,
        extends: None,
        main_workflow_id: "main".into(),
        workflows: vec![workflow],
    }
}

fn codes(diagnostics: &[Diagnostic]) -> Vec<&str> {
    diagnostics.iter().map(|d| d.code.as_str()).collect()
}

fn provider_and_agent() -> Vec<Node> {
    vec![
        node(
            "provider",
            vec![port(
                "chat",
                "llm.chat@1",
                Direction::Output,
                WireKind::Uses,
            )],
        ),
        node(
            "agent",
            vec![port(
                "model",
                "llm.chat@1",
                Direction::Input,
                WireKind::Uses,
            )],
        ),
    ]
}

#[test]
fn the_mock_profile_has_no_errors() {
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let profile: Profile = serde_json::from_value(fixture["profiles"][0].clone()).unwrap();
    let diagnostics = check_profile(&profile, |_| true);
    assert!(
        diagnostics.iter().all(|d| d.severity != Severity::Error),
        "{diagnostics:#?}"
    );
}

#[test]
fn a_matching_uses_wire_is_clean() {
    let p = profile(
        provider_and_agent(),
        vec![wire(
            "w",
            WireKind::Uses,
            ("provider", "chat"),
            ("agent", "model"),
        )],
    );
    assert!(check_profile(&p, |_| true).is_empty());
}

#[test]
fn capabilities_must_match_type_and_version() {
    let mut nodes = provider_and_agent();
    nodes[1].ports[0].capability = "llm.chat@2".into();
    let p = profile(
        nodes,
        vec![wire(
            "w",
            WireKind::Uses,
            ("provider", "chat"),
            ("agent", "model"),
        )],
    );
    assert_eq!(
        codes(&check_profile(&p, |_| true)),
        ["wire.capability_mismatch"]
    );
}

#[test]
fn wires_go_from_output_to_input_of_their_own_kind() {
    let p = profile(
        provider_and_agent(),
        vec![wire(
            "w",
            WireKind::Uses,
            ("agent", "model"),
            ("provider", "chat"),
        )],
    );
    assert_eq!(codes(&check_profile(&p, |_| true)), ["wire.direction"]);
    let p = profile(
        provider_and_agent(),
        vec![wire(
            "w",
            WireKind::Then,
            ("provider", "chat"),
            ("agent", "model"),
        )],
    );
    assert_eq!(codes(&check_profile(&p, |_| true)), ["wire.kind_mismatch"]);
}

#[test]
fn missing_endpoints_are_reported() {
    let p = profile(
        provider_and_agent(),
        vec![wire(
            "w",
            WireKind::Uses,
            ("provider", "nope"),
            ("agent", "model"),
        )],
    );
    assert_eq!(
        codes(&check_profile(&p, |_| true)),
        ["wire.endpoint_missing"]
    );
}

#[test]
fn an_input_of_one_takes_one_wire_and_many_takes_several() {
    let mut nodes = provider_and_agent();
    nodes.push(node(
        "backup",
        vec![port(
            "chat",
            "llm.chat@1",
            Direction::Output,
            WireKind::Uses,
        )],
    ));
    let wires = vec![
        wire(
            "a",
            WireKind::Uses,
            ("provider", "chat"),
            ("agent", "model"),
        ),
        wire("b", WireKind::Uses, ("backup", "chat"), ("agent", "model")),
    ];
    assert_eq!(
        codes(&check_profile(
            &profile(nodes.clone(), wires.clone()),
            |_| true
        )),
        ["port.too_many_wires"]
    );
    nodes[1].ports[0].cardinality = Cardinality::Many;
    assert!(check_profile(&profile(nodes, wires), |_| true).is_empty());
}

#[test]
fn an_unwired_required_input_warns_that_the_node_will_be_disabled() {
    let mut nodes = provider_and_agent();
    nodes[1].ports[0].required = true;
    let diagnostics = check_profile(&profile(nodes, vec![]), |_| true);
    assert_eq!(codes(&diagnostics), ["port.required_unwired"]);
    assert_eq!(diagnostics[0].severity, Severity::Warning);
}

#[test]
fn a_missing_module_is_information_not_an_error() {
    let mut nodes = provider_and_agent();
    nodes[0].module_id = Some("someone/providers@0.1.0".into());
    let diagnostics = check_profile(&profile(nodes, vec![]), |_| false);
    assert_eq!(codes(&diagnostics), ["node.module_not_installed"]);
    assert_eq!(diagnostics[0].severity, Severity::Info);
}

#[test]
fn ids_are_unique_and_references_exist() {
    let mut nodes = provider_and_agent();
    nodes[1].id = "provider".into();
    nodes[0].internal_workflow_id = Some("ghost".into());
    let mut p = profile(nodes, vec![]);
    p.main_workflow_id = "missing".into();
    let found = codes(&check_profile(&p, |_| true)).join(" ");
    for code in [
        "profile.main_missing",
        "node.duplicate",
        "node.internal_workflow_missing",
    ] {
        assert!(found.contains(code), "{code} not in {found}");
    }
}
