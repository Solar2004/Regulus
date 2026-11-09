//! Sources, documents and chunks of one collection in the module's space of the store.
//! Without a store (a core built without one, as in some tests) they live in the process.
//!
//! Keys, under `doc/<collection>/`: `src/<source_id>` the source with its current
//! document, `chunk/<chunk_id>` each chunk, and `lex/…` the built-in index
//! ([`crate::lexical`]).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use regulus_core::{ModuleError, ModuleStore};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lock;

/// Every key of the module's space starts with this.
const PREFIX: &str = "doc";

/// Where keys and values are kept.
#[derive(Clone)]
pub(crate) enum Kv {
    Store(ModuleStore),
    Local(Arc<Mutex<BTreeMap<String, Vec<u8>>>>),
}

impl Kv {
    pub fn get<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>, ModuleError> {
        let bytes = match self {
            Self::Store(store) => store.get(key)?,
            Self::Local(map) => lock(map).get(key).cloned(),
        };
        bytes
            .map(|b| serde_json::from_slice(&b))
            .transpose()
            .map_err(|e| ModuleError::Failed(format!("`{key}` is not what was stored: {e}")))
    }

    pub fn set<T: Serialize>(&self, key: &str, value: &T) -> Result<(), ModuleError> {
        let bytes = serde_json::to_vec(value).map_err(|e| ModuleError::Failed(e.to_string()))?;
        match self {
            Self::Store(store) => store.set(key, &bytes),
            Self::Local(map) => {
                lock(map).insert(key.into(), bytes);
                Ok(())
            }
        }
    }

    pub fn delete(&self, key: &str) -> Result<bool, ModuleError> {
        match self {
            Self::Store(store) => store.delete(key),
            Self::Local(map) => Ok(lock(map).remove(key).is_some()),
        }
    }

    pub fn list<T: DeserializeOwned>(&self, prefix: &str) -> Result<Vec<(String, T)>, ModuleError> {
        let rows = match self {
            Self::Store(store) => store.list(prefix)?,
            Self::Local(map) => lock(map)
                .range(prefix.to_string()..)
                .take_while(|(k, _)| k.starts_with(prefix))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        };
        Ok(rows
            .into_iter()
            .filter_map(|(k, v)| Some((k, serde_json::from_slice(&v).ok()?)))
            .collect())
    }
}

/// A source with its current document.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub source_id: String,
    /// `file` or `text`.
    pub kind: String,
    /// The file's path, or `text:<title>`.
    pub location: String,
    /// Hash (blake2) of the bytes, in hex.
    pub version: String,
    #[serde(default)]
    pub metadata: Value,
    pub doc_id: String,
    pub title: String,
    pub chars: usize,
    pub chunks: usize,
    /// Milliseconds since the Unix epoch of this version's ingestion.
    pub at: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    pub chunk_id: String,
    pub doc_id: String,
    pub source_id: String,
    pub index: usize,
    pub start: usize,
    pub end: usize,
    pub text: String,
    #[serde(default)]
    pub metadata: Value,
}

/// A collection name: plain, so it cannot reach into another collection's keys.
pub(crate) fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('/') || name.chars().any(char::is_control) {
        return Err(format!(
            "`{name}` is not a collection name: use a non-empty name without `/`"
        ));
    }
    Ok(())
}

/// One collection.
#[derive(Clone)]
pub(crate) struct Collection {
    pub kv: Kv,
    pub name: String,
    prefix: String,
}

impl Collection {
    pub fn new(kv: Kv, name: &str) -> Result<Self, String> {
        check_name(name)?;
        Ok(Self {
            kv,
            name: name.into(),
            prefix: format!("{PREFIX}/{name}/"),
        })
    }

    /// The key of `rest` in this collection.
    pub fn key(&self, rest: &str) -> String {
        format!("{}{rest}", self.prefix)
    }

    pub fn source(&self, source_id: &str) -> Result<Option<Source>, ModuleError> {
        self.kv.get(&self.key(&format!("src/{source_id}")))
    }

    /// Every source, by id.
    pub fn sources(&self) -> Result<Vec<Source>, ModuleError> {
        Ok(self
            .kv
            .list(&self.key("src/"))?
            .into_iter()
            .map(|(_, s)| s)
            .collect())
    }

    pub fn put_source(&self, source: &Source) -> Result<(), ModuleError> {
        self.kv
            .set(&self.key(&format!("src/{}", source.source_id)), source)
    }

    pub fn delete_source(&self, source_id: &str) -> Result<bool, ModuleError> {
        self.kv.delete(&self.key(&format!("src/{source_id}")))
    }

    pub fn chunk(&self, chunk_id: &str) -> Result<Option<Chunk>, ModuleError> {
        self.kv.get(&self.key(&format!("chunk/{chunk_id}")))
    }

    pub fn put_chunk(&self, chunk: &Chunk) -> Result<(), ModuleError> {
        self.kv
            .set(&self.key(&format!("chunk/{}", chunk.chunk_id)), chunk)
    }

    /// Drops the chunks of `doc_id`; how many there were.
    pub fn delete_chunks(&self, doc_id: &str) -> Result<usize, ModuleError> {
        let keys: Vec<(String, Value)> = self.kv.list(&self.key(&format!("chunk/{doc_id}:")))?;
        for (key, _) in &keys {
            self.kv.delete(key)?;
        }
        Ok(keys.len())
    }
}
