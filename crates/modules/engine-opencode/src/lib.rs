//! The opencode engine: opencode speaks ACP natively (`opencode acp`), so this only finds
//! the binary and turns the profile and the provider into its configuration
//! (docs/12-agents.md §3, §6).
//!
//! It takes no hooks (docs/12-agents.md §3): opencode has no hook settings, only plugins,
//! which are JS code. Its plugin API (`@opencode-ai/plugin` 1.17) has
//! `tool.execute.before`/`after` but nothing that keeps the agent working when it stops,
//! no documented veto, and context only through `experimental.*` hooks. Wired hooks are
//! left out with a warning until a plugin bridge is worth writing.

use std::collections::BTreeMap;
use std::path::Path;

use regulus_acp::engine::{find_on_path, installations, AcpEngine, EngineModule, Prepared, Start};
use regulus_acp::Launch;
use regulus_contract_agent_engine::{slot, Installation, LlmEndpoint};
use regulus_contract_llm_endpoint::protocol;
use regulus_core::parse_manifest;
use serde_json::{json, Map, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const BINARY: &str = "opencode";
const CONFIG_DIR_ENV: &str = "OPENCODE_CONFIG_DIR";
/// Inside OPENCODE_CONFIG_DIR, where opencode looks for `skills/**/SKILL.md`.
const SKILLS_DIR: &str = "skills";
/// Configuration merged over everything else opencode reads.
const CONFIG_CONTENT_ENV: &str = "OPENCODE_CONFIG_CONTENT";
/// What an isolated profile switches off: the project's opencode config, and what
/// opencode would otherwise borrow from the user's Claude Code setup and skills.
const ISOLATION_ENV: &[&str] = &[
    "OPENCODE_DISABLE_PROJECT_CONFIG",
    "OPENCODE_DISABLE_CLAUDE_CODE",
    "OPENCODE_DISABLE_EXTERNAL_SKILLS",
];
/// Where opencode keeps its global config, credentials and state. An isolated profile
/// moves them inside itself; the cache stays shared, it holds nothing of the user's.
const ISOLATED_HOMES: &[(&str, &str)] = &[
    ("XDG_CONFIG_HOME", "config"),
    ("XDG_DATA_HOME", "data"),
    ("XDG_STATE_HOME", "state"),
];
/// The provider id the wired endpoint gets inside opencode: models are `regulus/<id>`. A
/// slot on another endpoint gets `regulus-<slot>`.
const PROVIDER_ID: &str = "regulus";
/// The node's own choice of model, used when no provider is wired into the box.
const MODEL_FIELD: &str = "model";
/// opencode's built-in subagents, whose model the `subagent` slot sets.
const SUBAGENTS: &[&str] = &["general", "explore"];

pub struct Opencode;

impl AcpEngine for Opencode {
    fn detect(&self) -> Vec<Installation> {
        installations(find_on_path(|name| name == BINARY))
    }

    fn protocols(&self) -> &'static [&'static str] {
        &[protocol::ANTHROPIC, protocol::OPENAI]
    }

    fn slots(&self) -> &'static [&'static str] {
        &[slot::FAST, slot::SUBAGENT]
    }

    fn skills_dir(&self) -> Option<&'static str> {
        Some(SKILLS_DIR)
    }

    fn prepare(&self, start: &Start<'_>) -> Result<Prepared, String> {
        let program = start
            .settings
            .path("installation")
            .or_else(|| start.detected.first().map(|i| i.path.clone()))
            .ok_or("opencode is not installed: no `opencode` on PATH and no installation set")?;
        let mut launch = Launch {
            program,
            args: vec!["acp".into()],
            ..Launch::default()
        };
        if let Some(dir) = &start.config_dir {
            launch
                .env
                .insert(CONFIG_DIR_ENV.into(), dir.to_string_lossy().into_owned());
            if start.isolated {
                isolate(&mut launch, dir);
            }
        }
        let model = start.settings.text(MODEL_FIELD);
        if let Some(config) = config(start.provider, start.slots, model)? {
            launch
                .env
                .insert(CONFIG_CONTENT_ENV.into(), config.to_string());
        }
        Ok(Prepared {
            launch,
            ..Prepared::default()
        })
    }
}

fn isolate(launch: &mut Launch, dir: &Path) {
    for name in ISOLATION_ENV {
        launch.env.insert((*name).into(), "1".into());
    }
    for (name, sub) in ISOLATED_HOMES {
        launch
            .env
            .insert((*name).into(), dir.join(sub).to_string_lossy().into_owned());
    }
}

/// The wired providers as opencode configuration: one opencode provider per endpoint, the
/// main model, `small_model` for the `fast` slot and the subagents' model for `subagent`.
/// opencode holds several providers at once, so every slot can reach its own endpoint.
///
/// `model` is the node's `model` field, an opencode model id: the main model when no
/// provider is wired. A wired provider wins over it.
fn config(
    main: Option<&LlmEndpoint>,
    slots: &BTreeMap<String, LlmEndpoint>,
    model: Option<&str>,
) -> Result<Option<Value>, String> {
    let model = model.filter(|_| main.is_none());
    if main.is_none() && slots.is_empty() {
        return Ok(model.map(|model| json!({ "model": model })));
    }
    // Endpoints that reach the same place share one opencode provider.
    let mut routes: Vec<(String, LlmEndpoint, Vec<String>)> = Vec::new();
    let mut route = |endpoint: &LlmEndpoint, id: String, input: &str| {
        let model = endpoint
            .models
            .first()
            .ok_or_else(|| {
                format!("opencode needs a model: set `model` on the `{input}` provider")
            })?
            .clone();
        let found = routes.iter_mut().find(|(_, e, _)| {
            e.protocol == endpoint.protocol
                && e.base_url == endpoint.base_url
                && e.headers == endpoint.headers
        });
        let id = match found {
            Some((id, _, models)) => {
                models.extend(endpoint.models.iter().cloned());
                id.clone()
            }
            None => {
                routes.push((id.clone(), endpoint.clone(), endpoint.models.clone()));
                id
            }
        };
        Ok::<_, String>(format!("{id}/{model}"))
    };
    let mut root = Map::new();
    if let Some(main) = main {
        root.insert(
            "model".into(),
            json!(route(main, PROVIDER_ID.into(), "provider")?),
        );
    } else if let Some(model) = model {
        root.insert("model".into(), json!(model));
    }
    for (name, endpoint) in slots {
        let model = route(endpoint, format!("{PROVIDER_ID}-{name}"), name)?;
        match name.as_str() {
            slot::FAST => {
                root.insert("small_model".into(), json!(model));
            }
            slot::SUBAGENT => {
                let agents: Map<String, Value> = SUBAGENTS
                    .iter()
                    .map(|a| ((*a).to_string(), json!({ "model": model })))
                    .collect();
                root.insert("agent".into(), Value::Object(agents));
            }
            _ => {}
        }
    }
    let mut providers = Map::new();
    for (id, endpoint, models) in routes {
        providers.insert(id, provider_entry(&endpoint, &models)?);
    }
    root.insert("provider".into(), Value::Object(providers));
    Ok(Some(Value::Object(root)))
}

/// One endpoint as an opencode provider. The credential moves from the headers to the SDK
/// option that sends it, so it is not sent twice.
fn provider_entry(provider: &LlmEndpoint, models: &[String]) -> Result<Value, String> {
    let mut headers = provider.headers.clone();
    let mut options = Map::new();
    let bearer = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("authorization"))
        .and_then(|(k, v)| Some((k.clone(), v.strip_prefix("Bearer ")?.to_string())));
    let api_key = headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-api-key"))
        .map(|(k, v)| (k.clone(), v.clone()));
    let base = provider.base_url.trim_end_matches('/');
    let npm = match provider.protocol.as_str() {
        protocol::ANTHROPIC => {
            options.insert("baseURL".into(), json!(format!("{base}/v1")));
            if let Some((name, key)) = api_key {
                headers.remove(&name);
                options.insert("apiKey".into(), json!(key));
            } else if let Some((name, token)) = bearer {
                headers.remove(&name);
                options.insert("authToken".into(), json!(token));
            }
            "@ai-sdk/anthropic"
        }
        protocol::OPENAI => {
            options.insert("baseURL".into(), json!(base));
            if let Some((name, token)) = bearer {
                headers.remove(&name);
                options.insert("apiKey".into(), json!(token));
            }
            "@ai-sdk/openai-compatible"
        }
        other => return Err(format!("opencode cannot speak `{other}`")),
    };
    // The SDKs refuse to start without a credential; a local endpoint ignores this one.
    if !options.contains_key("apiKey") && !options.contains_key("authToken") {
        options.insert("apiKey".into(), json!(PROVIDER_ID));
    }
    if !headers.is_empty() {
        options.insert("headers".into(), json!(headers));
    }
    let models: Map<String, Value> = models.iter().map(|m| (m.clone(), json!({}))).collect();
    Ok(json!({ "npm": npm, "name": "Regulus", "options": options, "models": models }))
}

pub fn module() -> EngineModule<Opencode> {
    EngineModule::new(
        parse_manifest(MANIFEST).expect("engine-opencode manifest is valid"),
        Opencode,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn endpoint(protocol: &str, base_url: &str, header: (&str, &str)) -> LlmEndpoint {
        LlmEndpoint {
            protocol: protocol.into(),
            base_url: base_url.into(),
            headers: [(header.0.into(), header.1.into())].into(),
            models: vec!["m1".into(), "m2".into()],
        }
    }

    #[test]
    fn an_anthropic_endpoint_becomes_an_opencode_provider() {
        let provider = endpoint(
            "anthropic",
            "https://openrouter.ai/api/",
            ("authorization", "Bearer k"),
        );
        let config = config(Some(&provider), &BTreeMap::new(), None)
            .unwrap()
            .unwrap();
        let regulus = &config["provider"]["regulus"];
        assert_eq!(regulus["npm"], "@ai-sdk/anthropic");
        assert_eq!(
            regulus["options"]["baseURL"],
            "https://openrouter.ai/api/v1"
        );
        assert_eq!(regulus["options"]["authToken"], "k");
        assert!(regulus["options"].get("headers").is_none());
        assert_eq!(config["model"], "regulus/m1");
    }

    #[test]
    fn an_openai_endpoint_keeps_its_base_url() {
        let provider = endpoint("openai", "http://127.0.0.1:8000/v1", ("x-team", "a"));
        let config = config(Some(&provider), &BTreeMap::new(), None)
            .unwrap()
            .unwrap();
        let options = &config["provider"]["regulus"]["options"];
        assert_eq!(options["baseURL"], "http://127.0.0.1:8000/v1");
        assert_eq!(options["headers"]["x-team"], "a");
        assert_eq!(options["apiKey"], "regulus");
    }

    #[test]
    fn without_a_model_opencode_cannot_use_the_provider() {
        let mut provider = endpoint("anthropic", "http://127.0.0.1:3456", ("x", "y"));
        provider.models.clear();
        assert!(config(Some(&provider), &BTreeMap::new(), None).is_err());
        assert!(config(None, &BTreeMap::new(), None).unwrap().is_none());
    }

    #[test]
    fn slots_share_the_main_endpoint_or_get_their_own() {
        let main = endpoint(
            "anthropic",
            "https://opencode.ai/zen/go",
            ("x-api-key", "k"),
        );
        let mut same = main.clone();
        same.models = vec!["m3".into()];
        let other = endpoint(
            "openai",
            "https://openrouter.ai/api/v1",
            ("authorization", "Bearer o"),
        );
        let slots = [("fast".to_string(), same), ("subagent".to_string(), other)].into();
        let config = config(Some(&main), &slots, None).unwrap().unwrap();
        assert_eq!(config["model"], "regulus/m1");
        assert_eq!(config["small_model"], "regulus/m3");
        assert_eq!(config["agent"]["general"]["model"], "regulus-subagent/m1");
        assert_eq!(config["agent"]["explore"]["model"], "regulus-subagent/m1");
        let providers = config["provider"].as_object().unwrap();
        assert_eq!(providers.len(), 2);
        assert!(providers["regulus"]["models"].get("m3").is_some());
        assert_eq!(
            providers["regulus-subagent"]["npm"],
            "@ai-sdk/openai-compatible"
        );
    }

    #[test]
    fn the_model_field_applies_only_without_a_provider() {
        let free = Some("opencode/glm-5.3-flash-free");
        let config_of = |main, slots: &BTreeMap<String, LlmEndpoint>| {
            config(main, slots, free).unwrap().unwrap()
        };
        let alone = config_of(None, &BTreeMap::new());
        assert_eq!(alone, json!({ "model": "opencode/glm-5.3-flash-free" }));
        let main = endpoint("anthropic", "http://127.0.0.1:3456", ("x-api-key", "k"));
        assert_eq!(
            config_of(Some(&main), &BTreeMap::new())["model"],
            "regulus/m1"
        );
        let slots = [("fast".to_string(), main.clone())].into();
        let with_slot = config_of(None, &slots);
        assert_eq!(with_slot["model"], "opencode/glm-5.3-flash-free");
        assert_eq!(with_slot["small_model"], "regulus-fast/m1");
        assert!(config(None, &BTreeMap::new(), None).unwrap().is_none());
    }
}
