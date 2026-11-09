//! The tool mounts the Agent box hands over (`StartArgs.tools`, `toolset@1`): just enough
//! of an MCP client over stdio for tools (`initialize`, `tools/list`, `tools/call`,
//! `notifications/cancelled`), so swapping a CLI engine for this one keeps the box's
//! tools. One process per mount, started with the mount's own command, arguments and
//! environment (where the caller's binding travels), killed when the session ends.
//!
//! A reader thread per process routes each answer to whoever asked, so a turn waits on
//! its own channel and a cancel or a timeout never waits on the server.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use regulus_contract_tool as tool;
use regulus_contract_toolset::Mount;
use serde_json::{json, Value};

use crate::lock;

/// The only mount protocol this engine speaks.
pub const PROTOCOL: &str = "mcp-stdio";
/// The MCP revision asked for in `initialize`.
const REVISION: &str = "2025-06-18";
const METHOD_NOT_FOUND: i64 = -32601;
/// Environment values at least this long are hidden from every message about a mount.
const SECRET_MIN_LEN: usize = 8;
const HIDDEN: &str = "[hidden]";

/// Where an answer goes: called once, with the request id and the result or the error.
pub type Reply = Box<dyn FnOnce(u64, Result<Value, String>) + Send>;

/// The part the reader thread shares.
#[derive(Default)]
struct Link {
    child: Option<Child>,
    pending: HashMap<u64, Reply>,
    /// Why the server can no longer answer, once it cannot.
    closed: Option<String>,
}

/// One running mount.
pub struct Server {
    pub name: String,
    secrets: Vec<String>,
    link: Arc<Mutex<Link>>,
    stdin: Arc<Mutex<Option<ChildStdin>>>,
    next: AtomicU64,
}

impl Server {
    /// Starts the mount's process in `cwd` and runs `initialize` within `timeout`.
    pub fn start(mount: &Mount, cwd: &Path, timeout: Duration) -> Result<Self, String> {
        let mut command = Command::new(&mount.command);
        command
            .args(&mount.args)
            .envs(&mount.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        if cwd.is_dir() {
            command.current_dir(cwd);
        }
        let mut child = command
            .spawn()
            .map_err(|e| format!("cannot start `{}`: {e}", mount.command.display()))?;
        let stdin = child.stdin.take();
        let stdout = child.stdout.take();
        let server = Self {
            name: mount.name.clone(),
            secrets: secrets(mount),
            link: Arc::new(Mutex::new(Link {
                child: Some(child),
                ..Link::default()
            })),
            stdin: Arc::new(Mutex::new(stdin)),
            next: AtomicU64::new(0),
        };
        let stdout = stdout.ok_or("the process has no output")?;
        let (link, stdin) = (server.link.clone(), server.stdin.clone());
        std::thread::Builder::new()
            .name("regulus-native-mcp".into())
            .spawn(move || read(stdout, &link, &stdin))
            .map_err(|e| e.to_string())?;
        server.request(
            "initialize",
            json!({
                "protocolVersion": REVISION,
                "capabilities": {},
                "clientInfo": { "name": "regulus-native", "version": env!("CARGO_PKG_VERSION") },
            }),
            timeout,
        )?;
        server.notify("notifications/initialized", json!({}))?;
        Ok(server)
    }

    /// `text` with every secret of the mount's environment hidden.
    pub fn redact(&self, text: &str) -> String {
        hide(text, &self.secrets)
    }

    fn write(&self, message: &Value) -> Result<(), String> {
        let mut stdin = lock(&self.stdin);
        let pipe = stdin.as_mut().ok_or("the server is closed")?;
        writeln!(pipe, "{message}")
            .and_then(|()| pipe.flush())
            .map_err(|e| format!("the server stopped reading: {e}"))
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.write(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }

    /// Sends a request; `reply` gets its answer, or the reason it will never come.
    pub fn send(&self, method: &str, params: Value, reply: Reply) -> Result<u64, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        {
            let mut link = lock(&self.link);
            if let Some(why) = &link.closed {
                return Err(why.clone());
            }
            link.pending.insert(id, reply);
        }
        let message = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(error) = self.write(&message) {
            lock(&self.link).pending.remove(&id);
            return Err(error);
        }
        Ok(id)
    }

    /// Gives up on a request: its answer, if it comes, is dropped, and the server is told.
    pub fn abandon(&self, id: u64, reason: &str) {
        lock(&self.link).pending.remove(&id);
        let _ = self.notify(
            "notifications/cancelled",
            json!({ "requestId": id, "reason": reason }),
        );
    }

    /// Sends a request and waits for its answer, at most `timeout`.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let (tx, rx) = mpsc::channel();
        let id = self.send(
            method,
            params,
            Box::new(move |_, answer| {
                let _ = tx.send(answer);
            }),
        )?;
        match rx.recv_timeout(timeout) {
            Ok(answer) => answer,
            Err(_) => {
                self.abandon(id, "timed out");
                Err(format!(
                    "no answer to `{method}` in {} ms",
                    timeout.as_millis()
                ))
            }
        }
    }

    /// Every tool the server lists, page after page.
    pub fn list(&self, timeout: Duration) -> Result<Vec<tool::Spec>, String> {
        let mut specs = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let params = match &cursor {
                Some(cursor) => json!({ "cursor": cursor }),
                None => json!({}),
            };
            let page = self.request("tools/list", params, timeout)?;
            let tools = page["tools"]
                .as_array()
                .ok_or("`tools/list` answered without `tools`")?;
            for item in tools {
                let name = item["name"]
                    .as_str()
                    .ok_or("`tools/list` named a tool without a name")?;
                specs.push(tool::Spec {
                    name: name.to_string(),
                    description: item["description"].as_str().unwrap_or_default().to_string(),
                    input: item
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or_else(|| json!({ "type": "object" })),
                });
            }
            match page["nextCursor"].as_str() {
                Some(next) if cursor.as_deref() != Some(next) => cursor = Some(next.to_string()),
                _ => return Ok(specs),
            }
        }
    }

    /// Stops the process and answers whatever still waits with why.
    pub fn close(&self, why: &str) {
        let (child, pending) = {
            let mut link = lock(&self.link);
            link.closed.get_or_insert_with(|| why.to_string());
            let pending: Vec<(u64, Reply)> = link.pending.drain().collect();
            (link.child.take(), pending)
        };
        lock(&self.stdin).take();
        if let Some(mut child) = child {
            let _ = child.kill();
            let _ = child.wait();
        }
        for (id, reply) in pending {
            reply(id, Err(why.to_string()));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.close("the session ended");
    }
}

/// The values of a mount's environment long enough to be a secret.
fn secrets(mount: &Mount) -> Vec<String> {
    mount
        .env
        .values()
        .filter(|v| v.len() >= SECRET_MIN_LEN)
        .cloned()
        .collect()
}

fn hide(text: &str, secrets: &[String]) -> String {
    secrets.iter().fold(text.to_string(), |text, secret| {
        text.replace(secret, HIDDEN)
    })
}

/// What a `tools/call` result says, as text for the model, and whether it is an error.
pub fn outcome(result: &Value) -> (String, bool) {
    let error = result["isError"].as_bool().unwrap_or(false);
    let parts: Vec<String> = result["content"]
        .as_array()
        .into_iter()
        .flatten()
        .map(
            |item| match (item["type"].as_str(), item["text"].as_str()) {
                (Some("text"), Some(text)) => text.to_string(),
                _ => item.to_string(),
            },
        )
        .collect();
    if parts.is_empty() {
        if let Some(structured) = result.get("structuredContent") {
            return (structured.to_string(), error);
        }
    }
    (parts.join("\n"), error)
}

/// Reads the server's output until it closes: answers go to whoever waits, requests from
/// the server get an answer, and a line that is not JSON is skipped.
fn read(stdout: ChildStdout, link: &Mutex<Link>, stdin: &Mutex<Option<ChildStdin>>) {
    for line in BufReader::new(stdout).lines() {
        let Ok(line) = line else { break };
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match (message.get("id"), message.get("method")) {
            (Some(id), None) => {
                let Some(id) = id.as_u64() else { continue };
                let Some(reply) = lock(link).pending.remove(&id) else {
                    continue;
                };
                let answer = match message.get("error") {
                    Some(error) => Err(error["message"]
                        .as_str()
                        .map_or_else(|| error.to_string(), String::from)),
                    None => Ok(message.get("result").cloned().unwrap_or(Value::Null)),
                };
                reply(id, answer);
            }
            (Some(id), Some(method)) => {
                let answer = if method == "ping" {
                    json!({ "jsonrpc": "2.0", "id": id, "result": {} })
                } else {
                    json!({ "jsonrpc": "2.0", "id": id,
                            "error": { "code": METHOD_NOT_FOUND, "message": "method not found" } })
                };
                if let Some(pipe) = lock(stdin).as_mut() {
                    let _ = writeln!(pipe, "{answer}").and_then(|()| pipe.flush());
                }
            }
            _ => {}
        }
    }
    let pending: Vec<(u64, Reply)> = {
        let mut link = lock(link);
        link.closed
            .get_or_insert_with(|| "the server exited".into());
        link.pending.drain().collect()
    };
    for (id, reply) in pending {
        reply(id, Err("the server exited".into()));
    }
}

/// Starts every `mcp-stdio` mount at once; any other protocol, and any mount that does
/// not start, is left out with a warning that shows no secret.
pub fn start_all(
    mounts: &[Mount],
    cwd: &Path,
    timeout: Duration,
) -> (Vec<Arc<Server>>, Vec<String>) {
    let mut warnings = Vec::new();
    let mut starting = Vec::new();
    for mount in mounts {
        if mount.protocol != PROTOCOL {
            warnings.push(format!(
                "left out the tool mount `{}`: the native engine speaks {PROTOCOL}, not `{}`",
                mount.name, mount.protocol
            ));
            continue;
        }
        let (mount, cwd) = (mount.clone(), cwd.to_path_buf());
        let name = mount.name.clone();
        starting.push((
            name,
            std::thread::spawn(move || {
                Server::start(&mount, &cwd, timeout).map_err(|e| hide(&e, &secrets(&mount)))
            }),
        ));
    }
    let mut servers = Vec::new();
    for (name, thread) in starting {
        match thread.join() {
            Ok(Ok(server)) => servers.push(Arc::new(server)),
            Ok(Err(error)) => warnings.push(format!("left out the tool mount `{name}`: {error}")),
            Err(_) => warnings.push(format!(
                "left out the tool mount `{name}`: its start failed"
            )),
        }
    }
    (servers, warnings)
}
