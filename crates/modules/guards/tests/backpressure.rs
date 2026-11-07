//! The example `examples/backpressure.toml` against a real core with `process`: a task
//! goes round retry → budget → command → gate until the gate passes or the budget is spent.

use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore, Storage};
use regulus_module_guards::Guards;
use regulus_module_process::Process;
use regulus_proto::{method, BusEventKind, Diagnostic, EventPage, Profile};
use serde_json::{json, Value};

const EXAMPLE: &str = include_str!("../examples/backpressure.toml");

fn out(core: &Core, id: &str, count: usize) -> Vec<(String, Value)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        let got: Vec<(String, Value)> = page
            .events
            .into_iter()
            .filter(|e| e.from.node_id == "self" && e.from.port_id != "task")
            .filter_map(|e| match e.kind {
                BusEventKind::Emitted { value } => Some((e.from.port_id, value)),
                _ => None,
            })
            .collect();
        if got.len() >= count {
            return got;
        }
        assert!(Instant::now() < deadline, "only {got:?} came out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn a_task_passes_on_its_second_attempt_and_the_next_one_runs_out_of_budget() {
    let profile: Profile = toml::from_str(EXAMPLE).unwrap();
    let data = std::env::temp_dir().join(format!("regulus-backpressure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Process::new()), Box::new(Guards::new())];
    let storage = Storage {
        data_dir: Some(data.clone()),
        ..Storage::default()
    };
    let core = Core::build_with(modules, &[], profiles, storage);
    let check: Vec<Diagnostic> = serde_json::from_value(
        core.dispatch(method::PROFILE_CHECK, json!({ "id": "ralph" }))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(check, []);
    let start = || {
        let started = core
            .dispatch(method::EXECUTION_START, json!({ "profile_id": "ralph" }))
            .unwrap();
        started["id"].as_str().unwrap().to_string()
    };
    let send = |id: &str, task: &str| {
        core.dispatch(
            method::WORKFLOW_SEND,
            json!({ "execution_id": id, "port": "task", "value": task }),
        )
        .unwrap();
    };
    let id = start();
    send(&id, "alpha");
    assert_eq!(out(&core, &id, 1), [("passed".to_string(), json!("2"))]);
    // The budget is per execution: three attempts in, the fourth is refused.
    send(&id, "beta");
    assert_eq!(
        out(&core, &id, 2)[1],
        ("exceeded".to_string(), json!("beta"))
    );
    // A fresh execution: the work itself fails and the error node reports it.
    let id = start();
    send(&id, "no/such");
    let (port, error) = out(&core, &id, 1).remove(0);
    assert_eq!(port, "error");
    assert_eq!(error["from"]["node_id"], "work");
    for id in ["exec-1", "exec-2"] {
        core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
            .unwrap();
    }
    let _ = std::fs::remove_dir_all(&data);
}
