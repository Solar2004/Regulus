//! The repository's `templates/research-evidence.toml` (docs/15-evidence.md), through the
//! real `regulus` program and a real core in a temporary home. It applies and passes
//! `profile.check` with no diagnostic; against a fake OpenAI server and a local web fixture
//! the native engine searches, collects a page, cites two excerpts and answers, and the
//! report leaves with inspectable references. A made-up excerpt is refused, a report that
//! cites an entry that does not exist fails, and a changed page makes old citations stale.
//! No real provider and no real search engine are involved.

use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_core::{local, Config};
use serde_json::{json, Value};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const TEMPLATE: &str = "research-evidence";
const WAIT: Duration = Duration::from_secs(30);

const FIRST: &str = "The borrow checker enforces ownership rules at compile time.";
const SECOND: &str = "Memory errors are rejected before the program runs.";

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
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn call(home: &Path, method: &str, params: Value) -> Value {
    serde_json::from_str(&ok(home, &["call", method, &params.to_string()])).unwrap()
}

/// One request: its path and body.
struct Request {
    path: String,
    body: Vec<u8>,
}

/// Answers `(status, content type, body)` to each request on its own thread.
fn serve(
    handler: impl Fn(&Request) -> (u16, &'static str, String) + Send + Sync + 'static,
) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let handler = Arc::new(handler);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let handler = handler.clone();
            std::thread::spawn(move || {
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut start = String::new();
                if reader.read_line(&mut start).unwrap_or(0) == 0 {
                    return;
                }
                let path = start.split_whitespace().nth(1).unwrap_or("/").to_string();
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
                let (status, kind, reply) = handler(&Request { path, body });
                let _ = write!(
                    stream,
                    "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n\
                     connection: close\r\n\r\n{reply}",
                    reply.len()
                );
            });
        }
    });
    base
}

/// The local web: a SearXNG-style search endpoint and one page, whose body can be changed.
fn web() -> (String, Arc<Mutex<String>>) {
    let page = Arc::new(Mutex::new(format!(
        "<html><head><title>Rust ownership</title></head><body><h1>Rust ownership</h1>\
         <p>{FIRST}</p><p>{SECOND}</p></body></html>"
    )));
    let served = page.clone();
    // The page's URL is its own base: filled once the listener has an address.
    let base_cell: Arc<Mutex<String>> = Arc::default();
    let cell = base_cell.clone();
    let base = serve(move |request| {
        if request.path.starts_with("/search") {
            let url = format!("{}/rust", cell.lock().unwrap());
            let body = json!({ "results": [{ "title": "Rust ownership", "url": url,
                "content": "How Rust checks ownership." }] });
            return (200, "application/json", body.to_string());
        }
        if request.path == "/rust" {
            return (200, "text/html", served.lock().unwrap().clone());
        }
        (404, "text/plain", "not found".into())
    });
    *base_cell.lock().unwrap() = base.clone();
    (base, page)
}

/// One step of a scripted model.
enum Step {
    Call(&'static str, Value),
    Say(String),
}

/// A fake OpenAI chat-completions server that follows `script`, one step per request, and
/// keeps every request body.
fn fake_model(script: Vec<Step>) -> (String, Arc<Mutex<Vec<Value>>>) {
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
    let log = seen.clone();
    let script = Arc::new(script);
    let base = serve(move |request| {
        let body: Value = serde_json::from_slice(&request.body).unwrap_or(Value::Null);
        let count = {
            let mut log = log.lock().unwrap();
            log.push(body);
            log.len()
        };
        let message = match script.get(count - 1) {
            Some(Step::Call(name, args)) => json!({ "role": "assistant", "content": null,
                "tool_calls": [{ "id": format!("c{count}"), "type": "function", "function": {
                    "name": name, "arguments": args.to_string() } }] }),
            Some(Step::Say(text)) => json!({ "role": "assistant", "content": text }),
            None => json!({ "role": "assistant", "content": "(script over)" }),
        };
        (
            200,
            "application/json",
            json!({ "choices": [{ "message": message }] }).to_string(),
        )
    });
    (format!("{base}/v1"), seen)
}

/// The text of the tool result that answered request `n` (0-based) of the model: the last
/// `tool` message of that request.
fn observation(seen: &[Value], n: usize) -> String {
    seen[n]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .find(|m| m["role"] == "tool")
        .unwrap_or_else(|| panic!("no tool message in request {n}: {}", seen[n]))["content"]
        .as_str()
        .unwrap()
        .to_string()
}

fn apply(home: &Path, model: &str, web: &str, extra: &[&str]) {
    let base = format!("base_url={model}");
    let search = format!("search_url={web}/search");
    let mut args = vec![
        "template",
        "apply",
        TEMPLATE,
        "--set",
        &base,
        "--set",
        "model=fake-1",
        "--set",
        &search,
        "--set",
        "allow_private=yes",
    ];
    args.extend_from_slice(extra);
    ok(home, &args);
    assert!(
        !std::fs::read_to_string(home.join(format!("profiles/{TEMPLATE}.toml")))
            .unwrap()
            .contains("\n[template")
    );
    assert_check_is_clean(home, TEMPLATE);
}

fn tool_names(request: &Value) -> BTreeSet<String> {
    request["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn the_template_is_listed_applies_and_passes_the_check() {
    let (web, _) = web();
    let (model, _) = fake_model(vec![]);
    with_core("research-evidence-check", |home| {
        let ls = ok(home, &["template", "ls"]);
        assert!(ls.contains(TEMPLATE), "{ls}");
        apply(home, &model, &web, &[]);
        // Every node is wired, and the key wires are there.
        let copy = std::fs::read_to_string(home.join(format!("profiles/{TEMPLATE}.toml"))).unwrap();
        let profile: toml::Value = toml::from_str(&copy).unwrap();
        for workflow in profile["workflows"].as_array().unwrap() {
            let wires = workflow["wires"].as_array().unwrap();
            for node in workflow["nodes"].as_array().unwrap() {
                let id = node["id"].as_str().unwrap();
                let touched = wires.iter().any(|w| {
                    w["from"]["node_id"].as_str() == Some(id)
                        || w["to"]["node_id"].as_str() == Some(id)
                });
                assert!(touched, "{}/{id} has no wire", workflow["id"]);
            }
        }
        // No shell, no file tools: the only boxes that give the agent tools are these.
        assert!(
            !copy.contains("regulus/process") && !copy.contains("regulus/workspace"),
            "{copy}"
        );
        assert!(copy.contains(r#"tool_policy = "deny""#), "{copy}");
    });
}

#[test]
fn a_researcher_cites_exact_excerpts_and_the_report_carries_inspectable_references() {
    let (web, _) = web();
    let url = format!("{web}/rust");
    let draft =
        "Rust checks ownership at compile time [E1]. Memory errors are rejected early [E2].";
    let (model, seen) = fake_model(vec![
        Step::Call("web_search", json!({ "text": "rust ownership" })),
        Step::Call("collect_source", json!({ "text": url })),
        Step::Call("evidence_read", json!({ "source": "S1" })),
        Step::Call(
            "evidence_cite",
            json!({ "statement": "Rust checks ownership at compile time",
            "source": "S1", "excerpt": FIRST }),
        ),
        Step::Call(
            "evidence_cite",
            json!({ "statement": "Memory errors are rejected early",
            "source": "S1", "excerpt": SECOND }),
        ),
        Step::Call("evidence_check", json!({ "text": draft })),
        Step::Say(draft.to_string()),
    ]);
    with_core("research-evidence-run", |home| {
        apply(home, &model, &web, &[]);
        let exec = run(home, TEMPLATE);
        ok(
            home,
            &["send", &exec, "question", "How does Rust check ownership?"],
        );
        let report = until(home, &exec, "report", 1)[0]
            .as_str()
            .unwrap()
            .to_string();

        // The report is the model's text followed by references a reader can follow.
        assert!(
            report.starts_with("Rust checks ownership at compile time [E1]."),
            "{report}"
        );
        assert!(report.contains("## References"), "{report}");
        assert!(report.contains(&format!("[{url}]({url})")), "{report}");
        assert!(report.contains("source S1, version 1"), "{report}");
        assert!(report.contains(&format!("> {FIRST}")), "{report}");
        assert!(report.contains(&format!("> {SECOND}")), "{report}");
        let verdict = &until(home, &exec, "result", 1)[0];
        assert_eq!(verdict["valid"], true, "{verdict}");
        assert_eq!(verdict["cited"], json!(["E1", "E2"]));
        assert_eq!(verdict["entailment"], "not_checked");
        assert!(emitted(home, &exec, "error").is_empty());
        assert_eq!(until(home, &exec, "sourced", 1)[0]["id"], "S1");

        // What the model was given: the tools of the boxes wired into it, and no shell.
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 7, "one request per step");
        let expected: BTreeSet<String> = [
            "collect_source",
            "evidence_check",
            "evidence_cite",
            "evidence_entries",
            "evidence_read",
            "evidence_sources",
            "read_chunk",
            "search_documents",
            "web_search",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(tool_names(&seen[0]), expected);
        // The search went through the fixture engine, the page through the fetch box.
        assert!(
            observation(&seen, 1).contains(&url),
            "{}",
            observation(&seen, 1)
        );
        let collected = observation(&seen, 2);
        assert!(
            collected.contains(r#""id":"S1""#) && collected.contains(r#""version":1"#),
            "{collected}"
        );
        assert!(
            observation(&seen, 3).contains(FIRST),
            "evidence_read shows the text"
        );
        assert!(
            observation(&seen, 4).contains(r#""cite_as":"[E1]""#),
            "{}",
            observation(&seen, 4)
        );
        assert!(
            observation(&seen, 6).contains(r#""valid":true"#),
            "{}",
            observation(&seen, 6)
        );
        drop(seen);

        // The book is inspectable through the module's methods: the export is a document a
        // reader can check on its own.
        let target = |extra: Value| {
            let mut params = json!({ "execution_id": exec, "node_id": "book" });
            for (k, v) in extra.as_object().into_iter().flatten() {
                params[k] = v.clone();
            }
            params
        };
        let export = call(home, "evidence.export", target(json!({})));
        assert_eq!(
            export["summary"],
            json!({ "sources": 1, "entries": 2, "valid": 2, "not_valid": 0 })
        );
        let source = &export["sources"][0];
        assert_eq!(source["location"], url.as_str());
        let source_text = source["text"].as_str().unwrap();
        for entry in export["entries"].as_array().unwrap() {
            let support = &entry["supports"][0];
            let (start, end) = (
                support["start"].as_u64().unwrap() as usize,
                support["end"].as_u64().unwrap() as usize,
            );
            let slice: String = source_text.chars().skip(start).take(end - start).collect();
            assert_eq!(slice, support["excerpt"].as_str().unwrap());
            assert_eq!(support["sha256"], source["sha256"]);
        }
        // The collected page is in the documents the agent can search.
        let docs = call(
            home,
            "documents.list",
            json!({ "execution_id": exec, "node_id": "retrieve" }),
        );
        assert_eq!(docs.as_array().map(Vec::len), Some(1), "{docs}");

        // The page changes: a source supplied by hand with the same location is a new
        // version, and both citations stop validating.
        let changed =
            json!({ "location": url, "text": "The page was rewritten and no longer says that." });
        ok(home, &["send", &exec, "seed", &changed.to_string()]);
        let sourced = until(home, &exec, "sourced", 2);
        assert_eq!(sourced[1]["version"], 2, "{sourced:?}");
        assert_eq!(sourced[1]["stale_entries"], json!(["E1", "E2"]));
        let list = call(home, "evidence.list", target(json!({})));
        assert_eq!(list["status_counts"], json!({ "stale": 2 }));
        let entry = call(home, "evidence.inspect", target(json!({ "entry": "E1" })));
        assert_eq!(entry["status"], "stale");
    });
}

#[test]
fn a_made_up_excerpt_and_an_unknown_reference_both_fail() {
    let (web, _) = web();
    let url = format!("{web}/rust");
    // A file only a shell could create, in a path of this test alone.
    let escape =
        std::env::temp_dir().join(format!("regulus-evidence-escape-{}", std::process::id()));
    let _ = std::fs::remove_file(&escape);
    let (model, seen) = fake_model(vec![
        Step::Call("collect_source", json!({ "text": url })),
        // A tool the template never gave it.
        Step::Call(
            "bash",
            json!({ "command": format!("echo hi > {}", escape.display()) }),
        ),
        // A quote the page does not hold.
        Step::Call(
            "evidence_cite",
            json!({ "statement": "Rust has no ownership rules",
            "source": "S1", "excerpt": "Ownership rules do not exist in the language." }),
        ),
        // A report that cites an entry nobody registered.
        Step::Say("Ownership is optional in Rust [E7].".into()),
    ]);
    with_core("research-evidence-fail", |home| {
        apply(home, &model, &web, &["--set", "min_citations=1"]);
        let exec = run(home, TEMPLATE);
        ok(home, &["send", &exec, "question", "Is ownership optional?"]);
        let errors = until(home, &exec, "error", 1);
        let error = errors[0].as_str().unwrap();
        assert!(error.contains("the report does not validate"), "{error}");
        assert!(
            error.contains("[E7] cites an entry that does not exist"),
            "{error}"
        );
        // The failed report left nothing on `report`.
        assert!(emitted(home, &exec, "report").is_empty());
        assert_eq!(until(home, &exec, "result", 1)[0]["valid"], false);

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        // The call was rejected (the engine said so in the tool result the model got), and
        // nothing ran.
        let shell = observation(&seen, 2);
        assert!(
            shell.to_lowercase().contains("bash")
                && [
                    "denied",
                    "not allowed",
                    "no tool",
                    "unknown tool",
                    "refused",
                    "not available"
                ]
                .iter()
                .any(|w| shell.to_lowercase().contains(w)),
            "the bash call was not rejected: {shell}"
        );
        assert!(!escape.exists(), "a shell ran: {shell}");
        let refused = observation(&seen, 3);
        assert!(
            refused.contains("is not in `S1`") && refused.contains("quote it exactly"),
            "{refused}"
        );
        // Nothing was registered by the refused citation.
        let list = call(
            home,
            "evidence.list",
            json!({ "execution_id": exec, "node_id": "book" }),
        );
        assert_eq!(list["entries"], json!([]));
        assert_eq!(list["sources"].as_array().map(Vec::len), Some(1));
    });
}
