//! `collect`: a bounded dataset of keyed records (docs/16-experiments.md §2.1).

use regulus_contract_tool as tool;
use regulus_core::ModuleError;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::json::{canonical, cut, key_of};
use crate::state::{Kind, Row};
use crate::{now_ms, Here, Limits, Rejected, REJECTED_LISTED};

const OUT: &str = "out";
const STORED: &str = "stored";
const ERROR: &str = "error";

const TOOL_STATS: &str = "dataset_stats";
const TOOL_READ: &str = "dataset_read";
const TOOL_ADD: &str = "dataset_add";

/// The row of a dataset: its records in the order they were stored.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct Dataset {
    /// Counts every record ever stored, so an item's `seq` orders them.
    #[serde(default)]
    seq: u64,
    #[serde(default)]
    items: Vec<Item>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Item {
    key: String,
    seq: u64,
    /// Milliseconds since the Unix epoch.
    at: u64,
    /// The key and the record as compact JSON.
    bytes: usize,
    record: Value,
}

impl Dataset {
    fn bytes(&self) -> usize {
        self.items.iter().map(|i| i.bytes).sum()
    }

    fn records(&self) -> Vec<Value> {
        self.items.iter().map(|i| i.record.clone()).collect()
    }
}

/// How a node stores: what it reads of its fields.
struct Settings {
    key_path: String,
    replace: bool,
    evict_oldest: bool,
    limits: Limits,
}

impl Settings {
    fn of(here: &Here) -> Self {
        Self {
            key_path: here.text("key_path"),
            replace: here.text("on_duplicate") == "replace",
            evict_oldest: here.text("evict") == "oldest",
            limits: here.limits(),
        }
    }
}

/// Records on their way in: those read, and the lines or values that could not be.
struct Intake {
    records: Vec<(Option<usize>, Value)>,
    rejected: Vec<Rejected>,
}

fn one(record: Value) -> Intake {
    Intake {
        records: vec![(None, record)],
        rejected: vec![],
    }
}

fn many(value: Value) -> Intake {
    match value {
        Value::Array(list) => Intake {
            records: list.into_iter().map(|r| (None, r)).collect(),
            rejected: vec![],
        },
        other => one(other),
    }
}

/// What an input holds: a record, an array of records, or a text read per `mode`.
fn intake(mode: &str, value: &Value) -> Result<Intake, String> {
    let Value::String(text) = value else {
        return Ok(many(value.clone()));
    };
    match mode {
        "record" => Ok(one(value.clone())),
        "jsonl" => {
            let mut intake = Intake {
                records: vec![],
                rejected: vec![],
            };
            for (n, line) in text.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                match serde_json::from_str(line) {
                    Ok(record) => intake.records.push((Some(n + 1), record)),
                    Err(e) => intake.rejected.push(Rejected {
                        line: Some(n + 1),
                        key: None,
                        reason: format!("not JSON: {e}"),
                    }),
                }
            }
            Ok(intake)
        }
        _ => serde_json::from_str(text)
            .map(many)
            .map_err(|e| format!("the text is not JSON: {e}")),
    }
}

/// What happened to one input.
struct Outcome {
    received: usize,
    added: usize,
    replaced: usize,
    duplicates: usize,
    evicted: Vec<String>,
    rejected: Vec<Rejected>,
    total: usize,
    bytes: usize,
}

impl Outcome {
    fn json(&self, name: &str, scope: &str) -> Value {
        let shown: Vec<&Rejected> = self.rejected.iter().take(REJECTED_LISTED).collect();
        json!({
            "dataset": name, "scope": scope, "received": self.received, "added": self.added,
            "replaced": self.replaced, "duplicates": self.duplicates,
            "evicted": self.evicted.len(),
            "evicted_keys": self.evicted.iter().take(REJECTED_LISTED).collect::<Vec<_>>(),
            "rejected_count": self.rejected.len(), "rejected": shown,
            "total": self.total, "bytes": self.bytes,
        })
    }
}

/// Stores `intake` in the dataset `row`, within `settings`. The caller holds the write
/// lock. One bad record never costs the others.
fn put(row: &Row, settings: &Settings, intake: Intake) -> Result<Outcome, String> {
    let mut set: Dataset = row.load()?.unwrap_or_default();
    let mut bytes = set.bytes();
    let mut out = Outcome {
        received: intake.records.len() + intake.rejected.len(),
        added: 0,
        replaced: 0,
        duplicates: 0,
        evicted: vec![],
        rejected: intake.rejected,
        total: 0,
        bytes: 0,
    };
    let limits = settings.limits;
    'records: for (line, record) in intake.records {
        let mut reject = |key: Option<String>, reason: String| {
            out.rejected.push(Rejected { line, key, reason });
        };
        if record.is_null() {
            reject(None, "null is not a record".into());
            continue;
        }
        let key = match key_of(&record, &settings.key_path, "") {
            Ok(key) => key,
            Err(reason) => {
                reject(None, reason);
                continue;
            }
        };
        let text = canonical(&record);
        if text.len() > limits.max_record_bytes {
            reject(
                Some(key),
                format!(
                    "the record has {} bytes, more than `max_record_bytes` {}",
                    text.len(),
                    limits.max_record_bytes
                ),
            );
            continue;
        }
        let size = key.len() + text.len();
        if size > limits.max_bytes {
            reject(
                Some(key),
                format!(
                    "the record needs {size} bytes, more than `max_bytes` {}",
                    limits.max_bytes
                ),
            );
            continue;
        }
        let mut replaced = None;
        if let Some(at) = set.items.iter().position(|i| i.key == key) {
            if !settings.replace {
                out.duplicates += 1;
                continue;
            }
            let old = set.items.remove(at);
            bytes -= old.bytes;
            replaced = Some((at, old));
        }
        while set.items.len() + 1 > limits.max_items || bytes + size > limits.max_bytes {
            if settings.evict_oldest && !set.items.is_empty() {
                let oldest = set.items.remove(0);
                bytes -= oldest.bytes;
                out.evicted.push(oldest.key);
                // A replaced record that was the oldest is gone for good.
                if let Some((at, _)) = &mut replaced {
                    *at = at.saturating_sub(1);
                }
                continue;
            }
            // Nothing is lost by refusing: a record this one would replace comes back.
            if let Some((at, old)) = replaced {
                bytes += old.bytes;
                set.items.insert(at.min(set.items.len()), old);
            }
            out.rejected.push(Rejected {
                line,
                key: Some(key),
                reason: format!(
                    "the dataset is full ({} records, {bytes} bytes; `max_items` {}, `max_bytes` {}) \
                     and `evict = refuse`",
                    set.items.len(),
                    limits.max_items,
                    limits.max_bytes
                ),
            });
            continue 'records;
        }
        set.seq += 1;
        set.items.push(Item {
            key,
            seq: set.seq,
            at: now_ms(),
            bytes: size,
            record,
        });
        bytes += size;
        match replaced {
            Some(_) => out.replaced += 1,
            None => out.added += 1,
        }
    }
    if out.added + out.replaced + out.evicted.len() > 0 {
        row.save(&set)?;
    }
    out.total = set.items.len();
    out.bytes = bytes;
    Ok(out)
}

/// A value on `in`.
pub(crate) fn add(here: &Here, value: &Value) -> Result<Value, ModuleError> {
    let intake = intake(&here.text("text"), value).map_err(|e| here.fail(e))?;
    let row = here.row(Kind::Dataset).map_err(|e| here.fail(e))?;
    let settings = Settings::of(here);
    let outcome = here
        .writing(|| put(&row, &settings, intake))
        .map_err(|e| here.fail(e))?;
    let summary = outcome.json(&here.text("dataset"), &here.scope());
    here.emit(STORED, summary.clone())?;
    if let Some(first) = outcome.rejected.first() {
        here.emit(
            ERROR,
            json!(format!(
                "dataset `{}`: {} of {} records rejected; first: {}",
                here.text("dataset"),
                outcome.rejected.len(),
                outcome.received,
                cut(&first.reason, 200)
            )),
        )?;
    }
    if here.text("emit") == "input" {
        emit_all(here, &row)?;
    }
    Ok(summary)
}

fn emit_all(here: &Here, row: &Row) -> Result<usize, ModuleError> {
    let set: Dataset = row.load().map_err(|e| here.fail(e))?.unwrap_or_default();
    here.emit(OUT, Value::Array(set.records()))?;
    Ok(set.items.len())
}

/// A value on `flush`: the whole dataset on `out`.
pub(crate) fn flush(here: &Here) -> Result<Value, ModuleError> {
    let row = here.row(Kind::Dataset).map_err(|e| here.fail(e))?;
    let records = emit_all(here, &row)?;
    Ok(json!({ "records": records }))
}

/// A value on `clear`.
pub(crate) fn clear(here: &Here) -> Result<Value, ModuleError> {
    let row = here.row(Kind::Dataset).map_err(|e| here.fail(e))?;
    let cleared = here
        .writing(|| {
            let before: Dataset = row.load()?.unwrap_or_default();
            row.delete().map(|_| before.items.len())
        })
        .map_err(|e| here.fail(e))?;
    let summary = json!({ "dataset": here.text("dataset"), "scope": here.scope(),
                          "cleared": cleared, "total": 0, "bytes": 0 });
    here.emit(STORED, summary.clone())?;
    Ok(summary)
}

pub(crate) fn tool_specs(here: &Here) -> Vec<tool::Spec> {
    let name = here.text("dataset");
    let key = json!({ "type": "string", "description": "The record's key." });
    let mut specs = vec![
        tool::Spec {
            name: TOOL_STATS.into(),
            description: format!("How many records the dataset `{name}` holds and its limits."),
            input: json!({ "type": "object", "properties": {} }),
        },
        tool::Spec {
            name: TOOL_READ.into(),
            description: format!(
                "Read the dataset `{name}`: the record with `key`, or without it the newest \
                 records first."
            ),
            input: json!({ "type": "object", "properties": {
                "key": key, "limit": { "type": "integer", "description": "How many records." } } }),
        },
    ];
    if here.text("agent_writes") == "yes" {
        specs.push(tool::Spec {
            name: TOOL_ADD.into(),
            description: format!(
                "Add one record to the dataset `{name}`. A record whose key is already there \
                 is a duplicate; one that does not fit its limits is refused."
            ),
            input: json!({ "type": "object", "properties": {
                "record": { "description": "The record: any JSON value." } },
                "required": ["record"] }),
        });
    }
    specs
}

pub(crate) fn run_tool(here: &Here, request: &tool::Call) -> Result<Value, String> {
    let row = here.row(Kind::Dataset)?;
    let name = here.text("dataset");
    match request.name.as_str() {
        TOOL_STATS => {
            let set: Dataset = row.load()?.unwrap_or_default();
            let limits = here.limits();
            Ok(
                json!({ "dataset": name, "scope": here.scope(), "items": set.items.len(),
                       "bytes": set.bytes(), "max_items": limits.max_items,
                       "max_bytes": limits.max_bytes,
                       "max_record_bytes": limits.max_record_bytes }),
            )
        }
        TOOL_READ => {
            let set: Dataset = row.load()?.unwrap_or_default();
            if let Some(key) = request.args.get("key").and_then(Value::as_str) {
                return set
                    .items
                    .iter()
                    .find(|i| i.key == key)
                    .map(|i| json!({ "key": i.key, "at": i.at, "record": i.record }))
                    .ok_or_else(|| format!("no record `{key}`"));
            }
            let max = here.bound("max_results", 20);
            let limit = request
                .args
                .get("limit")
                .and_then(Value::as_u64)
                .map_or(max, |n| (n as usize).clamp(1, max));
            let newest: Vec<Value> = set
                .items
                .iter()
                .rev()
                .take(limit)
                .map(|i| json!({ "key": i.key, "at": i.at, "record": i.record }))
                .collect();
            Ok(json!(newest))
        }
        TOOL_ADD if here.text("agent_writes") == "yes" => {
            let record = request
                .args
                .get("record")
                .cloned()
                .ok_or("`record` is required")?;
            let settings = Settings::of(here);
            let outcome = here.writing(|| put(&row, &settings, one(record)))?;
            if let Some(first) = outcome.rejected.first() {
                return Err(first.reason.clone());
            }
            Ok(outcome.json(&name, &here.scope()))
        }
        other => Err(format!("no tool `{other}`")),
    }
}

/// For `data.list`: what a dataset row holds, without its records.
pub(crate) fn summary(bytes: &[u8]) -> Value {
    match serde_json::from_slice::<Dataset>(bytes) {
        Ok(set) => json!({ "items": set.items.len(), "bytes": set.bytes(), "seq": set.seq }),
        Err(e) => json!({ "error": format!("unreadable: {e}") }),
    }
}

/// For `data.inspect` and `data.export`: the dataset with its first `limit` records.
pub(crate) fn show(row: &Row, name: &str, scope: &str, limit: usize) -> Result<Value, String> {
    let set: Dataset = row
        .load()?
        .ok_or_else(|| format!("no dataset `{name}` in `{scope}`: nothing was stored"))?;
    let shown: Vec<Value> = set
        .items
        .iter()
        .take(limit)
        .map(|i| json!({ "key": i.key, "seq": i.seq, "at": i.at, "record": i.record }))
        .collect();
    Ok(
        json!({ "kind": "dataset", "name": name, "scope": scope, "items": set.items.len(),
               "bytes": set.bytes(), "seq": set.seq, "truncated": shown.len() < set.items.len(),
               "records": shown }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Kv;
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    fn row() -> Row {
        let kv = Kv::Local(Arc::new(Mutex::new(BTreeMap::new())));
        Row::new(kv, Kind::Dataset, "project", "t").unwrap()
    }

    fn settings(replace: bool, evict_oldest: bool, max_items: usize) -> Settings {
        Settings {
            key_path: "/id".into(),
            replace,
            evict_oldest,
            limits: Limits {
                max_items,
                max_bytes: 1 << 20,
                max_record_bytes: 64,
            },
        }
    }

    fn keys(row: &Row) -> Vec<String> {
        let set: Dataset = row.load().unwrap().unwrap();
        set.items.into_iter().map(|i| i.key).collect()
    }

    #[test]
    fn jsonl_keeps_good_lines_and_names_the_bad_ones() {
        let read = intake("jsonl", &json!("{\"id\":1}\n\nnot json\n{\"id\":2}\n")).unwrap();
        assert_eq!(read.records.len(), 2);
        assert_eq!(read.records[1].0, Some(4));
        assert_eq!(read.rejected.len(), 1);
        assert_eq!(read.rejected[0].line, Some(3));
        assert!(intake_err("json", "nope"));
        let text = intake("record", &json!("plain")).unwrap();
        assert_eq!(text.records[0].1, json!("plain"));
    }

    fn intake_err(mode: &str, text: &str) -> bool {
        intake(mode, &json!(text)).is_err()
    }

    #[test]
    fn duplicates_keep_the_first_or_replace_with_the_newest() {
        let row = row();
        let batch = || many(json!([{"id": "a", "v": 1}, {"id": "b"}, {"id": "a", "v": 2}]));
        let out = put(&row, &settings(false, false, 10), batch()).unwrap();
        assert_eq!((out.added, out.duplicates, out.total), (2, 1, 2));
        let set: Dataset = row.load().unwrap().unwrap();
        assert_eq!(set.items[0].record["v"], 1);

        let out = put(
            &row,
            &settings(true, false, 10),
            many(json!([{"id": "a", "v": 3}])),
        )
        .unwrap();
        assert_eq!((out.added, out.replaced), (0, 1));
        assert_eq!(keys(&row), ["b", "a"]);
        let set: Dataset = row.load().unwrap().unwrap();
        assert_eq!(set.items[1].record["v"], 3);
    }

    #[test]
    fn a_full_dataset_refuses_or_drops_the_oldest_and_never_unbounded() {
        let row = row();
        let three = || many(json!([{"id": 1}, {"id": 2}, {"id": 3}]));
        let out = put(&row, &settings(false, false, 2), three()).unwrap();
        assert_eq!((out.added, out.rejected.len(), out.total), (2, 1, 2));
        assert!(out.rejected[0].reason.contains("evict = refuse"));
        assert_eq!(keys(&row), ["1", "2"]);

        let out = put(
            &row,
            &settings(false, true, 2),
            many(json!([{"id": 3}, {"id": 4}])),
        )
        .unwrap();
        assert_eq!(out.evicted, ["1", "2"]);
        assert_eq!(keys(&row), ["3", "4"]);
    }

    #[test]
    fn a_refused_replacement_keeps_the_old_record_in_place() {
        let row = row();
        let mut tight = settings(true, false, 10);
        tight.limits.max_bytes = 40;
        put(&row, &tight, many(json!([{"id": "a"}, {"id": "b"}]))).unwrap();
        let big = json!({"id": "a", "pad": "xxxxxxxxxxxxxxxxxxxxxxxxxxxx"});
        let out = put(&row, &tight, many(json!([big]))).unwrap();
        assert_eq!(out.rejected.len(), 1, "{:?}", out.rejected);
        assert_eq!(keys(&row), ["a", "b"]);
    }

    #[test]
    fn bad_records_are_rejected_one_by_one() {
        let row = row();
        let big = json!({"id": "big", "pad": "x".repeat(80)});
        let out = put(
            &row,
            &settings(false, false, 10),
            many(json!([{"name": "no key"}, null, big, {"id": "ok"}])),
        )
        .unwrap();
        assert_eq!((out.added, out.rejected.len()), (1, 3));
        assert!(out.rejected[0].reason.contains("no key at `/id`"));
        assert!(out.rejected[2].reason.contains("max_record_bytes"));
        assert_eq!(out.received, 4);
    }
}
