//! The Tools box (docs/11-composition.md §3.0). It gathers the operations nodes offer over
//! `tool@1` and presents them to agents over `toolset@1`, through whatever protocol
//! engine is wired into it (`tools.engine@1`). Three things stay apart so each can be
//! swapped alone: what the operations do (their modules), how they travel (the engine)
//! and how results are written (the format).
//!
//! Each mount gets a random binding that ties every call to one tools node and one
//! caller. The caller's name comes from the Agent box, never from the agent.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use regulus_contract_tool as tool;
use regulus_contract_toolset as toolset;
use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, NodeCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MANIFEST: &str = include_str!("../regulus-module.toml");
const LIST: &str = "tools.list";
const CALL: &str = "tools.call";
const TOOLS_PORT: &str = "tools";
const ENGINE_PORT: &str = "engine";
const SERVER_NAME_FIELD: &str = "server_name";
const FORMAT_FIELD: &str = "format";
const BINDING_BYTES: usize = 24;

/// How a result is written for the agent. Adding one is a variant here and an option in
/// the manifest's `format` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Json,
}

impl Format {
    pub fn parse(name: &str) -> Result<Self, String> {
        match name {
            "json" => Ok(Self::Json),
            other => Err(format!("unknown tools format `{other}`")),
        }
    }

    /// Text goes as it is; anything else is written in the format.
    pub fn write(self, data: &Value) -> String {
        if let Value::String(text) = data {
            return text.clone();
        }
        match self {
            Self::Json => serde_json::to_string_pretty(data).unwrap_or_default(),
        }
    }
}

/// One mount: which tools node, in whose name, writing results how.
struct Binding {
    node: NodeRef,
    caller: tool::Caller,
    format: Format,
}

#[derive(Deserialize)]
struct ListParams {
    binding: String,
}

#[derive(Deserialize)]
struct CallParams {
    binding: String,
    name: String,
    #[serde(default)]
    args: Value,
}

/// A result as the bridge hands it to the agent: already written in the format.
#[derive(Serialize)]
struct Written {
    text: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    error: bool,
}

pub struct Tools {
    manifest: Manifest,
    ctx: ModuleCtx,
    bindings: Mutex<HashMap<String, Binding>>,
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ModuleError> {
    serde_json::from_value(value).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn json(value: impl Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(|e| ModuleError::Failed(e.to_string()))
}

fn new_binding() -> Result<String, ModuleError> {
    let mut bytes = [0u8; BINDING_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| ModuleError::Failed(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

impl Tools {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("tools manifest is valid"),
            ctx: ModuleCtx::default(),
            bindings: Mutex::default(),
        }
    }

    fn bindings(&self) -> std::sync::MutexGuard<'_, HashMap<String, Binding>> {
        self.bindings.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn text(&self, call: &NodeCall, field: &str) -> String {
        self.manifest
            .setting(&call.node, field)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .to_string()
    }

    /// An agent asks for the tools in its name: bind, then let the engine mount it.
    fn mount(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let args: toolset::MountArgs = parse(call.args.clone())?;
        let caller = args.caller.trim().to_string();
        if caller.is_empty() {
            return Err(ModuleError::InvalidParams("a mount needs a caller".into()));
        }
        let format = Format::parse(&self.text(&call, FORMAT_FIELD)).map_err(ModuleError::Failed)?;
        let node = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        let engine = self
            .ctx
            .resolve(&node, ENGINE_PORT)
            .into_iter()
            .next()
            .ok_or_else(|| {
                ModuleError::Failed(format!(
                    "no engine wired into `{ENGINE_PORT}` of `{}`: connect MCP…",
                    node.node_id
                ))
            })?;
        let binding = new_binding()?;
        let name = match self.text(&call, SERVER_NAME_FIELD) {
            name if name.is_empty() => call.node.id.clone(),
            name => name,
        };
        let request = toolset::EngineMountArgs {
            name,
            binding: binding.clone(),
        };
        let mount = self
            .ctx
            .call(&engine, toolset::ENGINE_MOUNT, json(request)?)?;
        let mount: toolset::Mount = parse(mount)?;
        self.bindings().insert(
            binding,
            Binding {
                node,
                caller: tool::Caller { name: caller },
                format,
            },
        );
        json(mount)
    }

    fn bound(&self, binding: &str) -> Result<(NodeRef, tool::Caller, Format), ModuleError> {
        self.bindings()
            .get(binding)
            .map(|b| (b.node.clone(), b.caller.clone(), b.format))
            .ok_or_else(|| ModuleError::InvalidParams("unknown or expired binding".into()))
    }

    /// Every operation wired into the node, with the node that offers it. Two with the
    /// same name are an error: the agent could not tell them apart.
    fn gather(&self, node: &NodeRef) -> Result<Vec<(Endpoint, tool::Spec)>, ModuleError> {
        let mut found: Vec<(Endpoint, tool::Spec)> = Vec::new();
        for endpoint in self.ctx.resolve(node, TOOLS_PORT) {
            let specs: Vec<tool::Spec> =
                parse(self.ctx.call(&endpoint, tool::LIST, Value::Null)?)?;
            for spec in specs {
                if let Some((other, _)) = found.iter().find(|(_, s)| s.name == spec.name) {
                    return Err(ModuleError::Failed(format!(
                        "`{}` and `{}` both offer a tool called `{}`",
                        other.node().node_id,
                        endpoint.node().node_id,
                        spec.name
                    )));
                }
                found.push((endpoint.clone(), spec));
            }
        }
        Ok(found)
    }

    fn list(&self, params: ListParams) -> Result<Value, ModuleError> {
        let (node, _, _) = self.bound(&params.binding)?;
        let specs: Vec<tool::Spec> = self.gather(&node)?.into_iter().map(|(_, s)| s).collect();
        json(specs)
    }

    fn run(&self, params: CallParams) -> Result<Value, ModuleError> {
        let (node, caller, format) = self.bound(&params.binding)?;
        let (endpoint, _) = self
            .gather(&node)?
            .into_iter()
            .find(|(_, s)| s.name == params.name)
            .ok_or_else(|| ModuleError::InvalidParams(format!("no tool `{}`", params.name)))?;
        let request = tool::Call {
            name: params.name,
            args: params.args,
            caller,
        };
        let written = match self.ctx.call(&endpoint, tool::CALL, json(request)?) {
            Ok(answer) => {
                let outcome: tool::Outcome = parse(answer)?;
                Written {
                    text: format.write(&outcome.data),
                    error: outcome.error,
                }
            }
            // A failed operation is an answer for the agent, not a broken bridge.
            Err(e) => Written {
                text: e.to_string(),
                error: true,
            },
        };
        json(written)
    }

    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        self.bindings().retain(|_, b| {
            !(b.node.execution_id == call.execution_id && b.node.node_id == call.node.id)
        });
        Ok(Value::Null)
    }
}

impl Default for Tools {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Tools {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            toolset::MOUNT => self.mount(parse(params)?),
            LIST => self.list(parse(params)?),
            CALL => self.run(parse(params)?),
            method::NODE_STOP => self.stop(parse(params)?),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.bindings().clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_goes_as_it_is_and_data_is_written_in_the_format() {
        let json = Format::parse("json").unwrap();
        assert_eq!(json.write(&Value::String("hi".into())), "hi");
        assert_eq!(json.write(&serde_json::json!({"a": 1})), "{\n  \"a\": 1\n}");
        assert!(Format::parse("yaml").is_err());
    }

    #[test]
    fn bindings_are_long_and_different() {
        let (a, b) = (new_binding().unwrap(), new_binding().unwrap());
        assert_eq!(a.len(), BINDING_BYTES * 2);
        assert_ne!(a, b);
    }
}
