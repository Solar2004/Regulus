//! The opencode engine: opencode speaks ACP natively (`opencode acp`), so this only finds
//! the binary and turns the profile and the provider into its configuration
//! (docs/12-agents.md §3, §6).

use std::path::Path;

use regulus_acp::engine::{find_on_path, installations, AcpEngine, EngineModule, Prepared, Start};
use regulus_acp::Launch;
use regulus_contract_agent_engine::{Installation, LlmEndpoint};
use regulus_contract_llm_endpoint::protocol;
use regulus_core::parse_manifest;
use serde_json::{json, Map, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const BINARY: &str = "opencode";
const CONFIG_DIR_ENV: &str = "OPENCODE_CONFIG_DIR";
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
/// The provider id the wired endpoint gets inside opencode: models are `regulus/<id>`.
const PROVIDER_ID: &str = "regulus";

pub struct Opencode;

impl AcpEngine for Opencode {
    fn detect(&self) -> Vec<Installation> {
        installations(find_on_path(|name| name == BINARY))
    }

    fn protocols(&self) -> &'static [&'static str] {
        &[protocol::ANTHROPIC, protocol::OPENAI]
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
        if let Some(provider) = start.provider {
            launch
                .env
                .insert(CONFIG_CONTENT_ENV.into(), config(provider)?.to_string());
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

/// The endpoint as an opencode provider. The credential moves from the headers to the
/// SDK option that sends it, so it is not sent twice.
fn config(provider: &LlmEndpoint) -> Result<Value, String> {
    let model = provider
        .models
        .first()
        .ok_or("opencode needs a model: set `model` on the provider")?;
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
    let models: Map<String, Value> = provider
        .models
        .iter()
        .map(|m| (m.clone(), json!({})))
        .collect();
    Ok(json!({
        "provider": {
            PROVIDER_ID: { "npm": npm, "name": "Regulus", "options": options, "models": models }
        },
        "model": format!("{PROVIDER_ID}/{model}"),
    }))
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
        let config = config(&provider).unwrap();
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
        let config = config(&provider).unwrap();
        let options = &config["provider"]["regulus"]["options"];
        assert_eq!(options["baseURL"], "http://127.0.0.1:8000/v1");
        assert_eq!(options["headers"]["x-team"], "a");
        assert_eq!(options["apiKey"], "regulus");
    }

    #[test]
    fn without_a_model_opencode_cannot_use_the_provider() {
        let mut provider = endpoint("anthropic", "http://127.0.0.1:3456", ("x", "y"));
        provider.models.clear();
        assert!(config(&provider).is_err());
    }
}
