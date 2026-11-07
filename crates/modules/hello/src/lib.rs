//! A trivial module. It exists to prove that a module plugs in without touching the core,
//! that the core starts without it, and that "then" wires carry data: a greeter emits its
//! greeting on `tick` when it starts and takes whatever arrives on `echo`.

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, NodeCall, NodeEvent};
use serde_json::{json, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const GREET: &str = "hello.greet";
const NAME_FIELD: &str = "name";
/// The "then" output the greeting goes out on when a node starts.
const TICK_PORT: &str = "tick";
/// The "then" input whose values are echoed back.
const ECHO_PORT: &str = "echo";

pub struct Hello {
    manifest: Manifest,
    ctx: ModuleCtx,
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn greeting(name: &str) -> String {
    format!("Hello, {name}!")
}

impl Hello {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("hello manifest is valid"),
            ctx: ModuleCtx::default(),
        }
    }

    /// Emits the node's greeting on `tick`, if the node has that port.
    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        if call.node.port(TICK_PORT).is_none() {
            return Ok(Value::Null);
        }
        let name = self
            .manifest
            .setting(&call.node, NAME_FIELD)
            .and_then(Value::as_str)
            .map_or_else(|| self.default_name(), str::to_string);
        let node = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        self.ctx.emit(&node, TICK_PORT, json!(greeting(&name)))?;
        Ok(Value::Null)
    }

    /// `text@1` is a JSON string. The answer only reaches the core, which reports the
    /// delivery on the bus.
    fn event(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(call.args)?;
        if event.port != ECHO_PORT {
            return Err(ModuleError::InvalidParams(format!(
                "`{}` takes no events",
                event.port
            )));
        }
        let text = event.value.as_str().ok_or_else(|| {
            ModuleError::InvalidParams(format!("{} must be a string", event.capability))
        })?;
        Ok(json!({ "echoed": text }))
    }

    /// The greeting target defaults to what the manifest declares, not to a literal here.
    fn default_name(&self) -> String {
        self.manifest
            .nodes
            .iter()
            .find_map(|node| node.field(NAME_FIELD))
            .and_then(|field| field.default.as_ref())
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

impl Default for Hello {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Hello {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError> {
        match method {
            method::NODE_START => return self.start(parse(params)?),
            method::NODE_EVENT => return self.event(parse(params)?),
            method::NODE_STOP => return Ok(Value::Null),
            GREET => {}
            other => return Err(ModuleError::UnknownMethod(other.into())),
        }
        let name = match params.get(NAME_FIELD) {
            None | Some(Value::Null) => self.default_name(),
            Some(Value::String(name)) => name.clone(),
            Some(_) => {
                return Err(ModuleError::InvalidParams(format!(
                    "`{NAME_FIELD}` must be a string"
                )))
            }
        };
        Ok(json!({ "message": greeting(&name) }))
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greets_by_name_or_with_the_manifest_default() {
        let hello = Hello::new();
        assert_eq!(
            hello.call(GREET, json!({ "name": "Alexander" })).unwrap()["message"],
            "Hello, Alexander!"
        );
        assert_eq!(
            hello.call(GREET, json!({})).unwrap()["message"],
            "Hello, world!"
        );
        assert!(matches!(
            hello.call(GREET, json!({ "name": 3 })),
            Err(ModuleError::InvalidParams(_))
        ));
        assert!(matches!(
            hello.call("hello.other", json!({})),
            Err(ModuleError::UnknownMethod(_))
        ));
    }

    #[test]
    fn echo_answers_text_and_rejects_anything_else() {
        let hello = Hello::new();
        let call = |port: &str, value: Value| {
            let event = json!({ "port": port, "capability": "text@1", "value": value,
                "from": { "node_id": "a", "port_id": TICK_PORT }, "event": 1 });
            let node = json!({ "id": "b", "type": "greeter" });
            hello.call(
                method::NODE_EVENT,
                json!({ "execution_id": "e", "node": node, "args": event }),
            )
        };
        assert_eq!(
            call(ECHO_PORT, json!("Hi")).unwrap(),
            json!({ "echoed": "Hi" })
        );
        assert!(matches!(
            call(ECHO_PORT, json!(3)),
            Err(ModuleError::InvalidParams(_))
        ));
        assert!(matches!(
            call(TICK_PORT, json!("Hi")),
            Err(ModuleError::InvalidParams(_))
        ));
    }

    #[test]
    fn manifest_declares_what_it_serves() {
        let manifest = Hello::new().manifest().clone();
        assert!(manifest.module.methods.iter().any(|m| m == GREET));
        assert_eq!(manifest.nodes.len(), 1);
    }
}
