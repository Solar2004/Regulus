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
const DATA_DIR: &str = "data";
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
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub host: IpAddr,
    /// `0` picks a free port; the chosen one is written to `<home>/port` either way.
    pub port: u16,
    /// Largest request a client may send, in bytes.
    pub max_request_bytes: u32,
    pub max_connections: u32,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: 7777,
            max_request_bytes: 1 << 20,
            max_connections: 64,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProfilesConfig {
    /// Files or directories with profiles (`.json` or `.toml`). Relative paths are
    /// resolved against the home directory. Empty means `<home>/profiles`.
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ModulesConfig {
    /// Module ids (with or without version) that are not loaded.
    pub without: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecretsConfig {
    /// Where secrets are looked up, in order; new ones go to the first. Today: `file`.
    pub sources: Vec<String>,
    /// The `file` source. Relative paths are resolved against the home directory.
    pub file: PathBuf,
}

impl Default for SecretsConfig {
    fn default() -> Self {
        Self {
            sources: vec![FILE_SOURCE.into()],
            file: PathBuf::from(SECRETS_FILE),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct DataConfig {
    /// Root of the directories modules keep per node (`ModuleCtx::node_dir`). Relative
    /// paths are resolved against the home directory.
    pub dir: PathBuf,
}

impl Default for DataConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from(DATA_DIR),
        }
    }
}

/// The event bus that carries "then" wires (docs/02-architecture.md §6).
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct BusConfig {
    /// Longest chain of deliveries before the core drops the value and reports a cut:
    /// the safety net against loops such as A → B → A (docs/11-composition.md §4).
    pub max_depth: u32,
    /// Most values waiting to be delivered in one execution; `max_depth` bounds how
    /// deep a chain goes, this bounds how wide (a loop with fan-out). Past it, new
    /// deliveries are reported as failed instead of queued. 0 means no limit.
    pub max_queue: usize,
    /// Bus events kept in memory for `event.list`; the oldest go first.
    pub history: usize,
}

impl Default for BusConfig {
    fn default() -> Self {
        Self {
            max_depth: 64,
            max_queue: 4096,
            history: 1024,
        }
    }
}

/// How clients (the app, the terminal UI) find a core, or start one when none answers.
#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
}

impl Default for ClientConfig {
    fn default() -> Self {
        Self {
            autostart: true,
            core_command: PathBuf::new(),
            start_timeout_ms: 5000,
            intro: true,
            refresh_ms: 1000,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(skip)]
    pub home: PathBuf,
    pub server: ServerConfig,
    pub profiles: ProfilesConfig,
    pub modules: ModulesConfig,
    pub secrets: SecretsConfig,
    pub data: DataConfig,
    pub bus: BusConfig,
    pub client: ClientConfig,
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
        if config.profiles.paths.is_empty() {
            config.profiles.paths.push(PathBuf::from(PROFILES_DIR));
        }
        config.profiles.paths = config.profiles.paths.iter().map(|p| home.join(p)).collect();
        config.validate()?;
        Ok(config)
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

    pub fn default_profiles_dir(&self) -> PathBuf {
        self.home.join(PROFILES_DIR)
    }
}
