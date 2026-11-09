//! The documents nodes against a real core with a store, through the example profile:
//! the built-in lexical path end to end, the module's methods, and the swap of the index
//! and the chunker for wired `retrieval.engine@1` and `chunker@1` nodes.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_contract_chunker as chunker;
use regulus_contract_retrieval_engine as engine;
use regulus_contract_tool as tool;
use regulus_core::config::StoreConfig;
use regulus_core::{
    parse_manifest, Core, Module, ModuleCtx, ModuleError, ProfileStore, Storage, Store,
};
use regulus_module_documents::Documents;
use regulus_module_flow::Flow;
use regulus_proto::{method, BusEventKind, EventPage, Manifest, NodeCall, Profile};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(5);

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-docs-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const RUST: &str = "# Rust\n\nThe borrow checker enforces ownership rules at compile time.\n\n\
                    ## Cargo\n\nCargo builds crates and fetches dependencies.\n";
const PYTHON: &str = "# Python\n\nPip installs packages from the package index.\n";

/// The example profile, its documents folder set to `docs`.
fn profile(docs: &Path, engine: bool) -> Profile {
    let mut profile: Value = serde_json::to_value(
        toml::from_str::<Profile>(include_str!("../examples/rag.toml")).unwrap(),
    )
    .unwrap();
    let workflow = &mut profile["workflows"][0];
    for node in workflow["nodes"].as_array_mut().unwrap() {
        if node["id"] == "ingest" {
            node["config"]["dir"] = json!(docs.display().to_string());
        }
    }
    if engine {
        let nodes = workflow["nodes"].as_array_mut().unwrap();
        nodes.push(
            json!({ "id": "fake", "type": "engine", "module_id": "t/fake@1.0.0",
            "ports": [{ "id": "engine", "capability": "retrieval.engine@1",
                        "direction": "output", "kind": "uses" }] }),
        );
        nodes.push(
            json!({ "id": "cutter", "type": "chunker", "module_id": "t/fake@1.0.0",
            "ports": [{ "id": "chunker", "capability": "chunker@1",
                        "direction": "output", "kind": "uses" }] }),
        );
        let wires = workflow["wires"].as_array_mut().unwrap();
        for (id, from, port, to, input) in [
            ("w-e1", "fake", "engine", "ingest", "engine"),
            ("w-e2", "fake", "engine", "retrieve", "engine"),
            ("w-c", "cutter", "chunker", "ingest", "chunker"),
        ] {
            wires.push(json!({ "id": id, "kind": "uses",
                "from": { "node_id": from, "port_id": port },
                "to": { "node_id": to, "port_id": input } }));
        }
    }
    serde_json::from_value(profile).unwrap()
}

type Calls = Arc<Mutex<Vec<(String, Value)>>>;

/// A retrieval engine and a chunker of another module: records every call; the engine
/// finds chunks whose text holds the query's first word.
struct Fake {
    manifest: Manifest,
    calls: Calls,
}

const FAKE: &str = r#"
[module]
id = "t/fake@1.0.0"
name = "Fake"

[[node]]
type = "engine"
name = "Engine"

  [[node.port]]
  id = "engine"
  capability = "retrieval.engine@1"
  direction = "output"
  kind = "uses"

[[node]]
type = "chunker"
name = "Chunker"

  [[node.port]]
  id = "chunker"
  capability = "chunker@1"
  direction = "output"
  kind = "uses"
"#;

impl Module for Fake {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = match serde_json::from_value(params) {
            Ok(call) => call,
            Err(e) => return Err(ModuleError::InvalidParams(e.to_string())),
        };
        let args = call.args;
        self.calls.lock().unwrap().push((name.into(), args.clone()));
        match name {
            chunker::SPLIT => {
                let split: chunker::SplitArgs = serde_json::from_value(args).unwrap();
                // One chunk per line that is not blank, with its line number.
                let mut at = 0;
                let mut chunks = Vec::new();
                for (n, line) in split.text.split_inclusive('\n').enumerate() {
                    let content = line.trim_end();
                    if !content.is_empty() {
                        chunks.push(chunker::Piece {
                            start: at,
                            end: at + content.len(),
                            text: content.into(),
                            metadata: Some(json!({ "line": n + 1 })),
                        });
                    }
                    at += line.len();
                }
                Ok(json!(chunker::Split { chunks }))
            }
            engine::INDEX | engine::REMOVE => Ok(Value::Null),
            engine::QUERY => {
                let query: engine::QueryArgs = serde_json::from_value(args).unwrap();
                let word = query
                    .query
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_lowercase();
                let calls = self.calls.lock().unwrap();
                let removed: Vec<String> = calls
                    .iter()
                    .filter(|(m, _)| m == engine::REMOVE)
                    .map(|(_, a)| a["doc_id"].as_str().unwrap().to_string())
                    .collect();
                let mut hits: Vec<engine::Hit> = calls
                    .iter()
                    .filter(|(m, _)| m == engine::INDEX)
                    .flat_map(|(_, a)| a["chunks"].as_array().unwrap().clone())
                    .filter(|c| !removed.contains(&c["doc_id"].as_str().unwrap().to_string()))
                    .filter(|c| c["text"].as_str().unwrap().to_lowercase().contains(&word))
                    .map(|c| engine::Hit {
                        chunk_id: c["chunk_id"].as_str().unwrap().into(),
                        score: 0.5,
                    })
                    .collect();
                // A chunk the engine knows but the store does not: skipped by the module.
                hits.insert(
                    0,
                    engine::Hit {
                        chunk_id: "s0#0:0".into(),
                        score: 9.0,
                    },
                );
                hits.truncate(query.k);
                Ok(json!(engine::Hits { hits }))
            }
            _ => Err(ModuleError::UnknownMethod(name.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

fn core(home: &Path, profile: Profile, calls: Calls) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let storage = Storage {
        data_dir: Some(home.join("data")),
        store: Some(Arc::new(
            Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
        )),
        ..Storage::default()
    };
    let fake = Fake {
        manifest: parse_manifest(FAKE).unwrap(),
        calls,
    };
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(Documents::new()),
        Box::new(Flow::new()),
        Box::new(fake),
    ];
    Core::build_with(modules, &[], profiles, storage)
}

fn start(core: &Core) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "rag" }))
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

fn emitted(core: &Core, id: &str, port: &str) -> Vec<Value> {
    let page: EventPage = serde_json::from_value(
        core.dispatch(
            method::EVENT_LIST,
            json!({ "execution_id": id, "limit": 10000 }),
        )
        .unwrap(),
    )
    .unwrap();
    page.events
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
        if got.len() >= count {
            return got;
        }
        assert!(Instant::now() < deadline, "self.{port} got {got:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn last(core: &Core, id: &str, port: &str, count: usize) -> Value {
    until(core, id, port, count)[count - 1].clone()
}

#[test]
fn the_lexical_path_ingests_retrieves_cites_and_replaces_by_version() {
    let home = temp("lexical");
    let docs = home.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("rust.md"), RUST).unwrap();
    std::fs::write(docs.join("python.md"), PYTHON).unwrap();
    let core = core(&home, profile(&docs, false), Calls::default());
    let id = start(&core);

    send(&core, &id, "ingest", json!("rust.md"));
    send(
        &core,
        &id,
        "ingest",
        json!({ "path": "python.md", "metadata": { "lang": "py" } }),
    );
    let rust = last(&core, &id, "ingested", 1);
    let python = last(&core, &id, "ingested", 2);
    assert_eq!(rust["chunks"], 2, "{rust}");
    assert_eq!(rust["replaced"], false);
    let rust_id = rust["source_id"].as_str().unwrap().to_string();
    assert!(rust["doc_id"]
        .as_str()
        .unwrap()
        .starts_with(&format!("{rust_id}#")));

    // The same bytes again change nothing.
    send(&core, &id, "ingest", json!("rust.md"));
    let again = last(&core, &id, "ingested", 3);
    assert_eq!(
        (&again["unchanged"], &again["replaced"]),
        (&json!(true), &json!(false))
    );
    assert_eq!(again["doc_id"], rust["doc_id"]);

    // A path out of `dir` is refused, existing or not.
    send(&core, &id, "ingest", json!("../store"));
    let error = last(&core, &id, "error", 1);
    assert!(error.as_str().unwrap().contains("allow_outside"), "{error}");

    send(&core, &id, "ask", json!("Who enforces ownership?"));
    let hits = last(&core, &id, "hits", 1);
    let first = &hits["hits"][0];
    assert_eq!(first["source_id"], rust_id.as_str(), "{hits}");
    assert_eq!(first["metadata"]["headings"], json!(["Rust"]));
    assert!(first["location"].as_str().unwrap().ends_with("rust.md"));
    let context = last(&core, &id, "context", 1);
    let context = context.as_str().unwrap();
    assert!(
        context.contains("[1] # Rust\n\nThe borrow checker"),
        "{context}"
    );
    assert!(
        context.contains(&format!(
            "rust.md#{}-{} ({})",
            first["start"],
            first["end"],
            first["chunk_id"].as_str().unwrap()
        )),
        "{context}"
    );

    send(&core, &id, "answer", json!("The borrow checker [1]."));
    let passed = last(&core, &id, "passed", 1);
    assert_eq!(passed["cited"], json!([1]));
    let reply = last(&core, &id, "reply", 1);
    assert!(reply.as_str().unwrap().ends_with("rust.md"), "{reply}");
    // A second round: a citation to evidence that was never shown fails.
    send(&core, &id, "ask", json!("pip packages"));
    let hits = last(&core, &id, "hits", 2);
    assert_eq!(hits["hits"][0]["metadata"]["lang"], "py", "{hits}");
    send(&core, &id, "answer", json!("Guido did [7]."));
    let failed = last(&core, &id, "failed", 1);
    assert_eq!(failed["unknown"], json!([7]));

    // Changed bytes replace the old version, in the index too.
    std::fs::write(
        docs.join("rust.md"),
        "# Rust\n\nLifetimes annotate references.\n",
    )
    .unwrap();
    send(&core, &id, "ingest", json!("rust.md"));
    let changed = last(&core, &id, "ingested", 4);
    assert_eq!(
        (&changed["replaced"], &changed["unchanged"]),
        (&json!(true), &json!(false))
    );
    assert_eq!(changed["source_id"], rust_id.as_str());
    assert_ne!(changed["doc_id"], rust["doc_id"]);
    let target = |extra: Value| {
        let mut t = json!({ "execution_id": id, "node_id": "retrieve" });
        t.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        t
    };
    let found = core
        .dispatch(
            "documents.query",
            target(json!({ "query": "borrow checker" })),
        )
        .unwrap();
    assert_eq!(found["hits"], json!([]), "{found}");
    let found = core
        .dispatch(
            "documents.query",
            target(json!({ "query": "lifetimes", "k": 1 })),
        )
        .unwrap();
    assert_eq!(found["hits"][0]["doc_id"], changed["doc_id"]);

    // The methods: list, query with a filter, delete.
    let listed = core.dispatch("documents.list", target(json!({}))).unwrap();
    let listed = listed.as_array().unwrap();
    assert_eq!(listed.len(), 2);
    assert!(listed
        .iter()
        .all(|s| s["version"].as_str().unwrap().len() == 16));
    let filtered = core
        .dispatch(
            "documents.query",
            target(json!({ "query": "rust pip", "filter": { "lang": "py" } })),
        )
        .unwrap();
    assert!(
        filtered["hits"]
            .as_array()
            .unwrap()
            .iter()
            .all(|h| h["source_id"] == python["source_id"]),
        "{filtered}"
    );
    let deleted = core
        .dispatch(
            "documents.delete",
            target(json!({ "source_id": python["source_id"] })),
        )
        .unwrap();
    assert_eq!(deleted["deleted"], true);
    let found = core
        .dispatch("documents.query", target(json!({ "query": "pip" })))
        .unwrap();
    assert_eq!(found["hits"], json!([]));
    assert!(core
        .dispatch(
            "documents.list",
            json!({ "execution_id": id, "node_id": "context" })
        )
        .is_err());

    // The collection is the project's: the next execution finds it.
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let next = start(&core);
    let listed = core
        .dispatch(
            "documents.list",
            json!({ "execution_id": next, "node_id": "ingest" }),
        )
        .unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
}

#[test]
fn a_wired_engine_and_chunker_replace_the_built_in_ones_with_provenance() {
    let home = temp("engine");
    let docs = home.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("rust.md"), RUST).unwrap();
    let calls = Calls::default();
    let core = core(&home, profile(&docs, true), calls.clone());
    let id = start(&core);

    send(&core, &id, "ingest", json!("rust.md"));
    let ingested = last(&core, &id, "ingested", 1);
    // Four lines that are not blank: four chunks, cut by the wired chunker.
    assert_eq!(ingested["chunks"], 4, "{ingested}");
    let doc_id = ingested["doc_id"].as_str().unwrap().to_string();
    let source_id = ingested["source_id"].as_str().unwrap().to_string();
    {
        let calls = calls.lock().unwrap();
        let split = calls.iter().find(|(m, _)| m == chunker::SPLIT).unwrap();
        assert_eq!(split.1["text"], RUST);
        assert!(split.1["metadata"]["location"]
            .as_str()
            .unwrap()
            .ends_with("rust.md"));
        let index = calls.iter().find(|(m, _)| m == engine::INDEX).unwrap();
        assert_eq!(index.1["collection"], "docs");
        let chunks = index.1["chunks"].as_array().unwrap();
        assert_eq!(chunks.len(), 4);
        assert_eq!(chunks[1]["chunk_id"], format!("{doc_id}:1"));
        assert_eq!(chunks[1]["doc_id"], doc_id.as_str());
        assert_eq!(chunks[1]["source_id"], source_id.as_str());
        assert_eq!(chunks[1]["metadata"]["line"], 3);
        assert_eq!(
            chunks[1]["location"]
                .as_str()
                .map(|l| l.ends_with("rust.md")),
            Some(true)
        );
        assert_eq!(
            (&chunks[1]["start"], &chunks[1]["end"]),
            (&json!(8), &json!(68))
        );
    }
    // Nothing reached the built-in index: a query with the engine unwired would find
    // nothing, and the wired one is what answers.
    send(&core, &id, "ask", json!("cargo"));
    let hits = last(&core, &id, "hits", 1);
    let hits = hits["hits"].as_array().unwrap();
    assert_eq!(hits.len(), 2, "{hits:?}");
    assert_eq!(hits[0]["chunk_id"], format!("{doc_id}:2"));
    assert_eq!(hits[0]["text"], "## Cargo");
    assert_eq!(hits[1]["start"], 80);
    assert!(hits[1]["location"].as_str().unwrap().ends_with("rust.md"));
    assert_eq!(hits[1]["score"], 0.5);
    let query = calls
        .lock()
        .unwrap()
        .iter()
        .find(|(m, _)| m == engine::QUERY)
        .unwrap()
        .1
        .clone();
    assert_eq!(
        query,
        json!({ "collection": "docs", "query": "cargo", "k": 3 })
    );

    // Changed bytes: the engine is told to drop the old document before indexing the new.
    std::fs::write(docs.join("rust.md"), "Cargo is the build tool.\n").unwrap();
    send(&core, &id, "ingest", json!("rust.md"));
    let changed = last(&core, &id, "ingested", 2);
    assert_eq!(changed["replaced"], true);
    let methods: Vec<(String, Value)> = calls.lock().unwrap().clone();
    let removes: Vec<&Value> = methods
        .iter()
        .filter(|(m, _)| m == engine::REMOVE)
        .map(|(_, a)| a)
        .collect();
    assert_eq!(
        removes,
        [&json!({ "collection": "docs", "doc_id": doc_id })]
    );

    // Deleting the source goes to the engine too.
    let target = json!({ "execution_id": id, "node_id": "ingest", "source_id": source_id });
    let deleted = core.dispatch("documents.delete", target).unwrap();
    assert_eq!(
        (&deleted["deleted"], &deleted["chunks"]),
        (&json!(true), &json!(1))
    );
    let removes = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(m, _)| m == engine::REMOVE)
        .count();
    assert_eq!(removes, 2);
}

#[test]
fn tools_search_read_and_ingest_only_when_allowed() {
    let docs = Documents::new();
    let node = |kind: &str, config: Value| {
        json!({ "execution_id": "e", "node": { "id": kind, "type": kind,
                "module_id": "regulus/documents@0.1.0", "config": config } })
    };
    let names = |kind: &str, config: Value| -> Vec<String> {
        let specs: Vec<tool::Spec> =
            serde_json::from_value(docs.call(tool::LIST, node(kind, config)).unwrap()).unwrap();
        specs.into_iter().map(|s| s.name).collect()
    };
    assert_eq!(
        names("retrieve", json!({})),
        ["search_documents", "read_chunk"]
    );
    assert!(names("ingest", json!({})).is_empty());
    assert_eq!(
        names("ingest", json!({ "agent_writes": "yes" })),
        ["ingest_text"]
    );
    let call = |kind: &str, config: Value, name: &str, args: Value| -> tool::Outcome {
        let mut params = node(kind, config);
        params["args"] = json!({ "name": name, "args": args, "caller": { "name": "ana" } });
        serde_json::from_value(docs.call(tool::CALL, params).unwrap()).unwrap()
    };
    let refused = call(
        "ingest",
        json!({}),
        "ingest_text",
        json!({ "title": "t", "text": "x" }),
    );
    assert!(refused.error);
    let added = call(
        "ingest",
        json!({ "agent_writes": "yes" }),
        "ingest_text",
        json!({ "title": "Notes", "text": "Tokio is an async runtime." }),
    );
    assert!(!added.error, "{:?}", added.data);
    assert_eq!(added.data["location"], "text:Notes");
    let found = call(
        "retrieve",
        json!({}),
        "search_documents",
        json!({ "query": "tokio runtime" }),
    );
    let hit = &found.data["hits"][0];
    assert_eq!(hit["metadata"]["by"], "ana", "{:?}", found.data);
    let chunk_id = hit["chunk_id"].as_str().unwrap();
    let read = call(
        "retrieve",
        json!({}),
        "read_chunk",
        json!({ "chunk_id": chunk_id }),
    );
    assert_eq!(read.data["text"], "Tokio is an async runtime.");
    assert_eq!(read.data["title"], "Notes");
    assert!(
        call(
            "retrieve",
            json!({}),
            "read_chunk",
            json!({ "chunk_id": "nope" })
        )
        .error
    );
}
