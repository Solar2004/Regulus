//! The book: the sources and entries of one collection in one scope, in the module's space
//! of the store, within limits. Without a store (a core built without one, as in some
//! tests) they live in the process and go with it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use regulus_core::{ModuleError, ModuleStore};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::lock;
use crate::model::{
    check_entry, check_id, cut, locate, sha256, Entry, EntryCheck, Match, Past, Source, Support,
};

/// Keys in the module's space: `src/<scope>/<collection>/<id>`, `ent/…`, `seq/…`.
const SOURCES: &str = "src";
const ENTRIES: &str = "ent";
const COUNTERS: &str = "seq";
/// The longest location, in bytes.
const MAX_LOCATION: usize = 2048;
/// What the numbers of a remembered version (version, bytes, time) count for.
const PAST_FIXED_BYTES: usize = 24;

/// Where keys and values are kept.
#[derive(Clone)]
pub(crate) enum Kv {
    Store(ModuleStore),
    Local(Arc<Mutex<BTreeMap<String, Vec<u8>>>>),
}

impl Kv {
    pub(crate) fn set(&self, key: &str, value: &[u8]) -> Result<(), ModuleError> {
        match self {
            Self::Store(store) => store.set(key, value),
            Self::Local(map) => {
                lock(map).insert(key.into(), value.to_vec());
                Ok(())
            }
        }
    }

    pub(crate) fn get(&self, key: &str) -> Result<Option<Vec<u8>>, ModuleError> {
        match self {
            Self::Store(store) => store.get(key),
            Self::Local(map) => Ok(lock(map).get(key).cloned()),
        }
    }

    pub(crate) fn delete(&self, key: &str) -> Result<bool, ModuleError> {
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

/// What bounds one book. Every field is a field of the `store` node.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max_sources: usize,
    pub max_entries: usize,
    pub max_source_bytes: usize,
    pub max_total_bytes: usize,
    pub max_statement_chars: usize,
    pub max_excerpt_chars: usize,
    pub min_excerpt_chars: usize,
    pub max_supports: usize,
    pub max_versions: usize,
    pub max_title_chars: usize,
}

impl Limits {
    pub fn to_json(self) -> Value {
        serde_json::json!({
            "max_sources": self.max_sources,
            "max_entries": self.max_entries,
            "max_source_bytes": self.max_source_bytes,
            "max_total_bytes": self.max_total_bytes,
            "max_statement_chars": self.max_statement_chars,
            "max_excerpt_chars": self.max_excerpt_chars,
            "min_excerpt_chars": self.min_excerpt_chars,
            "max_supports": self.max_supports,
            "max_versions": self.max_versions,
            "max_title_chars": self.max_title_chars,
        })
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(default)]
struct Counters {
    /// Ids of forgotten sources and entries: never issued again, not even by hand, so a
    /// marker in an old report cannot come to name another claim.
    retired_sources: Vec<String>,
    retired_entries: Vec<String>,
    source: u64,
    entry: u64,
    /// Order of arrival of both, apart from the ids so ids have no gaps.
    order: u64,
}

/// A source to register.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct SourceInput {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub location: String,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub title: String,
}

/// One excerpt of an entry to register.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct SupportInput {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub excerpt: String,
}

/// An entry to register: `supports`, or the one `source` and `excerpt`.
#[derive(Debug, Default, Deserialize)]
pub(crate) struct EntryInput {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub statement: String,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub excerpt: Option<String>,
    #[serde(default)]
    pub supports: Vec<SupportInput>,
}

/// What registering a source did.
pub(crate) struct Sourced {
    pub source: Source,
    pub created: bool,
    pub changed: bool,
    pub unchanged: bool,
    /// Entries that cited an earlier text of it and no longer validate.
    pub stale_entries: Vec<String>,
}

impl Sourced {
    pub fn to_json(&self) -> Value {
        let mut value = self.source.summary();
        if let Some(object) = value.as_object_mut() {
            object.insert("created".into(), self.created.into());
            object.insert("changed".into(), self.changed.into());
            object.insert("unchanged".into(), self.unchanged.into());
            object.insert("stale_entries".into(), self.stale_entries.clone().into());
        }
        value
    }
}

/// One collection in one scope: `scope` is `project` or an execution id.
pub(crate) struct Book {
    kv: Kv,
    scope: String,
    collection: String,
}

/// A stored row, or an error naming it: a damaged row must stop the book, not vanish from it
/// (evidence lost) nor reset a counter (ids reused).
fn decode<T: serde::de::DeserializeOwned>(key: &str, bytes: &[u8]) -> Result<T, ModuleError> {
    serde_json::from_slice(bytes).map_err(|e| {
        ModuleError::Failed(format!(
            "the stored record `{key}` is unreadable ({e}): the evidence collection is damaged; \
             export what can be read or remove the row"
        ))
    })
}

/// A collection name: plain, so it cannot reach into another collection's keys.
pub(crate) fn check_collection(name: &str) -> Result<(), String> {
    if name.is_empty() || name.contains('/') || name.chars().any(char::is_control) {
        return Err(format!(
            "`{name}` is not a collection name: use a non-empty name without `/`"
        ));
    }
    Ok(())
}

impl Book {
    pub fn new(kv: Kv, scope: &str, collection: &str) -> Result<Self, String> {
        check_collection(collection)?;
        Ok(Self {
            kv,
            scope: scope.into(),
            collection: collection.into(),
        })
    }

    fn key(&self, kind: &str, id: &str) -> String {
        format!("{kind}/{}/{}/{id}", self.scope, self.collection)
    }

    fn prefix(&self, kind: &str) -> String {
        self.key(kind, "")
    }

    fn all<T: serde::de::DeserializeOwned>(&self, kind: &str) -> Result<Vec<T>, ModuleError> {
        self.kv
            .list(&self.prefix(kind))?
            .into_iter()
            .map(|(key, bytes)| decode(&key, &bytes))
            .collect()
    }

    /// Every source, in order of arrival.
    pub fn sources(&self) -> Result<Vec<Source>, ModuleError> {
        let mut found: Vec<Source> = self.all(SOURCES)?;
        found.sort_by(|a, b| a.seq.cmp(&b.seq).then_with(|| a.id.cmp(&b.id)));
        Ok(found)
    }

    /// Every entry, in order of arrival.
    pub fn entries(&self) -> Result<Vec<Entry>, ModuleError> {
        let mut found: Vec<Entry> = self.all(ENTRIES)?;
        found.sort_by(|a, b| a.seq.cmp(&b.seq).then_with(|| a.id.cmp(&b.id)));
        Ok(found)
    }

    pub fn source(&self, id: &str) -> Result<Option<Source>, ModuleError> {
        let key = self.key(SOURCES, id);
        self.kv
            .get(&key)?
            .map(|bytes| decode(&key, &bytes))
            .transpose()
    }

    pub fn entry(&self, id: &str) -> Result<Option<Entry>, ModuleError> {
        let key = self.key(ENTRIES, id);
        self.kv
            .get(&key)?
            .map(|bytes| decode(&key, &bytes))
            .transpose()
    }

    /// Every entry checked against the sources as they are now.
    pub fn checked(&self) -> Result<Vec<EntryCheck>, ModuleError> {
        let sources: BTreeMap<String, Source> = self
            .sources()?
            .into_iter()
            .map(|s| (s.id.clone(), s))
            .collect();
        Ok(self
            .entries()?
            .iter()
            .map(|entry| check_entry(entry, |id| sources.get(id).cloned()))
            .collect())
    }

    /// One entry checked against the sources as they are now.
    pub fn check(&self, id: &str) -> Result<Option<EntryCheck>, ModuleError> {
        let Some(entry) = self.entry(id)? else {
            return Ok(None);
        };
        let mut cache: BTreeMap<String, Option<Source>> = BTreeMap::new();
        let mut lookup = |source: &str| -> Result<Option<Source>, ModuleError> {
            if !cache.contains_key(source) {
                cache.insert(source.into(), self.source(source)?);
            }
            Ok(cache[source].clone())
        };
        for support in &entry.supports {
            lookup(&support.source)?;
        }
        Ok(Some(check_entry(&entry, |id| {
            cache.get(id).cloned().flatten()
        })))
    }

    /// The bytes the book holds: source texts, statements and excerpts.
    pub fn used_bytes(&self) -> Result<usize, ModuleError> {
        let sources: usize = self.sources()?.iter().map(source_bytes).sum();
        let entries: usize = self.entries()?.iter().map(entry_bytes).sum();
        Ok(sources + entries)
    }

    fn counters(&self) -> Result<Counters, ModuleError> {
        let key = self.key(COUNTERS, "");
        Ok(self
            .kv
            .get(&key)?
            .map(|bytes| decode(&key, &bytes))
            .transpose()?
            .unwrap_or_default())
    }

    fn save(&self, counters: &Counters) -> Result<(), ModuleError> {
        self.kv.set(
            &self.key(COUNTERS, ""),
            &serde_json::to_vec(counters).map_err(|e| ModuleError::Failed(e.to_string()))?,
        )
    }

    fn next(&self, which: fn(&mut Counters) -> &mut u64) -> Result<u64, ModuleError> {
        let mut counters = self.counters()?;
        let slot = which(&mut counters);
        *slot += 1;
        let n = *slot;
        self.save(&counters)?;
        Ok(n)
    }

    /// Whether `id` was issued before and forgotten.
    fn is_retired(&self, kind: &str, id: &str) -> Result<bool, String> {
        let counters = self.counters().map_err(|e| e.to_string())?;
        let list = if kind == SOURCES {
            &counters.retired_sources
        } else {
            &counters.retired_entries
        };
        Ok(list.iter().any(|r| r == id))
    }

    fn retire(&self, kind: &str, id: &str) -> Result<(), String> {
        let mut counters = self.counters().map_err(|e| e.to_string())?;
        let list = if kind == SOURCES {
            &mut counters.retired_sources
        } else {
            &mut counters.retired_entries
        };
        if !list.iter().any(|r| r == id) {
            list.push(id.into());
        }
        self.save(&counters).map_err(|e| e.to_string())
    }

    /// Registers a source, or the next text of one. The caller holds the module's write
    /// lock.
    pub fn add_source(
        &self,
        input: SourceInput,
        prefix: &str,
        limits: Limits,
        now: u64,
    ) -> Result<Sourced, String> {
        let location = input.location.trim();
        if location.is_empty() {
            return Err("a source needs a `location`: where its text came from".into());
        }
        if location.len() > MAX_LOCATION || location.chars().any(char::is_control) {
            return Err(format!(
                "the location is longer than {MAX_LOCATION} bytes or holds control characters"
            ));
        }
        if input.text.trim().is_empty() {
            return Err(format!("the source `{location}` has no text"));
        }
        if input.text.len() > limits.max_source_bytes {
            return Err(format!(
                "the text of `{location}` has {} bytes, more than a source holds \
                 (`max_source_bytes` {})",
                input.text.len(),
                limits.max_source_bytes
            ));
        }
        let title = input.title.trim().to_string();
        if title.chars().count() > limits.max_title_chars {
            return Err(format!(
                "the title is longer than {} characters (`max_title_chars`)",
                limits.max_title_chars
            ));
        }
        if title.chars().any(char::is_control) {
            return Err("the title holds control characters".into());
        }
        let sources = self.sources().map_err(|e| e.to_string())?;
        let given = input.id.filter(|id| !id.trim().is_empty());
        if let Some(id) = &given {
            check_id(id)?;
        }
        let hash = sha256(&input.text);
        let existing = match &given {
            Some(id) => sources.iter().find(|s| s.id == *id),
            None => sources.iter().find(|s| s.location == location),
        };
        // An id keeps the location it was registered with: a citation made against `S1`
        // must not come to point at another place. To move it, forget it and cite again.
        if let Some(old) = existing.filter(|old| old.location != location) {
            return Err(format!(
                "the source `{}` is registered at `{}` and cannot be moved to `{location}`: \
                 forget it first (its entries will read `missing_source`)",
                old.id, old.location
            ));
        }
        if let (None, Some(id)) = (existing, &given) {
            if self.is_retired(SOURCES, id)? {
                return Err(format!(
                    "the id `{id}` was used by a source that was forgotten; ids are never reused"
                ));
            }
        }
        let other_bytes: usize = sources
            .iter()
            .filter(|s| existing.is_none_or(|e| e.id != s.id))
            .map(source_bytes)
            .sum::<usize>()
            + self
                .entries()
                .map_err(|e| e.to_string())?
                .iter()
                .map(entry_bytes)
                .sum::<usize>();
        if existing.is_none() && sources.len() + 1 > limits.max_sources {
            return Err(format!(
                "the book already has {} sources (`max_sources` {}): forget one first",
                sources.len(),
                limits.max_sources
            ));
        }
        let incoming = input.text.len() + location.len() + title.len();
        if other_bytes + incoming > limits.max_total_bytes {
            return Err(format!(
                "the book would hold {} bytes (`max_total_bytes` {}): forget something first",
                other_bytes + incoming,
                limits.max_total_bytes
            ));
        }
        // The counter moves only now that nothing can refuse the write any more.
        let id = match (existing, given) {
            (Some(old), _) => old.id.clone(),
            (None, Some(id)) => id,
            (None, None) => self.fresh_id(SOURCES, prefix, &sources)?,
        };
        let record = match existing {
            Some(old) if old.sha256 == hash => Source {
                location: location.into(),
                title: if title.is_empty() {
                    old.title.clone()
                } else {
                    title
                },
                ..old.clone()
            },
            Some(old) => {
                let mut history = old.history.clone();
                history.push(Past {
                    version: old.version,
                    sha256: old.sha256.clone(),
                    bytes: old.bytes,
                    location: old.location.clone(),
                    at: old.at,
                });
                let extra = history.len().saturating_sub(limits.max_versions);
                history.drain(..extra);
                Source {
                    id: id.clone(),
                    location: location.into(),
                    title: if title.is_empty() {
                        old.title.clone()
                    } else {
                        title
                    },
                    version: old.version + 1,
                    sha256: hash.clone(),
                    bytes: input.text.len(),
                    at: now,
                    history,
                    seq: old.seq,
                    text: input.text,
                }
            }
            None => Source {
                id: id.clone(),
                location: location.into(),
                title,
                version: 1,
                sha256: hash.clone(),
                bytes: input.text.len(),
                at: now,
                history: Vec::new(),
                seq: self.next(|c| &mut c.order).map_err(|e| e.to_string())?,
                text: input.text,
            },
        };
        let unchanged = existing.is_some_and(|old| old.sha256 == hash);
        let stale_entries = if existing.is_some() && !unchanged {
            self.entries()
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|e| {
                    e.supports
                        .iter()
                        .any(|s| s.source == id && s.sha256 != record.sha256)
                })
                .map(|e| e.id)
                .collect()
        } else {
            Vec::new()
        };
        self.put(SOURCES, &id, &record)?;
        Ok(Sourced {
            created: existing.is_none(),
            changed: existing.is_some() && !unchanged,
            unchanged,
            source: record,
            stale_entries,
        })
    }

    /// Registers an entry: every reference is looked up and pinned. With `strict = false` a
    /// reference that does not validate is recorded as it is (it will read `missing_source`
    /// or `stale`/`excerpt_not_found`) instead of refusing the entry. The caller holds the
    /// module's write lock.
    #[allow(clippy::too_many_arguments)]
    pub fn add_entry(
        &self,
        input: EntryInput,
        mode: Match,
        strict: bool,
        prefix: &str,
        by: &str,
        limits: Limits,
        now: u64,
    ) -> Result<Entry, String> {
        let statement = input.statement.trim();
        if statement.is_empty() {
            return Err("an entry needs a `statement`: the claim the excerpts back".into());
        }
        if statement.chars().count() > limits.max_statement_chars {
            return Err(format!(
                "the statement is longer than {} characters (`max_statement_chars`)",
                limits.max_statement_chars
            ));
        }
        let mut wanted = input.supports;
        if let (Some(source), Some(excerpt)) = (input.source, input.excerpt) {
            wanted.insert(0, SupportInput { source, excerpt });
        }
        if wanted.is_empty() {
            return Err("an entry needs references: `source` and `excerpt`, or `supports`".into());
        }
        if wanted.len() > limits.max_supports {
            return Err(format!(
                "{} references; an entry holds at most {} (`max_supports`)",
                wanted.len(),
                limits.max_supports
            ));
        }
        let mut supports = Vec::with_capacity(wanted.len());
        for want in wanted {
            let excerpt_chars = want.excerpt.trim().chars().count();
            if excerpt_chars == 0 {
                return Err(format!("an empty excerpt of `{}`", want.source));
            }
            if excerpt_chars < limits.min_excerpt_chars {
                return Err(format!(
                    "the excerpt of `{}` has {excerpt_chars} characters, fewer than \
                     `min_excerpt_chars` {}: quote the passage",
                    want.source, limits.min_excerpt_chars
                ));
            }
            if want.excerpt.chars().count() > limits.max_excerpt_chars {
                return Err(format!(
                    "the excerpt of `{}` is longer than {} characters (`max_excerpt_chars`)",
                    want.source, limits.max_excerpt_chars
                ));
            }
            match self.pin(&want, mode)? {
                Ok(support) => supports.push(support),
                Err(reason) if strict => return Err(reason),
                Err(_) => supports.push(unpinned(&want, self.source(&want.source).ok().flatten())),
            }
        }
        let chosen = input.id.filter(|id| !id.trim().is_empty());
        if let Some(id) = &chosen {
            check_id(id)?;
        }
        let existing = match &chosen {
            Some(id) => self.entry(id).map_err(|e| e.to_string())?,
            None => None,
        };
        if let (None, Some(id)) = (&existing, &chosen) {
            if self.is_retired(ENTRIES, id)? {
                return Err(format!(
                    "the id `{id}` was used by an entry that was forgotten; ids are never reused"
                ));
            }
        }
        let entries = self.entries().map_err(|e| e.to_string())?;
        if existing.is_none() && entries.len() + 1 > limits.max_entries {
            return Err(format!(
                "the book already has {} entries (`max_entries` {}): forget one first",
                entries.len(),
                limits.max_entries
            ));
        }
        let draft_bytes = statement.len()
            + supports
                .iter()
                .map(|s| s.excerpt.len() + s.cited.len())
                .sum::<usize>();
        let others: usize = entries
            .iter()
            .filter(|e| chosen.as_deref() != Some(e.id.as_str()))
            .map(entry_bytes)
            .sum::<usize>()
            + self
                .sources()
                .map_err(|e| e.to_string())?
                .iter()
                .map(source_bytes)
                .sum::<usize>();
        if others + draft_bytes > limits.max_total_bytes {
            return Err(format!(
                "the book would hold {} bytes (`max_total_bytes` {}): forget something first",
                others + draft_bytes,
                limits.max_total_bytes
            ));
        }
        // The counters move only now that nothing can refuse the write any more.
        let id = match chosen {
            Some(id) => id,
            None => self.fresh_id(ENTRIES, prefix, &entries)?,
        };
        let entry = Entry {
            id: id.clone(),
            statement: statement.into(),
            supports,
            at: now,
            by: by.into(),
            seq: match &existing {
                Some(old) => old.seq,
                None => self.next(|c| &mut c.order).map_err(|e| e.to_string())?,
            },
        };
        self.put(ENTRIES, &id, &entry)?;
        Ok(entry)
    }

    /// Looks one reference up and pins it. The outer error is a failure of the store; the
    /// inner one says why the reference does not validate.
    fn pin(&self, want: &SupportInput, mode: Match) -> Result<Result<Support, String>, String> {
        check_id(&want.source)?;
        let Some(source) = self.source(&want.source).map_err(|e| e.to_string())? else {
            let known: Vec<String> = self
                .sources()
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|s| s.id)
                .collect();
            return Ok(Err(format!(
                "there is no source `{}` (sources: {})",
                want.source,
                if known.is_empty() {
                    "none".into()
                } else {
                    known.join(", ")
                }
            )));
        };
        let Some((start, end)) = locate(&source.text, &want.excerpt, mode) else {
            return Ok(Err(format!(
                "the excerpt `{}` is not in `{}` (version {}, {}): quote it exactly",
                cut(&want.excerpt, 80),
                source.id,
                source.version,
                match mode {
                    Match::Exact => "exact match",
                    Match::Whitespace => "whitespace-insensitive match",
                }
            )));
        };
        let slice = crate::model::char_slice(&source.text, start, end).to_string();
        Ok(Ok(Support {
            source: source.id,
            version: source.version,
            sha256: source.sha256,
            location: source.location,
            cited: if slice == want.excerpt {
                String::new()
            } else {
                want.excerpt.clone()
            },
            excerpt: slice,
            start,
            end,
        }))
    }

    /// Removes a source or an entry by id. Returns what it was and the entries left
    /// citing a removed source. The caller holds the module's write lock.
    pub fn forget(&self, id: &str) -> Result<(&'static str, Vec<String>), String> {
        check_id(id)?;
        if self
            .kv
            .delete(&self.key(SOURCES, id))
            .map_err(|e| e.to_string())?
        {
            self.retire(SOURCES, id)?;
            let affected = self
                .entries()
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|e| e.supports.iter().any(|s| s.source == id))
                .map(|e| e.id)
                .collect();
            return Ok(("source", affected));
        }
        if self
            .kv
            .delete(&self.key(ENTRIES, id))
            .map_err(|e| e.to_string())?
        {
            self.retire(ENTRIES, id)?;
            return Ok(("entry", Vec::new()));
        }
        Err(format!("there is no source or entry `{id}`"))
    }

    fn put<T: Serialize>(&self, kind: &str, id: &str, record: &T) -> Result<(), String> {
        let bytes = serde_json::to_vec(record).map_err(|e| e.to_string())?;
        self.kv
            .set(&self.key(kind, id), &bytes)
            .map_err(|e| e.to_string())
    }

    /// `<prefix><n>` with the next n of the book, skipping ids a caller chose by hand.
    fn fresh_id<T: HasId>(&self, kind: &str, prefix: &str, taken: &[T]) -> Result<String, String> {
        loop {
            let n = if kind == SOURCES {
                self.next(|c| &mut c.source)
            } else {
                self.next(|c| &mut c.entry)
            }
            .map_err(|e| e.to_string())?;
            let id = format!("{prefix}{n}");
            check_id(&id)?;
            if !taken.iter().any(|t| t.id() == id) && !self.is_retired(kind, &id)? {
                return Ok(id);
            }
        }
    }
}

trait HasId {
    fn id(&self) -> &str;
}

impl HasId for Source {
    fn id(&self) -> &str {
        &self.id
    }
}

impl HasId for Entry {
    fn id(&self) -> &str {
        &self.id
    }
}

/// A reference that did not validate, recorded as it was cited (`strict = no`).
fn unpinned(want: &SupportInput, source: Option<Source>) -> Support {
    let (version, hash, location) = source
        .map(|s| (s.version, s.sha256, s.location))
        .unwrap_or((0, String::new(), String::new()));
    Support {
        source: want.source.clone(),
        version,
        sha256: hash,
        excerpt: want.excerpt.clone(),
        location,
        cited: String::new(),
        start: 0,
        end: want.excerpt.chars().count(),
    }
}

/// What a source costs against `max_total_bytes`: text + title + location + for each
/// remembered version its location, hash and 24 bytes for its numbers.
fn source_bytes(source: &Source) -> usize {
    source.text.len()
        + source.title.len()
        + source.location.len()
        + source
            .history
            .iter()
            .map(|p| p.location.len() + p.sha256.len() + PAST_FIXED_BYTES)
            .sum::<usize>()
}

/// What an entry costs against `max_total_bytes`.
fn entry_bytes(entry: &Entry) -> usize {
    entry.statement.len()
        + entry
            .supports
            .iter()
            .map(|s| s.excerpt.len() + s.cited.len())
            .sum::<usize>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_damaged_row_is_an_error_not_a_missing_record_or_a_reset_counter() {
        let kv = Kv::Local(Arc::default());
        let book = Book::new(kv.clone(), "project", "c").unwrap();
        kv.set("src/project/c/S1", b"{not json").unwrap();
        for err in [book.sources().map(|_| ()), book.source("S1").map(|_| ())] {
            let message = err.unwrap_err().to_string();
            assert!(
                message.contains("src/project/c/S1") && message.contains("damaged"),
                "{message}"
            );
        }
        kv.set("seq/project/c/", b"garbage").unwrap();
        assert!(
            book.next(|c| &mut c.entry).is_err(),
            "ids must not restart from 1"
        );
        // Another collection is untouched by it.
        let other = Book::new(kv, "project", "d").unwrap();
        assert!(other.sources().unwrap().is_empty());
        assert_eq!(other.next(|c| &mut c.entry).unwrap(), 1);
    }
}
