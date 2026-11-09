//! Swapping a CLI engine for the native one keeps the Agent box's tools
//! (docs/12-agents.md §8), through the real `regulus` program and a real core in a
//! temporary home. The box's `tools` is wired as for Claude Code or opencode (Agent ⇠
//! `tools` box ⇠ `tools-mcp`, `memory` inside); the native engine starts the mount, the
//! `regulus tools-mcp` bridge, and a fake OpenAI server makes it write and read the memory
//! through it. Stopping the execution ends the bridge.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Command, Output};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_core::{local, Config};
use serde_json::{json, Value};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const WAIT: Duration = Duration::from_secs(20);

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn ok(home: &Path, args: &[&str]) -> String {
    let output = Command::new(PROGRAM)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "{args:?}: {}", text(&output));
    text(&output)
}

/// The pids of every process started with `home` among its arguments and `word` too.
fn processes(home: &Path, word: &str) -> Vec<String> {
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
        if args.contains(&home) && args.iter().any(|a| a == word) {
            found.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    found
}

/// A fake OpenAI chat-completions server: write the memory, read it back, then answer.
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
            let call = |id: &str, name: &str, args: Value| {
                json!({ "role": "assistant", "content": null, "tool_calls": [{
                    "id": id, "type": "function",
                    "function": { "name": name, "arguments": args.to_string() } }] })
            };
            let message = match count {
                1 => call(
                    "c1",
                    "memory_write",
                    json!({ "key": "cat", "text": "The cat is called Tom." }),
                ),
                2 => call("c2", "memory_read", json!({ "key": "cat" })),
                _ => json!({ "role": "assistant", "content": "Tom." }),
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

/// The Agent box with its `tools` wired as for any engine, and the native engine.
fn profile(url: &str, workdir: &Path) -> String {
    format!(
        r#"id = "swap"
name = "Native engine with the box's tools"
main_workflow_id = "main"

[[workflows]]
id = "main"
name = "Main"
kind = "main"
ports = [
  {{ id = "chat", capability = "text@1", direction = "input", kind = "then" }},
  {{ id = "reply", capability = "text@1", direction = "output", kind = "then" }},
  {{ id = "error", capability = "text@1", direction = "output", kind = "then", cardinality = "many" }},
]

  [[workflows.nodes]]
  id = "agent"
  type = "agent"
  module_id = "regulus/agent@0.1.0"
  config = {{ workdir = {workdir:?}, permissions = "allow", name = "scout" }}
  ports = [
    {{ id = "engine", capability = "agent.engine@1", direction = "input", kind = "uses", required = true }},
    {{ id = "tools", capability = "toolset@1", direction = "input", kind = "uses", cardinality = "many" }},
    {{ id = "prompt", capability = "text@1", direction = "input", kind = "then", cardinality = "many" }},
    {{ id = "reply", capability = "text@1", direction = "output", kind = "then" }},
    {{ id = "failed", capability = "text@1", direction = "output", kind = "then" }},
  ]

  [[workflows.nodes]]
  id = "native"
  type = "native"
  module_id = "regulus/engine-native@0.1.0"
  config = {{ tool_policy = "ask" }}
  ports = [
    {{ id = "engine", capability = "agent.engine@1", direction = "output", kind = "uses" }},
    {{ id = "model", capability = "llm.endpoint@1", direction = "input", kind = "uses", required = true }},
    {{ id = "tools", capability = "tool@1", direction = "input", kind = "uses", cardinality = "many" }},
    {{ id = "context", capability = "hook@1", direction = "input", kind = "uses", cardinality = "many" }},
  ]

  [[workflows.nodes]]
  id = "provider"
  type = "provider"
  module_id = "regulus/providers@0.1.0"
  config = {{ protocol = "openai", base_url = {url:?}, auth = "none", model = "fake-1" }}
  ports = [{{ id = "endpoint", capability = "llm.endpoint@1", direction = "output", kind = "uses" }}]

  [[workflows.nodes]]
  id = "memory"
  type = "store"
  module_id = "regulus/memory@0.1.0"
  config = {{ memory = "swap" }}
  ports = [{{ id = "tools", capability = "tool@1", direction = "output", kind = "uses" }}]

  [[workflows.nodes]]
  id = "tools"
  type = "tools"
  module_id = "regulus/tools@0.1.0"
  ports = [
    {{ id = "tools", capability = "tool@1", direction = "input", kind = "uses", cardinality = "many" }},
    {{ id = "engine", capability = "tools.engine@1", direction = "input", kind = "uses", required = true }},
    {{ id = "toolset", capability = "toolset@1", direction = "output", kind = "uses" }},
  ]

  [[workflows.nodes]]
  id = "mcp"
  type = "tools-mcp"
  module_id = "regulus/tools-mcp@0.1.0"
  ports = [{{ id = "engine", capability = "tools.engine@1", direction = "output", kind = "uses" }}]

  [[workflows.wires]]
  id = "native-engine"
  kind = "uses"
  from = {{ node_id = "native", port_id = "engine" }}
  to = {{ node_id = "agent", port_id = "engine" }}

  [[workflows.wires]]
  id = "provider-model"
  kind = "uses"
  from = {{ node_id = "provider", port_id = "endpoint" }}
  to = {{ node_id = "native", port_id = "model" }}

  [[workflows.wires]]
  id = "memory-tools"
  kind = "uses"
  from = {{ node_id = "memory", port_id = "tools" }}
  to = {{ node_id = "tools", port_id = "tools" }}

  [[workflows.wires]]
  id = "mcp-engine"
  kind = "uses"
  from = {{ node_id = "mcp", port_id = "engine" }}
  to = {{ node_id = "tools", port_id = "engine" }}

  [[workflows.wires]]
  id = "tools-agent"
  kind = "uses"
  from = {{ node_id = "tools", port_id = "toolset" }}
  to = {{ node_id = "agent", port_id = "tools" }}

  [[workflows.wires]]
  id = "chat-agent"
  kind = "then"
  from = {{ node_id = "self", port_id = "chat" }}
  to = {{ node_id = "agent", port_id = "prompt" }}

  [[workflows.wires]]
  id = "agent-reply"
  kind = "then"
  from = {{ node_id = "agent", port_id = "reply" }}
  to = {{ node_id = "self", port_id = "reply" }}

  [[workflows.wires]]
  id = "agent-failed"
  kind = "then"
  from = {{ node_id = "agent", port_id = "failed" }}
  to = {{ node_id = "self", port_id = "error" }}
"#
    )
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

fn steps(home: &Path, seen: &Mutex<Vec<Value>>) {
    let check = ok(home, &["call", "profile.check", r#"{"id":"swap"}"#]);
    assert_eq!(
        serde_json::from_str::<Value>(&check).unwrap(),
        json!([]),
        "{check}"
    );

    let started = ok(home, &["run", "swap", "--detach"]);
    let exec = started.split_whitespace().next().unwrap().to_string();
    ok(home, &["send", &exec, "chat", "What is my cat called?"]);
    let deadline = Instant::now() + WAIT;
    let reply = loop {
        if let Some(reply) = emitted(home, &exec, "reply").first() {
            break reply.clone();
        }
        let errors = emitted(home, &exec, "error");
        assert!(errors.is_empty(), "{errors:?}");
        assert!(Instant::now() < deadline, "no reply");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(reply, json!("Tom."));
    assert_eq!(
        processes(home, "tools-mcp").len(),
        1,
        "one bridge per mount"
    );

    let seen = seen.lock().unwrap().clone();
    assert_eq!(seen.len(), 3, "{seen:?}");
    let tools: Vec<&str> = seen[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["function"]["name"].as_str().unwrap())
        .collect();
    assert!(
        tools.contains(&"memory_write") && tools.contains(&"memory_read"),
        "{tools:?}"
    );
    let observation = |n: usize| -> String {
        seen[n]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .rfind(|m| m["role"] == "tool")
            .unwrap()["content"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert!(
        observation(2).contains("The cat is called Tom."),
        "{}",
        observation(2)
    );

    let params = json!({ "id": exec }).to_string();
    ok(home, &["call", "execution.stop", &params]);
    let deadline = Instant::now() + WAIT;
    while !processes(home, "tools-mcp").is_empty() {
        assert!(
            Instant::now() < deadline,
            "the bridge outlived the execution"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn the_native_engine_keeps_the_tools_the_agent_box_mounts() {
    let home = std::env::temp_dir().join(format!("regulus-native-mount-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    std::fs::write(home.join("config.toml"), "[server]\nport = 0\n").unwrap();
    let (url, seen) = fake_model();
    std::fs::write(home.join("profiles/swap.toml"), profile(&url, &home)).unwrap();
    let config = Config::load(&home).unwrap();
    let started = local::start(&config, Path::new(PROGRAM));
    let at = home.clone();
    let result = std::panic::catch_unwind(move || {
        started.unwrap();
        steps(&at, &seen);
    });
    for pid in processes(&home, "serve") {
        let _ = Command::new("kill").arg(pid).status();
    }
    let _ = std::fs::remove_dir_all(&home);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
