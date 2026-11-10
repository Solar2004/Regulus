//! The repository's `templates/regulus-code.toml` (docs/14-code.md), through the real
//! `regulus` program and a real core in a temporary home. It applies and passes
//! `profile.check`; every field and port its nodes use is one their manifests declare;
//! and against a fake OpenAI server a turn of the native loop reads a file without
//! asking, edits it and runs a command once a person approves each, is refused another
//! command, and delegates a question to a read-only worker. No real model is called.

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
const WAIT: Duration = Duration::from_secs(60);

const CODE_READS: [&str; 5] = [
    "code_list_files",
    "code_read_file",
    "code_search",
    "code_git_status",
    "code_git_diff",
];
const CODE_CHANGES: [&str; 3] = ["code_write_file", "code_edit_file", "code_run_command"];
const WORKER_TOOLS: [&str; 6] = [
    "worker_create",
    "worker_status",
    "worker_result",
    "worker_send",
    "worker_close",
    "worker_list",
];
/// What only a worker's system prompt says.
const WORKER_MARK: &str = "read-only worker";

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

/// A method of the running core, with its JSON answer.
fn call(home: &Path, method: &str, params: Value) -> Value {
    let answer = ok(home, &["call", method, &params.to_string()]);
    serde_json::from_str(&answer).unwrap_or_else(|e| panic!("{method}: {e}: {answer}"))
}

/// A method that must fail: what the program said.
fn refused(home: &Path, method: &str, params: Value) -> String {
    let output = regulus(home, &["call", method, &params.to_string()]);
    assert!(
        !output.status.success(),
        "{method} {params}: {}",
        text(&output)
    );
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

/// A home with the repo's templates and `extra` in its config, a core started in it,
/// `steps` run, then cleaned up.
fn with_core(name: &str, extra: &str, steps: impl FnOnce(&Path) + std::panic::UnwindSafe) {
    let home = std::env::temp_dir().join(format!("regulus-template-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    let config = format!(
        "[server]\nport = 0\n\n[templates]\npaths = [{:?}]\n{extra}",
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

/// A folder to work in: a file with a typo and the repository's instructions, in a git
/// repository when git is there.
fn work_folder(home: &Path) -> (PathBuf, bool) {
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("hello.txt"), "Hello, wrold\n").unwrap();
    std::fs::write(work.join("AGENTS.md"), "# Rules\nKeep greetings polite.\n").unwrap();
    let git = |args: &[&str]| {
        Command::new("git")
            .current_dir(&work)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    let repository = git(&["init", "-q", "-b", "main"])
        && git(&["add", "."])
        && git(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "-m",
            "init",
        ]);
    (work.canonicalize().unwrap(), repository)
}

fn apply(home: &Path, sets: &[String]) {
    let mut args = vec!["template", "apply", "regulus-code"];
    for set in sets {
        args.extend(["--set", set.as_str()]);
    }
    ok(home, &args);
}

fn run(home: &Path, profile: &str) -> String {
    let started = ok(home, &["run", profile, "--detach"]);
    assert!(started.contains("running"), "{started}");
    started.split_whitespace().next().unwrap().to_string()
}

/// Values the execution put on its own output `port`, oldest first.
fn emitted(home: &Path, execution: &str, port: &str) -> Vec<Value> {
    let page = call(
        home,
        "event.list",
        json!({ "execution_id": execution, "limit": 10000 }),
    );
    page["events"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["from"]["node_id"] == "self" && e["from"]["port_id"] == port)
        .filter(|e| e["kind"] == "emitted")
        .map(|e| e["value"].clone())
        .collect()
}

type Seen = Arc<Mutex<Vec<Value>>>;

/// A fake OpenAI chat-completions server: `script` reads each request and says what the
/// model answers. Keeps every request body.
fn fake_model(script: fn(&Value) -> Value) -> (String, Seen) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let seen: Seen = Arc::default();
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
            let message = script(&body);
            log.lock().unwrap().push(body);
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

fn messages(request: &Value) -> &[Value] {
    request["messages"].as_array().map_or(&[], Vec::as_slice)
}

fn system_of(request: &Value) -> &str {
    messages(request)
        .first()
        .filter(|m| m["role"] == "system")
        .and_then(|m| m["content"].as_str())
        .unwrap_or("")
}

/// The last prompt of a request, and the tool results the model has seen since.
fn turn_of(request: &Value) -> (&str, Vec<&str>) {
    let all = messages(request);
    let last_user = all.iter().rposition(|m| m["role"] == "user").unwrap_or(0);
    let observed = all[last_user..]
        .iter()
        .filter(|m| m["role"] == "tool")
        .map(|m| m["content"].as_str().unwrap_or(""))
        .collect();
    (all[last_user]["content"].as_str().unwrap_or(""), observed)
}

fn tools_of(request: &Value) -> BTreeSet<String> {
    request["tools"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|t| t["function"]["name"].as_str().unwrap().to_string())
        .collect()
}

/// The model: a coding agent that fixes a typo, checks it, tries something it should not,
/// asks a worker, and later writes two files; and a worker that searches and answers.
fn model(request: &Value) -> Value {
    let call = |id: &str, name: &str, args: Value| {
        json!({ "role": "assistant", "content": null, "tool_calls": [{
            "id": id, "type": "function",
            "function": { "name": name, "arguments": args.to_string() } }] })
    };
    let say = |text: &str| json!({ "role": "assistant", "content": text });
    let (prompt, observed) = turn_of(request);
    if system_of(request).contains(WORKER_MARK) {
        return match observed.len() {
            0 => call("w1", "code_search", json!({ "query": "Hello" })),
            _ => say("The greeting is in hello.txt, line 1."),
        };
    }
    if prompt.starts_with("Fix") {
        return match observed.len() {
            0 => call("c1", "code_read_file", json!({ "path": "hello.txt" })),
            1 => call(
                "c2",
                "code_edit_file",
                json!({ "path": "hello.txt", "old_text": "wrold", "new_text": "world" }),
            ),
            2 => call(
                "c3",
                "code_run_command",
                json!({ "command": "cat hello.txt" }),
            ),
            3 => call(
                "c4",
                "code_run_command",
                json!({ "command": "rm hello.txt" }),
            ),
            4 => call(
                "c5",
                "worker_create",
                json!({ "brief": "Where is the greeting?" }),
            ),
            5 => call(
                "c6",
                "worker_result",
                json!({ "worker": "w-1", "wait_ms": 30000 }),
            ),
            _ => say("Fixed the greeting in hello.txt."),
        };
    }
    let file = if prompt.contains("one") {
        "notes/one.txt"
    } else {
        "notes/two.txt"
    };
    match observed.len() {
        0 => call(
            "n1",
            "code_write_file",
            json!({ "path": file, "content": "a note\n" }),
        ),
        _ => say("Written."),
    }
}

/// Sends `prompt` to the agent node as a client does and waits for reply number `count`,
/// answering as a person every permission request on the way: `decide` gets what is
/// asked and says `approve` with an option, or `reject`. Returns what was asked.
fn turn(
    home: &Path,
    execution: &str,
    prompt: &str,
    count: usize,
    decide: fn(&str) -> Option<&'static str>,
) -> Vec<String> {
    let sent = call(
        home,
        "agent.send",
        json!({ "execution_id": execution, "node_id": "agent", "text": prompt }),
    );
    assert!(sent["turn"].is_string(), "{sent}");
    let mut asked = Vec::new();
    let mut answered = BTreeSet::new();
    let deadline = Instant::now() + WAIT;
    loop {
        let pending = call(home, "human.list", json!({ "execution_id": execution }));
        for decision in pending.as_array().unwrap() {
            let id = decision["id"].as_str().unwrap().to_string();
            if !answered.insert(id.clone()) {
                continue;
            }
            assert_eq!(decision["kind"], "decision", "{decision}");
            let prompt = decision["prompt"].as_str().unwrap().to_string();
            match decide(&prompt) {
                Some(option) => {
                    let field = format!("option={option}");
                    ok(
                        home,
                        &["human", "answer", &id, "approve", "--field", &field],
                    )
                }
                None => ok(home, &["human", "answer", &id, "reject"]),
            };
            asked.push(prompt);
        }
        if emitted(home, execution, "reply").len() >= count {
            return asked;
        }
        let failed = emitted(home, execution, "error");
        assert!(failed.is_empty(), "the turn failed: {failed:?}");
        assert!(
            Instant::now() < deadline,
            "no reply to `{prompt}`; asked {asked:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The events of a conversation's activity, as the client reads them.
fn activity(history: &Value, kind: &str) -> Vec<Value> {
    history["activity"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["event"].clone())
        .filter(|event| event["kind"] == kind)
        .collect()
}

#[test]
fn regulus_code_applies_and_its_nodes_use_only_what_their_manifests_declare() {
    with_core("regulus-code-applies", "", |home| {
        let listed = ok(home, &["template", "ls"]);
        let line = listed
            .lines()
            .find(|line| line.starts_with("regulus-code"))
            .unwrap_or_else(|| panic!("{listed}"));
        assert!(line.contains("[needs: root]"), "{line}");
        let missing = regulus(home, &["template", "apply", "regulus-code"]);
        assert!(!missing.status.success());
        assert!(text(&missing).contains("`root`"), "{}", text(&missing));

        let (work, _) = work_folder(home);
        apply(home, &[format!("root={}", work.display())]);
        assert_eq!(
            call(home, "profile.check", json!({ "id": "regulus-code" })),
            json!([])
        );

        // The contract's vars, with the defaults a client can count on.
        let templates = call(home, "template.list", json!({}));
        let template = templates
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "regulus-code")
            .unwrap()
            .clone();
        let default = |name: &str| {
            template["vars"]
                .as_array()
                .unwrap()
                .iter()
                .find(|v| v["name"] == name)
                .unwrap_or_else(|| panic!("no var `{name}`"))["default"]
                .clone()
        };
        assert_eq!(
            default("root"),
            Value::Null,
            "the only var without a default"
        );
        for (name, value) in [
            ("label", json!("Regulus Code")),
            ("preset", json!("ollama")),
            ("base_url", json!("")),
            ("credential", json!("")),
            ("model", json!("qwen3:4b")),
            ("tool_policy", json!("ask")),
            ("worker_access", json!("read")),
            ("worker_permissions", json!("reject")),
        ] {
            assert_eq!(default(name), value, "{name}");
        }
        for name in ["system", "max_workers", "max_steps", "command_timeout_ms"] {
            assert!(
                !default(name).is_null(),
                "{name} is editable and has a default"
            );
        }
        let unset: Vec<&str> = template["vars"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["default"].is_null())
            .map(|v| v["name"].as_str().unwrap())
            .collect();
        assert_eq!(unset, ["root"]);

        // The profile as written: the ids the contract names, and nodes that declare
        // nothing their module does not.
        let profile = call(home, "profile.get", json!({ "id": "regulus-code" }));
        assert_eq!(profile["name"], "Regulus Code");
        assert_eq!(profile["main_workflow_id"], "code");
        let workflows = profile["workflows"].as_array().unwrap();
        let mut checked = 0;
        for workflow in workflows {
            for node in workflow["nodes"].as_array().unwrap() {
                let module = node["module_id"].as_str().unwrap();
                let manifest = call(home, "module.manifest", json!({ "id": module }));
                let kind = manifest["node"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|kind| kind["type"] == node["type"])
                    .unwrap_or_else(|| panic!("{module} has no node type {}", node["type"]));
                let at = format!("{}.{}", workflow["id"], node["id"]);
                let fields: Vec<&str> = kind["field"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|f| f["id"].as_str().unwrap())
                    .collect();
                for key in node["config"].as_object().unwrap().keys() {
                    assert!(fields.contains(&key.as_str()), "{at}: no field `{key}`");
                }
                for port in node["ports"].as_array().unwrap() {
                    // The delegate hands any "uses" input of its own on to its workers:
                    // `decisions` is the template's, for the worker's Agent box.
                    if at == "\"code\".\"workers\"" && port["id"] == "decisions" {
                        continue;
                    }
                    let declared = kind["port"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .find(|p| p["id"] == port["id"])
                        .unwrap_or_else(|| panic!("{at}: no port {}", port["id"]));
                    for key in ["capability", "direction", "kind"] {
                        assert_eq!(port[key], declared[key], "{at}.{}: {key}", port["id"]);
                    }
                    assert_eq!(
                        port["required"].as_bool().unwrap_or(false),
                        declared["required"].as_bool().unwrap_or(false),
                        "{at}.{}: required",
                        port["id"]
                    );
                    checked += 1;
                }
                // Every wire of the workflow ends on a port its node declares.
                for wire in workflow["wires"].as_array().unwrap() {
                    for end in ["from", "to"] {
                        if wire[end]["node_id"] != node["id"] {
                            continue;
                        }
                        let found = node["ports"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .any(|p| p["id"] == wire[end]["port_id"]);
                        assert!(found, "{at}: wire {} uses an undeclared port", wire["id"]);
                    }
                }
            }
        }
        assert!(checked > 30, "only {checked} ports were compared");

        // What clients look for, by capability and never by id.
        let main = &workflows[0];
        assert_eq!(main["id"], "code");
        let offering = |capability: &str, direction: &str| -> Vec<&str> {
            main["nodes"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|node| {
                    node["ports"].as_array().unwrap().iter().any(|p| {
                        p["capability"] == capability
                            && p["direction"] == direction
                            && p["kind"] == "uses"
                    })
                })
                .map(|node| node["id"].as_str().unwrap())
                .collect()
        };
        assert_eq!(offering("code.workspace@1", "output"), ["workspace"]);
        let workspace = main["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|node| node["id"] == "workspace")
            .unwrap();
        assert_eq!(workspace["config"]["root"], work.display().to_string());
        assert!(workspace["ports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["capability"] == "tool@1" && p["direction"] == "output"));
        assert!(offering("agent.engine@1", "input").contains(&"agent"));
    });
}

#[test]
fn a_turn_reads_changes_runs_and_delegates_with_a_person_deciding() {
    with_core("regulus-code-turn", "", |home| {
        let (work, repository) = work_folder(home);
        let (url, seen) = fake_model(model);
        apply(
            home,
            &[
                format!("root={}", work.display()),
                format!("base_url={url}"),
                "model=fake-1".to_string(),
            ],
        );
        let exec = run(home, "regulus-code");
        let inspect = |look: Value| {
            let mut params = json!({ "execution_id": exec, "node_id": "workspace" });
            params
                .as_object_mut()
                .unwrap()
                .extend(look.as_object().unwrap().clone());
            call(home, "code.inspect", params)
        };
        let before = inspect(json!({ "action": "summary" }));
        assert_eq!(before["root"], work.display().to_string());
        if repository {
            assert_eq!(before["git"]["branch"], "main");
            assert_eq!(before["git"]["status"], "");
        }

        // One turn, as a client sends it. Reading asks nobody; each change, each command
        // and the worker ask the person, who refuses to delete the file.
        let asked = turn(home, &exec, "Fix the greeting in hello.txt", 1, |asked| {
            (!asked.contains("rm hello.txt")).then_some("allow_once")
        });
        let about: Vec<&str> = asked
            .iter()
            .map(|prompt| {
                let what = prompt.split("asks permission: ").nth(1).unwrap_or(prompt);
                what.split_whitespace().next().unwrap()
            })
            .collect();
        assert_eq!(
            about,
            [
                "code_edit_file",
                "code_run_command",
                "code_run_command",
                "worker_create"
            ],
            "{asked:?}"
        );
        assert!(
            asked[0].contains("hello.txt") && asked[0].contains("world"),
            "{}",
            asked[0]
        );
        assert!(
            asked[1].contains("cat hello.txt"),
            "a person sees the command: {}",
            asked[1]
        );
        assert_eq!(
            emitted(home, &exec, "reply"),
            [json!("Fixed the greeting in hello.txt.")]
        );
        assert_eq!(
            std::fs::read_to_string(work.join("hello.txt")).unwrap(),
            "Hello, world\n",
            "edited as approved, and still there: the refused command never ran"
        );

        // What the model saw.
        let requests = seen.lock().unwrap().clone();
        let (workers, agents): (Vec<&Value>, Vec<&Value>) = requests
            .iter()
            .partition(|request| system_of(request).contains(WORKER_MARK));
        let all_tools: BTreeSet<String> = CODE_READS
            .iter()
            .chain(&CODE_CHANGES)
            .chain(&WORKER_TOOLS)
            .map(|name| name.to_string())
            .collect();
        assert_eq!(tools_of(agents[0]), all_tools);
        let system = system_of(agents[0]);
        assert!(system.starts_with("You are a coding agent"), "{system}");
        assert!(
            system.contains(
                "Instructions from the repository file AGENTS.md:\n# Rules\nKeep greetings polite."
            ),
            "the repository's instructions are in the system prompt: {system}"
        );
        let results = turn_of(agents.last().unwrap()).1;
        assert_eq!(results.len(), 6, "{results:?}");
        assert!(results[0].contains("Hello, wrold"), "read: {}", results[0]);
        assert!(
            results[1].contains("\"replacements\":1"),
            "edit: {}",
            results[1]
        );
        assert!(
            results[2].contains("\"code\":0") && results[2].contains("Hello, world"),
            "command: {}",
            results[2]
        );
        assert!(
            results[3].starts_with("refused:"),
            "refused command: {}",
            results[3]
        );
        assert!(
            results[4].contains("\"worker\":\"w-1\""),
            "worker: {}",
            results[4]
        );
        assert!(
            results[5].contains("The greeting is in hello.txt, line 1."),
            "worker's answer: {}",
            results[5]
        );

        // A worker reads the same folder and is not even offered what changes it.
        assert_eq!(workers.len(), 2, "{workers:?}");
        let reads: BTreeSet<String> = CODE_READS.iter().map(|name| name.to_string()).collect();
        assert_eq!(tools_of(workers[0]), reads);
        assert!(system_of(workers[0]).contains("Keep greetings polite."));
        assert_eq!(turn_of(workers[0]).0, "Where is the greeting?");
        let found = turn_of(workers[1]).1;
        assert!(
            found[0].contains("hello.txt") && found[0].contains("Hello, world"),
            "{}",
            found[0]
        );

        // The same turn, as the conversation a client shows: what was asked, what was
        // decided and every tool call.
        let history = call(
            home,
            "agent.history",
            json!({ "execution_id": exec, "node_id": "agent" }),
        );
        let said: Vec<(&str, &str)> = history["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| (e["role"].as_str().unwrap(), e["text"].as_str().unwrap()))
            .collect();
        assert_eq!(
            said,
            [
                ("user", "Fix the greeting in hello.txt"),
                ("agent", "Fixed the greeting in hello.txt.")
            ]
        );
        let requested: Vec<String> = activity(&history, "permission_request")
            .iter()
            .map(|event| event["tool"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(requested, about);
        let resolved: Vec<String> = activity(&history, "permission_resolved")
            .iter()
            .map(|event| event["option"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            resolved,
            ["allow_once", "allow_once", "reject_once", "allow_once"]
        );
        let called: Vec<String> = activity(&history, "tool_call")
            .iter()
            .map(|event| event["title"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            called,
            [
                "code_read_file",
                "code_edit_file",
                "code_run_command",
                "code_run_command",
                "worker_create",
                "worker_result"
            ]
        );
        let failed = activity(&history, "tool_update")
            .iter()
            .filter(|event| event["status"] == "failed")
            .count();
        assert_eq!(failed, 1, "only the refused command failed");

        // The worker is an execution of its own, kept alive, with its own conversation.
        let tree = call(home, "execution.tree", json!({ "execution_id": exec }));
        let children = tree["children"].as_array().unwrap();
        assert_eq!(children.len(), 1, "{tree}");
        let worker = &children[0]["execution"];
        assert_eq!(
            (
                worker["tag"].as_str(),
                worker["caller"].as_str(),
                worker["workflow_id"].as_str()
            ),
            (Some("w-1"), Some("workers"), Some("worker"))
        );
        assert_eq!(worker["state"]["status"], "running");
        let worker_history = call(
            home,
            "agent.history",
            json!({ "execution_id": worker["id"], "node_id": "worker" }),
        );
        let worker_said: Vec<&str> = worker_history["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["text"].as_str().unwrap())
            .collect();
        assert_eq!(
            worker_said,
            [
                "Where is the greeting?",
                "The greeting is in hello.txt, line 1."
            ]
        );

        // What the workspace did left on the bus, in the name of who did it.
        let changed = emitted(home, &exec, "changed");
        assert_eq!(
            changed,
            [
                json!({ "tool": "code_edit_file", "path": "hello.txt", "bytes": 13,
                     "created": false, "by": "agent" })
            ]
        );
        let ran = emitted(home, &exec, "ran");
        assert_eq!(ran.len(), 1, "the refused command is not there: {ran:?}");
        assert_eq!(
            (ran[0]["command"].as_str(), ran[0]["code"].as_i64()),
            (Some("cat hello.txt"), Some(0))
        );
        let results = emitted(home, &exec, "worker_result");
        assert_eq!(
            results,
            [json!({ "worker": "w-1", "port": "reply",
                     "value": "The greeting is in hello.txt, line 1." })]
        );

        // The workspace as a client sees it after the change.
        let after = inspect(json!({ "action": "summary" }));
        let read = inspect(json!({ "action": "read", "path": "hello.txt" }));
        assert_eq!(
            (read["text"].as_str(), read["truncated"].as_bool()),
            (Some("Hello, world\n"), Some(false))
        );
        let search = inspect(json!({ "action": "search", "query": "hello, WORLD" }));
        assert_eq!(
            search["matches"],
            json!([{ "path": "hello.txt", "line": 1, "text": "Hello, world" }])
        );
        let listed = inspect(json!({ "action": "list" }));
        let names: Vec<&str> = listed["entries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["path"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["AGENTS.md", "hello.txt"]);
        let diff = inspect(json!({ "action": "diff" }));
        if repository {
            assert_eq!(after["git"]["status"], " M hello.txt\n");
            let text = diff["text"].as_str().unwrap();
            assert!(
                text.contains("-Hello, wrold") && text.contains("+Hello, world"),
                "{text}"
            );
        } else {
            assert_eq!(after["git"], Value::Null);
            assert_eq!(diff["text"], "");
        }

        // "Always allow" holds for the rest of the session: the first write asks, the
        // second does not.
        let asked = turn(home, &exec, "Write note one", 2, |_| Some("allow_always"));
        assert_eq!(asked.len(), 1, "{asked:?}");
        assert!(asked[0].contains("code_write_file") && asked[0].contains("notes/one.txt"));
        let asked = turn(home, &exec, "Write note two", 3, |_| None);
        assert_eq!(
            asked,
            Vec::<String>::new(),
            "nothing to decide the second time"
        );
        for note in ["notes/one.txt", "notes/two.txt"] {
            assert_eq!(
                std::fs::read_to_string(work.join(note)).unwrap(),
                "a note\n"
            );
        }

        // Stopping the execution stops the worker, and the workspace is gone with it.
        ok(home, &["stop", &exec]);
        let tree = call(home, "execution.tree", json!({ "execution_id": exec }));
        assert_eq!(
            tree["children"][0]["execution"]["state"]["status"],
            "stopped"
        );
        let gone = refused(
            home,
            "code.inspect",
            json!({ "execution_id": exec, "node_id": "workspace", "action": "summary" }),
        );
        assert!(
            gone.contains("no code workspace `workspace` is running"),
            "{gone}"
        );
    });
}

#[test]
fn a_worker_allowed_to_change_files_asks_the_same_person() {
    /// The agent only delegates; the worker tries to write.
    fn model(request: &Value) -> Value {
        let call = |id: &str, name: &str, args: Value| {
            json!({ "role": "assistant", "content": null, "tool_calls": [{
                "id": id, "type": "function",
                "function": { "name": name, "arguments": args.to_string() } }] })
        };
        let say = |text: &str| json!({ "role": "assistant", "content": text });
        let (_, observed) = turn_of(request);
        if system_of(request).contains(WORKER_MARK) {
            return match observed.len() {
                0 => call(
                    "w1",
                    "code_write_file",
                    json!({ "path": "by-worker.txt", "content": "from a worker\n" }),
                ),
                _ => say("Wrote by-worker.txt."),
            };
        }
        match observed.len() {
            0 => call("c1", "worker_create", json!({ "brief": "Write a file." })),
            1 => call(
                "c2",
                "worker_result",
                json!({ "worker": "w-1", "wait_ms": 30000 }),
            ),
            _ => say("The worker wrote it."),
        }
    }
    with_core("regulus-code-worker", "", |home| {
        let (work, _) = work_folder(home);
        let (url, seen) = fake_model(model);
        apply(
            home,
            &[
                format!("root={}", work.display()),
                format!("base_url={url}"),
                "model=fake-1".to_string(),
                "worker_access=write".to_string(),
                "worker_permissions=ask".to_string(),
            ],
        );
        assert_eq!(
            call(home, "profile.check", json!({ "id": "regulus-code" })),
            json!([])
        );
        let exec = run(home, "regulus-code");
        let asked = turn(home, &exec, "Delegate it", 1, |_| Some("allow_once"));
        assert_eq!(asked.len(), 2, "{asked:?}");
        assert!(asked[0].contains("worker_create"), "{}", asked[0]);
        // The worker's request reaches the parent's decider, in the worker's name.
        assert!(
            asked[1].contains("`worker`") && asked[1].contains("code_write_file"),
            "{}",
            asked[1]
        );
        assert_eq!(
            std::fs::read_to_string(work.join("by-worker.txt")).unwrap(),
            "from a worker\n"
        );
        let requests = seen.lock().unwrap().clone();
        let worker = requests
            .iter()
            .find(|request| system_of(request).contains(WORKER_MARK))
            .unwrap();
        let offered = tools_of(worker);
        assert!(offered.contains("code_write_file") && offered.contains("code_edit_file"));
        assert!(
            !offered.contains("code_run_command"),
            "`write` is not `all`: {offered:?}"
        );
        assert_eq!(emitted(home, &exec, "changed")[0]["by"], "worker");
    });
}

#[test]
fn without_the_code_module_the_template_says_what_is_missing_and_the_rest_works() {
    with_core(
        "regulus-code-ablated",
        "\n[modules]\nwithout = [\"regulus/code\"]\n",
        |home| {
            let modules = ok(home, &["module", "ls"]);
            assert!(!modules.contains("regulus/code"), "{modules}");
            assert!(modules.contains("regulus/engine-native"), "{modules}");
            let (work, _) = work_folder(home);
            let root = format!("root={}", work.display());
            let unmet = regulus(home, &["template", "apply", "regulus-code", "--set", &root]);
            assert!(!unmet.status.success());
            assert!(text(&unmet).contains("regulus/code"), "{}", text(&unmet));
            assert!(!home.join("profiles/regulus-code.toml").exists());
            let inspect = refused(
                home,
                "code.inspect",
                json!({ "execution_id": "exec-1", "node_id": "workspace", "action": "summary" }),
            );
            assert!(inspect.to_lowercase().contains("not found"), "{inspect}");
            // Another template still applies and runs.
            ok(
                home,
                &["template", "apply", "greeting", "--set", "who=Regulus"],
            );
            assert_eq!(
                call(home, "profile.check", json!({ "id": "greeting" })),
                json!([])
            );
        },
    );
}
