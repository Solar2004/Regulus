//! The topic watch (`templates/topic-watch.toml`) through the real `regulus` program and a
//! real core in a temporary home, against a local fixture server (no internet, no model): a
//! first poll, an identical second poll, and a third poll with a changed, a new and a vanished
//! item after a restart of the core.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_core::{local, Config};
use serde_json::{json, Value};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const WAIT: Duration = Duration::from_secs(30);

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

fn first(home: &Path, id: &str, port: &str) -> Value {
    until(home, id, port, 1)[0].clone()
}

/// What the fixture server answers: a SearXNG-style search and one text page.
#[derive(Default)]
struct Fixture {
    results: Vec<Value>,
    page: String,
    requests: usize,
}

fn fixture() -> (String, Arc<Mutex<Fixture>>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state: Arc<Mutex<Fixture>> = Arc::default();
    let shared = state.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { return };
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request = String::new();
            reader.read_line(&mut request).unwrap_or(0);
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 || line.trim().is_empty() {
                    break;
                }
            }
            let path = request.split_whitespace().nth(1).unwrap_or("/").to_string();
            let (kind, body) = {
                let mut state = shared.lock().unwrap();
                state.requests += 1;
                if path.starts_with("/search") {
                    (
                        "application/json",
                        json!({ "results": state.results }).to_string(),
                    )
                } else {
                    ("text/plain", state.page.clone())
                }
            };
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\ncontent-type: {kind}\r\ncontent-length: {}\r\n\
                 connection: close\r\n\r\n{body}",
                body.len()
            );
        }
    });
    (base, state)
}

fn result(n: u32, title: &str, content: &str) -> Value {
    json!({ "title": title, "url": format!("http://example.test/{n}"), "content": content })
}

fn keys(list: &Value) -> Vec<(&str, &str)> {
    list.as_array()
        .unwrap()
        .iter()
        .map(|n| (n["change"].as_str().unwrap(), n["key"].as_str().unwrap()))
        .collect()
}

fn apply(home: &Path, id: &str, extra: &[(&str, &str)]) {
    let mut sets = vec![
        // Never fires during a test: January the first.
        ("schedule".to_string(), "0 0 1 1 *".to_string()),
        ("allow_private".to_string(), "yes".to_string()),
    ];
    sets.extend(extra.iter().map(|(k, v)| (k.to_string(), v.to_string())));
    let mut args: Vec<String> = ["template", "apply", "topic-watch", "--id", id]
        .map(String::from)
        .to_vec();
    for (key, value) in sets {
        args.push("--set".into());
        args.push(format!("{key}={value}"));
    }
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    ok(home, &args);
    assert_check_is_clean(home, id);
}

fn core_running(home: &Path) -> bool {
    let home = home.to_string_lossy().into_owned();
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
                return false;
            };
            let args: Vec<String> = cmdline
                .split(|b| *b == 0)
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            args.contains(&home) && args.iter().any(|a| a == "serve")
        })
}

/// Stops the core of `home` and starts another on the same files.
fn restart(home: &Path) {
    stop_cores(home);
    let deadline = Instant::now() + WAIT;
    while core_running(home) {
        assert!(Instant::now() < deadline, "the core did not stop");
        std::thread::sleep(Duration::from_millis(50));
    }
    let config = Config::load(home).unwrap();
    local::start(&config, Path::new(PROGRAM)).unwrap();
}

#[test]
fn topic_watch_applies_for_both_sources_and_checks_clean() {
    with_core("watch-apply", |home| {
        apply(home, "watch-search", &[]);
        assert_eq!(
            target(home, "watch-search", "go-out-to-source-in"),
            "to_search"
        );
        apply(
            home,
            "watch-fetch",
            &[("source", "fetch"), ("url", "http://127.0.0.1:1/page")],
        );
        assert_eq!(
            target(home, "watch-fetch", "go-out-to-source-in"),
            "to_fetch"
        );
        let listed = ok(home, &["template", "ls"]);
        assert!(listed.contains("topic-watch"), "{listed}");
    });
}

#[test]
fn topic_watch_reports_first_same_and_changed_polls_across_a_restart() {
    with_core("watch-search", |home| {
        let (base, state) = fixture();
        state.lock().unwrap().results =
            vec![result(1, "Alpha", "first"), result(2, "Beta", "second")];
        apply(
            home,
            "watch",
            &[
                ("search_url", &format!("{base}/search")),
                ("query", "regulus"),
            ],
        );

        // Poll 1: no baseline yet, so everything is new.
        let exec = run(home, "watch");
        ok(home, &["send", &exec, "poll", "now"]);
        let report = first(home, &exec, "report");
        assert_eq!(
            (&report["first"], &report["seq"]),
            (&json!(true), &json!(1)),
            "{report}"
        );
        let novel = first(home, &exec, "novel");
        assert_eq!(
            keys(&novel),
            [
                ("added", "http://example.test/1"),
                ("added", "http://example.test/2")
            ]
        );
        assert_eq!(novel[0]["record"]["title"], "Alpha");
        let digest = first(home, &exec, "digest");
        assert!(
            digest.as_str().unwrap().contains("http://example.test/2"),
            "{digest}"
        );

        // Poll 2: the same results (the search was asked again): nothing is news.
        ok(home, &["send", &exec, "poll", "again"]);
        let same = first(home, &exec, "same");
        assert_eq!(same["counts"]["unchanged"], 2, "{same}");
        assert_eq!(until(home, &exec, "report", 2).len(), 2);
        assert_eq!(emitted(home, &exec, "novel").len(), 1);
        assert_eq!(emitted(home, &exec, "digest").len(), 1);
        assert!(state.lock().unwrap().requests >= 2);
        ok(home, &["stop", &exec]);

        // The source moves on, and the core restarts before the third poll.
        state.lock().unwrap().results = vec![
            result(2, "Beta", "second, edited"),
            result(3, "Gamma", "third"),
        ];
        restart(home);
        let exec = run(home, "watch");
        ok(home, &["send", &exec, "poll", "now"]);
        let report = first(home, &exec, "report");
        assert_eq!(
            (&report["first"], &report["seq"]),
            (&json!(false), &json!(3)),
            "{report}"
        );
        assert_eq!(report["counts"]["added"], 1);
        assert_eq!(report["counts"]["changed"], 1);
        assert_eq!(report["counts"]["removed"], 1);
        let novel = first(home, &exec, "novel");
        assert_eq!(
            keys(&novel),
            [
                ("changed", "http://example.test/2"),
                ("added", "http://example.test/3")
            ]
        );
        assert_eq!(novel[0]["before"]["snippet"], "second", "{novel}");
        let removed = first(home, &exec, "removed");
        assert_eq!(removed[0]["key"], "http://example.test/1");
        assert!(first(home, &exec, "digest")
            .as_str()
            .unwrap()
            .contains("Gamma"));
        assert!(emitted(home, &exec, "same").is_empty());

        // The baseline is inspectable from the CLI.
        let params = json!({ "execution_id": exec, "node_id": "snap_search" }).to_string();
        let seen: Value =
            serde_json::from_str(&ok(home, &["call", "data.inspect", &params])).unwrap();
        assert_eq!(
            (&seen["seq"], &seen["items"]),
            (&json!(3), &json!(2)),
            "{seen}"
        );
        assert_eq!(seen["history"].as_array().unwrap().len(), 3);
    });
}

#[test]
fn topic_watch_tracks_one_page_and_can_start_silent() {
    with_core("watch-fetch", |home| {
        let (base, state) = fixture();
        state.lock().unwrap().page = "Version 1 of the notes".into();
        let url = format!("{base}/page");
        apply(
            home,
            "page",
            &[("source", "fetch"), ("url", &url), ("baseline", "silent")],
        );
        let exec = run(home, "page");

        // Silent: the first poll stores the baseline and says nothing is news.
        ok(home, &["send", &exec, "poll", "now"]);
        let report = first(home, &exec, "report");
        assert_eq!(
            (&report["baselined"], &report["total"]),
            (&json!(true), &json!(1)),
            "{report}"
        );
        ok(home, &["send", &exec, "poll", "again"]);
        assert_eq!(first(home, &exec, "same")["counts"]["unchanged"], 1);
        assert!(emitted(home, &exec, "novel").is_empty());

        state.lock().unwrap().page = "Version 2 of the notes".into();
        ok(home, &["send", &exec, "poll", "now"]);
        let novel = first(home, &exec, "novel");
        assert_eq!(keys(&novel), [("changed", "page")]);
        assert_eq!(novel[0]["before"], "Version 1 of the notes");
        assert_eq!(novel[0]["record"], "Version 2 of the notes");
    });
}

#[test]
fn topic_watch_hands_the_news_to_a_workflow_of_the_profile() {
    with_core("watch-downstream", |home| {
        let (base, state) = fixture();
        state.lock().unwrap().results = vec![result(1, "Alpha", "first")];
        apply(home, "watch", &[("search_url", &format!("{base}/search"))]);

        // Add a workflow of our own and point `downstream` at it: a profile edit, no code.
        let path = home.join("profiles/watch.toml");
        let mut profile: toml::Value =
            toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let ours: toml::Value = toml::from_str(
            r#"
            [[workflows]]
            id = "shout"
            name = "Shout"
            kind = "extra"
            ports = [
              { id = "in", capability = "json@1", direction = "input", kind = "then" },
              { id = "out", capability = "text@1", direction = "output", kind = "then" },
            ]
              [[workflows.nodes]]
              id = "say"
              name = "Say"
              type = "convert"
              module_id = "regulus/flow@0.1.0"
              position = { x = 0, y = 0 }
              config = { conversion = "template", template = "NEWS: {{/0/record/title}}" }
              ports = [
                { id = "in", capability = "json@1", direction = "input", kind = "then", cardinality = "many" },
                { id = "out", capability = "text@1", direction = "output", kind = "then" },
              ]
              [[workflows.wires]]
              id = "in-say"
              kind = "then"
              from = { node_id = "self", port_id = "in" }
              to = { node_id = "say", port_id = "in" }
              [[workflows.wires]]
              id = "say-out"
              kind = "then"
              from = { node_id = "say", port_id = "out" }
              to = { node_id = "self", port_id = "out" }
            "#,
        )
        .unwrap();
        let workflows = profile["workflows"].as_array_mut().unwrap();
        workflows.push(ours["workflows"][0].clone());
        let main = workflows
            .iter_mut()
            .find(|w| w["id"].as_str() == Some("watch"))
            .unwrap();
        let notify = main["nodes"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|n| n["id"].as_str() == Some("notify"))
            .unwrap();
        notify["config"]["workflow"] = toml::Value::String("shout".into());
        std::fs::write(&path, toml::to_string(&profile).unwrap()).unwrap();
        ok(
            home,
            &["profile", "save", path.to_str().unwrap(), "--force"],
        );
        assert_check_is_clean(home, "watch");

        let exec = run(home, "watch");
        ok(home, &["send", &exec, "poll", "now"]);
        assert_eq!(first(home, &exec, "digest"), "NEWS: Alpha");
    });
}

#[test]
fn topic_watch_does_not_call_everything_new_after_an_empty_answer() {
    // (Results without a url never reach the snapshot: the web `search` node drops them.)
    with_core("watch-safe", |home| {
        let (base, state) = fixture();
        state.lock().unwrap().results =
            vec![result(1, "Alpha", "first"), result(2, "Beta", "second")];
        apply(home, "watch", &[("search_url", &format!("{base}/search"))]);
        let exec = run(home, "watch");
        ok(home, &["send", &exec, "poll", "now"]);
        assert_eq!(first(home, &exec, "novel").as_array().unwrap().len(), 2);

        // The endpoint answers 200 with no results: an error, and nothing is removed.
        state.lock().unwrap().results = vec![];
        ok(home, &["send", &exec, "poll", "again"]);
        let errors = until(home, &exec, "error", 1);
        assert!(
            errors[0].as_str().unwrap().contains("on_empty"),
            "{errors:?}"
        );
        assert!(emitted(home, &exec, "removed").is_empty());

        // The source is back to normal: the earlier results are not new again.
        state.lock().unwrap().results =
            vec![result(1, "Alpha", "first"), result(2, "Beta", "second")];
        ok(home, &["send", &exec, "poll", "again"]);
        let reports = until(home, &exec, "report", 2);
        assert_eq!(reports[1]["counts"]["added"], 0, "{}", reports[1]);
        assert_eq!(emitted(home, &exec, "novel").len(), 1);
    });
}

#[test]
fn topic_watch_tracks_a_page_bigger_than_the_old_record_limit() {
    with_core("watch-big-page", |home| {
        let (base, state) = fixture();
        // 100 000 bytes: over the former 64 KiB record limit, under the defaults.
        state.lock().unwrap().page = "word ".repeat(20_000);
        apply(
            home,
            "page",
            &[("source", "fetch"), ("url", &format!("{base}/page"))],
        );
        let exec = run(home, "page");
        ok(home, &["send", &exec, "poll", "now"]);
        assert_eq!(first(home, &exec, "report")["total"], 1);
        assert!(emitted(home, &exec, "error").is_empty());

        // Past the fetch limit it fails loudly instead of silently cutting the page.
        state.lock().unwrap().page = "word ".repeat(80_000);
        ok(home, &["send", &exec, "poll", "again"]);
        let errors = until(home, &exec, "error", 1);
        assert!(
            errors[0].as_str().unwrap().contains("max_bytes"),
            "{errors:?}"
        );
    });
}
