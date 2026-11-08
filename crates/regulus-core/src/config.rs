//! Core configuration. Every value has a default; `<home>/config.toml` overrides it, and
//! `REGULUS_HOME` moves the whole home directory.

use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::secrets::{Secrets, Source};

pub const HOME_ENV: &str = "REGULUS_HOME";
const HOME_DIR: &str = ".regulus";
const CONFIG_FILE: &str = "config.toml";
const TOKEN_FILE: &str = "token";
const PORT_FILE: &str = "port";
const PROFILES_DIR: &str = "profiles";
const TEMPLATES_DIR: &str = "templates";
const DATA_DIR: &str = "data";
const STORE_DIR: &str = "store";
const SECRETS_FILE: &str = "secrets.toml";
const CORE_LOG_FILE: &str = "core.log";
const FILE_SOURCE: &str = "file";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("secrets.sources: unknown source `{0}` (known: file)")]
    UnknownSecretSource(String),
    #[error("server.host = {0} is not a loopback address; binding beyond 127.0.0.1 needs an explicit decision (docs/06-stack.md §4)")]
    NotLoopback(IpAddr),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    pub host: IpAddr,
    /// `0` picks a free port; the chosen one is written to `<home>/port` either way.
    pub port: u16,
    /// Largest request a client may send, in bytes.
    pub max_request_bytes: u32,
    pub max_connections: u32,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 7777,
            max_request_bytes: 1 << 20,
            max_connections: 64,
            unknown: toml::Table::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ProfilesConfig {
    /// Files or directories with profiles (`.json` or `.toml`). Relative paths are
    /// resolved against the home directory. Empty means `<home>/profiles`.
    pub paths: Vec<PathBuf>,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct TemplatesConfig {
    /// Files or directories with templates (`.toml`), read only when a client asks.
    /// Relative paths are resolved against the home directory. Empty means
    /// `<home>/templates`.
    pub paths: Vec<PathBuf>,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct ModulesConfig {
    /// Module ids (with or without version) that are not loaded.
    pub without: Vec<String>,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct SecretsConfig {
    /// Where secrets are looked up, in order; new ones go to the first. Today: `file`.
    pub sources: Vec<String>,
    /// The `file` source. Relative paths are resolved against the home directory.
    pub file: PathBuf,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            sources: vec![FILE_SOURCE.into()],
            file: PathBuf::from(SECRETS_FILE),
            unknown: toml::Table::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct DataConfig {
    /// Root of the directories modules keep per node (`ModuleCtx::node_dir`). Relative
    /// paths are resolved against the home directory.
    pub dir: PathBuf,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for DataConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from(DATA_DIR),
            unknown: toml::Table::new(),
        }
    }
}

/// The event bus that carries "then" wires (docs/02-architecture.md §6).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct BusConfig {
    /// Longest chain of deliveries before the core drops the value and reports a cut:
    /// the safety net against loops such as A → B → A (docs/11-composition.md §4).
    pub max_depth: u32,
    /// Most values waiting to be delivered in one execution; `max_depth` bounds how
    /// deep a chain goes, this bounds how wide (a loop with fan-out). Past it, new
    /// deliveries are reported as failed instead of queued. 0 means no limit.
    pub max_queue: usize,
    /// Bus events kept in memory so `event.list` answers recent ones without reading the
    /// store; the oldest go first. Without a store, they are all there is.
    pub history: usize,
    /// Most events one `event.list` answers, whatever `limit` asks; the client follows
    /// with `next`. 0 means no limit.
    pub max_page: usize,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for BusConfig {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_queue: 4096,
            history: 1024,
            max_page: 1000,
            unknown: toml::Table::new(),
        }
    }
}

/// Child executions (`ModuleCtx::call_workflow`): a node starts a workflow of its profile
/// and hears back its result (docs/02-architecture.md §4).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct CallConfig {
    /// How deep children may nest: a top execution is at 0, its children at 1. Past it a
    /// call is refused: the safety net against a workflow that calls itself for ever.
    pub max_nesting: u32,
    /// Most live children one execution may have at once; past it a call is refused.
    /// 0 means no limit.
    pub max_children: usize,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for CallConfig {
    fn default() -> Self {
        Self {
            max_nesting: 8,
            max_children: 32,
            unknown: toml::Table::new(),
        }
    }
}

/// Hooks (`hook.dispatch`): how long the core waits for a hook node that names no time
/// limit of its own (docs/13-blocks.md §4).
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct HooksConfig {
    /// Milliseconds a hook may take to answer; past it, it counts as no answer and the
    /// agent goes on. A hook's own `timeout_ms` overrides it.
    pub timeout_ms: u64,
    /// The longest a request may ask a hook to take, in milliseconds.
    pub max_timeout_ms: u64,
    /// Requests one hook node may have unanswered at once. Each waits on a thread of its
    /// own, which outlives a hook that hangs; past this, the node is skipped and counts as
    /// no answer. 0 means no limit.
    pub max_in_flight: usize,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            timeout_ms: 10_000,
            max_timeout_ms: 600_000,
            max_in_flight: 4,
            unknown: toml::Table::new(),
        }
    }
}

/// What happens on start to the executions that were running when the core went down
/// (docs/02-architecture.md §6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Resume {
    /// Resume those with a checkpoint; mark the rest `interrupted`.
    #[default]
    Checkpoint,
    /// Resume all of them; nodes without a checkpoint start afresh.
    Always,
    /// Mark all of them `interrupted`.
    Never,
}

/// SQLite `synchronous`: how hard a write waits for the disk. `normal` with the WAL can lose
/// the last writes on a power cut, never on a crash of the core.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Synchronous {
    Off,
    #[default]
    Normal,
    Full,
    Extra,
}

/// The SQLite store: one database for what belongs to no project and one per project.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct StoreConfig {
    /// Where the databases live. Relative paths are resolved against the home directory.
    pub dir: PathBuf,
    pub resume: Resume,
    /// Bus events kept per database; past twice this, the oldest are dropped down to it.
    /// 0 keeps them all.
    pub max_events: usize,
    pub synchronous: Synchronous,
    /// How long a write waits for another connection holding the database, in ms.
    pub busy_timeout_ms: u64,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from(STORE_DIR),
            resume: Resume::default(),
            max_events: 100_000,
            synchronous: Synchronous::default(),
            busy_timeout_ms: 5000,
            unknown: toml::Table::new(),
        }
    }
}

/// How clients (the app, the terminal UI) find a core, or start one when none answers.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    /// Start a core when none answers. Off, the client shows the command to run instead.
    pub autostart: bool,
    /// The program to start it with; empty looks next to the client, then on `PATH`.
    pub core_command: PathBuf,
    /// How long a started core has to write its port, in milliseconds.
    pub start_timeout_ms: u64,
    /// Play the opening animation. It never delays the first screen; any key skips it.
    pub intro: bool,
    /// How often a client asks the core what is running, in milliseconds.
    pub refresh_ms: u64,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            autostart: true,
            core_command: PathBuf::new(),
            start_timeout_ms: 5000,
            intro: true,
            refresh_ms: 1000,
            unknown: toml::Table::new(),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    #[serde(skip)]
    pub home: PathBuf,
    /// What `load` did not understand: unknown keys, one line each. The core says them on
    /// start and goes on (docs/11-composition.md §13).
    #[serde(skip)]
    pub warnings: Vec<String>,
    pub server: ServerConfig,
    pub profiles: ProfilesConfig,
    pub templates: TemplatesConfig,
    pub modules: ModulesConfig,
    pub secrets: SecretsConfig,
    pub data: DataConfig,
    pub bus: BusConfig,
    pub call: CallConfig,
    pub hooks: HooksConfig,
    pub store: StoreConfig,
    pub client: ClientConfig,
    /// Keys this version does not know: kept, reported as warnings, never fatal.
    #[serde(flatten)]
    pub unknown: toml::Table,
}

/// `REGULUS_HOME`, or `~/.regulus`.
pub fn default_home() -> PathBuf {
    if let Some(home) = std::env::var_os(HOME_ENV) {
        return PathBuf::from(home);
    }
    std::env::home_dir().unwrap_or_default().join(HOME_DIR)
}

impl Config {
    /// Reads `<home>/config.toml` if it exists; otherwise every default applies.
    pub fn load(home: &Path) -> Result<Self, ConfigError> {
        let path = home.join(CONFIG_FILE);
        let mut config: Config = match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).map_err(|source| ConfigError::Parse {
                path: path.clone(),
                source,
            })?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => Config::default(),
            Err(source) => return Err(ConfigError::Read { path, source }),
        };
        config.home = home.to_path_buf();
        config.warnings = config.unknown_keys(&path);
        if config.profiles.paths.is_empty() {
            config.profiles.paths.push(PathBuf::from(PROFILES_DIR));
        }
        config.profiles.paths = config.profiles.paths.iter().map(|p| home.join(p)).collect();
        if config.templates.paths.is_empty() {
            config.templates.paths.push(PathBuf::from(TEMPLATES_DIR));
        }
        config.templates.paths = config
            .templates
            .paths
            .iter()
            .map(|p| home.join(p))
            .collect();
        config.validate()?;
        Ok(config)
    }

    /// One warning per key no section knows, as `section.key`.
    fn unknown_keys(&self, path: &Path) -> Vec<String> {
        let sections: [(&str, &toml::Table); 12] = [
            ("", &self.unknown),
            ("server.", &self.server.unknown),
            ("profiles.", &self.profiles.unknown),
            ("templates.", &self.templates.unknown),
            ("modules.", &self.modules.unknown),
            ("secrets.", &self.secrets.unknown),
            ("data.", &self.data.unknown),
            ("bus.", &self.bus.unknown),
            ("call.", &self.call.unknown),
            ("hooks.", &self.hooks.unknown),
            ("store.", &self.store.unknown),
            ("client.", &self.client.unknown),
        ];
        sections
            .iter()
            .flat_map(|(at, table)| table.keys().map(move |key| format!("{at}{key}")))
            .map(|key| format!("{}: unknown key `{key}`; ignored", path.display()))
            .collect()
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        if !self.server.host.is_loopback() {
            return Err(ConfigError::NotLoopback(self.server.host));
        }
        if let Some(unknown) = self.secrets.sources.iter().find(|s| *s != FILE_SOURCE) {
            return Err(ConfigError::UnknownSecretSource(unknown.clone()));
        }
        Ok(())
    }

    /// The secret store the configured sources describe.
    pub fn secrets(&self) -> Secrets {
        let sources = self
            .secrets
            .sources
            .iter()
            .map(|_| Source::File(self.home.join(&self.secrets.file)))
            .collect();
        Secrets::new(sources)
    }

    pub fn data_dir(&self) -> PathBuf {
        self.home.join(&self.data.dir)
    }

    pub fn store_dir(&self) -> PathBuf {
        self.home.join(&self.store.dir)
    }

    pub fn token_path(&self) -> PathBuf {
        self.home.join(TOKEN_FILE)
    }

    pub fn port_path(&self) -> PathBuf {
        self.home.join(PORT_FILE)
    }

    /// Where a core started by a client writes what it prints.
    pub fn core_log_path(&self) -> PathBuf {
        self.home.join(CORE_LOG_FILE)
    }

    /// Where templates are read from and where applied ones are written: the first
    /// `[profiles] paths` entry.
    pub fn template_places(&self) -> crate::templates::Places {
        crate::templates::Places {
            templates: self.templates.paths.clone(),
            profiles_dir: self.profiles.paths.first().cloned(),
            profile_paths: self.profiles.paths.clone(),
            home: Some(self.home.clone()),
        }
    }

    pub fn default_profiles_dir(&self) -> PathBuf {
        self.home.join(PROFILES_DIR)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_keys_warn_and_the_rest_still_loads() {
        let home = std::env::temp_dir().join(format!("regulus-config-{}", std::process::id()));
        let _ = fs::remove_dir_all(&home);
        fs::create_dir_all(&home).unwrap();
        fs::write(
            home.join(CONFIG_FILE),
            "typo = 1\n[server]\nport = 0\nprot = 9\n[client]\nrefresh_ms = 50\n[later]\nx = 1\n",
        )
        .unwrap();
        let config = Config::load(&home).unwrap();
        assert_eq!(config.server.port, 0);
        assert_eq!(config.client.refresh_ms, 50);
        let all = config.warnings.join("\n");
        for key in ["`typo`", "`server.prot`", "`later`"] {
            assert!(all.contains(key), "{key} in {all}");
        }
        assert_eq!(config.warnings.len(), 3, "{all}");
        let (_, warnings) = crate::assemble(&config, Vec::new());
        assert!(
            warnings.iter().any(|w| w.contains("`server.prot`")),
            "{warnings:?}"
        );
        let _ = fs::remove_dir_all(&home);
    }
}
