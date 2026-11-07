//! The memory nodes against a real core with a store, through the example profile.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_contract_tool as tool;
use regulus_core::config::StoreConfig;
use regulus_core::{Core, Module, ModuleCtx, ModuleError, ProfileStore, Storage, Store};
use regulus_module_memory::Memory;
use regulus_module_skills::Skills;
use regulus_proto::{method, BusEventKind, EventPage, Manifest, ModuleInfo, Profile};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(5);

/// Stands for the Agent box: a node with a `hooks` input and no behavior of its own.
struct Holder(Manifest);

impl Module for Holder {
    fn manifest(&self) -> &Manifest {
        &self.0
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, _: Value) -> Result<Value, ModuleError> {
        Err(ModuleError::UnknownMethod(name.into()))
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-memory-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The example, plus an agent stand-in with the `recall` hook wired into it.
fn profile() -> Profile {
    let mut profile: Value = serde_json::to_value(
        toml::from_str::<Profile>(include_str!("../examples/memory.toml")).unwrap(),
    )
    .unwrap();
    let workflow = &mut profile["workflows"][0];
    workflow["nodes"].as_array_mut().unwrap().push(json!({
        "id": "agent", "type": "agent", "module_id": "t/holder@1.0.0",
        "ports": [{ "id": "hooks", "capability": "hook@1", "direction": "input",
                    "kind": "uses", "cardinality": "many" }] }));
    workflow["wires"].as_array_mut().unwrap().push(json!({
        "id": "agent-recall", "kind": "uses",
        "from": { "node_id": "recall", "port_id": "hook" },
        "to": { "node_id": "agent", "port_id": "hooks" } }));
    serde_json::from_value(profile).unwrap()
}

fn core(home: &std::path::Path) -> Arc<Core> {
    let holder = Manifest {
        module: ModuleInfo {
            id: "t/holder@1.0.0".into(),
            name: "holder".into(),
            requires_proto: None,
            methods: vec![],
            workflow_id: None,
            origin: None,
        },
        nodes: vec![],
    };
    let mut profiles = ProfileStore::default();
    profiles.insert(profile()).unwrap();
    let storage = Storage {
        data_dir: Some(home.join("data")),
        store: Some(Arc::new(
            Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
        )),
        ..Storage::default()
    };
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Memory::new()), Box::new(Holder(holder))];
    Core::build_with(modules, &[], profiles, storage)
}

fn start(core: &Core) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "memory" }))
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

fn emitted(core: &Core, id: &str, port: &str) -> Vec<Value> {
    page(core, id)
        .events
        .into_iter()
        .filter(|e| e.from.node_id == "self" && e.from.port_id == port)
        .filter_map(|e| match e.kind {
            BusEventKind::Emitted { value } => Some(value),
            _ => None,
        })
        .collect()
}

fn until(core: &Core, id: &str, port: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + WAIT;
    loop {
        let got = emitted(core, id, port);
        if got.len() >= count && page(core, id).pending == 0 {
            return got;
        }
        assert!(Instant::now() < deadline, "self.{port} got {got:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn the_example_remembers_recalls_reflects_and_grows_skills() {
    let home = temp("example");
    let core = core(&home);
    let id = start(&core);
    send(
        &core,
        &id,
        "remember",
        json!({ "key": "tests", "text": "Run cargo test before committing" }),
    );
    send(
        &core,
        &id,
        "remember",
        json!("Commit messages go in English"),
    );
    let written = until(&core, &id, "written", 2);
    assert_eq!(written[0]["key"], "tests");
    let fresh = written[1]["key"].as_str().unwrap().to_string();
    assert!(fresh.starts_with("notes-"), "{fresh}");

    // Matched against what was asked: only the commit entry shares a word.
    send(&core, &id, "ask", json!("how do I commit in this repo?"));
    let recall = until(&core, &id, "recall", 1);
    assert_eq!(
        recall,
        [json!(format!(
            "What you remember from earlier work (memory):\n- {fresh}: Commit messages go in English"
        ))]
    );

    // The hook answers the same as context on a prompt, and nothing on another event.
    let ask = |event: &str, text: &str| {
        core.dispatch(
            method::HOOK_DISPATCH,
            json!({ "execution_id": id, "node_id": "agent",
                    "request": { "event": event, "text": text, "raw": {} } }),
        )
        .unwrap()
    };
    let answer = ask("prompt", "run the tests");
    assert_eq!(
        answer["replies"][0]["answer"]["decision"], "context",
        "{answer}"
    );
    assert!(answer["replies"][0]["answer"]["text"]
        .as_str()
        .unwrap()
        .contains("- tests: Run cargo test before committing"));
    assert_eq!(
        ask("prompt", "zzz")["replies"][0]["answer"]["decision"],
        "none"
    );
    assert_eq!(
        ask("stop", "run the tests")["replies"][0]["answer"]["decision"],
        "none"
    );

    // What an agent sees through a Tools box.
    let target = json!({ "execution_id": id, "node_id": "notes" });
    let tools: Vec<tool::Spec> =
        serde_json::from_value(core.dispatch("memory.tools", target.clone()).unwrap()).unwrap();
    let read = tools.iter().find(|t| t.name == "memory_read").unwrap();
    let description = &read.description;
    assert!(
        description.contains("Keys stored now (2): "),
        "{description}"
    );
    assert!(
        description.contains("tests") && description.contains(&fresh),
        "{description}"
    );
    assert_eq!(tools.len(), 5);

    // Two failures make a batch; the lesson goes to the same memory.
    send(&core, &id, "fail", json!("cargo test: 2 failed"));
    send(
        &core,
        &id,
        "fail",
        json!({ "message": "exit 101", "value": "cargo build" }),
    );
    let batch = until(&core, &id, "reflection", 1);
    let batch = batch[0].as_str().unwrap();
    assert!(batch.starts_with("These attempts failed."), "{batch}");
    assert!(
        batch.contains("Failure 1:\ncargo test: 2 failed"),
        "{batch}"
    );
    assert!(
        batch.contains("Failure 2:\nexit 101\nInput: cargo build"),
        "{batch}"
    );
    send(&core, &id, "lesson", json!("Build before you test"));
    let learned = until(&core, &id, "learned", 1);
    assert!(learned[0]["key"].as_str().unwrap().starts_with("lesson-"));
    let entries = core.dispatch("memory.entries", target).unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 3, "{entries}");

    // A skill written by the library loads with the Skills module as it is.
    send(
        &core,
        &id,
        "skill",
        json!({ "name": "run-tests", "description": "How to run the tests", "body": "Run cargo test." }),
    );
    let path = until(&core, &id, "skill_path", 1)[0].clone();
    assert!(path.as_str().unwrap().ends_with("/run-tests"), "{path}");
    let skill = Skills::new()
        .call(
            "skill",
            json!({ "execution_id": "e", "node": { "id": "s", "type": "skill",
                    "module_id": "regulus/skills@0.1.0", "config": { "path": path } } }),
        )
        .unwrap();
    assert_eq!(skill["name"], "run-tests");

    // Project scope: the next execution sees the same entries.
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let next = start(&core);
    let entries = core
        .dispatch(
            "memory.entries",
            json!({ "execution_id": next, "node_id": "notes" }),
        )
        .unwrap();
    assert_eq!(entries.as_array().unwrap().len(), 3);
}

fn call_tool(memory: &Memory, config: Value, name: &str, args: Value) -> Value {
    let node = json!({ "id": "m", "type": "store", "module_id": "regulus/memory@0.1.0",
                       "config": config });
    let args = json!({ "name": name, "args": args, "caller": { "name": "dev" } });
    memory
        .call(
            tool::CALL,
            json!({ "execution_id": "e", "node": node, "args": args }),
        )
        .unwrap()
}

#[test]
fn agents_write_read_search_and_forget_through_the_tools() {
    let memory = Memory::new();
    let config = json!({ "scope": "execution", "max_entries": 2 });
    let wrote = call_tool(
        &memory,
        config.clone(),
        "memory_write",
        json!({ "key": "a", "text": "alpha one" }),
    );
    assert_eq!(wrote["data"]["by"], "dev", "{wrote}");
    call_tool(
        &memory,
        config.clone(),
        "memory_write",
        json!({ "key": "b", "text": "beta two" }),
    );
    call_tool(
        &memory,
        config.clone(),
        "memory_write",
        json!({ "key": "c", "text": "gamma alpha" }),
    );
    // max_entries 2: the oldest went.
    let read = call_tool(
        &memory,
        config.clone(),
        "memory_read",
        json!({ "key": "a" }),
    );
    assert_eq!(read["error"], true, "{read}");
    let found = call_tool(
        &memory,
        config.clone(),
        "memory_search",
        json!({ "query": "alpha" }),
    );
    assert_eq!(found["data"][0]["key"], "c");
    let gone = call_tool(
        &memory,
        config.clone(),
        "memory_delete",
        json!({ "key": "c" }),
    );
    assert_eq!(gone["data"]["deleted"], true);
    let listed = call_tool(&memory, config.clone(), "memory_list", json!({}));
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);
    // Read only: no writes offered or accepted.
    let read_only = json!({ "scope": "execution", "agent_writes": "no" });
    let refused = call_tool(
        &memory,
        read_only,
        "memory_write",
        json!({ "key": "x", "text": "y" }),
    );
    assert_eq!(refused["error"], true);
}

#[test]
fn project_memory_survives_a_restart_of_the_core() {
    let home = temp("restart");
    let core = core(&home);
    let id = start(&core);
    send(
        &core,
        &id,
        "remember",
        json!({ "key": "deploy", "text": "Tag before you ship" }),
    );
    until(&core, &id, "written", 1);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    core.shutdown();
    drop(core);

    // A new core on the same store: the entry is in SQLite, not in the old process.
    let core = self::core(&home);
    let id = start(&core);
    let entries = core
        .dispatch(
            "memory.entries",
            json!({ "execution_id": id, "node_id": "notes" }),
        )
        .unwrap();
    assert_eq!(entries[0]["key"], "deploy", "{entries}");
    assert_eq!(entries[0]["text"], "Tag before you ship");
    send(&core, &id, "ask", json!("when do we ship?"));
    let recall = until(&core, &id, "recall", 1);
    assert!(
        recall[0]
            .as_str()
            .unwrap()
            .contains("- deploy: Tag before you ship"),
        "{recall:?}"
    );
}
