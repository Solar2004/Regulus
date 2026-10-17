//! The opencode engine: opencode speaks ACP natively (`opencode acp`), so this only finds
//! the binary and picks its configuration (docs/12-agents.md §3).

use regulus_acp::engine::{find_on_path, installations, AcpEngine, EngineModule, Settings};
use regulus_acp::Launch;
use regulus_contract_agent_engine::Installation;
use regulus_core::parse_manifest;

const MANIFEST: &str = include_str!("../regulus-module.toml");
const BINARY: &str = "opencode";
const CONFIG_DIR_ENV: &str = "OPENCODE_CONFIG_DIR";

pub struct Opencode;

impl AcpEngine for Opencode {
    fn detect(&self) -> Vec<Installation> {
        installations(find_on_path(|name| name == BINARY))
    }

    fn launch(&self, settings: &Settings<'_>, detected: &[Installation]) -> Result<Launch, String> {
        let program = settings
            .path("installation")
            .or_else(|| detected.first().map(|i| i.path.clone()))
            .ok_or("opencode is not installed: no `opencode` on PATH and no installation set")?;
        let mut launch = Launch {
            program,
            args: vec!["acp".into()],
            ..Launch::default()
        };
        if let Some(dir) = settings.path("config_dir") {
            launch
                .env
                .insert(CONFIG_DIR_ENV.into(), dir.to_string_lossy().into_owned());
        }
        Ok(launch)
    }
}

pub fn module() -> EngineModule<Opencode> {
    EngineModule::new(
        parse_manifest(MANIFEST).expect("engine-opencode manifest is valid"),
        Opencode,
    )
}
