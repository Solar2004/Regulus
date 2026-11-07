//! The `call` node against a real core, with the example profile (`examples/call.toml`):
//! the greeting comes back from a child execution and leaves the outer workflow.

use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore};
use regulus_module_flow::Flow;
use regulus_module_hello::Hello;
use regulus_proto::{method, BusEventKind, Diagnostic, EventPage, Execution, Profile};
use serde_json::{json, Value};

const EXAMPLE: &str = include_str!("../examples/call.toml");

fn example() -> Profile {
    toml::from_str(EXAMPLE).unwrap()
}

fn core(profile: Profile) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Flow::new()), Box::new(Hello::new())];
    Core::build(modules, &[], profiles)
}

fn events(core: &Core, id: &str) -> EventPage {
    serde_json::from_value(
        core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
            .unwrap(),
    )
    .unwrap()
}

/// What left `self.<port>` in `id`, once its bus and its children are done.
fn answers(core: &Core, id: &str, port: &str) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page = events(core, id);
        if page.pending == 0 {
            return page
                .events
                .into_iter()
                .filter(|e| e.from.node_id == "self" && e.from.port_id == port)
                .filter_map(|e| match e.kind {
                    BusEventKind::Emitted { value } => Some(value),
                    _ => None,
                })
                .collect();
        }
        assert!(Instant::now() < deadline, "the call never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_example_greets_through_one_child_per_value() {
    let profile = example();
    let core = core(profile);
    let check: Vec<Diagnostic> = serde_json::from_value(
        core.dispatch(method::PROFILE_CHECK, json!({ "id": "call" }))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(check, []);

    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "call" }))
        .unwrap();
    let id = started["id"].as_str().unwrap().to_string();
    for name in ["Alexander", "Regulus"] {
        core.dispatch(
            method::WORKFLOW_SEND,
            json!({ "execution_id": id, "port": "ask", "value": name }),
        )
        .unwrap();
    }
    let mut replies = answers(&core, &id, "reply");
    replies.sort_by_key(|v| v.to_string());
    assert_eq!(
        replies,
        [json!("Hello, Alexander!"), json!("Hello, Regulus!")]
    );
    let done = answers(&core, &id, "done");
    assert_eq!(done.len(), 2);
    assert!(done
        .iter()
        .all(|d| d["state"]["status"] == "stopped" && d["workflow_id"] == "greeter"));

    let executions: Vec<Execution> =
        serde_json::from_value(core.dispatch(method::EXECUTION_LIST, json!({})).unwrap()).unwrap();
    let children: Vec<&Execution> = executions
        .iter()
        .filter(|e| e.parent.as_deref() == Some(id.as_str()))
        .collect();
    assert_eq!(children.len(), 2);
    for child in children {
        assert!(events(&core, &child.id)
            .events
            .iter()
            .all(|e| e.execution_id == child.id));
    }
}

#[test]
fn the_check_catches_a_call_to_a_missing_workflow_or_one_without_ports() {
    let codes = |target: &str| {
        let mut profile = example();
        profile.workflows[0].nodes[0]
            .config
            .insert("workflow".into(), json!(target));
        // A workflow with no ports at all: nothing goes in, nothing comes back.
        profile
            .workflows
            .push(serde_json::from_value(json!({ "id": "plain", "kind": "extra" })).unwrap());
        let core = core(profile);
        let check: Vec<Diagnostic> = serde_json::from_value(
            core.dispatch(method::PROFILE_CHECK, json!({ "id": "call" }))
                .unwrap(),
        )
        .unwrap();
        check.into_iter().map(|d| d.code).collect::<Vec<_>>()
    };
    assert_eq!(codes("nowhere"), ["field.workflow_missing"]);
    assert_eq!(codes("plain"), ["field.workflow_no_output"]);
}

#[test]
fn a_renamed_output_warns_and_a_missing_workflow_does_not_start() {
    let mut profile = example();
    let greeter = profile
        .workflows
        .iter_mut()
        .find(|w| w.id == "greeter")
        .unwrap();
    greeter.ports[1].id = "result".into();
    greeter.wires[1].to.port_id = "result".into();
    let checked = core(profile);
    let check: Vec<Diagnostic> = serde_json::from_value(
        checked
            .dispatch(method::PROFILE_CHECK, json!({ "id": "call" }))
            .unwrap(),
    )
    .unwrap();
    let codes: Vec<&str> = check.iter().map(|d| d.code.as_str()).collect();
    assert_eq!(codes, ["field.workflow_ports_mismatch"]);

    let mut profile = example();
    profile.workflows[0].nodes[0]
        .config
        .insert("workflow".into(), json!("nowhere"));
    let error = core(profile)
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "call" }))
        .unwrap_err();
    assert!(
        error.message.contains("`nowhere` does not exist"),
        "{}",
        error.message
    );
}
