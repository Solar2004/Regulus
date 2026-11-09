//! The repository's `templates/native-harness.toml` (docs/12-agents.md §8), through the
//! real `regulus` program and a real core in a temporary home: it applies, passes
//! `profile.check` with no diagnostic, and against a fake OpenAI server a chat message
//! makes the native engine call `search_documents` over ingested files, then answer.

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

const RUST: &str = "# Rust\n\nThe borrow checker enforces ownership rules at compile time.\n";

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

/// A fake OpenAI chat-completions server: the first request gets a call to
/// `search_documents`, the rest a text answer. Keeps every request body.
fn fake_model() -> (String, Arc<Mutex<Vec<Value>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen: Arc<Mutex<Vec<Value>>> = Arc::default();
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
            let count = {
                let mut log = log.lock().unwrap();
                log.push(body);
                log.len()
            };
            let message = if count == 1 {
                json!({ "role": "assistant", "content": null, "tool_calls": [{
                    "id": "c1", "type": "function", "function": {
                        "name": "search_documents",
                        "arguments": "{\"query\":\"borrow checker\"}" } }] })
            } else {
                json!({ "role": "assistant", "content": "The borrow checker does (rust.md)." })
            };
            let reply = json!({ "choices": [{ "message": message }] }).to_string();
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
fn the_native_harness_applies_checks_and_answers_with_a_tool_through_a_fake_model() {
    with_core("native-harness", |home| {
        let missing = regulus(home, &["template", "apply", "native-harness"]);
        assert!(!missing.status.success());
        assert!(text(&missing).contains("`docs_dir`"), "{}", text(&missing));

        let docs = home.join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        std::fs::write(docs.join("rust.md"), RUST).unwrap();
        let (url, seen) = fake_model();
        let dir = format!("docs_dir={}", docs.display());
        let base = format!("base_url={url}");
        ok(
            home,
            &[
                "template",
                "apply",
                "native-harness",
                "--set",
                &dir,
                "--set",
                &base,
                "--set",
                "model=fake-1",
                "--set",
                "rules=House rule: cite the file.",
            ],
        );
        assert_check_is_clean(home, "native-harness");
        assert_eq!(target(home, "native-harness", "native-engine"), "agent");

        let exec = run(home, "native-harness");
        ok(home, &["send", &exec, "ingest", "."]);
        until(home, &exec, "ingested", 1);
        ok(home, &["send", &exec, "chat", "Who enforces ownership?"]);
        let reply = until(home, &exec, "reply", 1)[0].clone();
        assert_eq!(reply, json!("The borrow checker does (rust.md)."));

        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "{seen:?}");
        let system = seen[0]["messages"][0]["content"].as_str().unwrap();
        assert!(system.contains("House rule: cite the file."), "{system}");
        let tools: Vec<&str> = seen[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert!(
            tools.contains(&"search_documents") && tools.contains(&"memory_write"),
            "{tools:?}"
        );
        let observation = seen[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "tool")
            .unwrap()["content"]
            .as_str()
            .unwrap()
            .to_string();
        assert!(observation.contains("borrow checker"), "{observation}");
    });
}
