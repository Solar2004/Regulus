//! Secrets (docs/11-composition.md §9). Workflows hold only references (`secret:<name>`);
//! the core swaps them for the values when it hands a node to its module, so no module
//! knows where secrets live and changing the store touches no module.
//!
//! Sources are an ordered list: a name is looked up in each until one has it, and new
//! secrets go to the first. Only `file` exists today; the system keyring would be one
//! more variant of [`Source`].

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use serde_json::Value;

/// What a config value starts with when it names a secret instead of holding a value.
pub const PREFIX: &str = "secret:";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("secret `{0}` is not set: run `regulus secret set {0}`")]
    Missing(String),
    #[error("`{0}` is not a valid secret name: use letters, digits, `-`, `_` and `.`")]
    InvalidName(String),
    #[error("no secret source accepts new secrets")]
    ReadOnly,
    #[error("{path}: {message}")]
    Store { path: PathBuf, message: String },
}

#[derive(Debug, Clone)]
pub enum Source {
    /// A TOML table of `name = "value"`, readable by its owner only.
    File(PathBuf),
}

#[derive(Debug, Clone, Default)]
pub struct Secrets {
    sources: Vec<Source>,
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn store_error(path: &std::path::Path, message: impl ToString) -> SecretError {
    SecretError::Store {
        path: path.to_path_buf(),
        message: message.to_string(),
    }
}

impl Source {
    fn read(&self) -> Result<BTreeMap<String, String>, SecretError> {
        let Source::File(path) = self;
        match fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).map_err(|e| store_error(path, e)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(error) => Err(store_error(path, error)),
        }
    }

    /// Replaces the whole file through a temporary one created with owner-only access,
    /// so the secrets are never readable by others, not even halfway through.
    fn write(&self, secrets: &BTreeMap<String, String>) -> Result<(), SecretError> {
        let Source::File(path) = self;
        let text = toml::to_string(secrets).map_err(|e| store_error(path, e))?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| store_error(path, e))?;
        }
        let temporary = path.with_extension("tmp");
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|e| store_error(&temporary, e))?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| store_error(&temporary, e))?;
        fs::rename(&temporary, path).map_err(|e| store_error(path, e))
    }
}

impl Secrets {
    pub fn new(sources: Vec<Source>) -> Self {
        Self { sources }
    }

    pub fn get(&self, name: &str) -> Result<String, SecretError> {
        for source in &self.sources {
            if let Some(value) = source.read()?.remove(name) {
                return Ok(value);
            }
        }
        Err(SecretError::Missing(name.into()))
    }

    /// Every secret name, sorted, never the values.
    pub fn names(&self) -> Result<Vec<String>, SecretError> {
        let mut names = Vec::new();
        for source in &self.sources {
            names.extend(source.read()?.into_keys());
        }
        names.sort();
        names.dedup();
        Ok(names)
    }

    pub fn set(&self, name: &str, value: &str) -> Result<(), SecretError> {
        if !valid_name(name) {
            return Err(SecretError::InvalidName(name.into()));
        }
        let source = self.sources.first().ok_or(SecretError::ReadOnly)?;
        let mut secrets = source.read()?;
        secrets.insert(name.into(), value.into());
        source.write(&secrets)
    }

    /// Removes a secret from every source. False if none had it.
    pub fn delete(&self, name: &str) -> Result<bool, SecretError> {
        let mut found = false;
        for source in &self.sources {
            let mut secrets = source.read()?;
            if secrets.remove(name).is_some() {
                source.write(&secrets)?;
                found = true;
            }
        }
        Ok(found)
    }

    /// Swaps every `secret:<name>` string inside `value` for the secret, at any depth.
    pub fn resolve(&self, value: &mut Value) -> Result<(), SecretError> {
        match value {
            Value::String(text) => {
                if let Some(name) = text.strip_prefix(PREFIX) {
                    *text = self.get(name.trim())?;
                }
            }
            Value::Array(items) => {
                for item in items {
                    self.resolve(item)?;
                }
            }
            Value::Object(map) => {
                for item in map.values_mut() {
                    self.resolve(item)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Secrets, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "regulus-secrets-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&dir);
        let path = dir.join("secrets.toml");
        (Secrets::new(vec![Source::File(path.clone())]), path)
    }

    #[test]
    fn stores_resolves_and_deletes() {
        let (secrets, path) = store();
        secrets.set("openrouter", "sk-1").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let mut config = serde_json::json!({
            "credential": "secret:openrouter",
            "nested": ["secret:openrouter", "plain"],
            "model": "m"
        });
        secrets.resolve(&mut config).unwrap();
        assert_eq!(config["credential"], "sk-1");
        assert_eq!(config["nested"][0], "sk-1");
        assert_eq!(config["nested"][1], "plain");
        assert_eq!(secrets.names().unwrap(), vec!["openrouter".to_string()]);
        assert!(secrets.delete("openrouter").unwrap());
        assert!(matches!(
            secrets.get("openrouter"),
            Err(SecretError::Missing(_))
        ));
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn rejects_names_that_could_escape() {
        let (secrets, _) = store();
        assert!(matches!(
            secrets.set("../x", "v"),
            Err(SecretError::InvalidName(_))
        ));
        assert!(matches!(
            secrets.set("", "v"),
            Err(SecretError::InvalidName(_))
        ));
    }
}
