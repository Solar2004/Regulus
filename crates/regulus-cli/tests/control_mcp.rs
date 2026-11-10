//! `regulus mcp` end to end (docs/17-control-mcp.md): the real program as a subprocess,
//! speaking MCP on its stdio, against a real core in a temporary home. A client discovers
//! modules, saves a profile, applies a template, starts, feeds and stops workflows and reads
//! their events; the core refuses what is invalid and the server says why; read-only mode
//! hides every change; a missing or restarted core is survived; closing stdin ends the
//! server; stdout carries protocol and nothing else; the token never shows.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use regulus_core::{local, Config};
use serde_json::{json, Value};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const WAIT: Duration = Duration::from_secs(20);

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

/// The processes whose command line has `home` and `serve`.
fn cores(home: &Path) -> Vec<String> {
    let home = home.to_string_lossy().into_owned();
    let mut found = Vec::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = cmdline
            .split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        if args.contains(&home) && args.iter().any(|a| a == "serve") {
            found.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    found
}

/// Stops every core serving `home` and waits until they are gone; a started core is
/// detached, so nobody else will.
fn stop_cores(home: &Path) {
    for pid in cores(home) {
        let _ = Command::new("kill").arg(pid).status();
    }
    let start = Instant::now();
    while !cores(home).is_empty() && start.elapsed() < WAIT {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// A temporary home (and a project folder beside it) with a core in it.
struct Host {
    root: PathBuf,
    home: PathBuf,
    folder: PathBuf,
}

impl Host {
    /// A home with templates from the repository; the core is not started.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("regulus-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let home = root.join("home");
        let folder = root.join("demo");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(folder.join(".regulus/profiles")).unwrap();
        std::fs::write(
            home.join("config.toml"),
            format!(
                "[server]\nport = 0\n\n[templates]\npaths = [{:?}]\n",
                templates_dir().display().to_string()
            ),
        )
        .unwrap();
        Self { root, home, folder }
    }

    fn start(&self) {
        let config = Config::load(&self.home).unwrap();
        local::start(&config, Path::new(PROGRAM)).unwrap();
    }

    fn token(&self) -> String {
        std::fs::read_to_string(self.home.join("token"))
            .unwrap()
            .trim()
            .to_string()
    }

    fn mcp(&self, extra: &[&str]) -> Mcp {
        Mcp::spawn(&self.home, extra)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        stop_cores(&self.home);
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// An MCP client over the stdio of a `regulus mcp` subprocess.
struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<String>,
    next: u64,
    /// Every line the server wrote on stdout, to check what it never says.
    seen: Vec<String>,
}

impl Mcp {
    fn spawn(home: &Path, extra: &[&str]) -> Self {
        let mut child = Command::new(PROGRAM)
            .arg("--home")
            .arg(home)
            .arg("mcp")
            .args(extra)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let (sender, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            lines,
            next: 0,
            seen: Vec::new(),
        }
    }

    fn send(&mut self, message: &Value) {
        let stdin = self.stdin.as_mut().expect("stdin is open");
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    /// The next line, which must be a JSON object: stdout is protocol only.
    fn read(&mut self, wait: Duration) -> Option<Value> {
        match self.lines.recv_timeout(wait) {
            Ok(line) => {
                self.seen.push(line.clone());
                let message: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|e| panic!("stdout carries a non-JSON line ({e}): {line}"));
                assert_eq!(message["jsonrpc"], "2.0", "{line}");
                Some(message)
            }
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("the server closed stdout"),
        }
    }

    /// Sends a request and returns its whole answer.
    fn ask(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        let answer = self.read(WAIT).expect("an answer before the deadline");
        assert_eq!(answer["id"], id, "{answer}");
        answer
    }

    fn handshake(&mut self) -> Value {
        let init = self.ask(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "control-mcp-test", "version": "0" },
            }),
        );
        self.send(&json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
        init
    }

    /// A tool call: the text it wrote and whether it is an error.
    fn call(&mut self, tool: &str, args: Value) -> (String, bool) {
        let answer = self.ask("tools/call", json!({ "name": tool, "arguments": args }));
        let result = &answer["result"];
        assert!(
            result.is_object(),
            "a tool call answers with a result: {answer}"
        );
        let text = result["content"][0]["text"].as_str().unwrap_or_default();
        (text.to_string(), result["isError"] == true)
    }

    /// A tool call that must succeed, with the JSON it returned.
    fn ok(&mut self, tool: &str, args: Value) -> Value {
        let (text, error) = self.call(tool, args);
        assert!(!error, "{tool} failed: {text}");
        serde_json::from_str(&text).unwrap_or(Value::String(text))
    }

    /// A tool call that must fail, with the message.
    fn refused(&mut self, tool: &str, args: Value) -> String {
        let (text, error) = self.call(tool, args);
        assert!(error, "{tool} should have failed, it said: {text}");
        text
    }

    /// Closes stdin as a client that goes away does; the server must exit by itself. Checks
    /// the exit, what it wrote on stderr and that the token never appeared anywhere.
    fn finish(mut self, token: Option<&str>) {
        drop(self.stdin.take());
        let start = Instant::now();
        let status = loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                break status;
            }
            assert!(start.elapsed() < WAIT, "the server did not exit on EOF");
            std::thread::sleep(Duration::from_millis(20));
        };
        let mut stderr = String::new();
        std::io::Read::read_to_string(self.child.stderr.as_mut().unwrap(), &mut stderr).unwrap();
        assert!(status.success(), "exit {status}: {stderr}");
        assert!(stderr.contains("input closed"), "{stderr}");
        // Whatever arrived after the last answer is still protocol.
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
        if let Some(token) = token {
            for line in self.seen.iter().chain([&stderr]) {
                assert!(!line.contains(token), "the token leaked: {line}");
            }
        }
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `main`: `in` → the hello greeter → `out`, so a text sent in comes back greeted.
fn greeter_profile(id: &str) -> Value {
    let port = |id: &str, direction: &str| json!({ "id": id, "capability": "text@1", "direction": direction, "kind": "then" });
    json!({
        "id": id,
        "name": "Greet what arrives",
        "main_workflow_id": "main",
        "workflows": [{
            "id": "main", "name": "Main", "kind": "main",
            "ports": [port("in", "input"), port("out", "output")],
            "nodes": [{
                "id": "g", "name": "Greeter", "type": "greeter",
                "module_id": "regulus/hello@0.1.0",
                "ports": [
                    { "id": "echo", "capability": "text@1", "direction": "input", "kind": "then", "cardinality": "many" },
                    port("reply", "output"),
                ],
            }],
            "wires": [
                { "id": "w1", "kind": "then", "from": { "node_id": "self", "port_id": "in" }, "to": { "node_id": "g", "port_id": "echo" } },
                { "id": "w2", "kind": "then", "from": { "node_id": "g", "port_id": "reply" }, "to": { "node_id": "self", "port_id": "out" } },
            ],
        }],
    })
}

/// The values emitted on `node.port` so far in `execution`, polling until the bus is idle
/// and `count` of them showed (or the deadline).
fn emitted(mcp: &mut Mcp, execution: &str, node: &str, port: &str, count: usize) -> Vec<Value> {
    let start = Instant::now();
    loop {
        let page = mcp.ok("event_list", json!({ "execution_id": execution }));
        let values: Vec<Value> = page["events"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| {
                e["kind"] == "emitted"
                    && e["from"]["node_id"] == node
                    && e["from"]["port_id"] == port
            })
            .map(|e| e["value"].clone())
            .collect();
        if values.len() >= count && page["pending"] == 0 {
            return values;
        }
        assert!(
            start.elapsed() < WAIT,
            "waiting for {count} on {node}.{port}, got {values:?}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The errors of the deliveries that failed in `execution`, once the bus is idle.
fn failed_deliveries(mcp: &mut Mcp, execution: &str) -> Vec<String> {
    let start = Instant::now();
    loop {
        let page = mcp.ok("event_list", json!({ "execution_id": execution }));
        if page["pending"] == 0 {
            return page["events"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["kind"] == "failed")
                .map(|e| e["error"].as_str().unwrap_or_default().to_string())
                .collect();
        }
        assert!(start.elapsed() < WAIT, "the bus never went idle");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn names(tools: &Value) -> Vec<String> {
    tools["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn it_speaks_mcp_and_lists_described_annotated_tools() {
    let host = Host::new("mcp-protocol");
    host.start();
    let mut mcp = host.mcp(&[]);
    // A revision it speaks is answered as asked; any other gets the newest it speaks.
    let init = mcp.handshake();
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18", "{init}");
    assert_eq!(init["result"]["serverInfo"]["name"], "regulus");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    assert!(init["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("status"));
    for (asked, answered) in [
        (json!("2025-03-26"), "2025-03-26"),
        (json!("2025-11-25"), "2025-11-25"),
        (json!("2099-01-01"), "2025-11-25"),
        (json!("not a date"), "2025-11-25"),
        (json!(7), "2025-11-25"),
        (Value::Null, "2025-11-25"),
    ] {
        let again = mcp.ask(
            "initialize",
            json!({ "protocolVersion": asked, "capabilities": {} }),
        );
        assert_eq!(
            again["result"]["protocolVersion"], answered,
            "asked {asked}: {again}"
        );
    }
    let bare = mcp.ask("initialize", Value::Null);
    assert_eq!(bare["result"]["protocolVersion"], "2025-11-25", "{bare}");
    // The notification got no answer: the next line is the answer to the next request.
    assert!(mcp.read(Duration::from_millis(300)).is_none());
    assert_eq!(mcp.ask("ping", Value::Null)["result"], json!({}));

    let tools = mcp.ask("tools/list", Value::Null);
    let listed = names(&tools);
    for wanted in [
        "status",
        "module_list",
        "module_manifest",
        "project_list",
        "project_get",
        "profile_list",
        "profile_get",
        "profile_check",
        "profile_save",
        "template_list",
        "template_get",
        "template_apply",
        "workflow_start",
        "workflow_send",
        "workflow_stop",
        "execution_list",
        "execution_tree",
        "event_list",
        "module_authoring_guide",
    ] {
        assert!(listed.iter().any(|n| n == wanted), "{wanted} in {listed:?}");
    }
    // Nothing that reaches secrets, trust, projects' code or an arbitrary method.
    for name in &listed {
        for bad in [
            "secret",
            "trust",
            "call",
            "rpc",
            "shell",
            "exec_",
            "project_open",
        ] {
            assert!(!name.contains(bad), "{name} must not be a tool");
        }
    }
    for tool in tools["result"]["tools"].as_array().unwrap() {
        let name = tool["name"].as_str().unwrap();
        assert_eq!(tool["inputSchema"]["type"], "object", "{name}");
        assert!(!tool["description"].as_str().unwrap().is_empty(), "{name}");
        let annotations = &tool["annotations"];
        let writes = [
            "profile_save",
            "template_apply",
            "workflow_start",
            "workflow_send",
            "workflow_stop",
        ]
        .contains(&name);
        assert_eq!(annotations["readOnlyHint"], !writes, "{name}");
        if writes {
            assert!(annotations["destructiveHint"].is_boolean(), "{name}");
        }
    }
    let by_name = |wanted: &str| {
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == wanted)
            .unwrap()
            .clone()
    };
    assert_eq!(
        by_name("workflow_stop")["annotations"]["destructiveHint"],
        true
    );
    assert_eq!(
        by_name("workflow_start")["annotations"]["destructiveHint"],
        false
    );
    assert!(by_name("profile_save")["inputSchema"]["required"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "profile"));

    // What it does not know gets a protocol error, and a broken line does not end it.
    let unknown = mcp.ask("resources/list", Value::Null);
    assert_eq!(unknown["error"]["code"], -32601, "{unknown}");
    // Malformed input is answered and never ends the server.
    let raw = |mcp: &mut Mcp, bytes: &[u8]| {
        let stdin = mcp.stdin.as_mut().unwrap();
        stdin.write_all(bytes).unwrap();
        stdin.flush().unwrap();
        mcp.read(WAIT).unwrap()
    };
    let parse = raw(&mut mcp, b"{ this is not json\n");
    assert_eq!(parse["error"]["code"], -32700, "{parse}");
    let utf8 = raw(
        &mut mcp,
        b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"\xff\xfe\"}\n",
    );
    assert_eq!(utf8["error"]["code"], -32700, "{utf8}");
    assert!(
        utf8["error"]["message"].as_str().unwrap().contains("UTF-8"),
        "{utf8}"
    );
    let batch = raw(
        &mut mcp,
        b"[{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}]\n",
    );
    assert_eq!(batch["error"]["code"], -32600, "{batch}");
    assert!(
        batch["error"]["message"]
            .as_str()
            .unwrap()
            .contains("batch"),
        "{batch}"
    );
    assert_eq!(batch["id"], Value::Null);
    let scalar = raw(&mut mcp, b"\"just a string\"\n");
    assert_eq!(scalar["error"]["code"], -32600, "{scalar}");
    let no_method = raw(&mut mcp, b"{\"jsonrpc\":\"2.0\",\"id\":9}\n");
    assert_eq!(no_method["error"]["code"], -32600, "{no_method}");
    assert_eq!(no_method["id"], 9);
    let odd_method = raw(&mut mcp, b"{\"jsonrpc\":\"2.0\",\"id\":10,\"method\":5}\n");
    assert_eq!(odd_method["error"]["code"], -32600, "{odd_method}");
    // tools/call without a string name is invalid params, not an unknown tool.
    for params in [
        json!({}),
        json!({ "name": 7 }),
        json!({ "arguments": {} }),
        Value::Null,
    ] {
        let answer = mcp.ask("tools/call", params);
        assert_eq!(answer["error"]["code"], -32602, "{answer}");
        assert!(answer["result"].is_null());
    }
    // An unknown tool name stays a readable tool error.
    let (text, error) = mcp.call("no_such_tool", json!({}));
    assert!(error && text.contains("unknown tool"), "{text}");
    assert_eq!(mcp.ask("ping", Value::Null)["result"], json!({}));
    let token = host.token();
    mcp.finish(Some(&token));
}

#[test]
fn print_config_gives_a_copyable_client_entry_with_absolute_paths() {
    let host = Host::new("mcp-config");
    let out = regulus(&host.home, &["mcp", "--print-config"]);
    assert!(out.status.success(), "{}", text(&out));
    let config: Value = serde_json::from_slice(&out.stdout).unwrap();
    let entry = &config["mcpServers"]["regulus"];
    let command = entry["command"].as_str().unwrap();
    assert!(Path::new(command).is_absolute(), "{command}");
    assert_eq!(Path::new(command), Path::new(PROGRAM));
    let args: Vec<&str> = entry["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(args[0], "--home");
    assert!(Path::new(args[1]).is_absolute(), "{args:?}");
    assert_eq!(Path::new(args[1]), host.home);
    assert_eq!(args[2], "mcp");
    let out = regulus(
        &host.home,
        &[
            "mcp",
            "--print-config",
            "--deny-module",
            "regulus/process",
            "--deny-module",
            "regulus/files@0.1.0",
        ],
    );
    let config: Value = serde_json::from_slice(&out.stdout).unwrap();
    let args = config["mcpServers"]["regulus"]["args"].as_array().unwrap();
    assert_eq!(
        &args[3..],
        &[
            json!("--deny-module"),
            json!("regulus/process"),
            json!("--deny-module"),
            json!("regulus/files@0.1.0")
        ]
    );
    // A relative --home is made absolute, and read-only travels in the entry.
    let out = Command::new(PROGRAM)
        .current_dir(&host.root)
        .args(["--home", "home", "mcp", "--print-config", "--read-only"])
        .output()
        .unwrap();
    let config: Value = serde_json::from_slice(&out.stdout).unwrap();
    let args = config["mcpServers"]["regulus"]["args"].as_array().unwrap();
    assert_eq!(args[1], host.home.display().to_string().as_str());
    assert_eq!(args[3], "--read-only");
}

#[test]
fn a_client_discovers_creates_applies_runs_and_stops() {
    let host = Host::new("mcp-run");
    host.start();
    let token = host.token();
    let mut mcp = host.mcp(&[]);
    mcp.handshake();

    // Discover.
    let status = mcp.ok("status", json!({}));
    assert_eq!(status["read_only"], false);
    assert!(
        status["core"].as_str().unwrap().starts_with("regulus/"),
        "{status}"
    );
    assert!(status["modules"]["total"].as_u64().unwrap() > 5, "{status}");
    assert_eq!(status["executions"]["live"], 0);
    let modules = mcp.ok("module_list", json!({}));
    let hello = modules
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"].as_str().unwrap().starts_with("regulus/hello"))
        .unwrap_or_else(|| panic!("hello is loaded: {modules}"));
    assert!(hello["state"]["status"].is_string());
    let manifest = mcp.ok("module_manifest", json!({ "id": "regulus/hello" }));
    assert_eq!(
        manifest["node"][0]["type"]
            .as_str()
            .or(manifest["nodes"][0]["type"].as_str()),
        Some("greeter"),
        "{manifest}"
    );
    let projects = mcp.ok("project_list", json!({}));
    assert_eq!(projects, json!([]));
    assert!(mcp
        .ok("profile_list", json!({}))
        .as_array()
        .unwrap()
        .is_empty());
    let guide = mcp.ok("module_authoring_guide", json!({}));
    let guide = guide.as_str().unwrap();
    assert!(
        guide.contains("[[modules.local]]") && guide.contains("project open"),
        "{guide}"
    );

    // Create a workflow; the core checks it and loads it.
    let saved = mcp.ok(
        "profile_save",
        json!({ "profile": greeter_profile("greet") }),
    );
    assert_eq!(saved["profile_id"], "greet");
    let written = PathBuf::from(saved["path"].as_str().unwrap());
    assert!(written.is_file(), "{saved}");
    let got = mcp.ok("profile_get", json!({ "id": "greet" }));
    assert_eq!(
        got["workflows"][0]["nodes"][0]["module_id"],
        "regulus/hello@0.1.0"
    );
    let checked = mcp.ok("profile_check", json!({ "id": "greet" }));
    assert!(
        checked
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["severity"] != "error"),
        "{checked}"
    );
    // Updating needs force.
    let mut changed = greeter_profile("greet");
    changed["name"] = json!("Greet, renamed");
    let again = mcp.refused("profile_save", json!({ "profile": changed.clone() }));
    assert!(again.contains("already exists"), "{again}");
    mcp.ok("profile_save", json!({ "profile": changed, "force": true }));
    assert_eq!(
        mcp.ok("profile_get", json!({ "id": "greet" }))["name"],
        "Greet, renamed"
    );

    // Run it: start, send, read the events, the tree, stop.
    let execution = mcp.ok("workflow_start", json!({ "profile_id": "greet" }));
    let id = execution["id"].as_str().unwrap().to_string();
    assert_eq!(execution["state"]["status"], "running", "{execution}");
    let sent = mcp.ok(
        "workflow_send",
        json!({ "execution_id": id, "port": "in", "value": "Alexander" }),
    );
    assert!(sent["event"].as_u64().unwrap() > 0, "{sent}");
    let replies = emitted(&mut mcp, &id, "g", "reply", 1);
    assert_eq!(replies, vec![json!("Hello, Alexander!")]);
    assert_eq!(
        emitted(&mut mcp, &id, "self", "out", 1),
        vec![json!("Hello, Alexander!")]
    );
    let wrong = mcp.refused(
        "workflow_send",
        json!({ "execution_id": id, "port": "nope", "value": "x" }),
    );
    assert!(
        wrong.contains("`nope`") && wrong.contains("hint"),
        "{wrong}"
    );
    let tree = mcp.ok("execution_tree", json!({ "execution_id": id }));
    assert_eq!(tree["execution"]["id"], id.as_str());
    assert_eq!(tree["children"], json!([]));
    let live = mcp.ok("execution_list", json!({}));
    assert_eq!(live[0]["state"]["status"], "running");
    assert_eq!(mcp.ok("status", json!({}))["executions"]["live"], 1);
    let page = mcp.ok("event_list", json!({ "execution_id": id, "limit": 1.0 }));
    assert_eq!(page["events"].as_array().unwrap().len(), 1, "{page}");
    let stopped = mcp.ok("workflow_stop", json!({ "execution_id": id }));
    assert_eq!(stopped["state"]["status"], "stopped", "{stopped}");
    assert_eq!(mcp.ok("status", json!({}))["executions"]["live"], 0);

    // Apply a template from the repository.
    let templates = mcp.ok("template_list", json!({}));
    assert!(
        templates
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["name"] == "greeting"),
        "{templates}"
    );
    let body = mcp.ok("template_get", json!({ "template": "greeting" }));
    assert!(body["text"].as_str().unwrap().contains("{{who}}"));
    let missing = mcp.refused("template_apply", json!({ "template": "greeting" }));
    assert!(missing.contains("who"), "{missing}");
    let applied = mcp.ok(
        "template_apply",
        json!({ "template": "greeting", "vars": { "who": "Ana" } }),
    );
    assert_eq!(applied["profile_id"], "greeting", "{applied}");
    let run = mcp.ok("workflow_start", json!({ "profile_id": "greeting" }));
    let run_id = run["id"].as_str().unwrap().to_string();
    assert_eq!(
        emitted(&mut mcp, &run_id, "ana", "tick", 1),
        vec![json!("Hello, Ana!")]
    );
    mcp.ok("workflow_stop", json!({ "execution_id": run_id }));
    mcp.finish(Some(&token));
}

#[test]
fn the_core_refuses_what_is_invalid_and_the_server_says_why() {
    let host = Host::new("mcp-invalid");
    host.start();
    let token = host.token();
    let mut mcp = host.mcp(&[]);
    mcp.handshake();

    // A module that is not installed is not an error for the core: the profile is saved,
    // says so, and the node does not run. The server shows what the core said.
    let mut ghost = greeter_profile("ghost");
    ghost["workflows"][0]["nodes"][0]["module_id"] = json!("nobody/ghost@1.0.0");
    let saved = mcp.ok("profile_save", json!({ "profile": ghost }));
    assert_eq!(
        saved["diagnostics"][0]["code"], "node.module_not_installed",
        "{saved}"
    );
    assert_eq!(saved["diagnostics"][0]["severity"], "info", "{saved}");
    let running = mcp.ok("workflow_start", json!({ "profile_id": "ghost" }));
    let id = running["id"].as_str().unwrap().to_string();
    mcp.ok(
        "workflow_send",
        json!({ "execution_id": id, "port": "in", "value": "anyone?" }),
    );
    let failed = failed_deliveries(&mut mcp, &id);
    assert!(
        failed.iter().any(|e| e.contains("not running")),
        "{failed:?}"
    );
    mcp.ok("workflow_stop", json!({ "execution_id": id }));
    // A node whose required input only the missing module could feed is left off, and the
    // execution says which and why.
    let mut chain = greeter_profile("chain");
    chain["workflows"][0]["nodes"][0]["module_id"] = json!("nobody/ghost@1.0.0");
    let mut second = greeter_profile("chain")["workflows"][0]["nodes"][0].clone();
    second["id"] = json!("h");
    second["ports"][0]["required"] = json!(true);
    chain["workflows"][0]["nodes"]
        .as_array_mut()
        .unwrap()
        .push(second);
    chain["workflows"][0]["wires"][1] = json!({
        "id": "w2", "kind": "then",
        "from": { "node_id": "g", "port_id": "reply" }, "to": { "node_id": "h", "port_id": "echo" },
    });
    chain["workflows"][0]["wires"].as_array_mut().unwrap().push(json!({
        "id": "w3", "kind": "then",
        "from": { "node_id": "h", "port_id": "reply" }, "to": { "node_id": "self", "port_id": "out" },
    }));
    mcp.ok("profile_save", json!({ "profile": chain }));
    let started = mcp.ok("workflow_start", json!({ "profile_id": "chain" }));
    assert_eq!(started["disabled"][0]["node_id"], "h", "{started}");
    let note = started["notes"][0].as_str().unwrap();
    assert!(
        note.contains("`h`") && note.contains("echo") && note.contains("module_list"),
        "{note}"
    );
    mcp.ok("workflow_stop", json!({ "execution_id": started["id"] }));
    // A wire to a port that is not there.
    let mut dangling = greeter_profile("dangling");
    dangling["workflows"][0]["wires"][0]["to"]["port_id"] = json!("nowhere");
    let error = mcp.refused("profile_save", json!({ "profile": dangling }));
    assert!(error.contains("nowhere"), "{error}");
    // Not even a profile.
    let error = mcp.refused("profile_save", json!({ "profile": { "id": "x" } }));
    assert!(
        error.contains("main_workflow_id") || error.contains("missing field"),
        "{error}"
    );
    // Nothing of the refused ones was written or loaded.
    let loaded: Vec<String> = mcp
        .ok("profile_list", json!({}))
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap().to_string())
        .collect();
    assert!(!loaded.contains(&"dangling".to_string()), "{loaded:?}");
    assert!(!host.home.join("profiles/dangling.toml").exists());
    // Ids that would leave the profiles folder.
    let mut escape = greeter_profile("../escape");
    escape["id"] = json!("../escape");
    mcp.refused("profile_save", json!({ "profile": escape }));
    assert!(!host.root.join("escape.toml").exists());

    // Unknown things.
    let error = mcp.refused("module_manifest", json!({ "id": "nobody/ghost" }));
    assert!(
        error.contains("Unknown module") && error.contains("hint"),
        "{error}"
    );
    let error = mcp.refused("profile_get", json!({ "id": "nope" }));
    assert!(error.contains("Unknown profile"), "{error}");
    let error = mcp.refused("workflow_start", json!({ "profile_id": "nope" }));
    assert!(error.contains("hint"), "{error}");
    let error = mcp.refused("workflow_stop", json!({ "execution_id": "exec-404" }));
    assert!(error.contains("hint"), "{error}");
    let error = mcp.refused(
        "workflow_send",
        json!({ "execution_id": "exec-404", "port": "in", "value": "x" }),
    );
    assert!(
        error.contains("exec-404") || error.contains("hint"),
        "{error}"
    );
    let error = mcp.refused("template_apply", json!({ "template": "nothing-like-it" }));
    assert!(error.contains("nothing-like-it"), "{error}");
    // Templates are names, never paths.
    let error = mcp.refused("template_get", json!({ "template": "/etc/passwd" }));
    assert!(error.contains("not a path"), "{error}");
    let error = mcp.refused(
        "template_apply",
        json!({ "template": "../templates/greeting.toml" }),
    );
    assert!(error.contains("not a path"), "{error}");

    // What the tools themselves ask for.
    let error = mcp.refused("not_a_tool", json!({}));
    assert!(
        error.contains("unknown tool `not_a_tool`") && error.contains("module_list"),
        "{error}"
    );
    let error = mcp.refused("secret_list", json!({}));
    assert!(error.contains("unknown tool"), "{error}");
    let error = mcp.refused("module_list", json!({ "surprise": true }));
    assert!(error.contains("unknown argument `surprise`"), "{error}");
    let error = mcp.refused("profile_get", json!({}));
    assert!(error.contains("missing required argument `id`"), "{error}");
    let error = mcp.refused("event_list", json!({ "limit": "many" }));
    assert!(error.contains("`limit` must be"), "{error}");
    let token_text = mcp.refused("profile_get", json!({ "id": token }));
    assert!(!token_text.contains(&token), "{token_text}");

    // The server is still there.
    assert!(mcp.ok("module_list", json!({})).as_array().unwrap().len() > 5);
    mcp.finish(Some(&token));
}

#[test]
fn read_only_offers_no_change_and_refuses_one() {
    let host = Host::new("mcp-readonly");
    host.start();
    let mut mcp = host.mcp(&["--read-only"]);
    mcp.handshake();
    let tools = mcp.ask("tools/list", Value::Null);
    let listed = names(&tools);
    assert!(listed.contains(&"module_list".to_string()));
    for write in [
        "profile_save",
        "template_apply",
        "workflow_start",
        "workflow_send",
        "workflow_stop",
    ] {
        assert!(!listed.iter().any(|n| n == write), "{write} in {listed:?}");
    }
    for tool in tools["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["annotations"]["readOnlyHint"], true);
    }
    assert_eq!(mcp.ok("status", json!({}))["read_only"], true);
    let error = mcp.refused(
        "profile_save",
        json!({ "profile": greeter_profile("greet") }),
    );
    assert!(error.contains("read-only"), "{error}");
    let error = mcp.refused("workflow_start", json!({ "profile_id": "greet" }));
    assert!(error.contains("read-only"), "{error}");
    assert!(mcp
        .ok("profile_list", json!({}))
        .as_array()
        .unwrap()
        .is_empty());
    let token = host.token();
    mcp.finish(Some(&token));
}

#[test]
fn without_a_core_tools_say_how_to_start_one_and_a_restarted_core_is_found() {
    let host = Host::new("mcp-core-gone");
    let mut mcp = host.mcp(&[]);
    // The handshake and the list need no core.
    mcp.handshake();
    assert!(names(&mcp.ask("tools/list", Value::Null)).len() > 10);
    let error = mcp.refused("module_list", json!({}));
    assert!(
        error.contains("no running core") && error.contains("serve"),
        "{error}"
    );
    let guide = mcp.ok("module_authoring_guide", json!({}));
    assert!(guide.as_str().unwrap().contains("regulus-module.toml"));

    host.start();
    let first = mcp.ok("module_list", json!({}));
    assert!(first.as_array().unwrap().len() > 5);
    let token = host.token();
    mcp.ok(
        "profile_save",
        json!({ "profile": greeter_profile("kept") }),
    );

    // The core goes away under a live connection.
    stop_cores(&host.home);
    let error = mcp.refused("module_list", json!({}));
    assert!(!error.is_empty());
    let error = mcp.refused("module_list", json!({}));
    assert!(error.contains("serve"), "{error}");

    // A new core in the same home: the next call reaches it, and the profile written
    // before survives.
    host.start();
    let after = mcp.ok("profile_list", json!({}));
    assert_eq!(after[0]["id"], "kept", "{after}");
    mcp.finish(Some(&host.token()));
    assert_ne!(token, "");
}

#[test]
fn a_module_is_authored_through_the_server_and_trust_stays_with_the_user() {
    if !Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("skipped: python3 is not available to run the echo example");
        return;
    }
    let host = Host::new("mcp-authoring");
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/local-modules/echo");
    let program = std::fs::read_to_string(example.join("echo.py")).unwrap();
    let manifest = std::fs::read_to_string(example.join("regulus-module.toml")).unwrap();
    // The user's project: a folder with a regulus.toml, opened without trust.
    std::fs::write(
        host.folder.join("regulus.toml"),
        "[project]\nid = \"demo\"\n",
    )
    .unwrap();
    host.start();
    let opened = regulus(
        &host.home,
        &["project", "open", host.folder.to_str().unwrap()],
    );
    assert!(opened.status.success(), "{}", text(&opened));
    let token = host.token();
    let mut mcp = host.mcp(&[]);
    mcp.handshake();
    assert_eq!(mcp.ok("project_list", json!({}))[0]["id"], "demo");

    // An MCP-only client has no file tools: it writes the module with the workflow the
    // guide gives, rooted in the project folder.
    let guide = mcp.ok("module_authoring_guide", json!({}));
    let guide = guide.as_str().unwrap();
    let (_, after) = guide
        .split_once("**B. With only this server**")
        .expect("way B in the guide");
    let json_text = after
        .split("```json\n")
        .nth(1)
        .unwrap()
        .split("\n```")
        .next()
        .unwrap();
    let json_text = json_text
        .replace("<project folder>", host.folder.to_str().unwrap())
        .replace("<id>", "echo")
        .replace("run.py", "echo.py");
    let author: Value = serde_json::from_str(&json_text).expect("the guide's profile is JSON");
    let saved = mcp.ok("profile_save", json!({ "profile": author }));
    assert!(
        saved["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["severity"] != "error"),
        "{saved}"
    );
    let run = mcp.ok("workflow_start", json!({ "profile_id": "module-author" }));
    let id = run["id"].as_str().unwrap().to_string();
    let block =
        "\n[[modules.local]]\nid = \"echo\"\ncommand = [\"python3\", \"tools/echo/echo.py\"]\n";
    for (port, value) in [
        ("manifest", manifest.as_str()),
        ("program", program.as_str()),
        ("project", block),
    ] {
        mcp.ok(
            "workflow_send",
            json!({ "execution_id": id, "port": port, "value": value }),
        );
    }
    for node in ["manifest", "program", "project"] {
        let written = emitted(&mut mcp, &id, node, "written", 1);
        assert!(
            Path::new(written[0].as_str().unwrap()).is_file(),
            "{written:?}"
        );
    }
    assert!(failed_deliveries(&mut mcp, &id).is_empty());
    mcp.ok("workflow_stop", json!({ "execution_id": id }));
    assert_eq!(
        std::fs::read_to_string(host.folder.join("tools/echo/echo.py")).unwrap(),
        program
    );
    assert_eq!(
        std::fs::read_to_string(host.folder.join("tools/echo/regulus-module.toml")).unwrap(),
        manifest
    );
    let project_file = std::fs::read_to_string(host.folder.join("regulus.toml")).unwrap();
    assert!(
        project_file.starts_with("[project]") && project_file.contains("[[modules.local]]"),
        "{project_file}"
    );
    // The write stayed inside the project folder.
    assert!(!host.root.join("tools").exists());

    // Written is not running: the entry is read when the project opens.
    let listed = |mcp: &mut Mcp| {
        mcp.ok("module_list", json!({}))
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"].as_str().unwrap().starts_with("demo/echo"))
            .cloned()
    };
    assert!(listed(&mut mcp).is_none());
    // The user reopens the project. Without trust the module is blocked, and the server has
    // no tool that could change that.
    let closed = regulus(&host.home, &["project", "close", "demo"]);
    assert!(closed.status.success(), "{}", text(&closed));
    let opened = regulus(
        &host.home,
        &["project", "open", host.folder.to_str().unwrap()],
    );
    assert!(opened.status.success(), "{}", text(&opened));
    let module = listed(&mut mcp).expect("demo/echo is listed once the project reopens");
    assert_eq!(module["state"]["status"], "blocked", "{module}");
    assert!(
        module["state"]["reason"]
            .as_str()
            .unwrap()
            .contains("trust"),
        "{module}"
    );
    let attention = mcp.ok("status", json!({}));
    assert_eq!(
        attention["modules"]["needing_attention"][0]["id"], module["id"],
        "{attention}"
    );
    let tools = names(&mcp.ask("tools/list", Value::Null));
    assert!(
        !tools
            .iter()
            .any(|n| n.contains("trust") || n.contains("open")),
        "{tools:?}"
    );
    let pipe = echo_profile("pipe");
    mcp.ok("profile_save", json!({ "profile": pipe }));
    // A node of a blocked module cannot start: the start fails and says why.
    let refused = mcp.refused("workflow_start", json!({ "profile_id": "pipe" }));
    assert!(
        refused.contains("untrusted project") && refused.contains("--trust"),
        "{refused}"
    );

    // The user decides to trust it. The module comes up and the same profile works.
    let trusted = regulus(
        &host.home,
        &["project", "trust", host.folder.to_str().unwrap()],
    );
    assert!(trusted.status.success(), "{}", text(&trusted));
    let up = listed(&mut mcp).unwrap();
    assert!(
        matches!(up["state"]["status"].as_str(), Some("idle" | "active")),
        "{up}"
    );
    let manifest = mcp.ok("module_manifest", json!({ "id": "demo/echo" }));
    assert!(manifest.to_string().contains("shout"), "{manifest}");
    let checked = mcp.ok("profile_check", json!({ "id": "pipe" }));
    assert!(
        checked
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["severity"] != "error"),
        "{checked}"
    );
    let run = mcp.ok("workflow_start", json!({ "profile_id": "pipe" }));
    let id = run["id"].as_str().unwrap().to_string();
    mcp.ok(
        "workflow_send",
        json!({ "execution_id": id, "port": "in", "value": "hello there" }),
    );
    assert_eq!(
        emitted(&mut mcp, &id, "s", "out", 1),
        vec![json!("HELLO THERE")]
    );
    mcp.ok("workflow_stop", json!({ "execution_id": id }));
    mcp.finish(Some(&token));
    let _ = regulus(&host.home, &["project", "close", "demo"]);
}

/// `in` → local `echo` → local `shout` → `out`.
fn echo_profile(id: &str) -> Value {
    let port = |id: &str, direction: &str| json!({ "id": id, "capability": "text@1", "direction": direction, "kind": "then" });
    let node = |id: &str, kind: &str| {
        json!({
            "id": id, "name": kind, "type": kind, "module_id": "demo/echo",
            "ports": [
                { "id": "in", "capability": "text@1", "direction": "input", "kind": "then", "cardinality": "many" },
                port("out", "output"),
            ],
        })
    };
    let wire = |id: &str, from: (&str, &str), to: (&str, &str)| {
        json!({
            "id": id, "kind": "then",
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 },
        })
    };
    json!({
        "id": id, "name": "Echo then shout", "main_workflow_id": "main",
        "workflows": [{
            "id": "main", "name": "Pipe", "kind": "main",
            "ports": [port("in", "input"), port("out", "output")],
            "nodes": [node("e", "echo"), node("s", "shout")],
            "wires": [
                wire("w1", ("self", "in"), ("e", "in")),
                wire("w2", ("e", "out"), ("s", "in")),
                wire("w3", ("s", "out"), ("self", "out")),
            ],
        }],
    })
}

/// `in` → the process module's `command` node → `out`: a program the profile names.
fn process_profile(id: &str) -> Value {
    let mut profile = greeter_profile(id);
    let node = &mut profile["workflows"][0]["nodes"][0];
    node["module_id"] = json!("regulus/process@0.1.0");
    node["type"] = json!("command");
    profile
}

/// A box that carries a denied module in a second workflow of the profile.
fn nested_process_profile(id: &str) -> Value {
    let mut profile = greeter_profile(id);
    let mut inner = process_profile("inner")["workflows"][0].clone();
    inner["id"] = json!("inner");
    inner["kind"] = json!("extra");
    profile["workflows"].as_array_mut().unwrap().push(inner);
    profile
}

#[test]
fn denied_modules_cannot_be_saved_applied_or_started_through_the_server() {
    let host = Host::new("mcp-deny");
    host.start();
    let token = host.token();
    // The profile exists before the server denies the module: it still cannot be started here.
    let mut open = host.mcp(&[]);
    open.handshake();
    open.ok(
        "profile_save",
        json!({ "profile": process_profile("already") }),
    );
    open.finish(Some(&token));

    let mut mcp = host.mcp(&[
        "--deny-module",
        "regulus/process",
        "--deny-module",
        "regulus/judge@9.9",
    ]);
    mcp.handshake();
    assert_eq!(
        mcp.ok("status", json!({}))["denied_modules"],
        json!(["regulus/process", "regulus/judge@9.9"])
    );
    // Saving: refused with a reason, nothing written; a profile without it is fine.
    let error = mcp.refused(
        "profile_save",
        json!({ "profile": process_profile("shell") }),
    );
    assert!(
        error.contains("regulus/process") && error.contains("--deny-module"),
        "{error}"
    );
    let error = mcp.refused(
        "profile_save",
        json!({ "profile": nested_process_profile("nested") }),
    );
    assert!(
        error.contains("node `g`") || error.contains("regulus/process"),
        "{error}"
    );
    let listed = mcp.ok("profile_list", json!({}));
    let ids: Vec<&str> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert!(
        !ids.contains(&"shell") && !ids.contains(&"nested"),
        "{ids:?}"
    );
    assert!(!host.home.join("profiles/shell.toml").exists());
    mcp.ok(
        "profile_save",
        json!({ "profile": greeter_profile("fine") }),
    );
    // A denied version that is not the one installed does not match; the same base without
    // a version in the profile does.
    let mut other = greeter_profile("judged");
    other["workflows"][0]["nodes"][0]["module_id"] = json!("regulus/judge@0.1.0");
    mcp.ok("profile_save", json!({ "profile": other }));
    // Starting a stored profile that uses it.
    let error = mcp.refused("workflow_start", json!({ "profile_id": "already" }));
    assert!(
        error.contains("regulus/process") && error.contains("profile `already`"),
        "{error}"
    );
    assert!(mcp
        .ok("execution_list", json!({}))
        .as_array()
        .unwrap()
        .is_empty());
    let run = mcp.ok("workflow_start", json!({ "profile_id": "fine" }));
    mcp.ok("workflow_stop", json!({ "execution_id": run["id"] }));
    // Templates: one that names the module is refused before anything is written.
    let templates = host.home.join("custom-templates");
    std::fs::create_dir_all(&templates).unwrap();
    let text = |name: &str, module: &str| {
        format!(
            "id = \"t\"\nname = \"T\"\nmain_workflow_id = \"main\"\n\n[template]\nname = \"{name}\"\n\n\
             [template.vars]\nm = {{ description = \"x\", default = \"regulus/hello\" }}\n\n\
             [[workflows]]\nid = \"main\"\nname = \"Main\"\nkind = \"main\"\n\n  \
             [[workflows.nodes]]\n  id = \"n\"\n  type = \"greeter\"\n  module_id = \"{module}\"\n"
        )
    };
    std::fs::write(
        templates.join("uses-process.toml"),
        text("uses-process", "regulus/process"),
    )
    .unwrap();
    std::fs::write(
        templates.join("by-variable.toml"),
        text("by-variable", "{{m}}"),
    )
    .unwrap();
    std::fs::write(templates.join("clean.toml"), text("clean", "regulus/hello")).unwrap();
    let config = std::fs::read_to_string(host.home.join("config.toml")).unwrap();
    let config = config.replace(
        "paths = [",
        &format!("paths = [{:?}, ", templates.display().to_string()),
    );
    // The paths are read at start: restart the core on the new catalog.
    mcp.finish(Some(&token));
    stop_cores(&host.home);
    std::fs::write(host.home.join("config.toml"), config).unwrap();
    host.start();
    let mut mcp = host.mcp(&["--deny-module", "regulus/process"]);
    mcp.handshake();
    let error = mcp.refused("template_apply", json!({ "template": "uses-process" }));
    assert!(error.contains("regulus/process"), "{error}");
    let error = mcp.refused("template_apply", json!({ "template": "by-variable" }));
    assert!(error.contains("variable"), "{error}");
    assert!(!host.home.join("profiles/t.toml").exists());
    let applied = mcp.ok("template_apply", json!({ "template": "clean" }));
    assert_eq!(applied["profile_id"], "t", "{applied}");
    mcp.finish(Some(&token));
    // Without the flag nothing is denied.
    let mut mcp = host.mcp(&[]);
    mcp.handshake();
    assert_eq!(mcp.ok("status", json!({}))["denied_modules"], json!([]));
    mcp.ok(
        "profile_save",
        json!({ "profile": process_profile("shell") }),
    );
    mcp.finish(Some(&token));
}
