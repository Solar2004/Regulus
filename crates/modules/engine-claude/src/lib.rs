//! The Claude Code engine. Claude speaks ACP through the official adapter, which drives
//! the `claude` binary we point it at, so the user's own installation and setup are the
//! ones that run (docs/12-agents.md §4, measured 2026-10-17).

use std::collections::BTreeMap;

use regulus_acp::engine::{find_on_path, installations, AcpEngine, EngineModule, Prepared, Start};
use regulus_acp::{Launch, SetupRequest};
use regulus_contract_agent_engine::{slot, Installation, LlmEndpoint};
use regulus_contract_llm_endpoint::protocol;
use regulus_core::parse_manifest;
use serde_json::{json, Map, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const BINARY: &str = "claude";
const EXECUTABLE_ENV: &str = "CLAUDE_CODE_EXECUTABLE";
const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";
/// The adapter's one provider slot (`providers/list`), set before the session opens.
const SET_PROVIDER: &str = "providers/set";
const PROVIDER_ID: &str = "main";
/// The model setting each slot becomes, in the session's environment.
const SLOT_ENV: &[(&str, &str)] = &[
    (slot::FAST, "ANTHROPIC_DEFAULT_HAIKU_MODEL"),
    (slot::SUBAGENT, "CLAUDE_CODE_SUBAGENT_MODEL"),
];

pub struct Claude;

/// `claude`, and the per-profile binaries a setup like claude-sub installs (`claude-*-bin`).
fn is_claude(name: &str) -> bool {
    name == BINARY || (name.starts_with("claude-") && name.ends_with("-bin"))
}

impl AcpEngine for Claude {
    fn detect(&self) -> Vec<Installation> {
        let mut found = find_on_path(is_claude);
        // Plain `claude` first: it is what the user runs by default.
        found.sort_by_key(|path| path.file_name().is_none_or(|n| n != BINARY));
        installations(found)
    }

    fn protocols(&self) -> &'static [&'static str] {
        &[protocol::ANTHROPIC, protocol::BEDROCK, protocol::VERTEX]
    }

    fn slots(&self) -> &'static [&'static str] {
        &[slot::FAST, slot::SUBAGENT]
    }

    fn prepare(&self, start: &Start<'_>) -> Result<Prepared, String> {
        let settings = &start.settings;
        let executable = settings
            .path("installation")
            .or_else(|| start.detected.first().map(|i| i.path.clone()))
            .ok_or("Claude Code is not installed: no `claude` on PATH and no installation set")?;
        let adapter = settings
            .text("adapter")
            .ok_or("no ACP adapter command set")?;
        let mut words = adapter.split_whitespace().map(String::from);
        let program = words.next().ok_or("no ACP adapter command set")?;
        let mut launch = Launch {
            program: program.into(),
            args: words.collect(),
            ..Launch::default()
        };
        launch.env.insert(
            EXECUTABLE_ENV.into(),
            executable.to_string_lossy().into_owned(),
        );
        if let Some(dir) = &start.config_dir {
            launch
                .env
                .insert(CONFIG_DIR_ENV.into(), dir.to_string_lossy().into_owned());
        }
        Ok(Prepared {
            launch,
            meta: session_meta(
                start.isolated,
                start.provider,
                slot_env(start.provider, start.slots)?,
            ),
            setup: start.provider.map(set_provider).into_iter().collect(),
        })
    }
}

fn set_provider(provider: &LlmEndpoint) -> SetupRequest {
    SetupRequest {
        method: SET_PROVIDER.into(),
        params: json!({
            "providerId": PROVIDER_ID,
            "apiType": provider.protocol,
            "baseUrl": provider.base_url,
            "headers": provider.headers,
        }),
    }
}

/// Claude Code talks to one endpoint, so a slot can only pick another model there. A slot
/// wired to a different endpoint needs the gateway (docs/12-agents.md §6.4).
fn slot_env(
    main: Option<&LlmEndpoint>,
    slots: &BTreeMap<String, LlmEndpoint>,
) -> Result<Map<String, Value>, String> {
    let mut env = Map::new();
    for (name, endpoint) in slots {
        let Some((_, var)) = SLOT_ENV.iter().find(|(s, _)| s == name) else {
            continue;
        };
        let same = main.is_some_and(|m| {
            m.protocol == endpoint.protocol
                && m.base_url == endpoint.base_url
                && m.headers == endpoint.headers
        });
        if !same {
            return Err(format!(
                "Claude Code reaches one endpoint; the `{name}` provider is another one. \
                 Wire the same provider as the main one, or wait for the gateway"
            ));
        }
        let model = endpoint
            .models
            .first()
            .ok_or_else(|| format!("the `{name}` provider names no model"))?;
        env.insert((*var).into(), json!(model));
    }
    Ok(env)
}

/// An isolated profile reads only its own settings: not the project's `.claude/`, which
/// could carry hooks and permissions nobody wired in. The provider's first model, if it
/// names one, is the session's, and the slots add theirs to the session's environment.
fn session_meta(
    isolated: bool,
    provider: Option<&LlmEndpoint>,
    env: Map<String, Value>,
) -> Option<Map<String, Value>> {
    let mut options = Map::new();
    if isolated {
        options.insert("settingSources".into(), json!(["user"]));
    }
    if let Some(model) = provider.and_then(|p| p.models.first()) {
        options.insert("model".into(), json!(model));
    }
    if !env.is_empty() {
        options.insert("env".into(), Value::Object(env));
    }
    if options.is_empty() {
        return None;
    }
    let mut meta = Map::new();
    meta.insert("claudeCode".into(), json!({ "options": options }));
    Some(meta)
}

pub fn module() -> EngineModule<Claude> {
    EngineModule::new(
        parse_manifest(MANIFEST).expect("engine-claude manifest is valid"),
        Claude,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_provider_goes_to_the_adapter_and_isolation_drops_project_settings() {
        let provider = LlmEndpoint {
            protocol: "anthropic".into(),
            base_url: "https://openrouter.ai/api".into(),
            headers: [("authorization".into(), "Bearer k".into())].into(),
            models: vec!["anthropic/claude-sonnet-4".into()],
        };
        let setup = set_provider(&provider);
        assert_eq!(setup.method, "providers/set");
        assert_eq!(setup.params["apiType"], "anthropic");
        assert_eq!(setup.params["headers"]["authorization"], "Bearer k");
        let meta = session_meta(true, Some(&provider), Map::new()).unwrap();
        let options = &meta["claudeCode"]["options"];
        assert_eq!(options["settingSources"], json!(["user"]));
        assert_eq!(options["model"], "anthropic/claude-sonnet-4");
        assert!(options.get("env").is_none());
        assert!(session_meta(false, None, Map::new()).is_none());
    }

    fn endpoint(base_url: &str, model: &str) -> LlmEndpoint {
        LlmEndpoint {
            protocol: "anthropic".into(),
            base_url: base_url.into(),
            headers: [("x-api-key".into(), "k".into())].into(),
            models: vec![model.into()],
        }
    }

    #[test]
    fn slots_on_the_main_endpoint_pick_their_models() {
        let main = endpoint("https://opencode.ai/zen/go", "qwen3.8-max");
        let slots = [
            (
                "fast".to_string(),
                endpoint("https://opencode.ai/zen/go", "minimax-m3"),
            ),
            (
                "subagent".to_string(),
                endpoint("https://opencode.ai/zen/go", "qwen3.8-max"),
            ),
        ]
        .into();
        let env = slot_env(Some(&main), &slots).unwrap();
        assert_eq!(env["ANTHROPIC_DEFAULT_HAIKU_MODEL"], "minimax-m3");
        assert_eq!(env["CLAUDE_CODE_SUBAGENT_MODEL"], "qwen3.8-max");
        let meta = session_meta(false, Some(&main), env).unwrap();
        assert_eq!(
            meta["claudeCode"]["options"]["env"]["ANTHROPIC_DEFAULT_HAIKU_MODEL"],
            "minimax-m3"
        );
    }

    #[test]
    fn a_slot_on_another_endpoint_needs_the_gateway() {
        let main = endpoint("https://opencode.ai/zen/go", "qwen3.8-max");
        let slots = [(
            "fast".to_string(),
            endpoint("https://openrouter.ai/api", "x"),
        )]
        .into();
        let error = slot_env(Some(&main), &slots).unwrap_err();
        assert!(
            error.contains("`fast`") && error.contains("gateway"),
            "{error}"
        );
        assert!(slot_env(None, &slots).is_err());
    }

    #[test]
    fn detects_claude_and_profile_binaries_but_not_backups_or_launchers() {
        assert!(is_claude("claude"));
        assert!(is_claude("claude-ultra-bin"));
        assert!(!is_claude("claude-sub"));
        assert!(!is_claude("claude-sub-bin.bak-2.1.232"));
    }
}
