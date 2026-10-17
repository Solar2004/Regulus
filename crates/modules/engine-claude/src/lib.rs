//! The Claude Code engine. Claude speaks ACP through the official adapter, which drives
//! the `claude` binary we point it at, so the user's own installation and setup are the
//! ones that run (docs/12-agents.md §4, measured 2026-10-17).

use regulus_acp::engine::{find_on_path, installations, AcpEngine, EngineModule, Settings};
use regulus_acp::Launch;
use regulus_contract_agent_engine::Installation;
use regulus_core::parse_manifest;

const MANIFEST: &str = include_str!("../regulus-module.toml");
const BINARY: &str = "claude";
const EXECUTABLE_ENV: &str = "CLAUDE_CODE_EXECUTABLE";
const CONFIG_DIR_ENV: &str = "CLAUDE_CONFIG_DIR";

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

    fn launch(&self, settings: &Settings<'_>, detected: &[Installation]) -> Result<Launch, String> {
        let executable = settings
            .path("installation")
            .or_else(|| detected.first().map(|i| i.path.clone()))
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
        if let Some(dir) = settings.path("config_dir") {
            launch
                .env
                .insert(CONFIG_DIR_ENV.into(), dir.to_string_lossy().into_owned());
        }
        Ok(launch)
    }
}

pub fn module() -> EngineModule<Claude> {
    EngineModule::new(
        parse_manifest(MANIFEST).expect("engine-claude manifest is valid"),
        Claude,
    )
}

#[cfg(test)]
mod tests {
    use super::is_claude;

    #[test]
    fn detects_claude_and_profile_binaries_but_not_backups_or_launchers() {
        assert!(is_claude("claude"));
        assert!(is_claude("claude-ultra-bin"));
        assert!(!is_claude("claude-sub"));
        assert!(!is_claude("claude-sub-bin.bak-2.1.232"));
    }
}
