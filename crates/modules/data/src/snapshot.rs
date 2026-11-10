//! `snapshot`: the state of a source compared with the last one stored, by key
//! (docs/16-experiments.md §2.2). Deterministic: the same two states always give the same
//! added, changed and removed, in the order the records came.

use std::collections::HashMap;

use regulus_contract_tool as tool;
use regulus_core::ModuleError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::json::{at, canonical, cut, digest, key_of};
use crate::state::{Kind, Row};
use crate::{now_ms, Here, Limits, Rejected, REJECTED_LISTED};

const REPORT: &str = "report";
const NOVEL: &str = "novel";
const REMOVED: &str = "removed";
const SAME: &str = "same";
const ERROR: &str = "error";

const TOOL_STATUS: &str = "snapshot_status";
const TOOL_READ: &str = "snapshot_read";
/// Runs kept in the history when a node sets 0.
const DEFAULT_HISTORY: usize = 20;

/// The row of a snapshot: the records of the last state, in the order they came.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Snapshot {
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    at: u64,
    #[serde(default)]
    items: Vec<Entry>,
    #[serde(default)]
    history: Vec<Run>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    key: String,
    /// Hash of what `compare_paths` selects of the record.
    hash: String,
    /// The key and the record as compact JSON.
    bytes: usize,
    record: Value,
}

/// What one comparison found, kept for `data.inspect`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Run {
    seq: u64,
    at: u64,
    added: usize,
    changed: usize,
    removed: usize,
    unchanged: usize,
}

impl Snapshot {
    fn bytes(&self) -> usize {
        self.items.iter().map(|e| e.bytes).sum()
    }
}

struct Settings {
    key_path: String,
    key: String,
    compare: Vec<String>,
    keep_last: bool,
    silent: bool,
    /// A rejected record keeps the baseline entries the input did not repeat.
    keep_baseline: bool,
    /// A source that sends nothing is an error, not an emptied baseline.
    refuse_empty: bool,
    limits: Limits,
    history: usize,
}

impl Settings {
    fn of(here: &Here) -> Self {
        Self {
            key_path: here.text("key_path"),
            key: here.text("key"),
            compare: here
                .text("compare_paths")
                .split(',')
                .map(str::trim)
                .filter(|p| !p.is_empty())
                .map(String::from)
                .collect(),
            keep_last: here.text("on_duplicate") == "keep_last",
            silent: here.text("baseline") == "silent",
            keep_baseline: here.text("on_rejected") != "replace",
            refuse_empty: here.text("on_empty") != "accept",
            limits: here.limits(),
            history: here.bound("history", DEFAULT_HISTORY),
        }
    }
}

/// The comparison of an input with the stored state.
struct Diff {
    /// The new state.
    kept: Vec<Entry>,
    added: Vec<Entry>,
    changed: Vec<(Entry, Value)>,
    removed: Vec<Entry>,
    unchanged: usize,
    /// Baseline entries kept because a rejected record could have been theirs.
    carried: usize,
    duplicates: usize,
    received: usize,
    rejected: Vec<Rejected>,
}

fn entry(record: Value, key: String, settings: &Settings) -> Entry {
    let seen = if settings.compare.is_empty() {
        digest(&record)
    } else {
        let picked: Vec<Value> = settings
            .compare
            .iter()
            .map(|p| at(&record, p).cloned().unwrap_or(Value::Null))
            .collect();
        digest(&Value::Array(picked))
    };
    Entry {
        bytes: key.len() + canonical(&record).len(),
        key,
        hash: seen,
        record,
    }
}

/// The records of an input: an array is the state; anything else is a state of one.
fn records(value: &Value) -> Vec<Value> {
    match value {
        Value::Array(list) => list.clone(),
        other => vec![other.clone()],
    }
}

/// Compares `incoming` with `previous`. An input that cannot become the baseline is an
/// `Err` and changes nothing.
fn diff(
    previous: Option<&Snapshot>,
    incoming: Vec<Value>,
    settings: &Settings,
) -> Result<Diff, String> {
    let mut out = Diff {
        kept: vec![],
        added: vec![],
        changed: vec![],
        removed: vec![],
        unchanged: 0,
        carried: 0,
        duplicates: 0,
        received: incoming.len(),
        rejected: vec![],
    };
    if incoming.is_empty() && settings.refuse_empty {
        return Err(
            "the source sent no records; `on_empty = refuse` keeps the baseline (set \
                    `on_empty = accept` if an empty source is a real state)"
                .into(),
        );
    }
    let mut place: HashMap<String, usize> = HashMap::new();
    for record in incoming {
        if record.is_null() {
            out.rejected.push(Rejected {
                line: None,
                key: None,
                reason: "null is not a record".into(),
            });
            continue;
        }
        let key = match key_of(&record, &settings.key_path, &settings.key) {
            Ok(key) => key,
            Err(reason) => {
                out.rejected.push(Rejected {
                    line: None,
                    key: None,
                    reason,
                });
                continue;
            }
        };
        let size = canonical(&record).len();
        if size > settings.limits.max_record_bytes {
            out.rejected.push(Rejected {
                line: None,
                key: Some(key),
                reason: format!(
                    "the record has {size} bytes, more than `max_record_bytes` {}",
                    settings.limits.max_record_bytes
                ),
            });
            continue;
        }
        let entry = entry(record, key, settings);
        match place.get(&entry.key) {
            Some(&at) => {
                out.duplicates += 1;
                if settings.keep_last {
                    out.kept[at] = entry;
                }
            }
            None => {
                place.insert(entry.key.clone(), out.kept.len());
                out.kept.push(entry);
            }
        }
    }
    if out.kept.is_empty() && !out.rejected.is_empty() {
        let first = &out.rejected[0].reason;
        return Err(format!(
            "none of the {} records could be keyed (first: {first}); the baseline stays",
            out.received
        ));
    }
    let before: HashMap<&str, &Entry> = previous
        .map(|p| p.items.iter().map(|e| (e.key.as_str(), e)).collect())
        .unwrap_or_default();
    for now in &out.kept {
        match before.get(now.key.as_str()) {
            None => out.added.push(now.clone()),
            Some(old) if old.hash != now.hash => {
                out.changed.push((now.clone(), old.record.clone()));
            }
            Some(_) => out.unchanged += 1,
        }
    }
    if let Some(previous) = previous {
        let gone = previous
            .items
            .iter()
            .filter(|e| !place.contains_key(&e.key));
        if settings.keep_baseline && !out.rejected.is_empty() {
            // A record that could not be keyed may have been one of these: nothing is
            // called removed (nor, later, new again) on a partly unreadable input.
            let carried: Vec<Entry> = gone.cloned().collect();
            out.carried = carried.len();
            out.kept.extend(carried);
        } else {
            out.removed = gone.cloned().collect();
        }
    }
    let bytes: usize = out.kept.iter().map(|e| e.bytes).sum();
    if out.kept.len() > settings.limits.max_items || bytes > settings.limits.max_bytes {
        return Err(format!(
            "the state has {} records and {bytes} bytes, over `max_items` {} or `max_bytes` {}; \
             the baseline stays",
            out.kept.len(),
            settings.limits.max_items,
            settings.limits.max_bytes
        ));
    }
    Ok(out)
}

fn pair(entry: &Entry) -> Value {
    json!({ "key": entry.key, "record": entry.record })
}

/// A value on `in`: compare, store as the new baseline and send what was found.
pub(crate) fn compare(here: &Here, value: &Value) -> Result<Value, ModuleError> {
    let row = here.row(Kind::Snapshot).map_err(|e| here.fail(e))?;
    let settings = Settings::of(here);
    let (seq, previous_seq, first, found) = here
        .writing(|| -> Result<_, String> {
            let previous: Option<Snapshot> = row.load()?;
            let found = diff(previous.as_ref(), records(value), &settings)?;
            let previous_seq = previous.as_ref().map(|p| p.seq);
            let mut next = previous.unwrap_or_default();
            next.seq += 1;
            next.at = now_ms();
            next.history.push(Run {
                seq: next.seq,
                at: next.at,
                added: found.added.len(),
                changed: found.changed.len(),
                removed: found.removed.len(),
                unchanged: found.unchanged,
            });
            let extra = next.history.len().saturating_sub(settings.history);
            next.history.drain(..extra);
            next.items = found.kept.clone();
            row.save(&next)?;
            Ok((next.seq, previous_seq, previous_seq.is_none(), found))
        })
        .map_err(|e| here.fail(e))?;

    let baselined = first && settings.silent;
    let report = json!({
        "snapshot": here.text("snapshot"), "scope": here.scope(), "seq": seq,
        "previous_seq": previous_seq, "first": first, "baselined": baselined,
        "total": found.kept.len(),
        "counts": { "added": found.added.len(), "changed": found.changed.len(),
                    "removed": found.removed.len(), "unchanged": found.unchanged,
                    "duplicates": found.duplicates, "rejected": found.rejected.len(),
                    "carried": found.carried },
        "added": found.added.iter().map(pair).collect::<Vec<_>>(),
        "changed": found.changed.iter()
            .map(|(e, before)| json!({ "key": e.key, "record": e.record, "before": before }))
            .collect::<Vec<_>>(),
        "removed": found.removed.iter().map(pair).collect::<Vec<_>>(),
        "rejected": found.rejected.iter().take(REJECTED_LISTED).collect::<Vec<_>>(),
    });
    here.emit(REPORT, report.clone())?;
    if let Some(first_rejected) = found.rejected.first() {
        here.emit(
            ERROR,
            json!(format!(
                "snapshot `{}`: {} of {} records rejected; first: {}",
                here.text("snapshot"),
                found.rejected.len(),
                found.received,
                cut(&first_rejected.reason, 200)
            )),
        )?;
    }
    if !baselined {
        let mut novel: Vec<Value> = Vec::new();
        for e in &found.added {
            novel.push(json!({ "change": "added", "key": e.key, "record": e.record }));
        }
        for (e, before) in &found.changed {
            novel.push(
                json!({ "change": "changed", "key": e.key, "record": e.record,
                               "before": before }),
            );
        }
        // In the order the records came, whatever kind of news they are.
        let order: HashMap<&str, usize> = found
            .kept
            .iter()
            .enumerate()
            .map(|(n, e)| (e.key.as_str(), n))
            .collect();
        novel.sort_by_key(|n| order.get(n["key"].as_str().unwrap_or_default()).copied());
        if !novel.is_empty() {
            here.emit(NOVEL, Value::Array(novel))?;
        }
        if !found.removed.is_empty() {
            here.emit(
                REMOVED,
                Value::Array(found.removed.iter().map(pair).collect()),
            )?;
        }
        if found.added.is_empty() && found.changed.is_empty() && found.removed.is_empty() {
            here.emit(SAME, report.clone())?;
        }
    }
    Ok(report["counts"].clone())
}

pub(crate) fn tool_specs(here: &Here) -> Vec<tool::Spec> {
    let name = here.text("snapshot");
    vec![
        tool::Spec {
            name: TOOL_STATUS.into(),
            description: format!(
                "When the snapshot `{name}` was last taken, how many records it holds and \
                 what its recent comparisons found."
            ),
            input: json!({ "type": "object", "properties": {} }),
        },
        tool::Spec {
            name: TOOL_READ.into(),
            description: format!(
                "Read the records of the snapshot `{name}`: the one with `key`, or the first ones."
            ),
            input: json!({ "type": "object", "properties": {
                "key": { "type": "string", "description": "The record's key." },
                "limit": { "type": "integer", "description": "How many records." } } }),
        },
    ]
}

pub(crate) fn run_tool(here: &Here, request: &tool::Call) -> Result<Value, String> {
    let row = here.row(Kind::Snapshot)?;
    let name = here.text("snapshot");
    let snap: Snapshot = row
        .load()?
        .ok_or_else(|| format!("snapshot `{name}` has not been taken yet"))?;
    match request.name.as_str() {
        TOOL_STATUS => Ok(
            json!({ "snapshot": name, "scope": here.scope(), "seq": snap.seq,
                                  "at": snap.at, "items": snap.items.len(),
                                  "bytes": snap.bytes(), "history": snap.history }),
        ),
        TOOL_READ => {
            if let Some(key) = request.args.get("key").and_then(Value::as_str) {
                return snap
                    .items
                    .iter()
                    .find(|e| e.key == key)
                    .map(pair)
                    .ok_or_else(|| format!("no record `{key}`"));
            }
            let max = here.bound("max_results", 20);
            let limit = request
                .args
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(max, |n| (n as usize).clamp(1, max));
            Ok(json!(snap
                .items
                .iter()
                .take(limit)
                .map(pair)
                .collect::<Vec<_>>()))
        }
        other => Err(format!("no tool `{other}`")),
    }
}

/// For `data.list`: what a snapshot row holds, without its records.
pub(crate) fn summary(bytes: &[u8]) -> Value {
    match serde_json::from_slice::<Snapshot>(bytes) {
        Ok(s) => json!({ "items": s.items.len(), "bytes": s.bytes(), "seq": s.seq, "at": s.at,
                         "runs": s.history.len() }),
        Err(e) => json!({ "error": format!("unreadable: {e}") }),
    }
}

/// For `data.inspect` and `data.export`: the snapshot with its first `limit` records.
pub(crate) fn show(row: &Row, name: &str, scope: &str, limit: usize) -> Result<Value, String> {
    let snap: Snapshot = row
        .load()?
        .ok_or_else(|| format!("no snapshot `{name}` in `{scope}`: none was taken"))?;
    let shown: Vec<Value> = snap
        .items
        .iter()
        .take(limit)
        .map(|e| json!({ "key": e.key, "hash": e.hash, "record": e.record }))
        .collect();
    Ok(
        json!({ "kind": "snapshot", "name": name, "scope": scope, "seq": snap.seq,
               "at": snap.at, "items": snap.items.len(), "bytes": snap.bytes(),
               "history": snap.history, "truncated": shown.len() < snap.items.len(),
               "records": shown }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            key_path: "/url".into(),
            key: String::new(),
            compare: vec!["/title".into(), "/text".into()],
            keep_last: false,
            silent: false,
            keep_baseline: true,
            refuse_empty: true,
            limits: Limits {
                max_items: 10,
                max_bytes: 1 << 20,
                max_record_bytes: 1 << 10,
            },
            history: 5,
        }
    }

    fn state(records: Value) -> Snapshot {
        let found = diff(None, records.as_array().unwrap().clone(), &settings()).unwrap();
        Snapshot {
            seq: 1,
            at: 0,
            items: found.kept,
            history: vec![],
        }
    }

    fn keys(entries: &[Entry]) -> Vec<&str> {
        entries.iter().map(|e| e.key.as_str()).collect()
    }

    #[test]
    fn the_first_state_is_all_added() {
        let found = diff(
            None,
            vec![
                json!({"url": "a", "title": "A"}),
                json!({"url": "b", "title": "B"}),
            ],
            &settings(),
        )
        .unwrap();
        assert_eq!(keys(&found.added), ["a", "b"]);
        assert!(found.changed.is_empty() && found.removed.is_empty());
    }

    #[test]
    fn added_changed_removed_and_unchanged_are_told_apart_by_key_and_compared_paths() {
        let before = state(json!([
            {"url": "keep", "title": "K", "views": 1},
            {"url": "edit", "title": "old"},
            {"url": "gone", "title": "G"},
        ]));
        let found = diff(
            Some(&before),
            vec![
                // `views` is not compared: same record as far as the watch cares.
                json!({"url": "keep", "title": "K", "views": 99}),
                json!({"url": "edit", "title": "new"}),
                json!({"url": "fresh", "title": "F"}),
            ],
            &settings(),
        )
        .unwrap();
        assert_eq!(keys(&found.added), ["fresh"]);
        assert_eq!(found.changed.len(), 1);
        assert_eq!(found.changed[0].0.key, "edit");
        assert_eq!(found.changed[0].1["title"], "old");
        assert_eq!(keys(&found.removed), ["gone"]);
        assert_eq!(found.unchanged, 1);
    }

    #[test]
    fn identical_states_find_nothing_whatever_the_key_order_in_a_record() {
        let before = state(json!([{"url": "a", "title": "A", "text": "t"}]));
        let found = diff(
            Some(&before),
            vec![json!({"text": "t", "title": "A", "url": "a"})],
            &settings(),
        )
        .unwrap();
        assert!(found.added.is_empty() && found.changed.is_empty() && found.removed.is_empty());
        assert_eq!(found.unchanged, 1);
    }

    #[test]
    fn duplicates_collapse_to_the_first_or_the_last() {
        let two = || {
            vec![
                json!({"url": "a", "title": "1"}),
                json!({"url": "a", "title": "2"}),
            ]
        };
        let first = diff(None, two(), &settings()).unwrap();
        assert_eq!((first.kept.len(), first.duplicates), (1, 1));
        assert_eq!(first.kept[0].record["title"], "1");
        let mut last = settings();
        last.keep_last = true;
        assert_eq!(
            diff(None, two(), &last).unwrap().kept[0].record["title"],
            "2"
        );
    }

    #[test]
    fn what_cannot_become_the_baseline_is_refused_whole() {
        let mut tight = settings();
        tight.limits.max_items = 1;
        let err = diff(None, vec![json!({"url": "a"}), json!({"url": "b"})], &tight)
            .err()
            .unwrap();
        assert!(err.contains("max_items"), "{err}");
        let err = diff(
            None,
            vec![json!({"title": "no key"}), json!(null)],
            &settings(),
        )
        .err()
        .unwrap();
        assert!(err.contains("none of the 2 records"), "{err}");
        // Some bad ones among good ones are reported, not fatal.
        let found = diff(
            None,
            vec![json!({"title": "x"}), json!({"url": "ok"})],
            &settings(),
        )
        .unwrap();
        assert_eq!((found.kept.len(), found.rejected.len()), (1, 1));
        // An empty source wipes nothing unless the node says an empty source is real.
        let before = state(json!([{"url": "a"}]));
        let err = diff(Some(&before), vec![], &settings()).err().unwrap();
        assert!(err.contains("on_empty"), "{err}");
        assert!(diff(None, vec![], &settings()).is_err());
        let mut accept = settings();
        accept.refuse_empty = false;
        let empty = diff(Some(&before), vec![], &accept).unwrap();
        assert_eq!(keys(&empty.removed), ["a"]);
    }

    #[test]
    fn a_rejected_record_keeps_the_baseline_it_may_have_belonged_to() {
        let before = state(json!([{"url": "a", "title": "A"}, {"url": "b", "title": "B"}]));
        // `b` came back without a url (and `a` unchanged): b is neither removed nor lost.
        let incoming = || vec![json!({"url": "a", "title": "A"}), json!({"title": "B"})];
        let found = diff(Some(&before), incoming(), &settings()).unwrap();
        assert!(found.removed.is_empty() && found.added.is_empty());
        assert_eq!(
            (found.carried, found.unchanged, found.rejected.len()),
            (1, 1, 1)
        );
        assert_eq!(keys(&found.kept), ["a", "b"]);
        // The next, clean input finds b as it was: nothing is new.
        let next = Snapshot {
            seq: 2,
            at: 0,
            items: found.kept,
            history: vec![],
        };
        let clean = diff(
            Some(&next),
            vec![
                json!({"url": "a", "title": "A"}),
                json!({"url": "b", "title": "B"}),
            ],
            &settings(),
        )
        .unwrap();
        assert!(clean.added.is_empty() && clean.changed.is_empty() && clean.removed.is_empty());
        // With `replace`, the baseline follows the input and b is reported gone.
        let mut replace = settings();
        replace.keep_baseline = false;
        let found = diff(Some(&before), incoming(), &replace).unwrap();
        assert_eq!(keys(&found.removed), ["b"]);
    }

    #[test]
    fn one_thing_is_tracked_under_a_constant_key() {
        let mut page = settings();
        page.key_path = String::new();
        page.key = "page".into();
        page.compare.clear();
        let before = diff(None, records(&json!("version one")), &page).unwrap();
        let prev = Snapshot {
            seq: 1,
            at: 0,
            items: before.kept,
            history: vec![],
        };
        let same = diff(Some(&prev), records(&json!("version one")), &page).unwrap();
        assert_eq!(same.unchanged, 1);
        let moved = diff(Some(&prev), records(&json!("version two")), &page).unwrap();
        assert_eq!(
            (moved.changed.len(), moved.added.len(), moved.removed.len()),
            (1, 0, 0)
        );
        assert_eq!(moved.changed[0].1, json!("version one"));
    }
}
