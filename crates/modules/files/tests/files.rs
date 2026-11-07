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

/// The errors of deliveries that failed in `id`.
fn failures(core: &Core, id: &str) -> Vec<String> {
    page(core, id)
        .events
        .into_iter()
        .filter_map(|e| match e.kind {
            BusEventKind::Failed { error, .. } => Some(error),
            _ => None,
        })
        .collect()
}

/// A workflow `go` → `read` (path from the value) and `put` → `write`, both at `base`.
fn reader_and_writer(base: &Path, read: Value, write: Value) -> Profile {
    let port = |id: &str, capability: &str, direction: &str| json!({ "id": id, "capability": capability, "direction": direction, "kind": "then" });
    let mut read_config = json!({ "dir": base });
    read_config
        .as_object_mut()
        .unwrap()
        .extend(read.as_object().unwrap().clone());
    let mut write_config = json!({ "dir": base });
    write_config
        .as_object_mut()
        .unwrap()
        .extend(write.as_object().unwrap().clone());
    let wire = |id: &str, from: (&str, &str), to: (&str, &str)| {
        json!({ "id": id, "kind": "then",
                "from": { "node_id": from.0, "port_id": from.1 },
                "to": { "node_id": to.0, "port_id": to.1 } })
    };
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "ports": [port("go", "text@1", "input"), port("put", "text@1", "input"),
                      port("text", "text@1", "output")],
            "nodes": [
                { "id": "read", "type": "read", "module_id": "regulus/files@0.1.0",
                  "config": read_config,
                  "ports": [port("in", "text@1", "input"), port("out", "text@1", "output")] },
                { "id": "write", "type": "write", "module_id": "regulus/files@0.1.0",
                  "config": write_config,
                  "ports": [port("in", "text@1", "input")] },
            ],
            "wires": [
                wire("1", ("self", "go"), ("read", "in")),
                wire("2", ("read", "out"), ("self", "text")),
                wire("3", ("self", "put"), ("write", "in")),
            ] }]
    }))
    .unwrap()
}

/// Waits for the bus of `id` to go idle.
fn idle(core: &Core, id: &str) {
    let deadline = Instant::now() + WAIT;
    while page(core, id).pending > 0 {
        assert!(Instant::now() < deadline, "the bus never went idle");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_path_that_comes_as_a_value_stays_inside_the_base_unless_allowed() {
    let root = temp("boundary");
    let base = root.join("base");
    fs::create_dir_all(&base).unwrap();
    fs::write(root.join("secret"), "do not read").unwrap();
    fs::write(base.join("ok.md"), "fine").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(root.join("secret"), base.join("link")).unwrap();

    let profile = reader_and_writer(&base, json!({}), json!({ "path": "../escaped.md" }));
    let core = core(profile, &root.join("data"));
    let id = start(&core, "p");
    let secret = root.join("secret");
    for path in [
        json!("ok.md"),
        json!("../secret"),
        json!(secret),
        json!("link"),
    ] {
        send(&core, &id, "go", path);
    }
    send(&core, &id, "put", json!("out"));
    idle(&core, &id);
    let texts: Vec<Value> = page(&core, &id)
        .events
        .into_iter()
        .filter(|e| e.from.node_id == "self" && e.from.port_id == "text")
        .filter_map(|e| match e.kind {
            BusEventKind::Emitted { value } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(texts, [json!("fine")], "nothing outside was read");
    let refused = failures(&core, &id);
    let outside = refused
        .iter()
        .filter(|e| e.contains("allow_outside"))
        .count();
    assert_eq!(outside, if cfg!(unix) { 4 } else { 3 }, "{refused:?}");
    assert!(
        !root.join("escaped.md").exists(),
        "nothing outside was written"
    );

    let allowed = reader_and_writer(
        &base,
        json!({ "allow_outside": "yes" }),
        json!({ "path": "../escaped.md", "allow_outside": "yes" }),
    );
    let core = self::core(allowed, &root.join("data"));
    let id = start(&core, "p");
    send(&core, &id, "go", json!("../secret"));
    send(&core, &id, "put", json!("out"));
    assert_eq!(until(&core, &id, "self", "text", 1), [json!("do not read")]);
    assert_eq!(fs::read_to_string(root.join("escaped.md")).unwrap(), "out");
}

#[test]
fn a_glob_read_stops_at_its_total_cap_after_sending_what_fit() {
    let root = temp("total");
    fs::create_dir_all(root.join("specs")).unwrap();
    fs::write(root.join("specs/a.md"), "spec a").unwrap();
    fs::write(root.join("specs/b.md"), "spec b").unwrap();
    let read = json!({ "path": "specs/*.md", "each": "yes", "max_total_bytes": 10 });
    let core = core(
        reader_and_writer(&root, read, json!({})),
        &root.join("data"),
    );
    let id = start(&core, "p");
    send(&core, &id, "go", json!("now"));
    idle(&core, &id);
    let sent: Vec<Value> = page(&core, &id)
        .events
        .into_iter()
        .filter(|e| e.from.node_id == "read")
        .filter_map(|e| match e.kind {
            BusEventKind::Emitted { value } => Some(value),
            _ => None,
        })
        .collect();
    assert_eq!(sent, [json!("spec a")], "the first went out before the cap");
    let refused = failures(&core, &id);
    assert!(
        refused.iter().any(|e| e.contains("max_total_bytes")),
        "{refused:?}"
    );
}

#[test]
fn write_replaces_the_file_whole_and_leaves_nothing_beside_it() {
    let root = temp("atomic");
    let write = json!({ "path": "plan.md", "mode": "append" });
    let core = core(
        reader_and_writer(&root, json!({}), write),
        &root.join("data"),
    );
    let id = start(&core, "p");
    for line in ["one", "two", "three"] {
        send(&core, &id, "put", json!(line));
    }
    idle(&core, &id);
    assert!(failures(&core, &id).is_empty());
    assert_eq!(
        fs::read_to_string(root.join("plan.md")).unwrap(),
        "one\ntwo\nthree"
    );
    let left: Vec<String> = fs::read_dir(&root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "data")
        .collect();
    assert_eq!(left, ["plan.md"], "no temporary file stays");
}

#[test]
fn a_noisy_file_beside_the_watched_one_does_not_hold_back_its_change() {
    let dir = temp("noise");
    let profile = example(
        include_str!("../examples/watch.toml"),
        &dir,
        &[("notes", "notes.md")],
    );
    let core = core(profile, &dir.join("data"));
    let id = start(&core, "watch");
    let noisy = dir.join("noise.md");
    let stop = Instant::now() + Duration::from_millis(1500);
    let noise = std::thread::spawn(move || {
        while Instant::now() < stop {
            fs::write(&noisy, "x").unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
    });
    let changed = Instant::now();
    fs::write(dir.join("notes.md"), "note").unwrap();
    // debounce 200 ms: the change goes out long before the noise stops at 1.5 s.
    until(&core, &id, "self", "changed", 1);
    assert!(
        changed.elapsed() < Duration::from_millis(1000),
        "{:?}",
        changed.elapsed()
    );
    noise.join().unwrap();
}
