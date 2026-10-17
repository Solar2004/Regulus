//! Providers (docs/12-agents.md §6). A provider node does not proxy anything: it answers
//! `llm.endpoint` with where its model is and how to reach it, and the agent's CLI talks
//! to the model directly. The credential arrives already resolved by the core; this
//! module never sees the secret store.
//!
//! OpenRouter, opencode Go, Anthropic… are templates, not code (§6.3): a node picks one
//! with `preset`, from the catalog that ships with the module or from its own `catalog`
//! file. Each setting comes from the node if it sets it, then from the template, then from
//! the manifest's default.

use std::collections::BTreeMap;
use std::path::PathBuf;

use regulus_contract_llm_endpoint::{LlmEndpoint, DESCRIBE};
use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError};
use regulus_proto::{Manifest, NodeCall};
use serde::Deserialize;
use serde_json::Value;

const MANIFEST: &str = include_str!("../regulus-module.toml");
const PRESETS: &str = include_str!("../presets.toml");

/// One template of the provider node. Every field is optional: a template fills in what
/// it knows and the node sets the rest.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub protocol: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub auth: Option<String>,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    /// One line on what the provider allows: who may use the key, and how.
    #[serde(default)]
    pub terms: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    #[serde(default)]
    presets: BTreeMap<String, Preset>,
}

fn parse_catalog(source: &str, origin: &str) -> Result<BTreeMap<String, Preset>, ModuleError> {
    toml::from_str::<Catalog>(source)
        .map(|catalog| catalog.presets)
        .map_err(|e| ModuleError::Failed(format!("provider catalog {origin} is invalid: {e}")))
}

fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

pub struct Providers {
    manifest: Manifest,
    presets: BTreeMap<String, Preset>,
}

impl Providers {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("providers manifest is valid"),
            presets: parse_catalog(PRESETS, "presets.toml").expect("built-in catalog is valid"),
        }
    }

    /// The templates that ship with the module.
    pub fn presets(&self) -> &BTreeMap<String, Preset> {
        &self.presets
    }

    fn text(value: Option<&Value>) -> Option<&str> {
        value
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// What the node itself sets, ignoring the manifest's defaults.
    fn own<'a>(call: &'a NodeCall, field: &str) -> Option<&'a str> {
        Self::text(call.node.config.get(field))
    }

    /// The manifest's default for a field.
    fn default<'a>(&'a self, call: &'a NodeCall, field: &str) -> Option<&'a str> {
        let node_type = self.manifest.node_type(&call.node.node_type)?;
        Self::text(node_type.field(field)?.default.as_ref())
    }

    /// The template the node picks, looked up in its own catalog first.
    fn preset(&self, call: &NodeCall) -> Result<Preset, ModuleError> {
        let Some(id) = Self::own(call, "preset") else {
            return Ok(Preset::default());
        };
        let mut presets = self.presets.clone();
        if let Some(path) = Self::own(call, "catalog") {
            let path = expand_home(path);
            let source = std::fs::read_to_string(&path).map_err(|e| {
                ModuleError::Failed(format!(
                    "provider `{}` cannot read its catalog {}: {e}",
                    call.node.id,
                    path.display()
                ))
            })?;
            presets.extend(parse_catalog(&source, &path.display().to_string())?);
        }
        let known = presets.keys().cloned().collect::<Vec<_>>().join(", ");
        presets.remove(id).ok_or_else(|| {
            ModuleError::Failed(format!(
                "provider `{}` asks for preset `{id}`, which no catalog has. Known: {known}",
                call.node.id
            ))
        })
    }

    fn describe(&self, call: &NodeCall) -> Result<LlmEndpoint, ModuleError> {
        let preset = self.preset(call)?;
        let pick = |field: &str, from_preset: Option<&str>| -> Option<String> {
            Self::own(call, field)
                .or(from_preset)
                .or_else(|| self.default(call, field))
                .map(String::from)
        };
        let missing = |field: &str| {
            ModuleError::Failed(format!(
                "provider `{}` has no `{field}` set, neither on the node nor in a preset",
                call.node.id
            ))
        };
        let mut endpoint = LlmEndpoint {
            protocol: pick("protocol", preset.protocol.as_deref())
                .ok_or_else(|| missing("protocol"))?,
            base_url: pick("base_url", preset.base_url.as_deref())
                .ok_or_else(|| missing("base_url"))?,
            headers: preset.headers,
            ..LlmEndpoint::default()
        };
        // The node's own headers go over the template's, one by one.
        if let Some(Value::Object(own)) = call.node.config.get("headers") {
            for (name, value) in own {
                match value.as_str() {
                    Some(value) => endpoint.headers.insert(name.clone(), value.to_string()),
                    None => {
                        return Err(ModuleError::InvalidParams(format!(
                            "provider `{}`: header `{name}` must be text",
                            call.node.id
                        )))
                    }
                };
            }
        }
        if let Some(key) = Self::own(call, "credential") {
            match pick("auth", preset.auth.as_deref()).as_deref() {
                Some("none") => {}
                Some("x-api-key") => {
                    endpoint.headers.insert("x-api-key".into(), key.to_string());
                }
                _ => {
                    endpoint
                        .headers
                        .insert("authorization".into(), format!("Bearer {key}"));
                }
            }
        }
        endpoint.models = match Self::own(call, "model") {
            Some(list) => list
                .split(',')
                .map(str::trim)
                .filter(|m| !m.is_empty())
                .map(String::from)
                .collect(),
            None => preset.models,
        };
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

    fn try_call(config: Value) -> Result<Value, ModuleError> {
        let node = json!({
            "id": "openrouter", "type": "provider", "name": "OpenRouter",
            "module_id": "regulus/providers@0.1.0", "config": config,
        });
        let params = json!({ "execution_id": "exec-1", "node": node });
        Providers::new().call(DESCRIBE, params)
    }

    fn call(config: Value) -> Value {
        try_call(config).unwrap()
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

    #[test]
    fn a_preset_fills_in_what_the_node_does_not_set() {
        let endpoint = call(json!({ "preset": "opencode-go", "credential": "k" }));
        assert_eq!(endpoint["base_url"], "https://opencode.ai/zen/go");
        assert_eq!(endpoint["headers"]["x-api-key"], "k");
        assert_eq!(endpoint["headers"]["x-opencode-session"], "regulus");
        assert_eq!(endpoint["models"][0], "qwen3.8-max");
    }

    #[test]
    fn the_node_wins_over_its_preset() {
        let endpoint = call(json!({
            "preset": "opencode-go", "credential": "k", "auth": "bearer", "model": "minimax-m3",
            "headers": { "x-opencode-session": "dev-box", "x-extra": "1" },
        }));
        assert_eq!(endpoint["headers"]["authorization"], "Bearer k");
        assert!(endpoint["headers"].get("x-api-key").is_none());
        assert_eq!(endpoint["headers"]["x-opencode-session"], "dev-box");
        assert_eq!(endpoint["headers"]["x-extra"], "1");
        assert_eq!(endpoint["models"], json!(["minimax-m3"]));
    }

    #[test]
    fn a_keyless_preset_sends_no_credential() {
        let endpoint =
            call(json!({ "preset": "ollama", "credential": "ignored", "model": "qwen3" }));
        assert_eq!(endpoint["protocol"], "openai");
        assert!(endpoint.get("headers").is_none());
    }

    #[test]
    fn the_nodes_own_catalog_adds_and_replaces_presets() {
        let dir = std::env::temp_dir().join(format!("regulus-presets-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("catalog.toml");
        std::fs::write(
            &file,
            "[presets.mine]\nprotocol = \"openai\"\nbase_url = \"http://10.0.0.2:8000/v1\"\n\
             [presets.openrouter]\nbase_url = \"https://eu.openrouter.ai/api\"\n",
        )
        .unwrap();
        let catalog = file.to_string_lossy();
        let mine = call(json!({ "preset": "mine", "catalog": catalog }));
        assert_eq!(mine["base_url"], "http://10.0.0.2:8000/v1");
        let replaced = call(json!({ "preset": "openrouter", "catalog": catalog }));
        assert_eq!(replaced["base_url"], "https://eu.openrouter.ai/api");
        // A replaced preset is replaced whole: what it leaves out falls to the defaults.
        assert_eq!(replaced["protocol"], "anthropic");
        assert!(replaced.get("models").is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_unknown_preset_names_the_known_ones() {
        let error = try_call(json!({ "preset": "nope" }))
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("`nope`") && error.contains("openrouter"),
            "{error}"
        );
    }

    #[test]
    fn every_built_in_preset_describes_an_endpoint() {
        for (id, preset) in Providers::new().presets() {
            let endpoint = call(json!({ "preset": id, "credential": "k" }));
            assert!(!endpoint["base_url"].as_str().unwrap().is_empty(), "{id}");
            assert!(preset.terms.is_some(), "{id} has no terms");
        }
    }
}
