//! Core configuration. Every value has a default; `<home>/config.toml` overrides it, and
//! `REGULUS_HOME` moves the whole home directory.

use std::fs;
use std::io;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};

use serde::Deserialize;

pub const HOME_ENV: &str = "REGULUS_HOME";
const HOME_DIR: &str = ".regulus";
const CONFIG_FILE: &str = "config.toml";
const TOKEN_FILE: &str = "token";
const PORT_FILE: &str = "port";
const PROFILES_DIR: &str = "profiles";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {path}: {source}")]
    Read { path: PathBuf, source: io::Error },
    #[error("invalid {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
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

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    #[serde(skip)]
    pub home: PathBuf,
    pub server: ServerConfig,
    pub profiles: ProfilesConfig,
    pub modules: ModulesConfig,
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
        Ok(())
    }

    pub fn token_path(&self) -> PathBuf {
        self.home.join(TOKEN_FILE)
    }

    pub fn port_path(&self) -> PathBuf {
        self.home.join(PORT_FILE)
    }

    pub fn default_profiles_dir(&self) -> PathBuf {
        self.home.join(PROFILES_DIR)
    }
}
