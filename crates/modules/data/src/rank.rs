//! `rank`: keyed metrics put in order (docs/16-experiments.md §2.3). Stateless and
//! deterministic: equal inputs give the same ranking, ties broken by key.

use std::collections::BTreeMap;

use regulus_contract_tool as tool;
use regulus_core::ModuleError;
use serde_json::{json, Value};

use crate::json::{at, key_text};
use crate::Here;

const RANKING: &str = "ranking";
const BEST: &str = "best";
const TOOL_RANK: &str = "rank_compare";

struct Settings {
    metric_path: String,
    ascending: bool,
    key_path: String,
    tolerance: f64,
    baseline: String,
    labels: BTreeMap<String, String>,
    scored_path: String,
    max_items: usize,
}

impl Settings {
    fn of(here: &Here) -> Self {
        Self {
            metric_path: here.text("metric_path"),
            ascending: here.text("order") == "asc",
            key_path: here.text("key_path"),
            tolerance: here.number("tolerance").max(0.0),
            baseline: here.text("baseline").trim().to_string(),
            labels: here.map("labels"),
            scored_path: here.text("scored_path").trim().to_string(),
            max_items: here.bound("max_items", crate::DEFAULT_MAX_ITEMS),
        }
    }
}

/// A derived number (a difference) without the noise of float subtraction. Large values
/// are kept as they are; one that cannot be represented is an error, never null or inf.
fn tidy(x: f64) -> Result<f64, String> {
    if !x.is_finite() {
        return Err("a difference between metrics is too large to represent".into());
    }
    let scaled = x * 1e12;
    Ok(if scaled.is_finite() {
        scaled.round() / 1e12
    } else {
        x
    })
}

/// What `rank` found, and the text for `best`.
fn compute(settings: &Settings, value: &Value) -> Result<(Value, String), String> {
    let entries: Vec<(String, Value)> = match value {
        Value::Object(map) => map.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        Value::Array(list) => {
            let mut keyed = Vec::with_capacity(list.len());
            for (n, item) in list.iter().enumerate() {
                let key = if settings.key_path.trim().is_empty() {
                    n.to_string()
                } else {
                    let found = at(item, &settings.key_path).ok_or_else(|| {
                        format!("entry {n} has no key at `{}`", settings.key_path)
                    })?;
                    key_text(found).map_err(|e| format!("entry {n}: {e}"))?
                };
                keyed.push((key, item.clone()));
            }
            keyed
        }
        _ => return Err("rank takes an object of metrics by key, or an array of metrics".into()),
    };
    if entries.is_empty() {
        return Err("nothing to rank".into());
    }
    if entries.len() > settings.max_items {
        return Err(format!(
            "{} entries, more than `max_items` {}",
            entries.len(),
            settings.max_items
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    let mut measured: Vec<(String, f64, Value)> = Vec::with_capacity(entries.len());
    let mut bad = Vec::new();
    for (key, metrics) in entries {
        if !seen.insert(key.clone()) {
            return Err(format!("the key `{key}` appears twice"));
        }
        match at(&metrics, &settings.metric_path).and_then(Value::as_f64) {
            Some(number) if number.is_finite() => measured.push((key, number, metrics)),
            _ => bad.push(key),
        }
    }
    if !bad.is_empty() {
        return Err(format!(
            "no number at `{}` in: {}",
            settings.metric_path,
            bad.join(", ")
        ));
    }
    measured.sort_by(|a, b| {
        let order = a.1.total_cmp(&b.1);
        let order = if settings.ascending {
            order
        } else {
            order.reverse()
        };
        order.then_with(|| a.0.cmp(&b.0))
    });
    // Metrics over different or no items are not comparable.
    let mut scored: Option<f64> = None;
    if !settings.scored_path.is_empty() {
        let mut counts = Vec::with_capacity(measured.len());
        for (key, _, metrics) in &measured {
            match at(metrics, &settings.scored_path).and_then(Value::as_f64) {
                Some(n) => counts.push((key.as_str(), n)),
                None => {
                    return Err(format!(
                        "not comparable: no number at `{}` in `{key}`",
                        settings.scored_path
                    ))
                }
            }
        }
        if counts.iter().any(|(_, n)| *n < 1.0) {
            return Err(format!(
                "not comparable: nothing was scored ({})",
                counts
                    .iter()
                    .map(|(k, n)| format!("{k}: {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if counts.iter().any(|(_, n)| *n != counts[0].1) {
            return Err(format!(
                "not comparable: the entries were measured on different items ({})",
                counts
                    .iter()
                    .map(|(k, n)| format!("{k}: {n}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        scored = Some(counts[0].1);
    }
    let baseline = if settings.baseline.is_empty() {
        None
    } else {
        let found = measured.iter().find(|m| m.0 == settings.baseline);
        Some(found.map(|m| m.1).ok_or_else(|| {
            format!(
                "the baseline `{}` is not among the keys: {}",
                settings.baseline,
                measured
                    .iter()
                    .map(|m| m.0.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?)
    };
    let top = measured[0].1;
    let (low, high) = measured
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), m| {
            (lo.min(m.1), hi.max(m.1))
        });
    let label = |key: &str| {
        settings
            .labels
            .get(key)
            .cloned()
            .unwrap_or_else(|| key.to_string())
    };
    let mut ranking = Vec::with_capacity(measured.len());
    let mut ranks: Vec<usize> = Vec::with_capacity(measured.len());
    // The value that opens the current group of ties: an entry joins it when within
    // `tolerance` of that one, not of the entry before (which would chain a slope).
    let mut leader = top;
    for (n, (key, number, metrics)) in measured.iter().enumerate() {
        let rank = match n {
            0 => 1,
            _ if (leader - number).abs() <= settings.tolerance + 1e-12 => ranks[n - 1],
            _ => {
                leader = *number;
                n + 1
            }
        };
        ranks.push(rank);
        let mut row = json!({ "rank": rank, "key": key, "label": label(key), "value": number,
                              "delta_best": tidy(number - top)?, "metrics": metrics });
        if let Some(base) = baseline {
            row["delta_baseline"] = json!(tidy(number - base)?);
        }
        if let Some(n) = scored {
            row["scored"] = json!(n);
        }
        ranking.push(row);
    }
    let best: Vec<&(String, f64, Value)> = measured
        .iter()
        .zip(&ranks)
        .filter(|(_, rank)| **rank == 1)
        .map(|(m, _)| m)
        .collect();
    let best_text = best
        .iter()
        .map(|m| label(&m.0))
        .collect::<Vec<_>>()
        .join(", ");
    let mut report = json!({
        "metric": settings.metric_path,
        "order": if settings.ascending { "asc" } else { "desc" },
        "count": measured.len(), "tie": best.len() > 1,
        "best": best.iter().map(|m| m.0.clone()).collect::<Vec<_>>(),
        "spread": tidy(high - low)?, "ranking": ranking,
    });
    if !settings.baseline.is_empty() {
        report["baseline"] = json!(settings.baseline);
    }
    if let Some(n) = scored {
        report["scored"] = json!(n);
    }
    Ok((report, best_text))
}

/// A value on `in`.
pub(crate) fn rank(here: &Here, value: &Value) -> Result<Value, ModuleError> {
    let (report, best) = compute(&Settings::of(here), value).map_err(|e| here.fail(e))?;
    here.emit(RANKING, report.clone())?;
    here.emit(BEST, json!(best))?;
    Ok(json!({ "best": report["best"], "tie": report["tie"] }))
}

pub(crate) fn tool_specs(here: &Here) -> Vec<tool::Spec> {
    vec![tool::Spec {
        name: TOOL_RANK.into(),
        description: format!(
            "Rank metrics by the number at `{}` ({}). Nothing is stored.",
            here.text("metric_path"),
            if here.text("order") == "asc" {
                "lowest first"
            } else {
                "highest first"
            }
        ),
        input: json!({ "type": "object", "properties": {
            "entries": { "description": "An object { key: metrics } or an array of metrics." } },
            "required": ["entries"] }),
    }]
}

pub(crate) fn run_tool(here: &Here, request: &tool::Call) -> Result<Value, String> {
    if request.name != TOOL_RANK {
        return Err(format!("no tool `{}`", request.name));
    }
    let entries = request.args.get("entries").ok_or("`entries` is required")?;
    compute(&Settings::of(here), entries).map(|(report, _)| report)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            metric_path: "/recall".into(),
            ascending: false,
            key_path: String::new(),
            tolerance: 0.0,
            baseline: String::new(),
            labels: BTreeMap::new(),
            scored_path: String::new(),
            max_items: 10,
        }
    }

    #[test]
    fn the_best_leads_and_every_entry_knows_how_far_it_is() {
        let mut s = settings();
        s.baseline = "a".into();
        s.labels.insert("b".into(), "Fixed 300".into());
        let (report, best) = compute(
            &s,
            &json!({ "a": {"recall": 0.5, "found": 1}, "b": {"recall": 1.0}, "c": {"recall": 0.25} }),
        )
        .unwrap();
        assert_eq!(best, "Fixed 300");
        assert_eq!(report["best"], json!(["b"]));
        assert_eq!(report["tie"], false);
        let rows = report["ranking"].as_array().unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| r["key"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["b", "a", "c"]
        );
        assert_eq!(rows[1]["delta_best"], json!(-0.5));
        assert_eq!(rows[1]["delta_baseline"], json!(0.0));
        assert_eq!(rows[0]["delta_baseline"], json!(0.5));
        assert_eq!(rows[1]["metrics"]["found"], 1);
        assert_eq!(report["spread"], json!(0.75));
    }

    #[test]
    fn huge_metrics_keep_their_deltas_and_unrepresentable_ones_are_errors() {
        let (report, _) = compute(
            &settings(),
            &json!({ "a": {"recall": 1e300}, "b": {"recall": 4e299} }),
        )
        .unwrap();
        assert_eq!(report["ranking"][1]["delta_best"], json!(-6e299));
        let err = compute(
            &settings(),
            &json!({ "a": {"recall": 1.7e308}, "b": {"recall": -1.7e308} }),
        )
        .err()
        .unwrap();
        assert!(err.contains("too large"), "{err}");
    }

    #[test]
    fn ties_share_a_rank_and_break_by_key() {
        let (report, best) = compute(
            &settings(),
            &json!({ "z": {"recall": 0.9}, "m": {"recall": 0.9}, "q": {"recall": 0.1} }),
        )
        .unwrap();
        assert_eq!(report["tie"], true);
        assert_eq!(report["best"], json!(["m", "z"]));
        assert_eq!(best, "m, z");
        assert_eq!(report["ranking"][2]["rank"], 3);

        let mut loose = settings();
        loose.tolerance = 0.05;
        let (report, _) = compute(
            &loose,
            &json!({ "a": {"recall": 0.90}, "b": {"recall": 0.87}, "c": {"recall": 0.5} }),
        )
        .unwrap();
        assert_eq!(report["best"], json!(["a", "b"]));
    }

    #[test]
    fn a_slope_does_not_chain_into_one_tie() {
        let mut loose = settings();
        loose.tolerance = 0.05;
        let (report, _) = compute(
            &loose,
            &json!({ "a": {"recall": 1.00}, "b": {"recall": 0.95}, "c": {"recall": 0.90} }),
        )
        .unwrap();
        let ranks: Vec<u64> = report["ranking"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["rank"].as_u64().unwrap())
            .collect();
        assert_eq!(ranks, [1, 1, 3]);
        assert_eq!(report["best"], json!(["a", "b"]));
    }

    #[test]
    fn metrics_over_other_or_no_items_are_not_compared() {
        let mut s = settings();
        s.scored_path = "/total".into();
        let (report, _) = compute(
            &s,
            &json!({ "a": {"recall": 0.5, "total": 4}, "b": {"recall": 1.0, "total": 4} }),
        )
        .unwrap();
        assert_eq!(
            (
                report["scored"].clone(),
                report["ranking"][0]["scored"].clone()
            ),
            (json!(4.0), json!(4.0))
        );
        let err = compute(
            &s,
            &json!({ "a": {"recall": 0.5, "total": 3}, "b": {"recall": 1.0, "total": 4} }),
        )
        .err()
        .unwrap();
        assert!(
            err.contains("different items") && err.contains("a: 3") && err.contains("b: 4"),
            "{err}"
        );
        let err = compute(
            &s,
            &json!({ "a": {"recall": 0.0, "total": 0}, "b": {"recall": 0.0, "total": 0} }),
        )
        .err()
        .unwrap();
        assert!(err.contains("nothing was scored"), "{err}");
        let err = compute(&s, &json!({ "a": {"recall": 0.0} })).err().unwrap();
        assert!(err.contains("no number at `/total` in `a`"), "{err}");
    }

    #[test]
    fn lower_is_better_when_ascending_and_arrays_are_keyed_by_path() {
        let mut s = settings();
        s.ascending = true;
        s.metric_path = "/ms".into();
        s.key_path = "/name".into();
        let (report, best) = compute(
            &s,
            &json!([{"name": "slow", "ms": 90}, {"name": "fast", "ms": 12}]),
        )
        .unwrap();
        assert_eq!(best, "fast");
        assert_eq!(report["ranking"][1]["delta_best"], json!(78.0));
    }

    #[test]
    fn a_missing_metric_or_baseline_refuses_the_whole_input_by_name() {
        let err = compute(
            &settings(),
            &json!({ "a": {"recall": 1}, "b": {"x": 1}, "c": {"recall": "n"} }),
        )
        .err()
        .unwrap();
        assert_eq!(err, "no number at `/recall` in: b, c");
        let mut s = settings();
        s.baseline = "nope".into();
        let err = compute(&s, &json!({ "a": {"recall": 1} })).err().unwrap();
        assert!(err.contains("baseline `nope`"), "{err}");
        assert!(compute(&settings(), &json!("text")).is_err());
        assert!(compute(&settings(), &json!({})).is_err());
    }
}
