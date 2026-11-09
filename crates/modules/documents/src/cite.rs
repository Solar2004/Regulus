//! Evidence for a model and the deterministic checks on what comes back: the `context`
//! text with its refs, the citation check and recall@k. Pure functions over JSON.

use std::collections::BTreeSet;

use serde_json::{json, Value};

fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `{{query}}`, `{{evidence}}` and `{{sources}}` replaced in one pass, so a value that
/// holds one of those words is never replaced again.
pub fn fill(template: &str, values: &[(&str, &str)]) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let Some(close) = after.find("}}") else {
            out.push_str(&rest[open..]);
            return out;
        };
        let name = after[..close].trim();
        match values.iter().find(|(n, _)| *n == name) {
            Some((_, value)) => out.push_str(value),
            None => out.push_str(&rest[open..open + 2 + close + 2]),
        }
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    out
}

/// What `context` makes of hits.
pub struct Context {
    pub text: String,
    pub refs: Vec<Value>,
}

/// The hits of a `retrieve` value (or an array of hits) as numbered evidence, at most
/// `max_chars` of it in whole blocks; a first block longer than that is cut.
pub fn context(value: &Value, template: &str, max_chars: usize, empty: &str) -> Context {
    let query = text_of(&value["query"]);
    let hits: &[Value] = match value {
        Value::Array(hits) => hits,
        _ => value["hits"].as_array().map_or(&[], Vec::as_slice),
    };
    let (mut blocks, mut sources, mut refs) = (Vec::new(), Vec::new(), Vec::new());
    let mut used = 0;
    for hit in hits {
        let n = refs.len() + 1;
        let mut block = format!("[{n}] {}", text_of(&hit["text"]));
        let size = block.chars().count();
        if used + size > max_chars {
            if n > 1 {
                break;
            }
            block = block.chars().take(max_chars).collect();
        }
        used += block.chars().count();
        blocks.push(block);
        let location = text_of(&hit["location"]);
        let (start, end) = (&hit["start"], &hit["end"]);
        sources.push(format!(
            "[{n}] {location}#{start}-{end} ({})",
            text_of(&hit["chunk_id"])
        ));
        refs.push(json!({
            "n": n,
            "chunk_id": hit["chunk_id"],
            "source_id": hit["source_id"],
            "location": location,
            "start": start,
            "end": end,
        }));
    }
    let evidence = if blocks.is_empty() {
        empty.to_string()
    } else {
        blocks.join("\n\n")
    };
    let text = fill(
        template,
        &[
            ("query", &query),
            ("evidence", &evidence),
            ("sources", &sources.join("\n")),
        ],
    );
    Context { text, refs }
}

/// The numbers cited as `[n]` or `[n, m]`, in the order they first appear.
pub fn citations(answer: &str) -> Vec<u64> {
    let mut found = Vec::new();
    let mut rest = answer;
    while let Some(open) = rest.find('[') {
        rest = &rest[open + 1..];
        let Some(close) = rest.find(']') else { break };
        let inside = &rest[..close];
        let numbers: Option<Vec<u64>> = inside
            .split(',')
            .map(|n| n.trim().parse::<u64>().ok())
            .collect();
        if let Some(numbers) = numbers.filter(|_| !inside.trim().is_empty()) {
            for n in numbers {
                if !found.contains(&n) {
                    found.push(n);
                }
            }
            rest = &rest[close + 1..];
        }
    }
    found
}

/// The answer and the refs of a value: `{ answer, refs }`, or an array holding the
/// answer text and the refs array in any order.
pub fn answer_and_refs(value: &Value) -> Result<(String, Vec<Value>), String> {
    let (answer, refs) = match value {
        Value::Object(o) => (o.get("answer").cloned(), o.get("refs").cloned()),
        Value::Array(items) => (
            items.iter().find(|v| !v.is_array()).cloned(),
            items.iter().find(|v| v.is_array()).cloned(),
        ),
        _ => (None, None),
    };
    let answer = answer.ok_or("no `answer` in the value")?;
    let refs = match refs {
        Some(Value::Array(refs)) => refs,
        Some(Value::Object(o)) => o
            .get("refs")
            .and_then(Value::as_array)
            .cloned()
            .ok_or("`refs` is not a list")?,
        _ => return Err("no `refs` list in the value".into()),
    };
    Ok((text_of(&answer), refs))
}

/// The settings of a citation check.
pub struct CiteRules<'a> {
    pub min_refs: usize,
    pub require_all: bool,
    pub sources_header: &'a str,
}

/// Whether the answer passes, the verdict as JSON, and the answer with its sources.
/// Provenance only: every cited `[n]` must exist in `refs`; whether the answer says what
/// its sources say is not checked.
pub fn cite_check(answer: &str, refs: &[Value], rules: &CiteRules) -> (bool, Value, String) {
    let cited = citations(answer);
    let known: BTreeSet<u64> = refs.iter().filter_map(|r| r["n"].as_u64()).collect();
    let unknown: Vec<u64> = cited
        .iter()
        .copied()
        .filter(|n| !known.contains(n))
        .collect();
    let uncited: Vec<u64> = known
        .iter()
        .copied()
        .filter(|n| !cited.contains(n))
        .collect();
    let distinct = cited.len() - unknown.len();
    let reason = if !unknown.is_empty() {
        format!("cites sources that do not exist: {unknown:?}")
    } else if distinct < rules.min_refs {
        format!(
            "cites {distinct} source(s), fewer than {} (`min_refs`)",
            rules.min_refs
        )
    } else if rules.require_all && !uncited.is_empty() {
        format!("does not cite every source (`require_all`): {uncited:?} missing")
    } else {
        String::new()
    };
    let mut sources: Vec<&Value> = refs
        .iter()
        .filter(|r| r["n"].as_u64().is_some_and(|n| cited.contains(&n)))
        .collect();
    sources.sort_by_key(|r| r["n"].as_u64());
    let lines: Vec<String> = sources
        .iter()
        .map(|r| format!("[{}] {}", r["n"], text_of(&r["location"])))
        .collect();
    let text = if lines.is_empty() {
        answer.to_string()
    } else {
        format!(
            "{}\n\n{}\n{}",
            answer.trim_end(),
            rules.sources_header,
            lines.join("\n")
        )
    };
    let pass = reason.is_empty();
    let verdict = json!({
        "answer": answer,
        "cited": cited,
        "unknown": unknown,
        "uncited": uncited,
        "sources": sources,
        "reason": reason,
    });
    (pass, verdict, text)
}

/// Whether `hit` matches one expected id: the start of its ids, or the end of its
/// location.
fn matches(hit: &Value, expected: &str) -> bool {
    if expected.is_empty() {
        return false;
    }
    ["source_id", "doc_id", "chunk_id"]
        .iter()
        .any(|f| hit[*f].as_str().is_some_and(|id| id.starts_with(expected)))
        || hit["location"]
            .as_str()
            .is_some_and(|l| l.ends_with(expected))
}

/// Recall@k over items shaped like `retrieve`'s output, the expected ids at
/// `expect_path` (a JSON Pointer: a list of ids, or one). Items expecting nothing are
/// skipped.
pub fn recall(value: &Value, expect_path: &str) -> Value {
    let items: Vec<&Value> = match value {
        Value::Array(items) => items.iter().collect(),
        other => vec![other],
    };
    let (mut total, mut found, mut skipped) = (0usize, 0usize, 0usize);
    let mut results = Vec::new();
    for item in items {
        let expected: Vec<String> = match item.pointer(expect_path) {
            Some(Value::Array(ids)) => ids.iter().map(text_of).collect(),
            Some(Value::String(id)) => vec![id.clone()],
            _ => Vec::new(),
        };
        if expected.is_empty() {
            skipped += 1;
            continue;
        }
        let hits = item["hits"].as_array().map_or(&[][..], Vec::as_slice);
        let matched = expected
            .iter()
            .find(|e| hits.iter().any(|h| matches(h, e)))
            .cloned();
        total += 1;
        found += usize::from(matched.is_some());
        results.push(json!({
            "query": item["query"],
            "expected": expected,
            "hit": matched.is_some(),
            "matched": matched,
        }));
    }
    let recall = if total == 0 {
        0.0
    } else {
        found as f64 / total as f64
    };
    json!({ "recall": recall, "total": total, "found": found, "skipped": skipped,
            "items": results })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hits() -> Value {
        json!({ "query": "what is rust?", "hits": [
            { "chunk_id": "sa#1:0", "source_id": "sa", "text": "Rust is a language.",
              "location": "/d/rust.md", "start": 0, "end": 19 },
            { "chunk_id": "sb#2:3", "source_id": "sb", "text": "Cargo builds it.",
              "location": "/d/cargo.md", "start": 40, "end": 56 },
        ]})
    }

    #[test]
    fn context_numbers_evidence_and_lists_sources() {
        let made = context(
            &hits(),
            "Q: {{query}}\n{{evidence}}\nSources:\n{{sources}}",
            1000,
            "none",
        );
        assert_eq!(
            made.text,
            "Q: what is rust?\n[1] Rust is a language.\n\n[2] Cargo builds it.\nSources:\n\
             [1] /d/rust.md#0-19 (sa#1:0)\n[2] /d/cargo.md#40-56 (sb#2:3)"
        );
        assert_eq!(made.refs[1]["chunk_id"], "sb#2:3");
        assert_eq!(made.refs[1]["n"], 2);
        // Whole blocks only, past the first.
        let short = context(&hits(), "{{evidence}}", 25, "none");
        assert_eq!(short.text, "[1] Rust is a language.");
        assert_eq!(short.refs.len(), 1);
        assert_eq!(context(&hits(), "{{evidence}}", 5, "").text, "[1] R");
        assert_eq!(
            context(&json!({ "hits": [] }), "{{evidence}}|{{x}}", 5, "none").text,
            "none|{{x}}"
        );
    }

    #[test]
    fn citations_are_numbers_in_brackets() {
        assert_eq!(
            citations("A [2] and [1, 3][2] but not [a] or [] or [link](x) [4"),
            [2, 1, 3]
        );
    }

    #[test]
    fn cite_check_requires_real_and_enough_citations() {
        let refs = context(&hits(), "", 1000, "").refs;
        let rules = CiteRules {
            min_refs: 1,
            require_all: false,
            sources_header: "Sources:",
        };
        let (pass, verdict, text) = cite_check("Rust is a language [1].", &refs, &rules);
        assert!(pass, "{verdict}");
        assert_eq!(text, "Rust is a language [1].\n\nSources:\n[1] /d/rust.md");
        assert_eq!(verdict["uncited"], json!([2]));
        let (pass, verdict, _) = cite_check("Made up [3].", &refs, &rules);
        assert!(!pass);
        assert!(verdict["reason"].as_str().unwrap().contains("[3]"));
        let (pass, _, _) = cite_check("No citation.", &refs, &rules);
        assert!(!pass);
        let all = CiteRules {
            require_all: true,
            ..rules
        };
        assert!(!cite_check("Only [1].", &refs, &all).0);
        assert!(cite_check("Both [1][2].", &refs, &all).0);
        let parsed = answer_and_refs(&json!(["Text [1]", refs])).unwrap();
        assert_eq!(parsed.0, "Text [1]");
        assert!(answer_and_refs(&json!({ "answer": "x" })).is_err());
    }

    #[test]
    fn recall_counts_items_with_an_expected_source_in_their_hits() {
        let mut a = hits();
        a["request"] = json!({ "expect_source_ids": ["sb"] });
        let mut b = hits();
        b["request"] = json!({ "expect_source_ids": ["zz", "missing.md"] });
        let mut c = hits();
        c["request"] = json!({ "expect_source_ids": "rust.md" });
        let d = hits();
        let score = recall(&json!([a, b, c, d]), "/request/expect_source_ids");
        assert_eq!(score["total"], 3);
        assert_eq!(score["found"], 2);
        assert_eq!(score["skipped"], 1);
        assert!((score["recall"].as_f64().unwrap() - 2.0 / 3.0).abs() < 1e-9);
        assert_eq!(score["items"][0]["matched"], "sb");
        assert_eq!(recall(&json!([]), "/x")["recall"], 0.0);
    }
}
