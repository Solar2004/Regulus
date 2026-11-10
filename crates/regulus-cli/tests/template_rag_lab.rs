//! The RAG lab (`templates/rag-lab.toml`) through the real `regulus` program and a real core
//! in a temporary home. No model and no network: two retrieval candidates over one local
//! corpus and dataset, scored with the documents evaluator and ranked by the data module.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{Duration, Instant};

use regulus_core::{local, Config};
use serde_json::{json, Value};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const WAIT: Duration = Duration::from_secs(30);

const RUST: &str = "# Rust\n\nThe borrow checker enforces ownership rules at compile time.\n\n\
                    ## Cargo\n\nCargo builds crates and fetches dependencies.\n";
const PYTHON: &str = "# Python\n\nPip installs packages from the package index.\n";
const NOTES: &str =
    "# Notes\n\nA reminder: Cargo is mentioned once in these notes about dependencies.\n";

fn templates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates")
}

fn regulus(home: &Path, args: &[&str]) -> Output {
    Command::new(PROGRAM)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn ok(home: &Path, args: &[&str]) -> String {
    let output = regulus(home, args);
    assert!(output.status.success(), "{args:?}: {}", text(&output));
    text(&output)
}

/// Stops every core serving `home`; the started core is detached, so nobody else will.
fn stop_cores(home: &Path) {
    let home = home.to_string_lossy().into_owned();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = cmdline
            .split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        if args.contains(&home) && args.iter().any(|a| a == "serve") {
            let pid = entry.file_name().to_string_lossy().into_owned();
            let _ = Command::new("kill").arg(pid).status();
        }
    }
}

/// A home with the repo's templates, a core started in it, `steps` run, then cleaned up.
fn with_core(name: &str, steps: impl FnOnce(&Path) + std::panic::UnwindSafe) {
    let home = std::env::temp_dir().join(format!("regulus-template-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    let config = format!(
        "[server]\nport = 0\n\n[templates]\npaths = [{:?}]\n",
        templates_dir().display().to_string()
    );
    std::fs::write(home.join("config.toml"), config).unwrap();
    let config = Config::load(&home).unwrap();
    let started = local::start(&config, Path::new(PROGRAM));
    let at = home.clone();
    let result = std::panic::catch_unwind(move || {
        started.unwrap();
        steps(&at);
    });
    stop_cores(&home);
    let _ = std::fs::remove_dir_all(&home);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

fn assert_check_is_clean(home: &Path, id: &str) {
    let params = format!(r#"{{"id":"{id}"}}"#);
    let check = ok(home, &["call", "profile.check", &params]);
    let diagnostics: Value = serde_json::from_str(&check).unwrap();
    assert_eq!(diagnostics, json!([]), "{id}: {diagnostics}");
}

/// The node a wire of the written profile goes to.
fn target(home: &Path, id: &str, wire: &str) -> String {
    let copy = std::fs::read_to_string(home.join(format!("profiles/{id}.toml"))).unwrap();
    let profile: toml::Value = toml::from_str(&copy).unwrap();
    let wires = profile["workflows"][0]["wires"].as_array().unwrap();
    let wire = wires
        .iter()
        .find(|w| w["id"].as_str() == Some(wire))
        .unwrap_or_else(|| panic!("no wire {wire}"));
    wire["to"]["node_id"].as_str().unwrap().to_owned()
}

fn run(home: &Path, profile: &str) -> String {
    let started = ok(home, &["run", profile, "--detach"]);
    assert!(started.contains("running"), "{started}");
    started.split_whitespace().next().unwrap().to_string()
}

/// Values the execution put on its own output `port`, oldest first.
fn emitted(home: &Path, execution: &str, port: &str) -> Vec<Value> {
    let params = json!({ "execution_id": execution, "limit": 10000 }).to_string();
    let page: Value = serde_json::from_str(&ok(home, &["call", "event.list", &params])).unwrap();
    page["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["from"]["node_id"] == "self" && e["from"]["port_id"] == port)
        .filter(|e| e["kind"] == "emitted")
        .map(|e| e["value"].clone())
        .collect()
}

fn until(home: &Path, execution: &str, port: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + WAIT;
    loop {
        let got = emitted(home, execution, port);
        if got.len() >= count {
            return got;
        }
        assert!(Instant::now() < deadline, "self.{port}: {got:?}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The corpus (rust.md, python.md, notes.md) and a dataset with an identical repeated line,
/// the same query with another expectation, a blank line and a line that is not JSON.
/// For "cargo builds crates dependencies" rust.md is the best hit and notes.md the second:
/// k=1 misses the expected notes.md, k=3 finds it. "borrow checker ownership" expecting
/// python.md is a case neither candidate can win.
fn lab_files(home: &Path) -> PathBuf {
    let docs = home.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("rust.md"), RUST).unwrap();
    std::fs::write(docs.join("python.md"), PYTHON).unwrap();
    std::fs::write(docs.join("notes.md"), NOTES).unwrap();
    std::fs::write(
        docs.join("lab.jsonl"),
        "{\"query\": \"borrow checker ownership\", \"expect_source_ids\": [\"rust.md\"]}\n\
         \n\
         {\"query\": \"pip packages\", \"expect_source_ids\": [\"python.md\"]}\n\
         this is not json\n\
         {\"query\": \"borrow checker ownership\", \"expect_source_ids\": [\"python.md\"]}\n\
         {\"query\": \"cargo builds crates dependencies\", \"expect_source_ids\": [\"notes.md\"]}\n\
         {\"query\": \"pip packages\", \"expect_source_ids\": [\"python.md\"]}\n",
    )
    .unwrap();
    docs
}

fn apply(home: &Path, id: &str, docs: &Path, extra: &[&str]) {
    let dir = format!("docs_dir={}", docs.display());
    let mut args = vec!["template", "apply", "rag-lab", "--id", id, "--set", &dir];
    args.extend_from_slice(&["--set", "dataset=lab.jsonl"]);
    args.extend_from_slice(extra);
    ok(home, &args);
    assert_check_is_clean(home, id);
}

fn first(home: &Path, id: &str, port: &str) -> Value {
    until(home, id, port, 1)[0].clone()
}

#[test]
fn rag_lab_needs_its_folder_and_applies_with_clean_checks() {
    with_core("rag-lab-apply", |home| {
        let missing = regulus(home, &["template", "apply", "rag-lab"]);
        assert!(!missing.status.success());
        assert!(text(&missing).contains("`docs_dir`"), "{}", text(&missing));
        let shown = ok(home, &["template", "ls"]);
        assert!(
            shown.contains("rag-lab") && shown.contains("needs"),
            "{shown}"
        );

        let docs = lab_files(home);
        apply(home, "lab-default", &docs, &[]);
        // The candidates are workflows of the profile, swappable by name.
        let copy = std::fs::read_to_string(home.join("profiles/lab-default.toml")).unwrap();
        let profile: toml::Value = toml::from_str(&copy).unwrap();
        let ids: Vec<&str> = profile["workflows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["lab", "retrieve_a", "retrieve_b"]);
        apply(
            home,
            "lab-swapped",
            &docs,
            &["--set", "candidate_a=retrieve_b"],
        );
        assert_eq!(
            target(home, "lab-swapped", "collect-out-batch-a-in"),
            "batch_a"
        );
    });
}

#[test]
fn rag_lab_scores_two_candidates_on_one_dataset_and_ranks_them() {
    with_core("rag-lab-run", |home| {
        let docs = lab_files(home);
        apply(
            home,
            "lab",
            &docs,
            &["--set", "k_a=1", "--set", "k_b=3", "--set", "threshold=0.9"],
        );
        let exec = run(home, "lab");
        ok(home, &["send", &exec, "go", "now"]);

        // One dataset for both: the identical repeat is dropped, the same query with another
        // expectation is kept, the bad line is reported by number.
        let stored = first(home, &exec, "dataset");
        assert_eq!(
            (&stored["added"], &stored["duplicates"], &stored["total"]),
            (&json!(4), &json!(1), &json!(4)),
            "{stored}"
        );
        assert_eq!(stored["rejected"][0]["line"], 4, "{stored}");
        let errors = until(home, &exec, "error", 1);
        assert!(
            errors[0].as_str().unwrap().contains("not JSON"),
            "{errors:?}"
        );

        // Candidate metrics, both scored on the same four items: a finds 2 (k=1), b 3 (k=3).
        let candidates = first(home, &exec, "candidates");
        assert_eq!(
            (&candidates["a"]["total"], &candidates["a"]["found"]),
            (&json!(4), &json!(2)),
            "{candidates}"
        );
        assert_eq!(candidates["b"]["total"], 4, "{candidates}");
        assert_eq!(candidates["b"]["found"], 3, "{candidates}");
        assert_eq!(candidates["a"]["items"].as_array().unwrap().len(), 4);

        // The comparison: b leads, a is a quarter behind and measured against itself.
        let comparison = first(home, &exec, "comparison");
        assert_eq!(comparison["best"], json!(["b"]), "{comparison}");
        assert_eq!(comparison["tie"], false);
        let rows = comparison["ranking"].as_array().unwrap();
        assert_eq!(
            (rows[0]["label"].as_str(), rows[1]["label"].as_str()),
            (Some("fixed-300"), Some("markdown-1200"))
        );
        assert!(
            (rows[1]["delta_best"].as_f64().unwrap() + 0.25).abs() < 1e-9,
            "{rows:?}"
        );
        assert!((rows[0]["delta_baseline"].as_f64().unwrap() - 0.25).abs() < 1e-9);
        assert_eq!(
            (&rows[0]["scored"], &comparison["scored"]),
            (&json!(4.0), &json!(4.0))
        );
        assert_eq!(first(home, &exec, "winner"), "fixed-300");
        // Both collections were ingested: three files each, nothing twice.
        assert_eq!(until(home, &exec, "ingested", 6).len(), 6);
        ok(home, &["stop", &exec]);

        // The data module's own commands see the dataset the lab collected.
        let again = run(home, "lab");
        ok(home, &["send", &again, "go", "now"]);
        first(home, &again, "comparison");
        let ingested = until(home, &again, "ingested", 6);
        assert!(
            ingested.iter().all(|i| i["unchanged"] == true),
            "{ingested:?}"
        );
        let params = json!({ "execution_id": again, "node_id": "collect" }).to_string();
        let exported: Value =
            serde_json::from_str(&ok(home, &["call", "data.export", &params])).unwrap();
        assert_eq!(exported["items"], 4, "{exported}");
        assert_eq!(
            exported["records"][3]["record"]["query"],
            "cargo builds crates dependencies"
        );
    });
}

#[test]
fn rag_lab_candidates_swap_by_workflow_name_and_ties_are_told() {
    with_core("rag-lab-swap", |home| {
        let docs = lab_files(home);
        // Candidate a runs b's workflow (k=3 over b's collection… a's own collection is
        // ingested too, but the workflow reads `rag-lab-b`): both candidates are the same.
        apply(
            home,
            "same",
            &docs,
            &[
                "--set",
                "candidate_a=retrieve_b",
                "--set",
                "k_a=1",
                "--set",
                "k_b=3",
            ],
        );
        let exec = run(home, "same");
        ok(home, &["send", &exec, "go", "now"]);
        let comparison = first(home, &exec, "comparison");
        assert_eq!(comparison["tie"], true, "{comparison}");
        assert_eq!(comparison["best"], json!(["a", "b"]));
        let winner = first(home, &exec, "winner");
        assert_eq!(winner, "markdown-1200, fixed-300");
        assert_eq!(first(home, &exec, "candidates")["a"]["found"], 3);
    });
}

#[test]
fn rag_lab_ingests_afresh_when_a_candidate_is_chunked_another_way() {
    with_core("rag-lab-rerun", |home| {
        let docs = lab_files(home);
        apply(home, "first", &docs, &[]);
        let exec = run(home, "first");
        ok(home, &["send", &exec, "go", "now"]);
        first(home, &exec, "comparison");
        let before = until(home, &exec, "ingested", 6);
        assert!(before.iter().all(|i| i["unchanged"] == false), "{before:?}");
        let chunks_a: u64 = before
            .iter()
            .filter(|i| i["collection"].as_str().unwrap().starts_with("rag-lab-a"))
            .map(|i| i["chunks"].as_u64().unwrap())
            .sum();
        ok(home, &["stop", &exec]);

        // Candidate a is now cut in 40-character pieces: its files are ingested again, in a
        // collection named for that chunking, and not reused as the old chunks. Candidate b,
        // untouched, is unchanged.
        apply(
            home,
            "second",
            &docs,
            &["--set", "chunker_a=fixed", "--set", "chunk_chars_a=40"],
        );
        let exec = run(home, "second");
        ok(home, &["send", &exec, "go", "now"]);
        first(home, &exec, "comparison");
        let after = until(home, &exec, "ingested", 6);
        let of = |prefix: &str| -> Vec<&Value> {
            after
                .iter()
                .filter(|i| i["collection"].as_str().unwrap().starts_with(prefix))
                .collect()
        };
        assert!(
            of("rag-lab-a-fixed-40-150")
                .iter()
                .all(|i| i["unchanged"] == false),
            "{after:?}"
        );
        assert_eq!(of("rag-lab-a-fixed-40-150").len(), 3);
        let chunks_now: u64 = of("rag-lab-a-fixed-40-150")
            .iter()
            .map(|i| i["chunks"].as_u64().unwrap())
            .sum();
        assert!(
            chunks_now > chunks_a,
            "{chunks_now} chunks of 40 characters vs {chunks_a}"
        );
        assert!(
            of("rag-lab-b-fixed-300-0")
                .iter()
                .all(|i| i["unchanged"] == true),
            "{after:?}"
        );
    });
}

#[test]
fn rag_lab_refuses_to_compare_candidates_that_were_not_scored_alike() {
    with_core("rag-lab-uncomparable", |home| {
        let docs = lab_files(home);
        apply(home, "lab", &docs, &[]);
        // `broken` answers every query with an error text, never with hits: nothing is
        // scored for candidate a (the batch would otherwise be a quiet recall of 0.0).
        let path = home.join("profiles/lab.toml");
        let mut profile: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let broken: toml::Value = toml::from_str(
            r#"
            [[workflows]]
            id = "broken"
            name = "Broken candidate"
            kind = "extra"
            ports = [
              { id = "in", capability = "json@1", direction = "input", kind = "then" },
              { id = "hits", capability = "json@1", direction = "output", kind = "then" },
              { id = "error", capability = "text@1", direction = "output", kind = "then" },
            ]
              [[workflows.nodes]]
              id = "boom"
              name = "Boom"
              type = "convert"
              module_id = "regulus/flow@0.1.0"
              position = { x = 0, y = 0 }
              config = { conversion = "template", template = "no engine today" }
              ports = [
                { id = "in", capability = "json@1", direction = "input", kind = "then", cardinality = "many" },
                { id = "out", capability = "text@1", direction = "output", kind = "then" },
              ]
              [[workflows.wires]]
              id = "in-boom"
              kind = "then"
              from = { node_id = "self", port_id = "in" }
              to = { node_id = "boom", port_id = "in" }
              [[workflows.wires]]
              id = "boom-error"
              kind = "then"
              from = { node_id = "boom", port_id = "out" }
              to = { node_id = "self", port_id = "error" }
            "#,
        )
        .unwrap();
        let workflows = profile["workflows"].as_array_mut().unwrap();
        workflows.push(broken["workflows"][0].clone());
        let main = workflows
            .iter_mut()
            .find(|w| w["id"].as_str() == Some("lab"))
            .unwrap();
        let batch = main["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|n| n["id"].as_str() == Some("batch_a"))
            .unwrap();
        batch["config"]["workflow"] = toml::Value::String("broken".into());
        std::fs::write(&path, toml::to_string(&profile).unwrap()).unwrap();
        ok(
            home,
            &["profile", "save", path.to_str().unwrap(), "--force"],
        );

        let exec = run(home, "lab");
        ok(home, &["send", &exec, "go", "now"]);
        let errors = until(home, &exec, "error", 2);
        let said: Vec<&str> = errors.iter().map(|e| e.as_str().unwrap()).collect();
        assert!(
            said.iter()
                .any(|e| e.contains("not comparable") && e.contains("a: 0")),
            "{said:?}"
        );
        assert!(emitted(home, &exec, "comparison").is_empty());
        assert!(emitted(home, &exec, "winner").is_empty());
        // The metrics each candidate did produce are still there to read.
        assert_eq!(first(home, &exec, "candidates")["b"]["total"], 4);
    });
}
