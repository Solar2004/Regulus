//! The evidence nodes against a real core with a store, through the example profile, and
//! the agent tools against the module directly.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_contract_tool as tool;
use regulus_core::config::StoreConfig;
use regulus_core::{Core, Module, ProfileStore, Storage, Store};
use regulus_module_evidence::Evidence;
use regulus_proto::{method, BusEventKind, EventPage, Profile};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const WAIT: Duration = Duration::from_secs(5);

const PAGE: &str = "# Rust ownership\n\nThe borrow checker enforces ownership rules at compile\n\
                    time, so most memory errors never reach a running program.\n\n\
                    Garbage collectors do the same job while the program runs.\n";
const LOCATION: &str = "https://example.org/rust";

fn temp(name: &str) -> PathBuf {
    let dir =
        std::env::temp_dir().join(format!("regulus-evidence-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The example, as profiles `evidence` (global), and `evidence-a` and `evidence-b` in two
/// projects.
fn profiles() -> ProfileStore {
    let base: Profile = toml::from_str(include_str!("../examples/evidence.toml")).unwrap();
    let mut store = ProfileStore::default();
    store.insert(base.clone()).unwrap();
    for project in ["a", "b"] {
        let mut profile = base.clone();
        profile.id = format!("evidence-{project}");
        profile.project_id = Some(format!("proj-{project}"));
        store.insert(profile).unwrap();
    }
    store
}

fn core(home: &Path) -> Arc<Core> {
    let storage = Storage {
        data_dir: Some(home.join("data")),
        store: Some(Arc::new(
            Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
        )),
        ..Storage::default()
    };
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Evidence::new())];
    Core::build_with(modules, &[], profiles(), storage)
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

/// What the execution put on its output `port`, once everything it sent has been handled.
fn emitted(core: &Core, id: &str, port: &str) -> Vec<Value> {
    let deadline = Instant::now() + WAIT;
    loop {
        let events = page(core, id);
        if events.pending == 0 {
            return events
                .events
                .into_iter()
                .filter(|e| e.from.node_id == "self" && e.from.port_id == port)
                .filter_map(|e| match e.kind {
                    BusEventKind::Emitted { value } => Some(value),
                    _ => None,
                })
                .collect();
        }
        assert!(Instant::now() < deadline, "still pending");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn target(id: &str, node: &str) -> Value {
    json!({ "execution_id": id, "node_id": node })
}

fn rpc(core: &Core, name: &str, id: &str, node: &str, extra: Value) -> Value {
    // What was sent has been handled before the module is asked.
    let _ = emitted(core, id, "error");
    let mut params = target(id, node);
    for (k, v) in extra.as_object().into_iter().flatten() {
        params[k] = v.clone();
    }
    core.dispatch(name, params).unwrap()
}

fn sha(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// What a reader of an export alone can establish: the hash of every source's text and the
/// slice every support points at. Returns each entry's status as that reader computes it.
fn verify_export(export: &Value) -> Vec<(String, String)> {
    let sources = export["sources"].as_array().unwrap();
    let mut out = Vec::new();
    for entry in export["entries"].as_array().unwrap() {
        let mut status = "valid";
        for support in entry["supports"].as_array().unwrap() {
            let source = sources.iter().find(|s| s["id"] == support["source"]);
            let Some(source) = source else {
                status = "missing_source";
                continue;
            };
            let text = source["text"].as_str().unwrap();
            assert_eq!(
                sha(text),
                source["sha256"],
                "the export's hash is the text's"
            );
            if support["sha256"] != source["sha256"] {
                status = "stale";
                continue;
            }
            let (start, end) = (
                support["start"].as_u64().unwrap() as usize,
                support["end"].as_u64().unwrap() as usize,
            );
            let slice: String = text.chars().skip(start).take(end - start).collect();
            if slice != support["excerpt"].as_str().unwrap() && status == "valid" {
                status = "excerpt_not_found";
            }
        }
        out.push((
            entry["id"].as_str().unwrap().to_string(),
            status.to_string(),
        ));
    }
    out
}

#[test]
fn sources_are_versioned_entries_are_checked_and_reports_cite_them() {
    let home = temp("flow");
    let core = core(&home);
    let id = start(&core, "evidence");

    send(
        &core,
        &id,
        "source",
        json!({ "location": LOCATION, "text": PAGE }),
    );
    let sourced = emitted(&core, &id, "sourced");
    assert_eq!(sourced.len(), 1);
    assert_eq!(sourced[0]["id"], "S1");
    assert_eq!(sourced[0]["version"], 1);
    assert_eq!(sourced[0]["created"], true);
    assert_eq!(sourced[0]["sha256"], sha(PAGE));
    assert!(
        sourced[0].get("text").is_none(),
        "outputs carry no source text"
    );

    // An exact excerpt is pinned to the version and the characters it was found at.
    send(
        &core,
        &id,
        "cite",
        json!({ "statement": "Ownership is checked at compile time",
                "source": "S1", "excerpt": "enforces ownership rules at compile\ntime" }),
    );
    let cited = emitted(&core, &id, "cited");
    assert_eq!(cited.len(), 1, "{cited:?}");
    assert_eq!(cited[0]["id"], "E1");
    assert_eq!(cited[0]["status"], "valid");
    assert_eq!(cited[0]["checked"], "excerpt_inclusion");
    assert_eq!(cited[0]["entailment"], "not_checked");
    let support = &cited[0]["supports"][0];
    assert_eq!(support["pinned_version"], 1);
    assert_eq!(support["pinned_sha256"], sha(PAGE));

    // Invalid references are refused, and say why. Nothing is registered.
    send(
        &core,
        &id,
        "cite",
        json!({ "statement": "x", "source": "S1", "excerpt": "garbage collectors never run" }),
    );
    send(
        &core,
        &id,
        "cite",
        json!({ "statement": "x", "source": "S9", "excerpt": "enforces ownership rules" }),
    );
    send(
        &core,
        &id,
        "cite",
        json!({ "statement": "x", "source": "S1", "excerpt": "ownership" }),
    );
    send(&core, &id, "cite", json!({ "statement": "no references" }));
    let errors = emitted(&core, &id, "error");
    let errors: Vec<&str> = errors.iter().map(|e| e.as_str().unwrap()).collect();
    assert_eq!(errors.len(), 4, "{errors:?}");
    assert!(errors[0].contains("is not in `S1`"), "{}", errors[0]);
    assert!(
        errors[1].contains("there is no source `S9` (sources: S1)"),
        "{}",
        errors[1]
    );
    assert!(errors[2].contains("min_excerpt_chars"), "{}", errors[2]);
    assert!(errors[3].contains("needs references"), "{}", errors[3]);
    assert_eq!(emitted(&core, &id, "cited").len(), 1);

    // `loose` matches across whitespace: the quote has one space where the page has a line
    // break; what is stored is the page's own slice.
    send(
        &core,
        &id,
        "cite_loose",
        json!({ "statement": "Errors are caught before running",
                "source": "S1", "excerpt": "ownership rules at compile time, so most memory errors" }),
    );
    let cited = emitted(&core, &id, "cited");
    assert_eq!(cited.len(), 2, "{cited:?}");
    assert_eq!(cited[1]["id"], "E2");
    assert_eq!(cited[1]["status"], "valid");
    assert!(cited[1]["supports"][0]["excerpt"]
        .as_str()
        .unwrap()
        .contains("compile\ntime"));

    // A report that cites valid entries passes, with the references to inspect.
    send(
        &core,
        &id,
        "draft",
        json!("Ownership is enforced before the program runs [E1]. Errors never run [E2][E1]."),
    );
    let report = emitted(&core, &id, "report");
    assert_eq!(report.len(), 1);
    let report = report[0].as_str().unwrap();
    assert!(report.starts_with("Ownership is enforced"), "{report}");
    assert!(report.contains("## References"), "{report}");
    assert!(
        report.contains(&format!("[{LOCATION}]({LOCATION})")),
        "{report}"
    );
    assert!(report.contains("source S1, version 1"), "{report}");
    assert!(report.contains(&sha(PAGE)[..12]), "{report}");
    assert!(
        report.contains("> enforces ownership rules at compile time"),
        "{report}"
    );
    let result = emitted(&core, &id, "result");
    assert_eq!(result[0]["valid"], true);
    assert_eq!(result[0]["cited"], json!(["E1", "E2"]));
    assert_eq!(result[0]["entailment"], "not_checked");

    // An unknown marker fails the report: nothing on `report`, the reasons on `error`.
    send(
        &core,
        &id,
        "draft",
        json!("Ownership is great [E1] and proven [E9]."),
    );
    assert_eq!(emitted(&core, &id, "report").len(), 1, "no second report");
    let errors = emitted(&core, &id, "error");
    let last = errors.last().unwrap().as_str().unwrap();
    assert!(
        last.contains("[E9] cites an entry that does not exist"),
        "{last}"
    );
    let result = emitted(&core, &id, "result");
    assert_eq!(result[1]["valid"], false);
    assert_eq!(result[1]["problems"][0]["kind"], "unknown_entry");

    // A draft with no citation does not pass `min_citations`; the strict gate also wants two
    // and one in every long paragraph.
    send(&core, &id, "draft", json!("Nothing cited here."));
    send(
        &core,
        &id,
        "strict_draft",
        json!("A paragraph that is long enough to need a citation but has none at all.\n\nCited [E1]."),
    );
    let result = emitted(&core, &id, "result");
    assert_eq!(result[2]["problems"][0]["kind"], "too_few_citations");
    let strict = emitted(&core, &id, "strict_error");
    let strict = strict[0].as_str().unwrap();
    assert!(
        strict.contains("cites 1 valid entries; at least 2"),
        "{strict}"
    );
    assert!(strict.contains("a paragraph cites nothing"), "{strict}");

    // The page changes (it still contains the quoted words): the source moves to version 2
    // and every citation made against version 1 stops validating.
    let revised = PAGE.replace("Garbage collectors", "Tracing collectors");
    send(
        &core,
        &id,
        "source",
        json!({ "location": LOCATION, "text": revised }),
    );
    let sourced = emitted(&core, &id, "sourced");
    assert_eq!(sourced[1]["id"], "S1", "same location, same source");
    assert_eq!(sourced[1]["version"], 2);
    assert_eq!(sourced[1]["changed"], true);
    assert_eq!(sourced[1]["stale_entries"], json!(["E1", "E2"]));
    send(&core, &id, "draft", json!("Still enforced [E1]."));
    let errors = emitted(&core, &id, "error");
    let last = errors.last().unwrap().as_str().unwrap();
    assert!(last.contains("[E1] does not validate (stale)"), "{last}");
    assert!(
        last.contains("cited against version 1 of `S1`, which is now version 2"),
        "{last}"
    );
    let listed = rpc(&core, "evidence.list", &id, "book", json!({}));
    assert_eq!(listed["status_counts"], json!({ "stale": 2 }));
    assert_eq!(listed["sources"][0]["version"], 2);

    // The same text again changes nothing; citing again pins the new version.
    send(
        &core,
        &id,
        "source",
        json!({ "id": "S1", "location": LOCATION, "text": revised }),
    );
    let sourced = emitted(&core, &id, "sourced");
    assert_eq!(sourced[2]["unchanged"], true);
    assert_eq!(sourced[2]["version"], 2);
    send(
        &core,
        &id,
        "cite",
        json!({ "statement": "Ownership is checked at compile time again",
                "source": "S1", "excerpt": "enforces ownership rules at compile\ntime" }),
    );
    assert_eq!(
        emitted(&core, &id, "cited")[2]["supports"][0]["pinned_version"],
        2
    );

    // Inspect: an entry with the text around its excerpt; a source with its versions.
    let entry = rpc(
        &core,
        "evidence.inspect",
        &id,
        "book",
        json!({ "entry": "E3", "context": 12 }),
    );
    assert_eq!(entry["status"], "valid");
    assert_eq!(entry["context"][0]["before"], "row checker ");
    assert_eq!(entry["context"][0]["after"], ", so most me");
    let source = rpc(
        &core,
        "evidence.inspect",
        &id,
        "book",
        json!({ "source": "S1" }),
    );
    assert_eq!(source["version"], 2);
    assert_eq!(source["history"][0]["version"], 1);
    assert_eq!(source["history"][0]["sha256"], sha(PAGE));
    assert!(source.get("text").is_none());
    assert_eq!(source["cited_by"], json!(["E1", "E2", "E3"]));
    let with_text = rpc(
        &core,
        "evidence.inspect",
        &id,
        "gate",
        json!({ "source": "S1", "text": true }),
    );
    assert_eq!(
        with_text["text"], revised,
        "any node of the module reads the collection"
    );

    // Export: a document whose verdicts a reader recomputes from the document alone.
    let export = rpc(&core, "evidence.export", &id, "book", json!({}));
    assert_eq!(export["schema"], "regulus.evidence.export/1");
    assert_eq!(
        export["summary"],
        json!({ "sources": 1, "entries": 3, "valid": 1, "not_valid": 2 })
    );
    let recomputed = verify_export(&export);
    assert_eq!(
        recomputed,
        [
            ("E1".to_string(), "stale".to_string()),
            ("E2".to_string(), "stale".to_string()),
            ("E3".to_string(), "valid".to_string())
        ]
    );
    for verdict in export["verdicts"].as_array().unwrap() {
        let mine = recomputed
            .iter()
            .find(|(i, _)| verdict["id"] == i.as_str())
            .unwrap();
        assert_eq!(
            verdict["status"],
            mine.1.as_str(),
            "the export agrees with its own data"
        );
    }
    let bare = rpc(
        &core,
        "evidence.export",
        &id,
        "book",
        json!({ "include_text": false }),
    );
    assert!(bare["sources"][0].get("text").is_none());
    let markdown = rpc(
        &core,
        "evidence.export",
        &id,
        "book",
        json!({ "format": "markdown" }),
    );
    let markdown = markdown["markdown"].as_str().unwrap();
    assert!(
        markdown.contains("[E3] (valid)") && markdown.contains("[E1] (stale)"),
        "{markdown}"
    );

    // Forgetting a source leaves its entries, reading `missing_source`.
    send(&core, &id, "forget", json!("S1"));
    let forgotten = emitted(&core, &id, "forgotten");
    assert_eq!(forgotten[0]["kind"], "source");
    assert_eq!(forgotten[0]["affected_entries"], json!(["E1", "E2", "E3"]));
    let listed = rpc(&core, "evidence.list", &id, "book", json!({}));
    assert_eq!(listed["status_counts"], json!({ "missing_source": 3 }));
    send(&core, &id, "forget", json!("E3"));
    assert_eq!(emitted(&core, &id, "forgotten")[1]["kind"], "entry");
    send(&core, &id, "forget", json!("nothing"));
    let errors = emitted(&core, &id, "error");
    assert!(errors
        .last()
        .unwrap()
        .as_str()
        .unwrap()
        .contains("there is no source or entry"));

    // An id that was never seen by this core's nodes is a clear error.
    let unknown = core
        .dispatch(
            "evidence.list",
            json!({ "execution_id": id, "node_id": "ghost" }),
        )
        .unwrap_err();
    let unknown = format!("{unknown:?}");
    assert!(unknown.contains("no evidence node `ghost`"), "{unknown}");
}

#[test]
fn limits_refuse_with_the_name_of_the_field() {
    let home = temp("limits");
    let core = core(&home);
    let id = start(&core, "evidence");
    // `tiny`: max_sources 1, max_entries 1, max_source_bytes 60, min_excerpt_chars 5.
    send(
        &core,
        &id,
        "tiny_source",
        json!({ "location": "a", "text": "x".repeat(61) }),
    );
    send(
        &core,
        &id,
        "tiny_source",
        json!({ "location": "a", "text": "   " }),
    );
    send(
        &core,
        &id,
        "tiny_source",
        json!({ "location": " ", "text": "text" }),
    );
    send(
        &core,
        &id,
        "tiny_source",
        json!({ "location": "a", "text": "alpha beta gamma delta" }),
    );
    send(
        &core,
        &id,
        "tiny_source",
        json!({ "location": "b", "text": "second source text" }),
    );
    let errors = emitted(&core, &id, "tiny_error");
    let errors: Vec<&str> = errors.iter().map(|e| e.as_str().unwrap()).collect();
    assert_eq!(errors.len(), 4, "{errors:?}");
    assert!(errors[0].contains("max_source_bytes"), "{}", errors[0]);
    assert!(errors[1].contains("has no text"), "{}", errors[1]);
    assert!(errors[2].contains("needs a `location`"), "{}", errors[2]);
    assert!(errors[3].contains("max_sources"), "{}", errors[3]);
    assert_eq!(emitted(&core, &id, "tiny_sourced").len(), 1);

    send(
        &core,
        &id,
        "tiny_cite",
        json!({ "statement": "s", "source": "S1", "excerpt": "alpha beta" }),
    );
    send(
        &core,
        &id,
        "tiny_cite",
        json!({ "statement": "t", "source": "S1", "excerpt": "gamma delta" }),
    );
    send(
        &core,
        &id,
        "tiny_cite",
        json!({ "statement": "t", "source": "S1", "excerpt": "beta" }),
    );
    let errors = emitted(&core, &id, "tiny_error");
    let (full, short) = (errors[4].as_str().unwrap(), errors[5].as_str().unwrap());
    assert!(full.contains("max_entries"), "{full}");
    assert!(short.contains("min_excerpt_chars"), "{short}");
    // The limits are a manifest field of the node, and the module says what it applies.
    let list = rpc(&core, "evidence.list", &id, "tiny", json!({}));
    assert_eq!(list["limits"]["max_sources"], 1);
    assert_eq!(list["limits"]["max_source_bytes"], 60);
    assert_eq!(list["sources"].as_array().unwrap().len(), 1);
    assert_eq!(list["entries"].as_array().unwrap().len(), 1);
}

#[test]
fn the_book_belongs_to_its_project_and_survives_a_restart() {
    let home = temp("restart");
    let core1 = core(&home);
    let a = start(&core1, "evidence-a");
    let b = start(&core1, "evidence-b");
    send(
        &core1,
        &a,
        "source",
        json!({ "location": "file:///a.md", "text": PAGE }),
    );
    send(
        &core1,
        &a,
        "cite",
        json!({ "statement": "A's claim", "source": "S1", "excerpt": "borrow checker enforces ownership" }),
    );
    assert_eq!(emitted(&core1, &a, "cited").len(), 1);

    // Another project sees none of it, and its own ids start from the beginning.
    let empty = rpc(&core1, "evidence.list", &b, "book", json!({}));
    assert_eq!(empty["sources"], json!([]));
    assert_eq!(empty["entries"], json!([]));
    send(
        &core1,
        &b,
        "source",
        json!({ "location": "file:///b.md", "text": "B's own page, with its own words" }),
    );
    assert_eq!(emitted(&core1, &b, "sourced")[0]["id"], "S1");
    send(
        &core1,
        &b,
        "cite",
        json!({ "statement": "x", "source": "S1", "excerpt": "borrow checker enforces ownership" }),
    );
    assert_eq!(emitted(&core1, &b, "cited").len(), 0, "A's text is not B's");

    // The global profile has a book of its own too.
    let global = start(&core1, "evidence");
    assert_eq!(
        rpc(&core1, "evidence.list", &global, "book", json!({}))["sources"],
        json!([])
    );

    for exec in [&a, &b, &global] {
        core1
            .dispatch(method::EXECUTION_STOP, json!({ "id": exec }))
            .unwrap();
    }
    core1.shutdown();
    drop(core1);

    // A new core on the same store: the book is in SQLite, not in the old process.
    let core2 = core(&home);
    let a = start(&core2, "evidence-a");
    let list = rpc(&core2, "evidence.list", &a, "book", json!({}));
    assert_eq!(list["sources"][0]["location"], "file:///a.md");
    assert_eq!(list["entries"][0]["id"], "E1");
    assert_eq!(list["entries"][0]["status"], "valid");
    // The citation still validates in the new process, and the report reads it.
    send(&core2, &a, "draft", json!("Ownership [E1]."));
    let report = emitted(&core2, &a, "report");
    assert!(
        report[0].as_str().unwrap().contains("file:///a.md"),
        "{report:?}"
    );
    // Ids are never reused: the next source is S2, the next entry E2.
    send(
        &core2,
        &a,
        "source",
        json!({ "location": "file:///c.md", "text": "Another page entirely" }),
    );
    assert_eq!(emitted(&core2, &a, "sourced")[0]["id"], "S2");
    // A change made after the restart still stales the old citation.
    send(
        &core2,
        &a,
        "source",
        json!({ "location": "file:///a.md", "text": "Rewritten after the restart" }),
    );
    let stale = rpc(
        &core2,
        "evidence.inspect",
        &a,
        "book",
        json!({ "entry": "E1" }),
    );
    assert_eq!(stale["status"], "stale");
    let b = start(&core2, "evidence-b");
    let list = rpc(&core2, "evidence.list", &b, "book", json!({}));
    assert_eq!(list["sources"][0]["location"], "file:///b.md");
    assert_eq!(list["sources"].as_array().unwrap().len(), 1);
}

fn call_tool(evidence: &Evidence, node: &str, config: Value, name: &str, args: Value) -> Value {
    let node = json!({ "id": node, "type": if node == "gate" { "report" } else { "store" },
                       "module_id": "regulus/evidence@0.1.0", "config": config });
    let args = json!({ "name": name, "args": args, "caller": { "name": "@dev" } });
    evidence
        .call(
            tool::CALL,
            json!({ "execution_id": "e", "node": node, "args": args }),
        )
        .unwrap()
}

fn specs(evidence: &Evidence, node: &str, config: Value) -> Vec<String> {
    let kind = if node == "gate" { "report" } else { "store" };
    let node = json!({ "id": node, "type": kind, "module_id": "regulus/evidence@0.1.0",
                       "config": config });
    let listed: Vec<tool::Spec> = serde_json::from_value(
        evidence
            .call(
                tool::LIST,
                json!({ "execution_id": "e", "node": node, "args": null }),
            )
            .unwrap(),
    )
    .unwrap();
    listed.into_iter().map(|s| s.name).collect()
}

#[test]
fn agents_read_cite_and_check_through_the_tools() {
    let evidence = Evidence::new();
    let config = json!({ "scope": "execution", "agent_sources": "yes", "max_read_chars": 40 });
    // What the agent is offered follows the node's fields.
    assert_eq!(
        specs(&evidence, "book", config.clone()),
        [
            "evidence_sources",
            "evidence_read",
            "evidence_entries",
            "evidence_cite",
            "evidence_add_source"
        ]
    );
    assert_eq!(
        specs(&evidence, "book", json!({ "agent_cites": "no" })),
        ["evidence_sources", "evidence_read", "evidence_entries"]
    );
    assert_eq!(specs(&evidence, "gate", json!({})), ["evidence_check"]);

    // Without `agent_sources` an agent cannot supply source text.
    let refused = call_tool(
        &evidence,
        "book",
        json!({ "scope": "execution" }),
        "evidence_add_source",
        json!({ "location": "x", "text": "y" }),
    );
    assert_eq!(refused["error"], true, "{refused}");

    let added = call_tool(
        &evidence,
        "book",
        config.clone(),
        "evidence_add_source",
        json!({ "id": "chosen", "location": LOCATION, "text": PAGE, "title": "Rust" }),
    );
    assert_eq!(
        added["data"]["id"], "S1",
        "an agent never picks an id: {added}"
    );
    let listed = call_tool(
        &evidence,
        "book",
        config.clone(),
        "evidence_sources",
        json!({}),
    );
    assert_eq!(listed["data"][0]["location"], LOCATION);
    assert!(listed["data"][0].get("text").is_none());

    // Reading in pieces, capped by `max_read_chars`.
    let first = call_tool(
        &evidence,
        "book",
        config.clone(),
        "evidence_read",
        json!({ "source": "S1", "length": 1000 }),
    );
    assert_eq!(first["data"]["end"], 40);
    assert_eq!(first["data"]["next_start"], 40);
    let next = call_tool(
        &evidence,
        "book",
        config.clone(),
        "evidence_read",
        json!({ "source": "S1", "start": 40 }),
    );
    let text: String = PAGE.chars().skip(40).take(40).collect();
    assert_eq!(next["data"]["text"], text.as_str());
    let tail = call_tool(
        &evidence,
        "book",
        config.clone(),
        "evidence_read",
        json!({ "source": "S1", "start": 10_000 }),
    );
    assert_eq!(tail["data"]["text"], "");
    assert_eq!(tail["data"]["next_start"], Value::Null);

    // Citing: the marker to write comes back; a made-up excerpt is an answer to read, not a
    // broken call; an agent's chosen id is ignored.
    let cited = call_tool(
        &evidence,
        "book",
        config.clone(),
        "evidence_cite",
        json!({ "id": "E99", "statement": "Memory errors are caught early",
                "source": "S1", "excerpt": "most memory errors never reach a running program" }),
    );
    assert_eq!(cited["data"]["cite_as"], "[E1]", "{cited}");
    assert_eq!(cited["data"]["entry"]["by"], "@dev");
    assert_eq!(cited["data"]["entry"]["status"], "valid");
    let made_up = call_tool(
        &evidence,
        "book",
        config.clone(),
        "evidence_cite",
        json!({ "statement": "s", "source": "S1", "excerpt": "most memory errors always reach production" }),
    );
    assert_eq!(made_up["error"], true);
    assert!(
        made_up["data"]
            .as_str()
            .unwrap()
            .contains("quote it exactly"),
        "{made_up}"
    );
    let entries = call_tool(
        &evidence,
        "book",
        config.clone(),
        "evidence_entries",
        json!({}),
    );
    assert_eq!(entries["data"].as_array().unwrap().len(), 1);
    assert_eq!(entries["data"][0]["status"], "valid");

    // The report node offers the same check the final gate makes.
    let gate = json!({ "scope": "execution" });
    let ok = call_tool(
        &evidence,
        "gate",
        gate.clone(),
        "evidence_check",
        json!({ "text": "Early [E1]." }),
    );
    assert_eq!(ok["data"]["valid"], true, "{ok}");
    let bad = call_tool(
        &evidence,
        "gate",
        gate,
        "evidence_check",
        json!({ "text": "Early [E2]." }),
    );
    assert_eq!(bad["data"]["valid"], false);
    assert_eq!(bad["data"]["problems"][0]["kind"], "unknown_entry");

    // Another execution has its own book when the scope is `execution`.
    let node = json!({ "id": "book", "type": "store", "module_id": "regulus/evidence@0.1.0",
                       "config": { "scope": "execution" } });
    let args = json!({ "name": "evidence_sources", "args": {}, "caller": { "name": "@dev" } });
    let other = evidence
        .call(
            tool::CALL,
            json!({ "execution_id": "another", "node": node, "args": args }),
        )
        .unwrap();
    assert_eq!(other["data"], json!([]));
}

#[test]
fn an_id_keeps_its_location_and_identical_text_revalidates() {
    let home = temp("location");
    let core = core(&home);
    let id = start(&core, "evidence");
    send(
        &core,
        &id,
        "source",
        json!({ "location": LOCATION, "text": PAGE, "title": "Rust" }),
    );
    send(
        &core,
        &id,
        "cite",
        json!({ "statement": "Compile-time ownership", "source": "S1",
                "excerpt": "enforces ownership rules at compile\ntime" }),
    );
    assert_eq!(emitted(&core, &id, "cited")[0]["status"], "valid");

    // The same id cannot be pointed at another place, with the same text or another.
    for text in [PAGE, "some other text entirely"] {
        send(
            &core,
            &id,
            "source",
            json!({ "id": "S1", "location": "https://elsewhere.test/x", "text": text }),
        );
    }
    let errors = emitted(&core, &id, "error");
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors[0].as_str().unwrap().contains("cannot be moved"),
        "{errors:?}"
    );
    let list = rpc(&core, "evidence.list", &id, "book", json!({}));
    assert_eq!(list["sources"][0]["location"], LOCATION);
    assert_eq!(list["sources"][0]["version"], 1);

    // An absurd title is refused, and counts against the bounds.
    send(
        &core,
        &id,
        "source",
        json!({ "location": "https://t.test", "text": "some text", "title": "t".repeat(500) }),
    );
    assert!(emitted(&core, &id, "error")[2]
        .as_str()
        .unwrap()
        .contains("max_title_chars"));

    // Text A → B → A: the version number moves on, but the hash is the pinned one again,
    // so the citation validates again (it is the same text at the same place).
    send(
        &core,
        &id,
        "source",
        json!({ "location": LOCATION, "text": "B text, entirely different" }),
    );
    assert_eq!(
        rpc(
            &core,
            "evidence.inspect",
            &id,
            "book",
            json!({ "entry": "E1" })
        )["status"],
        "stale"
    );
    send(
        &core,
        &id,
        "source",
        json!({ "location": LOCATION, "text": PAGE }),
    );
    let back = rpc(
        &core,
        "evidence.inspect",
        &id,
        "book",
        json!({ "entry": "E1" }),
    );
    assert_eq!(back["status"], "valid", "{back}");
    assert_eq!(back["supports"][0]["current_version"], 3);
    assert_eq!(back["supports"][0]["pinned_version"], 1);

    // Forgotten: the id is retired, so it cannot be issued again to another place or text.
    send(&core, &id, "forget", json!("S1"));
    send(
        &core,
        &id,
        "source",
        json!({ "id": "S1", "location": "https://mirror.test/rust", "text": PAGE }),
    );
    let errors = emitted(&core, &id, "error");
    assert!(
        errors
            .last()
            .unwrap()
            .as_str()
            .unwrap()
            .contains("ids are never reused"),
        "{errors:?}"
    );
    // The same text at another place gets a new id; the old citation does not follow it.
    send(
        &core,
        &id,
        "source",
        json!({ "location": "https://mirror.test/rust", "text": PAGE }),
    );
    assert_eq!(emitted(&core, &id, "sourced").last().unwrap()["id"], "S2");
    let moved = rpc(
        &core,
        "evidence.inspect",
        &id,
        "book",
        json!({ "entry": "E1" }),
    );
    assert_eq!(moved["status"], "missing_source", "{moved}");
}

#[test]
fn forgotten_ids_are_never_issued_again_and_batches_are_per_item() {
    let home = temp("retired");
    let core = core(&home);
    let id = start(&core, "evidence");
    send(
        &core,
        &id,
        "source",
        json!({ "location": LOCATION, "text": PAGE }),
    );
    let claim = |statement: &str, id: &str| {
        json!({ "id": id, "statement": statement, "source": "S1",
                "excerpt": "most memory errors never reach a running program" })
    };
    send(&core, &id, "cite", claim("first claim", "E9"));
    send(&core, &id, "forget", json!("E9"));
    send(&core, &id, "cite", claim("another claim", "E9"));
    let errors = emitted(&core, &id, "error");
    assert!(
        errors[0].as_str().unwrap().contains("ids are never reused"),
        "{errors:?}"
    );
    // The counter does not skip ids when a write is refused: the next free id is E1.
    send(&core, &id, "cite", claim("a claim", ""));
    assert_eq!(emitted(&core, &id, "cited").last().unwrap()["id"], "E1");

    // A batch registers what it can and reports the rest; the call succeeds, so a retry of
    // the whole batch is never needed (it would duplicate the good ones).
    send(
        &core,
        &id,
        "cite",
        json!([claim("second", ""), { "statement": "broken" }, claim("third", "")]),
    );
    let cited = emitted(&core, &id, "cited");
    let ids: Vec<&str> = cited.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert_eq!(ids, ["E9", "E1", "E2", "E3"], "{cited:?}");
    let errors = emitted(&core, &id, "error");
    assert_eq!(errors.len(), 2, "{errors:?}");
    assert!(
        errors[1].as_str().unwrap().contains("needs references"),
        "{errors:?}"
    );
    let list = rpc(&core, "evidence.list", &id, "book", json!({}));
    assert_eq!(list["entries"].as_array().unwrap().len(), 3);
}

#[test]
fn the_methods_find_a_node_of_an_ended_run_after_the_core_restarted() {
    let home = temp("methods-restart");
    let core1 = core(&home);
    let body = |g: &Core, exec: &str| {
        send(
            g,
            exec,
            "source",
            json!({ "location": LOCATION, "text": PAGE }),
        );
        send(
            g,
            exec,
            "cite",
            json!({ "statement": "Compile-time ownership", "source": "S1",
                    "excerpt": "enforces ownership rules at compile\ntime" }),
        );
        assert_eq!(emitted(g, exec, "cited").len(), 1);
        g.dispatch(method::EXECUTION_STOP, json!({ "id": exec }))
            .unwrap();
    };
    // A profile outside any project (global book) and one inside a project.
    let global = start(&core1, "evidence");
    body(&core1, &global);
    let project = start(&core1, "evidence-a");
    body(&core1, &project);
    core1.shutdown();
    drop(core1);

    // No execution is started in the new core: the old runs are only names, answered from
    // the persisted index of nodes.
    let core2 = core(&home);
    let params = json!({ "execution_id": global, "node_id": "book" });
    let list = core2.dispatch("evidence.list", params.clone()).unwrap();
    assert_eq!(list["sources"][0]["location"], LOCATION);
    assert_eq!(list["entries"][0]["status"], "valid");
    let export = core2.dispatch("evidence.export", params).unwrap();
    assert_eq!(
        verify_export(&export),
        [("E1".to_string(), "valid".to_string())]
    );

    // A project's book is read the same way: the core finds the project from the stored
    // run, with nothing running.
    let params = json!({ "execution_id": project, "node_id": "book" });
    let list = core2.dispatch("evidence.list", params.clone()).unwrap();
    assert_eq!(list["entries"][0]["status"], "valid", "{list}");
    let export = core2.dispatch("evidence.export", params).unwrap();
    assert_eq!(
        verify_export(&export),
        [("E1".to_string(), "valid".to_string())]
    );
}
