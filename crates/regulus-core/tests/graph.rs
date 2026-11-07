//! The wiring rules of docs/02-architecture.md §4, checked by `check_profile`.

use regulus_core::graph::{check_profile, flatten, start_order};
use regulus_proto::{
    Cardinality, Diagnostic, Direction, Node, Port, PortRef, Profile, Severity, Wire, WireKind,
    Workflow, WorkflowKind,
};
use serde_json::{json, Value};

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
        lazy: false,
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
        ports: vec![],
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

/// An agent whose tools come from a channel that has the agent as a participant.
fn channel_cycle(lazy: bool) -> Workflow {
    let mut participants = port(
        "participants",
        "participant@1",
        Direction::Input,
        WireKind::Uses,
    );
    participants.lazy = lazy;
    let nodes = vec![
        node(
            "agent",
            vec![
                port(
                    "participant",
                    "participant@1",
                    Direction::Output,
                    WireKind::Uses,
                ),
                port("tools", "toolset@1", Direction::Input, WireKind::Uses),
            ],
        ),
        node(
            "toolbox",
            vec![
                port("tools", "tool@1", Direction::Input, WireKind::Uses),
                port("toolset", "toolset@1", Direction::Output, WireKind::Uses),
            ],
        ),
        node(
            "channel",
            vec![
                participants,
                port("tools", "tool@1", Direction::Output, WireKind::Uses),
            ],
        ),
    ];
    let wires = vec![
        wire(
            "w1",
            WireKind::Uses,
            ("agent", "participant"),
            ("channel", "participants"),
        ),
        wire(
            "w2",
            WireKind::Uses,
            ("channel", "tools"),
            ("toolbox", "tools"),
        ),
        wire(
            "w3",
            WireKind::Uses,
            ("toolbox", "toolset"),
            ("agent", "tools"),
        ),
    ];
    profile(nodes, wires).workflows.remove(0)
}

fn ids(workflow: &Workflow) -> Vec<&str> {
    start_order(workflow)
        .iter()
        .map(|n| n.id.as_str())
        .collect()
}

#[test]
fn a_cycle_keeps_the_declared_order() {
    assert_eq!(ids(&channel_cycle(false)), ["agent", "toolbox", "channel"]);
}

#[test]
fn a_lazy_input_does_not_order_the_start() {
    assert_eq!(ids(&channel_cycle(true)), ["channel", "toolbox", "agent"]);
}

// --- workflows with ports --------------------------------------------------------------

fn text_port(id: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": "text@1", "direction": direction, "kind": "then" })
}

fn then_wire(id: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": "then",
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

/// `main` asks `greeter`, placed as the box `greet`; `greeter` answers through `hi`.
fn boxed() -> Value {
    json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main",
              "ports": [text_port("ask", "input"), text_port("reply", "output")],
              "nodes": [{ "id": "greet", "type": "workflow", "internal_workflow_id": "greeter" }],
              "wires": [
                then_wire("in", ("self", "ask"), ("greet", "ask")),
                then_wire("out", ("greet", "reply"), ("self", "reply")),
              ] },
            { "id": "greeter", "kind": "extra",
              "ports": [text_port("ask", "input"), text_port("reply", "output")],
              "nodes": [{ "id": "hi", "type": "t", "module_id": "t/x@1.0.0",
                          "ports": [text_port("in", "input"), text_port("out", "output")] }],
              "wires": [
                then_wire("in", ("self", "ask"), ("hi", "in")),
                then_wire("out", ("hi", "out"), ("self", "reply")),
              ] },
        ]
    })
}

fn checked(profile: Value) -> Vec<Diagnostic> {
    check_profile(&serde_json::from_value(profile).unwrap(), |_| true)
}

#[test]
fn a_workflow_with_ports_placed_as_a_box_is_clean() {
    assert_eq!(checked(boxed()), []);
}

#[test]
fn a_workflow_with_ports_is_valid_without_any_caller() {
    let mut profile = boxed();
    profile["workflows"][0]["ports"] = json!([]);
    profile["workflows"][0]["wires"] = json!([]);
    profile["workflows"][1]["wires"] = json!([]);
    assert_eq!(checked(profile), []);
}

#[test]
fn a_capability_mismatch_is_caught_on_both_sides_of_the_boundary() {
    let mut profile = boxed();
    profile["workflows"][1]["ports"][0]["capability"] = json!("image@1");
    let diagnostics = checked(profile);
    let mismatches: Vec<(Option<&str>, Option<&str>)> = diagnostics
        .iter()
        .filter(|d| d.code == "wire.capability_mismatch")
        .map(|d| (d.workflow_id.as_deref(), d.wire_id.as_deref()))
        .collect();
    assert_eq!(
        mismatches,
        [(Some("main"), Some("in")), (Some("greeter"), Some("in"))],
        "{diagnostics:#?}"
    );
}

#[test]
fn a_wire_to_a_missing_workflow_or_box_port_dangles() {
    let mut profile = boxed();
    profile["workflows"][0]["wires"][0]["to"]["port_id"] = json!("nope");
    profile["workflows"][1]["wires"][1]["to"]["port_id"] = json!("nope");
    let diagnostics = checked(profile);
    assert_eq!(
        codes(&diagnostics),
        ["wire.endpoint_missing", "wire.endpoint_missing"]
    );
}

#[test]
fn inside_a_workflow_its_inputs_feed_and_its_outputs_take() {
    let mut profile = boxed();
    // Into the workflow's own input, from inside.
    profile["workflows"][1]["wires"][1]["to"]["port_id"] = json!("ask");
    let diagnostics = checked(profile);
    assert_eq!(codes(&diagnostics), ["wire.direction"]);

    let mut profile = boxed();
    profile["workflows"][1]["wires"]
        .as_array_mut()
        .unwrap()
        .push(then_wire("again", ("hi", "out"), ("self", "reply")));
    assert_eq!(codes(&checked(profile)), ["port.too_many_wires"]);
}

#[test]
fn a_workflow_cannot_end_up_inside_itself() {
    let mut profile = boxed();
    profile["workflows"][1]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": "back", "type": "workflow", "internal_workflow_id": "main" }));
    let diagnostics = checked(profile.clone());
    let found: Vec<(Option<&str>, Option<&str>)> = diagnostics
        .iter()
        .filter(|d| d.code == "workflow.embeds_itself")
        .map(|d| (d.workflow_id.as_deref(), d.node_id.as_deref()))
        .collect();
    assert_eq!(
        found,
        [
            (Some("main"), Some("greet")),
            (Some("greeter"), Some("back"))
        ]
    );
    let profile: Profile = serde_json::from_value(profile).unwrap();
    let error = flatten(&profile, profile.workflow("main").unwrap()).unwrap_err();
    assert!(error.contains("inside itself"), "{error}");
}

#[test]
fn reserved_ids_declared_box_ports_and_workflow_ports_are_checked() {
    let mut profile = boxed();
    let greeter = &mut profile["workflows"][1];
    greeter["nodes"][0]["id"] = json!("self");
    greeter["ports"]
        .as_array_mut()
        .unwrap()
        .push(text_port("ask", "input"));
    let mut uses = text_port("model", "input");
    uses["kind"] = json!("uses");
    greeter["ports"].as_array_mut().unwrap().push(uses);
    greeter["wires"] = json!([]);
    let main = &mut profile["workflows"][0];
    main["nodes"][0]["ports"] = json!([text_port("ask", "output")]);
    main["nodes"]
        .as_array_mut()
        .unwrap()
        .push(json!({ "id": "a/b", "type": "t" }));
    main["wires"] = json!([]);
    let diagnostics = checked(profile);
    let mut found = codes(&diagnostics);
    found.sort();
    assert_eq!(
        found,
        [
            "node.box_ports_mismatch",
            "node.reserved_id",
            "node.reserved_id",
            "workflow.port_duplicate",
            "workflow.port_uses",
        ],
        "{diagnostics:#?}"
    );
}

#[test]
fn a_running_copy_inlines_boxes_under_their_id() {
    let profile: Profile = serde_json::from_value(boxed()).unwrap();
    let plan = flatten(&profile, profile.workflow("main").unwrap()).unwrap();
    let nodes: Vec<&str> = plan.nodes.iter().map(|n| n.id.as_str()).collect();
    assert_eq!(nodes, ["greet", "greet/hi"]);
    let greet = plan.node("greet").unwrap();
    assert_eq!(greet.ports, profile.workflow("greeter").unwrap().ports);
    let wires: Vec<String> = plan
        .wires
        .iter()
        .map(|w| {
            format!(
                "{} {}.{} -> {}.{}",
                w.id, w.from.node_id, w.from.port_id, w.to.node_id, w.to.port_id
            )
        })
        .collect();
    assert_eq!(
        wires,
        [
            "in self.ask -> greet.ask",
            "out greet.reply -> self.reply",
            "greet/in greet.ask -> greet/hi.in",
            "greet/out greet/hi.out -> greet.reply",
        ]
    );
    assert_eq!(plan.ports, profile.workflow("main").unwrap().ports);
}

#[test]
fn a_uses_port_on_a_workflow_or_a_box_is_an_error_and_never_runs() {
    let mut uses = text_port("model", "input");
    uses["kind"] = json!("uses");
    // Only on the box node: the workflow itself is fine.
    let mut on_box = boxed();
    let mut ports = on_box["workflows"][1]["ports"].clone();
    ports.as_array_mut().unwrap().push(uses.clone());
    on_box["workflows"][0]["nodes"][0]["ports"] = ports;
    let diagnostics = checked(on_box.clone());
    let found: Vec<(&str, Severity, Option<&str>)> = diagnostics
        .iter()
        .map(|d| (d.code.as_str(), d.severity, d.node_id.as_deref()))
        .collect();
    assert!(
        found.contains(&("workflow.port_uses", Severity::Error, Some("greet"))),
        "{diagnostics:#?}"
    );
    let profile: Profile = serde_json::from_value(on_box).unwrap();
    let error = flatten(&profile, profile.workflow("main").unwrap()).unwrap_err();
    assert!(error.starts_with("box `greet`"), "{error}");

    // On the workflow: an error there, and the box inherits it.
    let mut on_workflow = boxed();
    on_workflow["workflows"][1]["ports"]
        .as_array_mut()
        .unwrap()
        .push(uses);
    let diagnostics = checked(on_workflow.clone());
    assert_eq!(codes(&diagnostics), ["workflow.port_uses"]);
    assert_eq!(diagnostics[0].severity, Severity::Error);
    let profile: Profile = serde_json::from_value(on_workflow).unwrap();
    let error = flatten(&profile, profile.workflow("greeter").unwrap()).unwrap_err();
    assert!(error.starts_with("workflow `greeter`"), "{error}");
    assert!(error.contains("nothing would serve it"), "{error}");
}

#[test]
fn declared_box_ports_must_match_cardinality_and_laziness_too() {
    let declared = |change: &dyn Fn(&mut Value)| {
        let mut profile = boxed();
        let mut ports = profile["workflows"][1]["ports"].clone();
        change(&mut ports[0]);
        profile["workflows"][0]["nodes"][0]["ports"] = ports;
        codes(&checked(profile))
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>()
    };
    assert_eq!(declared(&|_| {}), Vec::<String>::new());
    assert_eq!(
        declared(&|port| port["cardinality"] = json!("many")),
        ["node.box_ports_mismatch"]
    );
    assert_eq!(
        declared(&|port| port["lazy"] = json!(true)),
        ["node.box_ports_mismatch"]
    );
}

/// A module whose node `caller` has a `workflow` field: the id of a workflow it runs as a
/// child (`ModuleCtx::call_workflow`).
fn caller_manifest() -> regulus_proto::Manifest {
    serde_json::from_value(json!({
        "module": { "id": "t/caller@1.0.0", "name": "caller" },
        "node": [{ "type": "caller", "name": "Caller",
                   "field": [{ "id": "workflow", "type": "workflow" }] }]
    }))
    .unwrap()
}

#[test]
fn a_workflow_field_must_name_a_workflow_with_an_output() {
    let manifest = caller_manifest();
    let check = |target: Value| {
        let mut profile = boxed();
        profile["workflows"][0]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(
                json!({ "id": "c", "type": "caller", "module_id": "t/caller@1.0.0",
                          "config": { "workflow": target } }),
            );
        profile["workflows"]
            .as_array_mut()
            .unwrap()
            .push(json!({ "id": "closed", "kind": "extra",
                          "ports": [text_port("ask", "input")] }));
        let profile: Profile = serde_json::from_value(profile).unwrap();
        let found = regulus_core::graph::check_fields(&profile, |id| {
            (id == manifest.module.id).then_some(&manifest)
        });
        found.into_iter().map(|d| d.code).collect::<Vec<_>>()
    };
    assert_eq!(check(json!("greeter")), Vec::<String>::new());
    assert_eq!(check(json!("nowhere")), ["field.workflow_missing"]);
    assert_eq!(check(json!("closed")), ["field.workflow_no_output"]);
    assert_eq!(check(json!("")), ["field.workflow_unset"]);
    // A module that is not loaded is not checked.
    let profile: Profile = serde_json::from_value(boxed()).unwrap();
    assert!(regulus_core::graph::check_fields(&profile, |_| None).is_empty());
}
