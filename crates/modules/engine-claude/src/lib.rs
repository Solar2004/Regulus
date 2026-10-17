//! The Claude Code engine. Claude speaks ACP through the official adapter, which drives
//! the `claude` binary we point it at, so the user's own installation and setup are the
//! ones that run (docs/12-agents.md §4, measured 2026-10-17).

use regulus_acp::engine::{find_on_path, installations, AcpEngine, EngineModule, Prepared, Start};
use regulus_acp::{Launch, SetupRequest};
use regulus_contract_agent_engine::{Installation, LlmEndpoint};
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
            meta: session_meta(start.isolated, start.provider),
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

/// An isolated profile reads only its own settings: not the project's `.claude/`, which
/// could carry hooks and permissions nobody wired in. The provider's first model, if it
/// names one, is the session's.
fn session_meta(isolated: bool, provider: Option<&LlmEndpoint>) -> Option<Map<String, Value>> {
    let mut options = Map::new();
    if isolated {
        options.insert("settingSources".into(), json!(["user"]));
    }
    if let Some(model) = provider.and_then(|p| p.models.first()) {
        options.insert("model".into(), json!(model));
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
        let meta = session_meta(true, Some(&provider)).unwrap();
        let options = &meta["claudeCode"]["options"];
        assert_eq!(options["settingSources"], json!(["user"]));
        assert_eq!(options["model"], "anthropic/claude-sonnet-4");
        assert!(session_meta(false, None).is_none());
    }

    #[test]
    fn detects_claude_and_profile_binaries_but_not_backups_or_launchers() {
        assert!(is_claude("claude"));
        assert!(is_claude("claude-ultra-bin"));
        assert!(!is_claude("claude-sub"));
        assert!(!is_claude("claude-sub-bin.bak-2.1.232"));
    }
}
