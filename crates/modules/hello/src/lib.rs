//! A trivial module. It exists to prove that a module plugs in without touching the core,
//! and that the core starts without it.

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError};
use regulus_proto::Manifest;
use serde_json::{json, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const GREET: &str = "hello.greet";
const NAME_FIELD: &str = "name";

pub struct Hello {
    manifest: Manifest,
}

impl Hello {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("hello manifest is valid"),
        }
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

    fn init(&mut self, _ctx: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError> {
        if method != GREET {
            return Err(ModuleError::UnknownMethod(method.into()));
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
        Ok(json!({ "message": format!("Hello, {name}!") }))
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
    fn manifest_declares_what_it_serves() {
        let manifest = Hello::new().manifest().clone();
        assert!(manifest.module.methods.iter().any(|m| m == GREET));
        assert_eq!(manifest.nodes.len(), 1);
    }
}
