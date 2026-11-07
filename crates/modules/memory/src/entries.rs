//! Entries: keyed texts in the module's space of the store, one memory per name and
//! scope, with its limits. Without a store (a core built without one, as in some tests)
//! they live in the process and go with it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use regulus_core::{ModuleError, ModuleStore};
use serde::{Deserialize, Serialize};

use crate::lock;

/// Every entry key in the module's space starts with this, so the space can hold other
/// things later.
const PREFIX: &str = "mem";

/// One memory entry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub key: String,
    pub text: String,
    /// Milliseconds since the Unix epoch of the last write.
    pub at: u64,
    /// Who wrote it: an agent's `@name` through the tools, or `node:<id>`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
}

/// Where keys and values are kept.
#[derive(Clone)]
pub(crate) enum Kv {
    Store(ModuleStore),
    Local(Arc<Mutex<BTreeMap<String, Vec<u8>>>>),
}

impl Kv {
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

/// What bounds one memory in one scope.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max_entries: usize,
    /// Keys and texts of all entries together.
    pub max_bytes: usize,
    pub max_key_chars: usize,
    /// Drop the oldest entries to make room, instead of refusing the write.
    pub evict_oldest: bool,
}

/// One memory in one scope: its entries are the keys under `prefix`.
pub(crate) struct Place {
    kv: Kv,
    prefix: String,
}

/// A memory name: plain, so it cannot reach into another memory's keys.
pub(crate) fn check_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('/') || name.chars().any(char::is_control) {
        return Err(format!(
            "`{name}` is not a memory name: use a non-empty name without `/`"
        ));
    }
    Ok(())
}

pub(crate) fn check_key(key: &str, max_chars: usize) -> Result<(), String> {
    if key.trim().is_empty() {
        return Err("an entry needs a non-empty key".into());
    }
    if key.chars().any(char::is_control) {
        return Err("a key cannot hold control characters".into());
    }
    if key.chars().count() > max_chars {
        return Err(format!(
            "the key is longer than {max_chars} characters (`max_key_chars`)"
        ));
    }
    Ok(())
}

impl Place {
    /// `scope` is `project` or an execution id; `memory` a name [`check_name`] accepts.
    pub fn new(kv: Kv, scope: &str, memory: &str) -> Result<Self, String> {
        check_name(memory)?;
        Ok(Self {
            kv,
            prefix: format!("{PREFIX}/{scope}/{memory}/"),
        })
    }

    /// Every entry, oldest first.
    pub fn entries(&self) -> Result<Vec<Entry>, ModuleError> {
        let mut found: Vec<Entry> = self
            .kv
            .list(&self.prefix)?
            .into_iter()
            .filter_map(|(_, bytes)| serde_json::from_slice(&bytes).ok())
            .collect();
        found.sort_by(|a, b| a.at.cmp(&b.at).then_with(|| a.key.cmp(&b.key)));
        Ok(found)
    }

    pub fn get(&self, key: &str) -> Result<Option<Entry>, ModuleError> {
        Ok(self.entries()?.into_iter().find(|e| e.key == key))
    }

    pub fn delete(&self, key: &str) -> Result<bool, ModuleError> {
        self.kv.delete(&format!("{}{key}", self.prefix))
    }

    /// A key no entry has: `<stem>-<ms>`, with `-<n>` added if that one is taken.
    pub fn fresh_key(&self, stem: &str, now: u64) -> Result<String, ModuleError> {
        let taken: Vec<String> = self.entries()?.into_iter().map(|e| e.key).collect();
        let base = format!("{stem}-{now}");
        let mut key = base.clone();
        let mut n = 1;
        while taken.contains(&key) {
            n += 1;
            key = format!("{base}-{n}");
        }
        Ok(key)
    }

    /// Writes `entry` over any entry with its key, within `limits`. Returns the keys it
    /// evicted to make room. The caller holds the module's write lock.
    pub fn write(&self, entry: &Entry, limits: Limits) -> Result<Vec<String>, String> {
        check_key(&entry.key, limits.max_key_chars)?;
        // An entry costs its key and its text.
        let size = entry.key.len() + entry.text.len();
        if size > limits.max_bytes {
            return Err(format!(
                "the entry has {size} bytes, more than the memory holds (`max_bytes` {})",
                limits.max_bytes
            ));
        }
        let mut others: Vec<Entry> = self
            .entries()
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|e| e.key != entry.key)
            .collect();
        let mut bytes: usize = others.iter().map(|e| e.key.len() + e.text.len()).sum();
        let mut evicted = Vec::new();
        while others.len() + 1 > limits.max_entries.max(1) || bytes + size > limits.max_bytes {
            if !limits.evict_oldest || others.is_empty() {
                return Err(format!(
                    "the memory is full ({} entries, {bytes} bytes; `max_entries` {}, \
                     `max_bytes` {}) and `evict = refuse`",
                    others.len(),
                    limits.max_entries,
                    limits.max_bytes
                ));
            }
            let oldest = others.remove(0);
            bytes -= oldest.key.len() + oldest.text.len();
            evicted.push(oldest.key);
        }
        for key in &evicted {
            self.delete(key).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec(entry).map_err(|e| e.to_string())?;
        self.kv
            .set(&format!("{}{}", self.prefix, entry.key), &bytes)
            .map_err(|e| e.to_string())?;
        Ok(evicted)
    }
}

/// Lowercase words of three letters or more.
fn words(text: &str) -> Vec<String> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .collect()
}

/// The entries that share words with `query`, most shared first, then newest first.
pub(crate) fn matching(entries: Vec<Entry>, query: &str) -> Vec<Entry> {
    let wanted = words(query);
    let mut scored: Vec<(usize, Entry)> = entries
        .into_iter()
        .filter_map(|entry| {
            let have = words(&format!("{} {}", entry.key, entry.text));
            let score = wanted.iter().filter(|w| have.contains(w)).count();
            (score > 0).then_some((score, entry))
        })
        .collect();
    scored.sort_by(|(a, x), (b, y)| b.cmp(a).then_with(|| y.at.cmp(&x.at)));
    scored.into_iter().map(|(_, e)| e).collect()
}

/// `- key: text` per entry under `header`, whole entries only, at most `max_chars`
/// characters. Nothing to show is an empty text.
pub(crate) fn render(header: &str, entries: &[Entry], max_chars: usize) -> String {
    let mut text = String::new();
    let mut used = header.chars().count();
    for entry in entries {
        let line = format!("\n- {}: {}", entry.key, entry.text.replace('\n', "\n  "));
        let size = line.chars().count();
        if used + size > max_chars {
            break;
        }
        used += size;
        text.push_str(&line);
    }
    if text.is_empty() {
        return text;
    }
    format!("{header}{text}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place() -> Place {
        Place::new(Kv::Local(Arc::default()), "project", "notes").unwrap()
    }

    fn entry(key: &str, text: &str, at: u64) -> Entry {
        Entry {
            key: key.into(),
            text: text.into(),
            at,
            by: String::new(),
        }
    }

    const LIMITS: Limits = Limits {
        max_entries: 2,
        max_bytes: 10,
        max_key_chars: 8,
        evict_oldest: true,
    };

    #[test]
    fn limits_evict_the_oldest_or_refuse() {
        let place = place();
        assert!(place
            .write(&entry("a", "1234", 1), LIMITS)
            .unwrap()
            .is_empty());
        assert!(place
            .write(&entry("b", "1234", 2), LIMITS)
            .unwrap()
            .is_empty());
        // A third entry: the oldest goes.
        assert_eq!(place.write(&entry("c", "12", 3), LIMITS).unwrap(), ["a"]);
        // Over the bytes: the oldest goes again.
        assert_eq!(
            place.write(&entry("d", "123456", 4), LIMITS).unwrap(),
            ["b"]
        );
        let keys: Vec<String> = place
            .entries()
            .unwrap()
            .into_iter()
            .map(|e| e.key)
            .collect();
        assert_eq!(keys, ["c", "d"]);
        // Rewriting a key replaces it without counting it twice.
        assert!(place
            .write(&entry("d", "123456", 5), LIMITS)
            .unwrap()
            .is_empty());
        let refuse = Limits {
            evict_oldest: false,
            ..LIMITS
        };
        assert!(place.write(&entry("e", "1", 6), refuse).is_err());
        assert!(place.write(&entry("e", "12345678901", 6), LIMITS).is_err());
        assert!(place
            .write(&entry("much-too-long", "1", 6), LIMITS)
            .is_err());
        assert!(place.write(&entry(" ", "1", 6), LIMITS).is_err());
    }

    #[test]
    fn names_cannot_reach_another_memory() {
        let kv = Kv::Local(Arc::default());
        assert!(Place::new(kv.clone(), "project", "a/b").is_err());
        assert!(Place::new(kv.clone(), "project", "").is_err());
        let a = Place::new(kv.clone(), "project", "a").unwrap();
        let ab = Place::new(kv.clone(), "project", "ab").unwrap();
        let other = Place::new(kv, "exec-1", "a").unwrap();
        a.write(&entry("x", "1", 1), LIMITS).unwrap();
        assert!(ab.entries().unwrap().is_empty());
        assert!(other.entries().unwrap().is_empty());
        assert_eq!(a.fresh_key("x", 5).unwrap(), "x-5");
        a.write(&entry("x-5", "1", 2), LIMITS).unwrap();
        assert_eq!(a.fresh_key("x", 5).unwrap(), "x-5-2");
    }

    #[test]
    fn matching_ranks_shared_words_and_render_keeps_whole_entries() {
        let entries = vec![
            entry("tests", "Run cargo test before you commit", 1),
            entry("style", "Use short names", 2),
            entry("commit", "Commit messages in English, run tests", 3),
        ];
        let found = matching(entries.clone(), "How do I commit after the tests?");
        let keys: Vec<&str> = found.iter().map(|e| e.key.as_str()).collect();
        assert_eq!(keys, ["commit", "tests"]);
        assert!(matching(entries.clone(), "an").is_empty());
        let text = render("Memory:", &entries[..2], 1000);
        assert_eq!(
            text,
            "Memory:\n- tests: Run cargo test before you commit\n- style: Use short names"
        );
        assert_eq!(
            render("Memory:", &entries, 50),
            "Memory:\n- tests: Run cargo test before you commit"
        );
        assert_eq!(render("Memory:", &[], 50), "");
    }
}
