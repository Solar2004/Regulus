//! `regulus mcp`: the control surface of a running core as an MCP server on stdio
//! (docs/17-control-mcp.md). It is a client like the app and the CLI: it connects to the
//! core's authenticated socket, says `hello`, and turns a fixed list of tools into core
//! methods. It owns no state, opens no port and teaches the core nothing; the core stays
//! the one that validates.
//!
//! The MCP protocol itself (initialize, ping, tools/call and its errors) is the one the
//! tools bridge already speaks (`regulus_module_tools_mcp`); what is added here is the
//! tool table with annotations (`tools/list`) and the connection to the core.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use jsonrpsee::ws_client::WsClient;
use regulus_contract_tool::Spec;
use regulus_core::{local, Config};
use regulus_module_tools_mcp::Backend;
use regulus_proto::{method, HelloResult, ModuleSummary, PROTOCOL};
use serde_json::{json, Value};

use crate::rpc::{self, CallError};

pub mod tools;

use tools::{Guard, Op, Tool};

/// The server name MCP clients see.
const SERVER_NAME: &str = "regulus";
const CLIENT_NAME: &str = concat!("regulus-mcp/", env!("CARGO_PKG_VERSION"));
const AUTHORING: &str = include_str!("authoring.md");
/// Where the guide shows the profile that writes a module's files, kept as a file of its own.
const AUTHOR_PROFILE: &str = include_str!("author-profile.json");
const AUTHOR_PLACEHOLDER: &str = "{{author_profile}}";
/// MCP revisions this server speaks, newest first. A client that asks for another one is
/// answered with the newest, and decides whether it can go on.
const REVISIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
/// What a client is told when it connects.
const INSTRUCTIONS: &str = "Controls a running Regulus host through its own core: discover \
modules and their manifests, create or update profiles (the core validates), apply templates, \
start, feed and stop workflows and read their events. Call `status` first. Anything the core \
refuses comes back as a tool error with its reason. This server adds no power of its own, but a \
workflow runs with whatever the installed modules allow (processes, files, the web, secrets \
referenced as `secret:<name>`): saving and starting one is as strong as the local CLI. \
Trusting a project's code is the user's decision and is not a tool here; \
`module_authoring_guide` explains how to add a module.";
/// JSON-RPC errors a request can earn before it reaches a tool.
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const INVALID_PARAMS: i64 = -32602;
/// A token shorter than this is not worth hiding: it cannot be a real one.
const MIN_TOKEN_LEN: usize = 8;

/// What the server is told on the command line.
#[derive(Debug, Clone)]
pub struct Options {
    /// Offer only the tools that read.
    pub read_only: bool,
    /// A tool result longer than this many bytes is cut, with a note.
    pub max_result_bytes: usize,
    /// How many events `event_list` returns when the caller does not say.
    pub default_events: usize,
    /// Print a client configuration and exit instead of serving.
    pub print_config: bool,
    /// Module ids (`author/module`, optionally `@version`) whose nodes this server will
    /// not save into a profile, apply from a template or start.
    pub deny_modules: Vec<String>,
}

/// The `mcpServers` entry that starts this server, for the client configuration of
/// Claude Code, Claude Desktop, Cursor and others: the absolute program and `--home`.
pub fn client_config(
    program: &std::path::Path,
    home: &std::path::Path,
    read_only: bool,
    deny_modules: &[String],
) -> Value {
    let mut args = vec![
        "--home".to_string(),
        home.display().to_string(),
        "mcp".to_string(),
    ];
    if read_only {
        args.push("--read-only".into());
    }
    for module in deny_modules {
        args.push("--deny-module".into());
        args.push(module.clone());
    }
    json!({ "mcpServers": { SERVER_NAME: { "command": program.display().to_string(), "args": args } } })
}

/// Runs the command: prints the client configuration, or serves MCP on stdin/stdout until
/// the client closes stdin. Diagnostics go to stderr; stdout carries protocol and nothing
/// else.
pub async fn run(config: &Config, timeout: Duration, options: Options) -> Result<()> {
    let program = std::env::current_exe().context("cannot find this program's path")?;
    if options.print_config {
        let home = std::path::absolute(&config.home)
            .with_context(|| format!("cannot resolve {}", config.home.display()))?;
        let config = client_config(&program, &home, options.read_only, &options.deny_modules);
        println!("{}", serde_json::to_string_pretty(&config)?);
        return Ok(());
    }
    let control = Control::new(config.clone(), program, timeout, options);
    eprintln!(
        "regulus mcp: serving {} tools ({}) for the core in {}",
        control.visible().count(),
        if control.options.read_only {
            "read-only"
        } else {
            "read and write"
        },
        control.config.home.display()
    );
    // stdio is blocking; the runtime keeps serving the core's answers meanwhile.
    let served = tokio::task::block_in_place(|| control.serve_stdio());
    eprintln!("regulus mcp: input closed, exiting");
    served.context("stdio failed")
}

/// A connection to the core and what it said in `hello`.
struct Conn {
    ws: WsClient,
    hello: HelloResult,
}

/// The server: the tool table, the options and the (lazily made) connection to the core.
pub struct Control {
    config: Config,
    program: PathBuf,
    timeout: Duration,
    options: Options,
    tools: Vec<Tool>,
    runtime: tokio::runtime::Handle,
    conn: tokio::sync::Mutex<Option<Conn>>,
}

impl Control {
    pub fn new(config: Config, program: PathBuf, timeout: Duration, options: Options) -> Self {
        Self {
            tools: tools::catalog(options.default_events),
            config,
            program,
            timeout,
            options,
            runtime: tokio::runtime::Handle::current(),
            conn: tokio::sync::Mutex::new(None),
        }
    }

    /// The tools this server offers: all of them, or those that read.
    fn visible(&self) -> impl Iterator<Item = &Tool> {
        self.tools
            .iter()
            .filter(|tool| !(self.options.read_only && tool.writes()))
    }

    /// One MCP message to its answer. `initialize` and `tools/list` are answered here;
    /// a malformed request gets its JSON-RPC error here too; the rest by the protocol the
    /// tools bridge speaks. A tool that fails is a result with `isError`, an unknown tool
    /// included, so the agent reads why (MCP 2025-11-25 would make that last one -32602;
    /// this server keeps it readable, see docs/17-control-mcp.md §7).
    fn handle(&self, message: &Value) -> Option<Value> {
        let reply = |id: &Value, code: i64, text: &str| {
            Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": text } }))
        };
        let Some(fields) = message.as_object() else {
            let text = if message.is_array() {
                "batches are not supported: send one request per line"
            } else {
                "a request is a JSON object"
            };
            return reply(&Value::Null, INVALID_REQUEST, text);
        };
        let id = fields.get("id");
        let Some(method) = fields.get("method").and_then(Value::as_str) else {
            // A response from the client, or nonsense; only a request with an id is answered.
            return id
                .and_then(|id| reply(id, INVALID_REQUEST, "a request needs a string `method`"));
        };
        let id = id?;
        let result = match method {
            "initialize" => {
                let asked = message["params"]["protocolVersion"].as_str();
                json!({
                    "protocolVersion": asked.filter(|v| REVISIONS.contains(v)).unwrap_or(REVISIONS[0]),
                    "capabilities": { "tools": { "listChanged": false } },
                    "serverInfo": {
                        "name": SERVER_NAME,
                        "title": "Regulus control",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                    "instructions": INSTRUCTIONS,
                })
            }
            "tools/list" => {
                json!({ "tools": self.visible().map(Tool::listed).collect::<Vec<_>>() })
            }
            "tools/call" if !message["params"]["name"].is_string() => {
                return reply(id, INVALID_PARAMS, "tools/call needs params.name, a string");
            }
            _ => return regulus_module_tools_mcp::answer(self, SERVER_NAME, message),
        };
        Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
    }

    /// Serves one JSON-RPC message per line until stdin closes. A line that is not valid
    /// UTF-8 or JSON earns a parse error and the next line is read.
    fn serve_stdio(&self) -> std::io::Result<()> {
        let mut stdin = std::io::stdin().lock();
        let mut stdout = std::io::stdout();
        let mut line = Vec::new();
        loop {
            line.clear();
            if stdin.read_until(b'\n', &mut line)? == 0 {
                return Ok(());
            }
            let parsed = match std::str::from_utf8(&line) {
                Ok(text) if text.trim().is_empty() => continue,
                Ok(text) => serde_json::from_str::<Value>(text).map_err(|e| e.to_string()),
                Err(e) => Err(format!("the line is not valid UTF-8: {e}")),
            };
            let reply = match parsed {
                Ok(message) => self.handle(&message),
                Err(e) => Some(json!({
                    "jsonrpc": "2.0", "id": null,
                    "error": { "code": PARSE_ERROR, "message": e },
                })),
            };
            if let Some(reply) = reply {
                writeln!(stdout, "{reply}")?;
                stdout.flush()?;
            }
        }
    }

    // --- the connection ---------------------------------------------------------------

    /// Connects to the core the home points at and says `hello`.
    async fn connect(&self) -> Result<Conn, String> {
        let start = local::start_command(&self.config, &self.program);
        let endpoint = local::endpoint(&self.config).ok_or_else(|| {
            format!(
                "no running core: {} is missing. Start one with: {start}",
                self.config.port_path().display()
            )
        })?;
        let (ws, hello) = rpc::connect(&endpoint, CLIENT_NAME, self.timeout)
            .await
            .map_err(|e| format!("{e:#}. Is the core running? Start one with: {start}"))?;
        Ok(Conn { ws, hello })
    }

    /// Calls a core method. A dropped connection is replaced before the call, never after
    /// it: a call that may have reached the core is not sent twice.
    async fn request(&self, method: &str, params: Value) -> Result<Value, CallError> {
        let mut guard = self.conn.lock().await;
        if guard.as_ref().is_none_or(|conn| !conn.ws.is_connected()) {
            *guard = Some(self.connect().await.map_err(|message| CallError {
                code: None,
                message,
            })?);
        }
        let conn = guard.as_ref().expect("connected above");
        let result = rpc::call_coded(&conn.ws, method, params).await;
        if matches!(&result, Err(CallError { code: None, .. })) {
            *guard = None;
        }
        result
    }

    // --- tools ------------------------------------------------------------------------

    /// Runs a tool: its result, or the message of the failed call.
    async fn dispatch(&self, name: &str, args: Value) -> Result<Value, String> {
        let Some(tool) = self.visible().find(|tool| tool.name == name) else {
            return Err(self.unknown(name));
        };
        let args = tool.prepare(args)?;
        let failure = |error: CallError| fail(tool, error);
        match &tool.op {
            Op::Guide => Ok(Value::String(
                AUTHORING.replace(AUTHOR_PLACEHOLDER, AUTHOR_PROFILE),
            )),
            Op::Status => self.status().await.map_err(failure),
            Op::Rpc {
                method,
                rename,
                names_only,
                execution,
                guard,
            } => {
                if let Some(key) = names_only {
                    if let Some(value) = args.get(*key) {
                        tools::check_name(tool.name, key, value)?;
                    }
                }
                self.guard(*guard, &args).await?;
                let mut params = serde_json::Map::new();
                for (key, value) in args {
                    let key = rename
                        .iter()
                        .find(|(from, _)| *from == key)
                        .map_or(key, |(_, to)| (*to).to_string());
                    params.insert(key, value);
                }
                let result = self
                    .request(method, Value::Object(params))
                    .await
                    .map_err(failure)?;
                if *execution {
                    return explain(result).map_err(|message| {
                        failure(CallError {
                            code: None,
                            message,
                        })
                    });
                }
                Ok(result)
            }
        }
    }

    /// Whether `module` (`author/module` or with `@version`) is on the deny list.
    fn denied(&self, module: &str) -> bool {
        let (base, version) = split_version(module);
        self.options.deny_modules.iter().any(|entry| {
            let (denied, wanted) = split_version(entry);
            denied == base && (wanted.is_none() || version.is_none() || wanted == version)
        })
    }

    /// Refuses a profile, template or stored profile that uses a denied module. This is a
    /// guard of this server for the client it serves, not a replacement for the project
    /// veto the core enforces (`[modules] disable` in `regulus.toml`).
    async fn guard(
        &self,
        guard: Guard,
        args: &serde_json::Map<String, Value>,
    ) -> Result<(), String> {
        if self.options.deny_modules.is_empty() || guard == Guard::None {
            return Ok(());
        }
        let refuse = |module: &str, place: &str| {
            Err(format!(
                "module `{module}` is denied by this server (--deny-module) and {place} uses it. \
                 Use another module, or ask the user to restart `regulus mcp` without that deny."
            ))
        };
        match guard {
            Guard::None => Ok(()),
            Guard::ProfileArg => match used_modules(&args["profile"]).find(|(m, _)| self.denied(m))
            {
                Some((module, node)) => refuse(&module, &format!("node `{node}`")),
                None => Ok(()),
            },
            Guard::StoredProfile => {
                let id = args["profile_id"].clone();
                // A profile the core does not know fails in the real call, with its reason.
                let Ok(profile) = self.request(method::PROFILE_GET, json!({ "id": id })).await
                else {
                    return Ok(());
                };
                let hit = used_modules(&profile).find(|(m, _)| self.denied(m));
                match hit {
                    Some((module, node)) => refuse(
                        &module,
                        &format!(
                            "node `{node}` of profile `{}`",
                            args["profile_id"].as_str().unwrap_or("?")
                        ),
                    ),
                    None => Ok(()),
                }
            }
            Guard::Template => {
                let name = args["template"].clone();
                let Ok(found) = self
                    .request(method::TEMPLATE_GET, json!({ "template": name }))
                    .await
                else {
                    return Ok(());
                };
                let text = found["text"].as_str().unwrap_or_default();
                for line in text.lines().filter(|l| l.contains("module_id")) {
                    if line.contains("{{") {
                        return Err(format!(
                            "template `{}` chooses a module id with a variable, which cannot be \
                             checked against --deny-module; apply is refused while modules are denied",
                            name.as_str().unwrap_or("?")
                        ));
                    }
                    for quoted in line.split('"').skip(1).step_by(2) {
                        if self.denied(quoted) {
                            return refuse(
                                quoted,
                                &format!("template `{}`", name.as_str().unwrap_or("?")),
                            );
                        }
                    }
                }
                Ok(())
            }
        }
    }

    fn unknown(&self, name: &str) -> String {
        if self.tools.iter().any(|tool| tool.name == name) {
            return format!(
                "`{name}` is not offered: this server runs read-only (--read-only); \
                 restart it without that flag to allow changes"
            );
        }
        let names: Vec<&str> = self.visible().map(|tool| tool.name).collect();
        format!("unknown tool `{name}`. Available: {}", names.join(", "))
    }

    async fn status(&self) -> Result<Value, CallError> {
        let modules: Vec<ModuleSummary> = serde_json::from_value(
            self.request(method::MODULE_LIST, Value::Null).await?,
        )
        .map_err(|e| CallError {
            code: None,
            message: format!("unexpected module.list answer: {e}"),
        })?;
        let executions = self.request(method::EXECUTION_LIST, Value::Null).await?;
        let executions = executions.as_array().cloned().unwrap_or_default();
        let live = executions
            .iter()
            .filter(|e| matches!(e["state"]["status"].as_str(), Some("starting" | "running")))
            .count();
        let mut states = serde_json::Map::new();
        let mut attention = Vec::new();
        for module in &modules {
            let state = serde_json::to_value(&module.state).unwrap_or_default();
            let status = state["status"].as_str().unwrap_or("unknown").to_string();
            let count = states.entry(status.clone()).or_insert(json!(0));
            *count = json!(count.as_u64().unwrap_or(0) + 1);
            if matches!(
                status.as_str(),
                "broken" | "blocked" | "incompatible" | "disabled"
            ) {
                attention.push(json!({ "id": module.id, "state": state }));
            }
        }
        let hello = {
            let guard = self.conn.lock().await;
            guard.as_ref().map(|conn| conn.hello.clone())
        };
        Ok(json!({
            "core": hello.as_ref().map(|h| h.core.as_str()),
            "core_protocol": hello.as_ref().map(|h| h.proto.as_str()),
            "client_protocol": PROTOCOL,
            "home": self.config.home.display().to_string(),
            "read_only": self.options.read_only,
            "denied_modules": self.options.deny_modules,
            "tools": self.visible().count(),
            "modules": { "total": modules.len(), "by_state": states, "needing_attention": attention },
            "executions": { "total": executions.len(), "live": live },
        }))
    }

    /// The text an agent reads: strings as they are, anything else as compact JSON; the
    /// token never appears and a long result is cut where it can still be read.
    fn render(&self, value: &Value) -> String {
        let text = match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        };
        self.limit(self.redact(text))
    }

    fn redact(&self, text: String) -> String {
        match std::fs::read_to_string(self.config.token_path()) {
            Ok(token) if token.trim().len() >= MIN_TOKEN_LEN => {
                text.replace(token.trim(), "[redacted]")
            }
            _ => text,
        }
    }

    fn limit(&self, mut text: String) -> String {
        let max = self.options.max_result_bytes;
        if text.len() <= max {
            return text;
        }
        let total = text.len();
        let mut end = max;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push_str(&format!(
            "\n[truncated: {} of {total} bytes shown; narrow the query (an id, a limit) or \
             raise --max-result-bytes]",
            end
        ));
        text
    }
}

/// `author/module@1.0` as (`author/module`, Some(`1.0`)).
fn split_version(module: &str) -> (&str, Option<&str>) {
    match module.split_once('@') {
        Some((base, version)) => (base, Some(version)),
        None => (module, None),
    }
}

/// Every module id a profile's nodes name, with the node: all its workflows, boxes included.
fn used_modules(profile: &Value) -> impl Iterator<Item = (String, String)> + '_ {
    profile["workflows"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|workflow| workflow["nodes"].as_array().into_iter().flatten())
        .filter_map(|node| {
            let module = node["module_id"].as_str()?;
            Some((
                module.to_string(),
                node["id"].as_str().unwrap_or("?").to_string(),
            ))
        })
}

/// An execution as the agent reads it: a failed start is an error with its reason; nodes
/// left off are listed with why, in `notes`, since the execution itself goes on.
fn explain(mut execution: Value) -> Result<Value, String> {
    let id = execution["id"].as_str().unwrap_or("?").to_string();
    if execution["state"]["status"] == "failed" {
        let reason = execution["state"]["reason"]
            .as_str()
            .unwrap_or("no reason given");
        return Err(format!("execution {id} failed to start: {reason}"));
    }
    let notes: Vec<Value> = execution["disabled"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|node| {
            let missing: Vec<&str> = node["missing"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            json!(format!(
                "node `{}` was left off: required input(s) {} have nothing usable wired in \
                 (no wire, or the node behind it is not running because its module is not \
                 installed, broken, blocked or vetoed). Check module_list and profile_check.",
                node["node_id"].as_str().unwrap_or("?"),
                missing.join(", ")
            ))
        })
        .collect();
    if !notes.is_empty() {
        execution["notes"] = Value::Array(notes);
    }
    Ok(execution)
}

/// A failed call as the agent reads it: the core's message, its code, and what to try.
fn fail(tool: &Tool, error: CallError) -> String {
    let mut text = error.message;
    if let Some(code) = error.code {
        text.push_str(&format!(" [core error {code}]"));
    }
    if !tool.hint.is_empty() {
        text.push_str(&format!("\nhint: {}", tool.hint));
    }
    text
}

impl Backend for Control {
    fn list(&self) -> Result<Vec<Spec>, String> {
        Ok(self
            .visible()
            .map(|tool| Spec {
                name: tool.name.into(),
                description: tool.description.into(),
                input: tool.schema.clone(),
            })
            .collect())
    }

    fn call(&self, name: &str, args: Value) -> Result<(String, bool), String> {
        Ok(match self.runtime.block_on(self.dispatch(name, args)) {
            Ok(value) => (self.render(&value), false),
            Err(message) => (self.limit(self.redact(message)), true),
        })
    }
}
