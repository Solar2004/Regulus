//! The RAG templates (`templates/rag-chat.toml`, `templates/rag-eval.toml`) and the
//! documents example (`crates/modules/documents/examples/rag.toml`), through the real
//! `regulus` program and a real core in a temporary home. No real model: rag-chat runs
//! with `answerer = complete` against a fake OpenAI server; rag-eval runs without its
//! judge; the example runs with no model at all and is driven with `regulus docs`.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_core::{local, Config};
use serde_json::{json, Value};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const WAIT: Duration = Duration::from_secs(20);

const RUST: &str = "# Rust\n\nThe borrow checker enforces ownership rules at compile time.\n\n\
                    ## Cargo\n\nCargo builds crates and fetches dependencies.\n";
const PYTHON: &str = "# Python\n\nPip installs packages from the package index.\n";

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

/// The documents folder: rust.md and python.md.
fn docs(home: &Path) -> PathBuf {
    let docs = home.join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("rust.md"), RUST).unwrap();
    std::fs::write(docs.join("python.md"), PYTHON).unwrap();
    docs
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

/// A fake OpenAI chat-completions server: answers each request with the next of
/// `answers` (the last repeats) and keeps the prompts it got.
fn fake_model(answers: Vec<&'static str>) -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<String>>> = Arc::default();
    let log = seen.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut length = 0;
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let line = line.trim_end().to_ascii_lowercase();
                if line.is_empty() {
                    break;
                }
                if let Some(value) = line.strip_prefix("content-length:") {
                    length = value.trim().parse().unwrap();
                }
            }
            let mut body = vec![0; length];
            reader.read_exact(&mut body).unwrap();
            let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
            let prompt = body["messages"]
                .as_array()
                .and_then(|m| m.last())
                .map(|m| m["content"].as_str().unwrap_or_default().to_string())
                .unwrap_or_default();
            let count = {
                let mut log = log.lock().unwrap();
                log.push(prompt);
                log.len()
            };
            let answer = answers[(count - 1).min(answers.len() - 1)];
            let reply =
                json!({ "choices": [{ "message": { "role": "assistant", "content": answer } }],
                                "usage": { "prompt_tokens": 10, "completion_tokens": 5 } })
                .to_string();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{reply}",
                reply.len()
            );
        }
    });
    (url, seen)
}

#[test]
fn rag_chat_applies_checks_and_answers_with_sources_through_a_fake_model() {
    with_core("rag-chat", |home| {
        let missing = regulus(home, &["template", "apply", "rag-chat"]);
        assert!(!missing.status.success());
        assert!(text(&missing).contains("`docs_dir`"), "{}", text(&missing));

        let docs = docs(home);
        let dir = format!("docs_dir={}", docs.display());
        ok(home, &["template", "apply", "rag-chat", "--set", &dir]);
        assert_eq!(
            target(home, "rag-chat", "context-out-to-answerer-in"),
            "to_agent"
        );
        assert_check_is_clean(home, "rag-chat");

        let (url, seen) = fake_model(vec![
            "The borrow checker enforces ownership [1].",
            "Guido says so [9].",
        ]);
        let base = format!("inference_base_url={url}");
        ok(
            home,
            &[
                "template",
                "apply",
                "rag-chat",
                "--set",
                &dir,
                "--set",
                "answerer=complete",
                "--set",
                &base,
                "--set",
                "inference_model=fake-1",
                "--set",
                "k=2",
                "--id",
                "rag-model",
            ],
        );
        assert_eq!(
            target(home, "rag-model", "context-out-to-answerer-in"),
            "to_complete"
        );
        assert_check_is_clean(home, "rag-model");

        let exec = run(home, "rag-model");
        ok(home, &["send", &exec, "ingest", "."]);
        let ingested = until(home, &exec, "ingested", 2);
        assert!(
            ingested.iter().all(|i| i["replaced"] == false),
            "{ingested:?}"
        );
        ok(home, &["send", &exec, "chat", "Who enforces ownership?"]);
        let reply = until(home, &exec, "reply", 1)[0].clone();
        let reply = reply.as_str().unwrap();
        assert!(
            reply.starts_with("The borrow checker enforces ownership [1]."),
            "{reply}"
        );
        assert!(
            reply.contains("Sources:\n[1] ") && reply.contains("rust.md"),
            "{reply}"
        );
        // The model got the numbered evidence.
        let prompt = seen.lock().unwrap()[0].clone();
        assert!(prompt.contains("[1] # Rust"), "{prompt}");
        assert!(prompt.contains("Who enforces ownership?"), "{prompt}");

        // A made-up citation comes back through the ungrounded branch.
        ok(home, &["send", &exec, "chat", "Who made pip?"]);
        let replies = until(home, &exec, "reply", 2);
        let second = replies[1].as_str().unwrap();
        assert!(second.starts_with("Guido says so [9]."), "{second}");
        assert!(second.contains("Not grounded in the documents"), "{second}");
        assert!(second.contains("[9]"), "{second}");
    });
}

#[test]
fn rag_eval_scores_recall_against_its_threshold_without_a_model() {
    with_core("rag-eval", |home| {
        let docs = docs(home);
        std::fs::write(
            docs.join("eval.jsonl"),
            "{\"query\": \"borrow checker ownership\", \"expect_source_ids\": [\"rust.md\"]}\n\
             \n\
             {\"query\": \"pip packages\", \"expect_source_ids\": [\"python.md\"]}\n\
             {\"query\": \"garbage collector\", \"expect_source_ids\": [\"java.md\"]}\n",
        )
        .unwrap();
        let dir = format!("docs_dir={}", docs.display());
        let common = [
            "--set",
            &dir,
            "--set",
            "dataset=eval.jsonl",
            "--set",
            "count=3",
        ];
        let apply = |id: &str, extra: &[&str]| {
            let mut args = vec!["template", "apply", "rag-eval", "--id", id];
            args.extend_from_slice(&common);
            args.extend_from_slice(extra);
            ok(home, &args);
            assert_check_is_clean(home, id);
        };
        apply("eval-judge", &["--set", "judge_by=judge"]);
        assert_eq!(
            target(home, "eval-judge", "context-out-to-judge-by-in"),
            "to_judge"
        );
        apply("eval-strict", &["--set", "threshold=0.9", "--set", "k=1"]);
        assert_eq!(
            target(home, "eval-strict", "context-out-to-judge-by-in"),
            "to_none"
        );
        apply("eval-loose", &["--set", "threshold=0.6", "--set", "k=1"]);

        let exec = run(home, "eval-strict");
        ok(home, &["send", &exec, "go", "now"]);
        let failed = until(home, &exec, "failed", 1)[0].clone();
        assert_eq!(
            (&failed["total"], &failed["found"]),
            (&json!(3), &json!(2)),
            "{failed}"
        );
        assert!(emitted(home, &exec, "passed").is_empty());
        let items = failed["items"].as_array().unwrap();
        assert_eq!(items[2]["hit"], false, "{failed}");
        assert_eq!(until(home, &exec, "ingested", 2).len(), 2);
        ok(home, &["stop", &exec]);

        let exec = run(home, "eval-loose");
        ok(home, &["send", &exec, "go", "now"]);
        let passed = until(home, &exec, "passed", 1)[0].clone();
        assert!(
            (passed["recall"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-9,
            "{passed}"
        );
        // The collection is the project's: the same bytes again change nothing.
        let ingested = until(home, &exec, "ingested", 2);
        assert!(
            ingested.iter().all(|i| i["unchanged"] == true),
            "{ingested:?}"
        );
    });
}

#[test]
fn the_documents_example_runs_by_cli_with_docs_commands() {
    with_core("rag-example", |home| {
        let docs = docs(home);
        let example =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../modules/documents/examples/rag.toml");
        let profile = std::fs::read_to_string(example).unwrap().replace(
            "dir = \"\"",
            &format!("dir = {:?}", docs.display().to_string()),
        );
        let file = home.join("rag.toml");
        std::fs::write(&file, profile).unwrap();
        ok(home, &["profile", "save", &file.display().to_string()]);
        let exec = run(home, "rag");
        ok(home, &["send", &exec, "ingest", "rust.md"]);
        ok(home, &["send", &exec, "ingest", "python.md"]);
        until(home, &exec, "ingested", 2);

        let listed = ok(home, &["docs", "ls", &exec, "ingest"]);
        assert_eq!(listed.lines().count(), 2, "{listed}");
        assert!(
            listed.contains("rust.md") && listed.contains("python.md"),
            "{listed}"
        );
        let rust_id = listed
            .lines()
            .find(|l| l.contains("rust.md"))
            .and_then(|l| l.split_whitespace().next())
            .unwrap()
            .to_string();

        ok(home, &["send", &exec, "ask", "Who enforces ownership?"]);
        let context = until(home, &exec, "context", 1)[0].clone();
        assert!(
            context.as_str().unwrap().contains("[1] # Rust"),
            "{context}"
        );
        ok(
            home,
            &["send", &exec, "answer", "The borrow checker does [1]."],
        );
        let reply = until(home, &exec, "reply", 1)[0].clone();
        assert!(
            reply.as_str().unwrap().contains("Sources:\n[1] "),
            "{reply}"
        );
        assert_eq!(until(home, &exec, "passed", 1)[0]["cited"], json!([1]));

        let found = ok(
            home,
            &[
                "docs", "query", &exec, "retrieve", "pip", "packages", "--k", "1",
            ],
        );
        assert!(
            found.starts_with("[1] ") && found.contains("python.md"),
            "{found}"
        );
        let removed = ok(home, &["docs", "rm", &exec, "ingest", &rust_id]);
        assert!(removed.contains(&format!("deleted {rust_id}")), "{removed}");
        let found = ok(
            home,
            &["docs", "query", &exec, "retrieve", "borrow", "checker"],
        );
        assert!(!found.contains("rust.md"), "{found}");
        let again = regulus(home, &["docs", "rm", &exec, "ingest", &rust_id]);
        assert!(!again.status.success());
    });
}
