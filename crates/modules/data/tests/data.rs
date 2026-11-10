//! The data nodes against a real core with a store, through the example profile.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_contract_tool as tool;
use regulus_core::config::StoreConfig;
use regulus_core::{Core, Module, ModuleCtx, ModuleError, ProfileStore, Storage, Store};
use regulus_module_data::Data;
use regulus_proto::{method, BusEventKind, EventPage, Manifest, ModuleInfo, Profile};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(5);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-data-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The example, as the profile `id` of `project`.
fn profile(id: &str, project: Option<&str>) -> Profile {
    let mut profile: Profile = toml::from_str(include_str!("../examples/data.toml")).unwrap();
    profile.id = id.into();
    profile.project_id = project.map(String::from);
    profile
}

fn core(home: &std::path::Path, profiles: &[Profile]) -> Arc<Core> {
    let mut store_of_profiles = ProfileStore::default();
    for profile in profiles {
        store_of_profiles.insert(profile.clone()).unwrap();
    }
    let storage = Storage {
        data_dir: Some(home.join("data")),
        store: Some(Arc::new(
            Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
        )),
        ..Storage::default()
    };
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Data::new())];
    Core::build_with(modules, &[], store_of_profiles, storage)
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

fn call(core: &Core, method: &str, id: &str, node: &str, extra: Value) -> Value {
    let mut params = json!({ "execution_id": id, "node_id": node });
    params
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().cloned().unwrap_or_default());
    core.dispatch(method, params).unwrap()
}

fn keys(list: &Value) -> Vec<&str> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|n| n["key"].as_str().unwrap())
        .collect()
}

#[test]
fn the_example_collects_a_dataset_compares_states_and_ranks_metrics() {
    let home = temp("example");
    let core = core(&home, &[profile("data", Some("proj-a"))]);
    let id = start(&core, "data");

    // Two records, a duplicate of the first, and a line that is not JSON.
    send(
        &core,
        &id,
        "rows_jsonl",
        json!("{\"id\":\"a\",\"q\":\"one\"}\n{\"id\":\"a\",\"q\":\"dup\"}\nnot json\n{\"id\":\"b\"}\n"),
    );
    let stored = until(&core, &id, "stored", 1)[0].clone();
    assert_eq!(
        (&stored["added"], &stored["duplicates"], &stored["total"]),
        (&json!(2), &json!(1), &json!(2)),
        "{stored}"
    );
    assert_eq!(stored["rejected_count"], 1);
    assert_eq!(stored["rejected"][0]["line"], 3);
    let errors = until(&core, &id, "error", 1);
    assert!(
        errors[0]
            .as_str()
            .unwrap()
            .contains("1 of 4 records rejected"),
        "{errors:?}"
    );
    send(&core, &id, "flush", json!("now"));
    let dataset = until(&core, &id, "dataset", 1)[0].clone();
    assert_eq!(dataset, json!([{ "id": "a", "q": "one" }, { "id": "b" }]));

    // A source polled three times: all new, nothing new, then one of each kind.
    let item = |url: &str, title: &str| json!({ "url": url, "title": title, "views": 1 });
    send(
        &core,
        &id,
        "state",
        json!([item("u1", "A"), item("u2", "B")]),
    );
    let novel = until(&core, &id, "novel", 1)[0].clone();
    assert_eq!(keys(&novel), ["u1", "u2"], "{novel}");
    assert_eq!(novel[0]["change"], "added");
    let first = until(&core, &id, "report", 1)[0].clone();
    assert_eq!((&first["first"], &first["seq"]), (&json!(true), &json!(1)));

    let again = json!([
        { "url": "u1", "title": "A", "views": 500 },
        { "url": "u2", "title": "B", "views": 1 }
    ]);
    send(&core, &id, "state", again);
    let same = until(&core, &id, "same", 1)[0].clone();
    assert_eq!(same["counts"]["unchanged"], 2, "{same}");
    assert_eq!(
        emitted(&core, &id, "novel").len(),
        1,
        "an identical poll has nothing new"
    );

    send(
        &core,
        &id,
        "state",
        json!([item("u2", "B edited"), item("u3", "C")]),
    );
    let novel = until(&core, &id, "novel", 2)[1].clone();
    assert_eq!(keys(&novel), ["u2", "u3"], "{novel}");
    assert_eq!(
        (novel[0]["change"].as_str(), novel[1]["change"].as_str()),
        (Some("changed"), Some("added"))
    );
    assert_eq!(novel[0]["before"]["title"], "B");
    let removed = until(&core, &id, "removed", 1)[0].clone();
    assert_eq!(keys(&removed), ["u1"]);

    // Metrics ranked by recall, with the labels of the node.
    send(
        &core,
        &id,
        "metrics",
        json!({ "bm25": { "recall": 0.5 }, "fixed": { "recall": 1.0 } }),
    );
    assert_eq!(until(&core, &id, "best", 1)[0], "Fixed chunks");
    let ranking = until(&core, &id, "ranking", 1)[0].clone();
    assert_eq!(
        ranking["ranking"][1]["delta_best"],
        json!(-0.5),
        "{ranking}"
    );
    // A metric that is not there is refused by name.
    send(
        &core,
        &id,
        "metrics",
        json!({ "a": { "recall": 1 }, "b": {} }),
    );
    let errors = until(&core, &id, "error", 2);
    assert!(
        errors[1]
            .as_str()
            .unwrap()
            .contains("no number at `/recall` in: b"),
        "{errors:?}"
    );
}

#[test]
fn the_baseline_survives_a_restart_and_stays_inside_its_project() {
    let home = temp("restart");
    let profiles = [
        profile("data-a", Some("proj-a")),
        profile("data-b", Some("proj-b")),
    ];
    let state = json!([{ "url": "u1", "title": "A" }, { "url": "u2", "title": "B" }]);

    let core = core(&home, &profiles);
    let id = start(&core, "data-a");
    send(&core, &id, "state", state.clone());
    until(&core, &id, "novel", 1);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    core.shutdown();
    drop(core);

    // A new core on the same store: the baseline is in SQLite, not in the old process.
    let core = self::core(&home, &profiles);
    let id = start(&core, "data-a");
    send(&core, &id, "state", state.clone());
    let same = until(&core, &id, "same", 1)[0].clone();
    assert_eq!(
        (&same["first"], &same["seq"]),
        (&json!(false), &json!(2)),
        "{same}"
    );
    assert!(emitted(&core, &id, "novel").is_empty());
    send(
        &core,
        &id,
        "state",
        json!([{ "url": "u1", "title": "A2" }, { "url": "u2", "title": "B" }, { "url": "u9" }]),
    );
    let novel = until(&core, &id, "novel", 1)[0].clone();
    assert_eq!(keys(&novel), ["u1", "u9"], "{novel}");

    // Another project has its own baseline: the same state is all new there.
    let other = start(&core, "data-b");
    send(&core, &other, "state", state);
    let report = until(&core, &other, "report", 1)[0].clone();
    assert_eq!(
        (&report["first"], &report["total"]),
        (&json!(true), &json!(2))
    );
    assert_eq!(report["counts"]["added"], 2);
    // And it cannot see the other project's rows.
    let listed = call(&core, "data.list", &other, "watch", json!({}));
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["state"]["seq"], 1);
    let listed = call(&core, "data.list", &id, "watch", json!({}));
    assert_eq!(listed[0]["state"]["seq"], 3, "{listed}");
}

#[test]
fn a_silent_baseline_sends_only_its_report() {
    let home = temp("silent");
    let mut silent = profile("quiet", None);
    for node in silent.workflows[0]
        .nodes
        .iter_mut()
        .filter(|n| n.id == "watch")
    {
        node.config.insert("baseline".into(), json!("silent"));
    }
    let core = core(&home, &[silent]);
    let id = start(&core, "quiet");
    send(&core, &id, "state", json!([{ "url": "u1" }]));
    let report = until(&core, &id, "report", 1)[0].clone();
    assert_eq!(
        (&report["first"], &report["baselined"]),
        (&json!(true), &json!(true))
    );
    assert!(emitted(&core, &id, "novel").is_empty() && emitted(&core, &id, "same").is_empty());
    send(
        &core,
        &id,
        "state",
        json!([{ "url": "u1" }, { "url": "u2" }]),
    );
    assert_eq!(keys(&until(&core, &id, "novel", 1)[0]), ["u2"]);
}

#[test]
fn the_module_methods_list_inspect_and_export_what_a_node_holds() {
    let home = temp("rpc");
    let core = core(&home, &[profile("data", Some("proj-a"))]);
    let id = start(&core, "data");
    let rows: Vec<Value> = (1..=5)
        .map(|n| json!({ "id": format!("r{n}"), "n": n }))
        .collect();
    send(&core, &id, "rows", json!(rows));
    send(&core, &id, "state", json!([{ "url": "u1", "title": "A" }]));
    until(&core, &id, "novel", 1);
    until(&core, &id, "stored", 1);

    let listed = call(&core, "data.list", &id, "rows", json!({}));
    let kinds: Vec<(&str, &str, u64)> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["kind"].as_str().unwrap(),
                r["name"].as_str().unwrap(),
                r["state"]["items"].as_u64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [("dataset", "rows", 5), ("snapshot", "watch", 1)],
        "{listed}"
    );

    let seen = call(&core, "data.inspect", &id, "rows", json!({ "limit": 2 }));
    assert_eq!(
        (&seen["items"], &seen["truncated"]),
        (&json!(5), &json!(true))
    );
    assert_eq!(seen["records"].as_array().unwrap().len(), 2);
    let all = call(&core, "data.export", &id, "rows", json!({}));
    assert_eq!(all["records"].as_array().unwrap().len(), 5);
    assert_eq!(all["records"][4]["record"]["n"], 5);
    let snapshot = call(&core, "data.inspect", &id, "watch", json!({}));
    assert_eq!(snapshot["kind"], "snapshot");
    assert_eq!(snapshot["history"][0]["added"], 1);
    // Another node's dataset by name; one that does not exist; a node that keeps nothing.
    let by_name = call(
        &core,
        "data.export",
        &id,
        "watch",
        json!({ "kind": "dataset", "name": "rows" }),
    );
    assert_eq!(by_name["items"], 5);
    let err = core
        .dispatch(
            "data.inspect",
            json!({ "execution_id": id, "node_id": "rows", "name": "nope" }),
        )
        .unwrap_err();
    assert!(err.message.contains("no dataset `nope`"), "{err:?}");
    let err = core
        .dispatch(
            "data.inspect",
            json!({ "execution_id": id, "node_id": "order" }),
        )
        .unwrap_err();
    assert!(err.message.contains("keeps nothing"), "{err:?}");
    let err = core
        .dispatch(
            "data.inspect",
            json!({ "execution_id": id, "node_id": "missing" }),
        )
        .unwrap_err();
    assert!(err.message.contains("is not running"), "{err:?}");
}

/// A Data wired to the context of a core that has no store at all (rows live in the
/// process), the way tests of other modules call tools directly. The core must outlive it.
fn tools_data() -> (Arc<Core>, Data) {
    struct Probe(Manifest, Arc<Mutex<Option<ModuleCtx>>>);
    impl Module for Probe {
        fn manifest(&self) -> &Manifest {
            &self.0
        }
        fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
            *self.1.lock().unwrap() = Some(ctx.clone());
            Ok(())
        }
        fn call(&self, name: &str, _: Value) -> Result<Value, ModuleError> {
            Err(ModuleError::UnknownMethod(name.into()))
        }
        fn shutdown(&mut self) -> Result<(), ModuleError> {
            Ok(())
        }
    }
    let manifest = Manifest {
        module: ModuleInfo {
            id: "t/probe@1.0.0".into(),
            name: "probe".into(),
            requires_proto: None,
            methods: vec![],
            workflow_id: None,
            origin: None,
        },
        nodes: vec![],
    };
    let seen = Arc::new(Mutex::new(None));
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Probe(manifest, seen.clone()))];
    let core = Core::build_with(modules, &[], ProfileStore::default(), Storage::default());
    let ctx = seen
        .lock()
        .unwrap()
        .clone()
        .expect("the core initialised the probe");
    let mut data = Data::new();
    data.init(&ctx).unwrap();
    (core, data)
}

fn call_tool(data: &Data, node_type: &str, config: Value, name: &str, args: Value) -> Value {
    let node = json!({ "id": "n", "type": node_type, "module_id": "regulus/data@0.1.0",
                       "config": config });
    let args = json!({ "name": name, "args": args, "caller": { "name": "dev" } });
    data.call(
        tool::CALL,
        json!({ "execution_id": "e", "node": node, "args": args }),
    )
    .unwrap()
}

fn list_tools(data: &Data, node_type: &str, config: Value) -> Vec<String> {
    let node = json!({ "id": "n", "type": node_type, "module_id": "regulus/data@0.1.0",
                       "config": config });
    let specs = data
        .call(tool::LIST, json!({ "execution_id": "e", "node": node }))
        .unwrap();
    specs
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn agents_read_and_add_to_a_dataset_through_its_tools_and_read_a_snapshot() {
    let (_core, data) = tools_data();
    let config =
        json!({ "scope": "execution", "key_path": "/id", "agent_writes": "yes", "max_items": 2 });
    assert_eq!(
        list_tools(&data, "collect", config.clone()),
        ["dataset_stats", "dataset_read", "dataset_add"]
    );
    assert_eq!(
        list_tools(&data, "collect", json!({ "scope": "execution" })).len(),
        2
    );

    let added = call_tool(
        &data,
        "collect",
        config.clone(),
        "dataset_add",
        json!({ "record": { "id": "a" } }),
    );
    assert_eq!(added["data"]["added"], 1, "{added}");
    let dup = call_tool(
        &data,
        "collect",
        config.clone(),
        "dataset_add",
        json!({ "record": { "id": "a" } }),
    );
    assert_eq!(dup["data"]["duplicates"], 1, "{dup}");
    call_tool(
        &data,
        "collect",
        config.clone(),
        "dataset_add",
        json!({ "record": { "id": "b" } }),
    );
    // max_items 2 with the default `evict = refuse`: the third is refused, as an answer.
    let full = call_tool(
        &data,
        "collect",
        config.clone(),
        "dataset_add",
        json!({ "record": { "id": "c" } }),
    );
    assert_eq!(full["error"], true, "{full}");
    assert!(
        full["data"].as_str().unwrap().contains("evict = refuse"),
        "{full}"
    );
    let read = call_tool(&data, "collect", config.clone(), "dataset_read", json!({}));
    assert_eq!(read["data"][0]["key"], "b", "newest first: {read}");
    let one = call_tool(
        &data,
        "collect",
        config.clone(),
        "dataset_read",
        json!({ "key": "a" }),
    );
    assert_eq!(one["data"]["record"], json!({ "id": "a" }));
    let stats = call_tool(&data, "collect", config.clone(), "dataset_stats", json!({}));
    assert_eq!(
        (&stats["data"]["items"], &stats["data"]["max_items"]),
        (&json!(2), &json!(2))
    );
    let read_only = call_tool(
        &data,
        "collect",
        json!({ "scope": "execution" }),
        "dataset_add",
        json!({ "record": 1 }),
    );
    assert_eq!(read_only["error"], true);

    // Nothing taken yet is an answer; rank ranks what it is given and keeps nothing.
    let snap = json!({ "scope": "execution", "key_path": "/url" });
    let status = call_tool(&data, "snapshot", snap, "snapshot_status", json!({}));
    assert_eq!(status["error"], true, "{status}");
    let ranked = call_tool(
        &data,
        "rank",
        json!({ "metric_path": "/ms", "order": "asc" }),
        "rank_compare",
        json!({ "entries": { "slow": { "ms": 90 }, "fast": { "ms": 12 } } }),
    );
    assert_eq!(ranked["data"]["best"], json!(["fast"]), "{ranked}");
}

#[test]
fn a_store_that_cannot_be_reached_is_an_error_not_a_quiet_detour() {
    // A Data that never met a core has no store to reach: its tools say so, and nothing is
    // kept in the process under keys shared by every project.
    let data = Data::new();
    let added = call_tool(
        &data,
        "collect",
        json!({ "scope": "execution", "agent_writes": "yes" }),
        "dataset_add",
        json!({ "record": { "id": 1 } }),
    );
    assert_eq!(added["error"], true, "{added}");
    assert!(
        added["data"].as_str().unwrap().contains("not running"),
        "{added}"
    );
}

fn send_expect(core: &Core, id: &str, port: &str, value: Value, node_error: &str) {
    send(core, id, port, value);
    let errors = until(core, id, "error", 1);
    assert!(
        errors[0].as_str().unwrap().contains(node_error),
        "{errors:?}"
    );
}

#[test]
fn an_empty_source_or_a_partly_unreadable_one_never_empties_the_baseline() {
    let home = temp("safe");
    let core = core(&home, &[profile("data", Some("proj-a"))]);
    let id = start(&core, "data");
    let state = json!([{ "url": "u1", "title": "A" }, { "url": "u2", "title": "B" }]);
    send(&core, &id, "state", state.clone());
    until(&core, &id, "novel", 1);

    // A source that answers with nothing: refused, the baseline stays.
    send_expect(&core, &id, "state", json!([]), "on_empty");
    // A record with no url among good ones: u2 is carried, not reported gone.
    send(
        &core,
        &id,
        "state",
        json!([{ "url": "u1", "title": "A" }, { "title": "B" }]),
    );
    let reports = until(&core, &id, "report", 2);
    assert_eq!(reports[1]["counts"]["carried"], 1, "{}", reports[1]);
    assert_eq!(reports[1]["counts"]["removed"], 0);
    assert!(emitted(&core, &id, "removed").is_empty());
    // The clean poll that follows has nothing new.
    send(&core, &id, "state", state);
    // (The poll with the unreadable record also had no news: `same` is its second.)
    let same = until(&core, &id, "same", 2);
    assert_eq!(same[1]["counts"]["unchanged"], 2, "{}", same[1]);
    assert_eq!(emitted(&core, &id, "novel").len(), 1);
}

#[test]
fn a_silent_first_poll_with_nothing_does_not_store_an_empty_baseline() {
    let home = temp("silent-empty");
    let mut quiet = profile("quiet", None);
    for node in quiet.workflows[0]
        .nodes
        .iter_mut()
        .filter(|n| n.id == "watch")
    {
        node.config.insert("baseline".into(), json!("silent"));
    }
    let core = core(&home, &[quiet]);
    let id = start(&core, "quiet");
    send_expect(&core, &id, "state", json!([]), "on_empty");
    // Had an empty baseline been stored, this would be a second poll and report news.
    send(&core, &id, "state", json!([{ "url": "u1" }]));
    let report = until(&core, &id, "report", 1)[0].clone();
    assert_eq!(
        (&report["first"], &report["baselined"]),
        (&json!(true), &json!(true))
    );
    assert!(emitted(&core, &id, "novel").is_empty());
}

#[test]
fn a_stopped_run_and_a_restarted_core_still_show_what_they_kept() {
    let home = temp("after");
    let profiles = [profile("data-a", Some("proj-a")), profile("data-g", None)];
    let core = core(&home, &profiles);
    let a = start(&core, "data-a");
    let g = start(&core, "data-g");
    for id in [&a, &g] {
        send(&core, id, "rows", json!([{ "id": "r1" }, { "id": "r2" }]));
        send(&core, id, "state", json!([{ "url": "u1", "title": "A" }]));
        until(&core, id, "novel", 1);
        until(&core, id, "stored", 1);
        core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
            .unwrap();
    }
    let by_name = json!({ "kind": "snapshot", "name": "watch" });
    let ask = |core: &Core, method: &str, extra: Value| {
        let mut params = by_name.clone();
        params
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().cloned().unwrap());
        core.dispatch(method, params)
    };

    // Same core, run over: the project's rows are read through the stopped execution.
    let listed = ask(
        &core,
        "data.list",
        json!({ "execution_id": a, "node_id": "watch" }),
    )
    .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2, "{listed}");
    let exported = ask(
        &core,
        "data.export",
        json!({ "execution_id": a, "node_id": "watch" }),
    )
    .unwrap();
    assert_eq!(exported["records"][0]["key"], "u1", "{exported}");
    core.shutdown();
    drop(core);

    // A new core, no execution started: a profile outside any project is read as global.
    let core = self::core(&home, &profiles);
    let listed = ask(&core, "data.list", json!({ "global": true })).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2, "{listed}");
    let exported = ask(&core, "data.export", json!({ "global": true })).unwrap();
    assert_eq!(
        (&exported["seq"], &exported["items"]),
        (&json!(1), &json!(1))
    );
    let rows = core
        .dispatch(
            "data.export",
            json!({ "global": true, "kind": "dataset", "name": "rows" }),
        )
        .unwrap();
    assert_eq!(rows["items"], 2);
    // The project's rows are read through the stopped execution of the old core, or by
    // naming the project, with nothing running.
    let listed = ask(
        &core,
        "data.list",
        json!({ "execution_id": a, "node_id": "watch" }),
    )
    .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2, "{listed}");
    let exported = ask(&core, "data.export", json!({ "project": "proj-a" })).unwrap();
    assert_eq!(exported["records"][0]["key"], "u1", "{exported}");
    // A project the core has no data for is refused, not created.
    let err = ask(&core, "data.list", json!({ "project": "nope" })).unwrap_err();
    assert!(err.message.contains("no project `nope`"), "{err:?}");
    // A request that names no node, `project` or `global` is refused plainly.
    let err = ask(&core, "data.list", json!({})).unwrap_err();
    assert!(err.message.contains("global"), "{err:?}");
    // A stopped node does not say what it kept: the kind and the name are asked for.
    let err = core
        .dispatch(
            "data.inspect",
            json!({ "execution_id": a, "node_id": "watch", "global": true }),
        )
        .unwrap_err();
    assert!(err.message.contains("name the `kind`"), "{err:?}");
}
