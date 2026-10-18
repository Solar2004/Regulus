//! The MCP engine of the tools box (docs/11-composition.md §3.0). A mount is a bridge
//! process the agent's CLI starts: it speaks MCP over stdio with the agent and forwards
//! every request to the tools box through the core. The binding travels in the
//! environment, not in the arguments, so it does not show in the process list.
//!
//! The protocol lives here and only here: the tools box and the modules that offer
//! tools never see MCP.

use std::io::{BufRead, Write};
use std::path::PathBuf;

use regulus_contract_tool::Spec;
use regulus_contract_toolset as toolset;
use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError};
use regulus_proto::{Manifest, NodeCall};
use serde_json::{json, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const PROTOCOL: &str = "mcp-stdio";
/// The variable the bridge reads its binding from.
pub const BINDING_ENV: &str = "REGULUS_TOOLS_BINDING";
/// The MCP revision answered when the client does not say one.
const DEFAULT_REVISION: &str = "2025-06-18";
// JSON-RPC error codes.
const PARSE_ERROR: i64 = -32700;
const METHOD_NOT_FOUND: i64 = -32601;
const INTERNAL_ERROR: i64 = -32603;

/// How to start the bridge: the host's own program and the arguments that reach the
/// running core. Given by the host, which is the only one that knows them.
#[derive(Debug, Clone, Default)]
pub struct Bridge {
    pub program: PathBuf,
    pub args: Vec<String>,
}

pub struct ToolsMcp {
    manifest: Manifest,
    bridge: Bridge,
}

impl ToolsMcp {
    pub fn new(bridge: Bridge) -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("tools-mcp manifest is valid"),
            bridge,
        }
    }

    fn mount(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let args: toolset::EngineMountArgs = serde_json::from_value(call.args)
            .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
        if self.bridge.program.as_os_str().is_empty() {
            return Err(ModuleError::Failed(
                "this host cannot start the MCP bridge".into(),
            ));
        }
        let mount = toolset::Mount {
            protocol: PROTOCOL.into(),
            name: args.name,
            command: self.bridge.program.clone(),
            args: self.bridge.args.clone(),
            env: [(BINDING_ENV.to_string(), args.binding)].into(),
        };
        serde_json::to_value(mount).map_err(|e| ModuleError::Failed(e.to_string()))
    }
}

impl Module for ToolsMcp {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, _ctx: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            toolset::ENGINE_MOUNT => self.mount(
                serde_json::from_value(params)
                    .map_err(|e| ModuleError::InvalidParams(e.to_string()))?,
            ),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

/// What the bridge forwards to: the tools box, reached however the host reaches it.
pub trait Backend {
    fn list(&self) -> Result<Vec<Spec>, String>;
    /// The result already written for the agent, and whether it is an error.
    fn call(&self, name: &str, args: Value) -> Result<(String, bool), String>;
}

/// Answers one MCP message; `None` for notifications, which get no answer.
pub fn answer(backend: &dyn Backend, server: &str, message: &Value) -> Option<Value> {
    let id = message.get("id")?.clone();
    let params = message.get("params").cloned().unwrap_or(Value::Null);
    let result = match message.get("method").and_then(Value::as_str).unwrap_or_default() {
        "initialize" => Ok(json!({
            "protocolVersion": params
                .get("protocolVersion")
                .and_then(Value::as_str)
                .unwrap_or(DEFAULT_REVISION),
            "capabilities": { "tools": {} },
            "serverInfo": { "name": server, "version": env!("CARGO_PKG_VERSION") },
        })),
        "ping" => Ok(json!({})),
        "tools/list" => backend
            .list()
            .map(|specs| {
                let tools: Vec<Value> = specs
                    .into_iter()
                    .map(|s| json!({ "name": s.name, "description": s.description, "inputSchema": s.input }))
                    .collect();
                json!({ "tools": tools })
            })
            .map_err(|e| (INTERNAL_ERROR, e)),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or_default();
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            // A failed call is still an answer the agent can read, not a protocol error.
            let (text, error) = backend.call(name, args).unwrap_or_else(|e| (e, true));
            Ok(json!({ "content": [{ "type": "text", "text": text }], "isError": error }))
        }
        other => Err((METHOD_NOT_FOUND, format!("method not found: {other}"))),
    };
    Some(match result {
        Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        Err((code, message)) => {
            json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
        }
    })
}

/// Serves MCP over stdio until the agent closes it: one JSON-RPC message per line.
pub fn serve(backend: &dyn Backend, server: &str) -> std::io::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(message) => answer(backend, server, &message),
            Err(e) => Some(json!({
                "jsonrpc": "2.0", "id": null,
                "error": { "code": PARSE_ERROR, "message": e.to_string() },
            })),
        };
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Echo;

    impl Backend for Echo {
        fn list(&self) -> Result<Vec<Spec>, String> {
            Ok(vec![Spec {
                name: "send".into(),
                description: "send a message".into(),
                input: json!({ "type": "object" }),
            }])
        }

        fn call(&self, name: &str, args: Value) -> Result<(String, bool), String> {
            match name {
                "send" => Ok((args.to_string(), false)),
                _ => Err(format!("no tool `{name}`")),
            }
        }
    }

    fn ask(method: &str, params: Value) -> Value {
        let message = json!({ "jsonrpc": "2.0", "id": 7, "method": method, "params": params });
        answer(&Echo, "regulus", &message).expect("a request gets an answer")
    }

    #[test]
    fn it_speaks_enough_mcp_for_tools() {
        let init = ask("initialize", json!({ "protocolVersion": "2025-03-26" }));
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(init["id"], 7);
        let list = ask("tools/list", Value::Null);
        assert_eq!(list["result"]["tools"][0]["inputSchema"]["type"], "object");
        let sent = ask(
            "tools/call",
            json!({ "name": "send", "arguments": { "text": "hi" } }),
        );
        assert_eq!(sent["result"]["content"][0]["text"], r#"{"text":"hi"}"#);
        assert_eq!(sent["result"]["isError"], false);
        let missing = ask("tools/call", json!({ "name": "nope" }));
        assert_eq!(missing["result"]["isError"], true);
        assert_eq!(
            ask("resources/list", Value::Null)["error"]["code"],
            METHOD_NOT_FOUND
        );
        let note = json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
        assert!(answer(&Echo, "regulus", &note).is_none());
    }
}
