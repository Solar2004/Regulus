//! The process nodes against a real core: values in through `workflow.send`, what the
//! nodes emit out through the workflow's own outputs.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore, Storage};
use regulus_module_process::Process;
use regulus_proto::{method, BusEventKind, EventPage, Profile};
use serde_json::{json, Value};

const MODULE: &str = "regulus/process@0.1.0";

fn port(id: &str, capability: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": "then",
            "cardinality": if direction == "input" { "many" } else { "one" } })
}

/// One node of `node_type` between the workflow's input `in` and an output per entry of
/// `outputs` (`(port, capability)`), wired by the same ids.
fn profile(node_type: &str, config: Value, input: &str, outputs: &[(&str, &str)]) -> Profile {
    let mut ports = vec![port("in", input, "input")];
    let mut own = vec![port("in", input, "input")];
    let mut wires = vec![json!({ "id": "w-in", "kind": "then",
        "from": { "node_id": "self", "port_id": "in" }, "to": { "node_id": "n", "port_id": "in" } })];
    for (id, capability) in outputs {
        ports.push(port(id, capability, "output"));
        own.push(port(id, capability, "output"));
        wires.push(json!({ "id": format!("w-{id}"), "kind": "then",
            "from": { "node_id": "n", "port_id": id }, "to": { "node_id": "self", "port_id": id } }));
    }
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "ports": own, "wires": wires, "nodes": [
            { "id": "n", "type": node_type, "module_id": MODULE, "config": config, "ports": ports }
        ]}]
    }))
    .unwrap()
}

fn data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-process-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn start(profile: Profile, data: &Path) -> (Arc<Core>, String) {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let storage = Storage {
        data_dir: Some(data.to_path_buf()),
        ..Storage::default()
    };
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Process::new())];
    let core = Core::build_with(modules, &[], profiles, storage);
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    let id = started["id"].as_str().unwrap().to_string();
    (core, id)
}

fn send(core: &Core, id: &str, value: Value) {
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": "in", "value": value }),
    )
    .unwrap();
}

/// What left the workflow's outputs, `(port, value)`, once `count` of them did.
fn outputs(core: &Core, id: &str, count: usize) -> Vec<(String, Value)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        let out: Vec<(String, Value)> = page
            .events
            .into_iter()
            .filter(|e| e.from.node_id == "self" && e.from.port_id != "in")
            .filter_map(|e| match e.kind {
                BusEventKind::Emitted { value } => Some((e.from.port_id, value)),
                _ => None,
            })
            .collect();
        if out.len() >= count {
            return out;
        }
        assert!(Instant::now() < deadline, "only {out:?} came out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn stop(core: &Core, id: &str) {
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
}

#[test]
fn a_command_prints_on_stdout_or_fails_with_its_exit() {
    let data = data_dir("command");
    let config = json!({ "command": "sh",
        "args": ["-c", "test \"$1\" != bad || { echo oops >&2; exit 3; }; printf '%s!\\n' \"$1\"", "sh", "{{input}}"] });
    let outs = [("stdout", "text@1"), ("exit", "exit@1"), ("fail", "exit@1")];
    let (core, id) = start(profile("command", config, "text@1", &outs), &data);
    send(&core, &id, json!("hi"));
    let got = outputs(&core, &id, 2);
    assert_eq!(got[0].0, "exit");
    assert_eq!(got[0].1["code"], 0);
    assert_eq!(got[1], ("stdout".into(), json!("hi!")));
    send(&core, &id, json!("bad"));
    let got = outputs(&core, &id, 4);
    assert_eq!(got[3].0, "fail");
    assert_eq!(got[3].1["code"], 3);
    assert_eq!(got[3].1["stderr"], "oops");
    // It ran in the node's own directory.
    let pwd = json!({ "command": "pwd" });
    let (core, id) = start(profile("command", pwd, "text@1", &outs), &data);
    send(&core, &id, json!(""));
    let dir = outputs(&core, &id, 2)[1].1.as_str().unwrap().to_string();
    assert!(dir.starts_with(data.to_str().unwrap()), "{dir}");
    stop(&core, &id);
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn a_command_reads_stdin_and_a_missing_program_fails_without_starting() {
    let data = data_dir("stdin");
    let outs = [("stdout", "text@1"), ("fail", "exit@1")];
    let cat = json!({ "command": "tr", "args": ["a-z", "A-Z"], "stdin": "yes" });
    let (core, id) = start(profile("command", cat, "text@1", &outs), &data);
    send(&core, &id, json!("shout"));
    assert_eq!(outputs(&core, &id, 1)[0], ("stdout".into(), json!("SHOUT")));
    let missing = json!({ "command": "regulus-no-such-program" });
    let (core, id) = start(profile("command", missing, "text@1", &outs), &data);
    send(&core, &id, json!("x"));
    let (port, exit) = outputs(&core, &id, 1).remove(0);
    assert_eq!(port, "fail");
    assert!(exit["code"].is_null());
    assert!(
        exit["error"].as_str().unwrap().contains("cannot run"),
        "{exit}"
    );
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn a_gate_forwards_the_same_value_on_pass_or_fail() {
    let data = data_dir("gate");
    let config = json!({ "command": "sh", "args": ["-c", "test \"$1\" = ok", "sh", "{{input}}"] });
    let value = json!({ "text": "ok" });
    let outs = [("pass", "doc@1"), ("fail", "doc@1")];
    let (core, id) = start(profile("gate", config.clone(), "doc@1", &outs), &data);
    // Any capability: the value is passed as JSON, `{{input}}` is its JSON text.
    send(&core, &id, value.clone());
    assert_eq!(outputs(&core, &id, 1)[0], ("fail".into(), value));
    let (core, id) = start(
        profile(
            "gate",
            config,
            "text@1",
            &[("pass", "text@1"), ("fail", "text@1")],
        ),
        &data,
    );
    send(&core, &id, json!("ok"));
    send(&core, &id, json!("ko"));
    let mut got = outputs(&core, &id, 2);
    got.sort_by_key(|(port, _)| port.clone());
    assert_eq!(
        got,
        [("fail".into(), json!("ko")), ("pass".into(), json!("ok"))]
    );
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn a_gate_that_changes_the_capability_does_not_start() {
    let data = data_dir("gate-shape");
    let mut profiles = ProfileStore::default();
    let config = json!({ "command": "true" });
    profiles
        .insert(profile("gate", config, "text@1", &[("pass", "exit@1")]))
        .unwrap();
    let storage = Storage {
        data_dir: Some(data.clone()),
        ..Storage::default()
    };
    let core = Core::build_with(vec![Box::new(Process::new())], &[], profiles, storage);
    let started = core.dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }));
    let state = started.map(|s| s["state"].clone()).unwrap();
    assert_eq!(state["status"], "failed");
    assert!(state["reason"]
        .as_str()
        .unwrap()
        .contains("a gate forwards"));
}

fn alive(pid: u64) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

fn pgrep(pattern: &str) -> String {
    let out = std::process::Command::new("pgrep")
        .args(["-f", pattern])
        .output()
        .unwrap();
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[test]
fn a_job_times_out_and_logs_what_it_printed() {
    let data = data_dir("job-timeout");
    let config = json!({ "command": "sh", "args": ["-c", "echo first; echo second; exec sleep 30"],
                         "timeout_ms": 300, "tail_lines": 1 });
    let outs = [
        ("started", "job@1"),
        ("done", "exit@1"),
        ("fail", "exit@1"),
        ("timeout", "exit@1"),
    ];
    let (core, id) = start(profile("job", config, "text@1", &outs), &data);
    send(&core, &id, json!("go"));
    let got = outputs(&core, &id, 2);
    assert_eq!(got[0].0, "started");
    assert_eq!(got[1].0, "timeout");
    let exit = &got[1].1;
    assert_eq!(exit["timed_out"], true);
    assert_eq!(exit["job"], 1);
    assert_eq!(exit["tail"], "second");
    let log = std::fs::read_to_string(exit["log"].as_str().unwrap()).unwrap();
    assert_eq!(log, "first\nsecond\n");
    assert!(!alive(got[0].1["pid"].as_u64().unwrap()));
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn stopping_the_execution_kills_a_job_and_all_it_started() {
    let data = data_dir("job-stop");
    // A unique duration so pgrep finds only this test's sleeps: one is a grandchild.
    let mark = format!("sleep 41.{}", std::process::id() % 1000);
    let script = format!("{mark} & {mark}; wait");
    let config = json!({ "command": "sh", "args": ["-c", script], "grace_ms": 200 });
    let outs = [("started", "job@1"), ("done", "exit@1"), ("fail", "exit@1")];
    let (core, id) = start(profile("job", config, "text@1", &outs), &data);
    send(&core, &id, json!("go"));
    let pid = outputs(&core, &id, 1)[0].1["pid"].as_u64().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while pgrep(&mark).lines().count() < 2 {
        assert!(Instant::now() < deadline, "the sleeps never started");
        std::thread::sleep(Duration::from_millis(10));
    }
    // A second value while it runs is refused: one job at a time by default.
    send(&core, &id, json!("again"));
    let refused = |page: &EventPage| {
        page.events.iter().any(|e| {
            matches!(&e.kind, BusEventKind::Failed { error, .. }
                if error.contains("already runs 1 at once"))
        })
    };
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        if refused(&page) {
            break;
        }
        assert!(Instant::now() < deadline, "the second job was not refused");
        std::thread::sleep(Duration::from_millis(10));
    }
    stop(&core, &id);
    assert_eq!(pgrep(&mark), "", "left behind");
    assert!(!alive(pid), "the job was not reaped");
    let _ = std::fs::remove_dir_all(&data);
}

#[test]
fn a_terminal_runs_what_it_is_typed_and_dies_with_the_execution() {
    let data = data_dir("terminal");
    let config = json!({ "command": "sh" });
    let (core, id) = start(
        profile(
            "terminal",
            config,
            "text@1",
            &[("out", "text@1"), ("exit", "exit@1")],
        ),
        &data,
    );
    // Quotes keep the echo of the typed line from matching: only the output does.
    send(&core, &id, json!("echo re'gu'lus"));
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let printed: String = outputs(&core, &id, 1)
            .into_iter()
            .filter_map(|(port, v)| (port == "out").then(|| v.as_str().unwrap().to_string()))
            .collect();
        if printed.contains("regulus") {
            break;
        }
        assert!(Instant::now() < deadline, "printed {printed:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
    send(&core, &id, json!("exit 4"));
    let deadline = Instant::now() + Duration::from_secs(10);
    let exit = loop {
        if let Some((_, exit)) = outputs(&core, &id, 1)
            .into_iter()
            .find(|(p, _)| p == "exit")
        {
            break exit;
        }
        assert!(Instant::now() < deadline, "the shell never exited");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(exit["code"], 4);
    stop(&core, &id);
    let _ = std::fs::remove_dir_all(&data);
}
