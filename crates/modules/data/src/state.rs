//! Where state is kept: one JSON row per dataset or snapshot in the module's space of the
//! store. Without a store (a core built without one, as in some tests) rows live in the
//! process and go with it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use regulus_core::{ModuleError, ModuleStore};
use serde::de::DeserializeOwned;
use serde::Serialize;

use crate::lock;

/// Every row key starts with this, so the space can hold other things later.
const PREFIX: &str = "data";

/// What a row holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Dataset,
    Snapshot,
}

impl Kind {
    fn tag(self) -> &'static str {
        match self {
            Kind::Dataset => "set",
            Kind::Snapshot => "snap",
        }
    }

    pub(crate) fn name(self) -> &'static str {
        match self {
            Kind::Dataset => "dataset",
            Kind::Snapshot => "snapshot",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "dataset" | "set" => Some(Kind::Dataset),
            "snapshot" | "snap" => Some(Kind::Snapshot),
            _ => None,
        }
    }
}

/// Where keys and values are kept.
#[derive(Clone)]
pub(crate) enum Kv {
    Store(ModuleStore),
    Local(Arc<Mutex<BTreeMap<String, Vec<u8>>>>),
}

impl Kv {
    fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ModuleError> {
        match self {
            Self::Store(store) => store.get(key),
            Self::Local(map) => Ok(lock(map).get(key).cloned()),
        }
    }

    fn set(&self, key: &str, value: &[u8]) -> Result<(), ModuleError> {
        match self {
            Self::Store(store) => store.set(key, value),
            Self::Local(map) => {
                lock(map).insert(key.into(), value.to_vec());
                Ok(())
            }
        }
    }

    fn delete(&self, key: &str) -> Result<bool, ModuleError> {
        match self {
            Self::Store(store) => store.delete(key),
            Self::Local(map) => Ok(lock(map).remove(key).is_some()),
        }
    }

    fn list(&self, prefix: &str) -> Result<Vec<(String, Vec<u8>)>, ModuleError> {
        match self {
            Self::Store(store) => store.list(prefix),
            Self::Local(map) => Ok(lock(map)
                .range(prefix.to_string()..)
                .take_while(|(k, _)| k.starts_with(prefix))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect()),
        }
    }
}

/// A name that cannot reach into another row's key.
pub(crate) fn check_name(what: &str, name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('/') || name.chars().any(char::is_control) {
        return Err(format!(
            "`{name}` is not a {what} name: use a non-empty name without `/`"
        ));
    }
    Ok(())
}

/// One row: the dataset or snapshot `name` in `scope` (`project` or an execution id).
pub(crate) struct Row {
    kv: Kv,
    key: String,
}

impl Row {
    pub(crate) fn new(kv: Kv, kind: Kind, scope: &str, name: &str) -> Result<Self, String> {
        check_name(kind.name(), name)?;
        Ok(Self {
            kv,
            key: format!("{PREFIX}/{}/{scope}/{name}", kind.tag()),
        })
    }

    /// `None` when nothing was ever written; a row that does not read is an error, never
    /// an empty state.
    pub(crate) fn load<T: DeserializeOwned>(&self) -> Result<Option<T>, String> {
        let Some(bytes) = self.kv.get(&self.key).map_err(|e| e.to_string())? else {
            return Ok(None);
        };
        serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("the stored state of `{}` is unreadable: {e}", self.key))
    }

    pub(crate) fn save<T: Serialize>(&self, state: &T) -> Result<(), String> {
        let bytes = serde_json::to_vec(state).map_err(|e| e.to_string())?;
        self.kv.set(&self.key, &bytes).map_err(|e| e.to_string())
    }

    pub(crate) fn delete(&self) -> Result<bool, String> {
        self.kv.delete(&self.key).map_err(|e| e.to_string())
    }
}

/// A row found while listing: its kind, scope and name, and its bytes.
pub(crate) struct Found {
    pub kind: Kind,
    pub scope: String,
    pub name: String,
    pub bytes: Vec<u8>,
}

/// Every row of the space.
pub(crate) fn rows(kv: &Kv) -> Result<Vec<Found>, ModuleError> {
    Ok(kv
        .list(&format!("{PREFIX}/"))?
        .into_iter()
        .filter_map(|(key, bytes)| {
            let mut parts = key.splitn(4, '/').skip(1);
            let kind = match parts.next()? {
                "set" => Kind::Dataset,
                "snap" => Kind::Snapshot,
                _ => return None,
            };
            Some(Found {
                kind,
                scope: parts.next()?.to_string(),
                name: parts.next()?.to_string(),
                bytes,
            })
        })
        .collect())
}
