//! The files nodes against a real core, with the example profiles where they fit.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore, Storage};
use regulus_module_files::Files;
use regulus_module_flow::Flow;
use regulus_proto::{method, BusEventKind, EventPage, Profile};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(5);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-files-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// An example profile with every node's `dir`, and `path` where `paths` says, moved under
/// `dir`.
fn example(source: &str, dir: &Path, paths: &[(&str, &str)]) -> Profile {
    let mut profile: Profile = toml::from_str(source).unwrap();
    for node in &mut profile.workflows[0].nodes {
        if node.config.contains_key("dir") {
            node.config.insert("dir".into(), json!(dir));
        }
        if let Some((_, file)) = paths.iter().find(|(id, _)| *id == node.id) {
            node.config.insert("path".into(), json!(dir.join(file)));
        }
    }
    profile
}

fn core(profile: Profile, data: &Path) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Files::new()), Box::new(Flow::new())];
    let storage = Storage {
        data_dir: Some(data.to_path_buf()),
        ..Storage::default()
    };
    Core::build_with(modules, &[], profiles, storage)
}

fn start(core: &Core, profile: &str) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": profile }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    started["id"].as_str().unwrap().to_string()
}

fn send(core: &Core, id: &str, port: &str, value: Value) {
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": port, "value": value }),
    )
    .unwrap();
}

fn page(core: &Core, id: &str) -> EventPage {
    serde_json::from_value(
        core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
            .unwrap(),
    )
    .unwrap()
}

fn emitted(core: &Core, id: &str, node: &str, port: &str) -> Vec<Value> {
    page(core, id)
        .events
        .into_iter()
        .filter(|e| e.from.node_id == node && e.from.port_id == port)
        .filter_map(|e| match e.kind {
            BusEventKind::Emitted { value } => Some(value),
            BusEventKind::Failed { error, .. } => panic!("delivery failed: {error}"),
            _ => None,
        })
        .collect()
}

/// Polls until `node.port` has emitted `count` values.
fn until(core: &Core, id: &str, node: &str, port: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + WAIT;
    loop {
        let got = emitted(core, id, node, port);
        if got.len() >= count && page(core, id).pending == 0 {
            return got;
        }
        assert!(Instant::now() < deadline, "{node}.{port} got {got:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_ralph_skeleton_writes_one_plan_line_per_turn() {
    let dir = temp("ralph");
    fs::write(dir.join("PROMPT.md"), "Fix the failing test\n").unwrap();
    let profile = example(include_str!("../examples/ralph.toml"), &dir, &[]);
    let core = core(profile, &dir.join("data"));
    let id = start(&core, "ralph");
    send(&core, &id, "go", json!("start"));
    let done = until(&core, &id, "self", "done", 1);
    let plan = dir.join("fix_plan.md");
    assert_eq!(done, [json!(plan)]);
    assert_eq!(
        fs::read_to_string(&plan).unwrap(),
        "- [ ] Fix the failing test\n".repeat(3)
    );
    assert_eq!(emitted(&core, &id, "loop", "body").len(), 3);
}

#[test]
fn a_document_speaks_on_start_and_a_watch_sends_each_settled_change() {
    let dir = temp("watch");
    let profile = example(
        include_str!("../examples/watch.toml"),
        &dir,
        &[("notes", "notes.md")],
    );
    let core = core(profile, &dir.join("data"));
    let id = start(&core, "watch");
    assert_eq!(
        until(&core, &id, "self", "rules", 1),
        [json!("Answer in Spanish. Keep it short.")]
    );
    let notes = dir.join("notes.md");
    fs::write(&notes, "first").unwrap();
    fs::write(&notes, "second").unwrap();
    assert_eq!(until(&core, &id, "self", "changed", 1), [json!(notes)]);
    let quoted = until(&core, &id, "self", "notes", 1);
    assert_eq!(quoted.last(), Some(&json!("notes.md now says: second")));

    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let before = emitted(&core, &id, "notes", "changed").len();
    fs::write(&notes, "after the stop").unwrap();
    std::thread::sleep(Duration::from_millis(600));
    assert_eq!(emitted(&core, &id, "notes", "changed").len(), before);
}

#[test]
fn read_sends_each_globbed_file_and_write_lands_in_the_node_dir() {
    let dir = temp("glob");
    fs::create_dir_all(dir.join("specs")).unwrap();
    fs::write(dir.join("specs/b.md"), "spec b").unwrap();
    fs::write(dir.join("specs/a.md"), "spec a").unwrap();
    let port = |id: &str, capability: &str, direction: &str| json!({ "id": id, "capability": capability, "direction": direction, "kind": "then" });
    let profile: Profile = serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "ports": [port("go", "text@1", "input"), port("written", "path@1", "output")],
            "nodes": [
                { "id": "specs", "type": "read", "module_id": "regulus/files@0.1.0",
                  "config": { "path": "specs/*.md", "dir": dir, "each": "yes" },
                  "ports": [port("in", "text@1", "input"), port("out", "text@1", "output")] },
                { "id": "out", "type": "write", "module_id": "regulus/files@0.1.0",
                  "config": { "path": "last.md" },
                  "ports": [port("in", "text@1", "input"), port("written", "path@1", "output")] },
            ],
            "wires": [
                { "id": "1", "kind": "then", "from": { "node_id": "self", "port_id": "go" },
                  "to": { "node_id": "specs", "port_id": "in" } },
                { "id": "2", "kind": "then", "from": { "node_id": "specs", "port_id": "out" },
                  "to": { "node_id": "out", "port_id": "in" } },
                { "id": "3", "kind": "then", "from": { "node_id": "out", "port_id": "written" },
                  "to": { "node_id": "self", "port_id": "written" } },
            ] }]
    }))
    .unwrap();
    let data = dir.join("data");
    let core = core(profile, &data);
    let id = start(&core, "p");
    send(&core, &id, "go", json!("again"));
    assert_eq!(
        until(&core, &id, "specs", "out", 2),
        [json!("spec a"), json!("spec b")]
    );
    let written = until(&core, &id, "self", "written", 2);
    let last = PathBuf::from(written[1].as_str().unwrap());
    assert!(last.starts_with(&data), "{last:?} is under the node dir");
    assert_eq!(fs::read_to_string(&last).unwrap(), "spec b", "overwritten");
}
