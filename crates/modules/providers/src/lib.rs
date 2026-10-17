//! Providers (docs/12-agents.md §6). A provider node does not proxy anything: it answers
//! `llm.endpoint` with where its model is and how to reach it, and the agent's CLI talks
//! to the model directly. The credential arrives already resolved by the core; this
//! module never sees the secret store.

use regulus_contract_llm_endpoint::{LlmEndpoint, DESCRIBE};
use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError};
use regulus_proto::{Manifest, NodeCall};
use serde_json::Value;

const MANIFEST: &str = include_str!("../regulus-module.toml");

pub struct Providers {
    manifest: Manifest,
}

impl Providers {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("providers manifest is valid"),
        }
    }

    fn text<'a>(&'a self, call: &'a NodeCall, field: &str) -> Option<&'a str> {
        self.manifest
            .setting(&call.node, field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    fn describe(&self, call: &NodeCall) -> Result<LlmEndpoint, ModuleError> {
        let missing = |field: &str| {
            ModuleError::Failed(format!("provider `{}` has no `{field}` set", call.node.id))
        };
        let mut endpoint = LlmEndpoint {
            protocol: self
                .text(call, "protocol")
                .ok_or_else(|| missing("protocol"))?
                .into(),
            base_url: self
                .text(call, "base_url")
                .ok_or_else(|| missing("base_url"))?
                .into(),
            ..LlmEndpoint::default()
        };
        if let Some(key) = self.text(call, "credential") {
            let (name, value) = match self.text(call, "auth") {
                Some("x-api-key") => ("x-api-key", key.to_string()),
                _ => ("authorization", format!("Bearer {key}")),
            };
            endpoint.headers.insert(name.into(), value);
        }
        endpoint.models = self
            .text(call, "model")
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .map(String::from)
            .collect();
        Ok(endpoint)
    }
}

impl Default for Providers {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Providers {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, _ctx: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            DESCRIBE => {
                let call: NodeCall = serde_json::from_value(params)
                    .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
                serde_json::to_value(self.describe(&call)?)
                    .map_err(|e| ModuleError::Failed(e.to_string()))
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(config: Value) -> Value {
        let node = json!({
            "id": "openrouter", "type": "provider", "name": "OpenRouter",
            "module_id": "regulus/providers@0.1.0", "config": config,
        });
        let params = json!({ "execution_id": "exec-1", "node": node });
        Providers::new().call(DESCRIBE, params).unwrap()
    }

    #[test]
    fn describes_the_endpoint_with_its_credential() {
        let endpoint = call(json!({
            "base_url": "https://openrouter.ai/api",
            "credential": "sk-or-1",
            "model": "anthropic/claude-sonnet-4, z-ai/glm-5",
        }));
        assert_eq!(endpoint["protocol"], "anthropic");
        assert_eq!(endpoint["headers"]["authorization"], "Bearer sk-or-1");
        assert_eq!(endpoint["models"][1], "z-ai/glm-5");
    }

    #[test]
    fn a_local_proxy_needs_no_credential() {
        let endpoint = call(json!({ "base_url": "http://127.0.0.1:3456", "auth": "x-api-key" }));
        assert!(endpoint.get("headers").is_none());
        assert!(endpoint.get("models").is_none());
    }
}
