//! One test on a value: what `switch` routes by and what ends a `loop`.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use regex::{Regex, RegexBuilder};
use regulus_core::ModuleError;
use serde::Deserialize;
use serde_json::Value;

/// `{ op, value, value_path, path, port }`, as a node's configuration spells it.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct Rule {
    /// A JSON Pointer (`/verdict/score`) or a dotted path (`verdict.score`) into the value.
    /// Empty: the whole value.
    #[serde(default)]
    pub path: String,
    pub op: String,
    /// What the subject is compared with; unused by `exists` and `empty`.
    #[serde(default)]
    pub value: Value,
    /// A path into the same value, like `path`: when set, the subject is compared with
    /// what it points at instead of `value`. If it points at nothing, the rule fails.
    #[serde(default)]
    pub value_path: String,
    /// For `switch`: the output a value that meets the rule goes out on.
    #[serde(default)]
    pub port: String,
}

/// Compiled patterns, kept across events: a pattern is compiled once per size limit, not
/// on every value. Patterns come from profiles, so the set stays as small as they are.
#[derive(Default)]
pub(crate) struct Regexes {
    cache: Mutex<HashMap<(String, usize), Arc<Regex>>>,
}

impl Regexes {
    fn get(&self, pattern: &str, limit: usize) -> Result<Arc<Regex>, String> {
        let key = (pattern.to_string(), limit);
        if let Some(found) = crate::lock(&self.cache).get(&key) {
            return Ok(found.clone());
        }
        // `size_limit` bounds the compiled program: a pattern that would take more memory
        // than the node allows is refused rather than built.
        let compiled = RegexBuilder::new(pattern)
            .size_limit(limit)
            .build()
            .map_err(|e| e.to_string())?;
        let compiled = Arc::new(compiled);
        crate::lock(&self.cache).insert(key, compiled.clone());
        Ok(compiled)
    }
}

/// What a rule needs to run besides the value: the module's compiled patterns and the
/// node's size limit for new ones.
pub(crate) struct Matcher<'a> {
    pub regexes: &'a Regexes,
    pub limit: usize,
}

type Test = fn(Option<&Value>, &Value, &Matcher) -> Result<bool, String>;

/// Every operator by name. A new one is a row here and nowhere else.
const OPS: &[(&str, Test)] = &[
    ("equals", |s, v, _| Ok(s.is_some_and(|s| same(s, v)))),
    ("not_equals", |s, v, _| Ok(!s.is_some_and(|s| same(s, v)))),
    ("contains", contains),
    ("regex", regex),
    ("exists", |s, _, _| Ok(s.is_some_and(|s| !s.is_null()))),
    ("empty", |s, _, _| Ok(s.is_none_or(is_empty))),
    ("greater", |s, v, _| {
        Ok(number(s).zip(number(Some(v))).is_some_and(|(a, b)| a > b))
    }),
    ("less", |s, v, _| {
        Ok(number(s).zip(number(Some(v))).is_some_and(|(a, b)| a < b))
    }),
];

/// The names `op` may take.
pub(crate) fn ops() -> impl Iterator<Item = &'static str> {
    OPS.iter().map(|(name, _)| *name)
}

/// A string as itself; anything else as its JSON.
pub(crate) fn text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

/// What `path` points at in `value`: a JSON Pointer, or a dotted path turned into one.
pub(crate) fn at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let path = path.trim();
    if path.is_empty() || path == "." {
        return Some(value);
    }
    if path.starts_with('/') {
        return value.pointer(path);
    }
    let pointer: String = path.split('.').flat_map(|part| ["/", part]).collect();
    value.pointer(&pointer)
}

fn same(subject: &Value, value: &Value) -> bool {
    subject == value || text(subject) == text(value)
}

fn contains(subject: Option<&Value>, value: &Value, _: &Matcher) -> Result<bool, String> {
    Ok(match subject {
        Some(Value::Array(items)) => items.iter().any(|item| same(item, value)),
        Some(Value::Object(map)) => map.contains_key(&text(value)),
        Some(other) => text(other).contains(&text(value)),
        None => false,
    })
}

fn regex(subject: Option<&Value>, value: &Value, matcher: &Matcher) -> Result<bool, String> {
    let pattern = matcher.regexes.get(&text(value), matcher.limit)?;
    Ok(subject.is_some_and(|s| pattern.is_match(&text(s))))
}

fn is_empty(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(s) => s.trim().is_empty(),
        Value::Array(a) => a.is_empty(),
        Value::Object(o) => o.is_empty(),
        _ => false,
    }
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

impl Rule {
    /// Reads a rule from a node's setting. `{}` or nothing: no rule.
    pub(crate) fn optional(setting: Option<&Value>) -> Result<Option<Self>, ModuleError> {
        match setting {
            None | Some(Value::Null) => Ok(None),
            Some(Value::Object(map)) if map.is_empty() => Ok(None),
            Some(Value::String(s)) if s.trim().is_empty() => Ok(None),
            Some(value) => Ok(Some(Self::parse(value.clone())?)),
        }
    }

    /// Reads a list of rules from a node's setting. Nothing: none.
    pub(crate) fn list(setting: Option<&Value>) -> Result<Vec<Self>, ModuleError> {
        match setting {
            None | Some(Value::Null) => Ok(Vec::new()),
            Some(Value::Array(items)) => items.iter().cloned().map(Self::parse).collect(),
            Some(other) => Err(ModuleError::InvalidParams(format!(
                "rules must be a list, not {other}"
            ))),
        }
    }

    fn parse(value: Value) -> Result<Self, ModuleError> {
        let rule: Self = serde_json::from_value(value)
            .map_err(|e| ModuleError::InvalidParams(format!("rule: {e}")))?;
        if !ops().any(|op| op == rule.op) {
            let known: Vec<&str> = ops().collect();
            return Err(ModuleError::InvalidParams(format!(
                "rule: unknown op `{}` (known: {})",
                rule.op,
                known.join(", ")
            )));
        }
        Ok(rule)
    }

    /// Whether `value` meets the rule.
    pub(crate) fn test(&self, value: &Value, matcher: &Matcher) -> Result<bool, ModuleError> {
        let (_, test) = OPS
            .iter()
            .find(|(name, _)| *name == self.op)
            .ok_or_else(|| ModuleError::InvalidParams(format!("unknown op `{}`", self.op)))?;
        let other = if self.value_path.trim().is_empty() {
            &self.value
        } else {
            match at(value, &self.value_path) {
                Some(other) => other,
                None => return Ok(false),
            }
        };
        test(at(value, &self.path), other, matcher)
            .map_err(|e| ModuleError::InvalidParams(format!("rule `{}`: {e}", self.op)))
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    /// A matcher with the manifest's default size limit.
    pub fn matcher(regexes: &Regexes) -> Matcher<'_> {
        Matcher {
            regexes,
            limit: 1 << 20,
        }
    }

    fn check(rule: Value, value: Value) -> bool {
        let regexes = Regexes::default();
        Rule::parse(rule)
            .unwrap()
            .test(&value, &matcher(&regexes))
            .unwrap()
    }

    #[test]
    fn a_pattern_is_compiled_once_and_refused_past_its_size_limit() {
        let regexes = Regexes::default();
        let first = regexes.get("^v\\d+$", 1 << 20).unwrap();
        let again = regexes.get("^v\\d+$", 1 << 20).unwrap();
        assert!(Arc::ptr_eq(&first, &again), "cached");
        assert!(
            regexes.get("\\w{1000}", 1000).is_err(),
            "too large to build"
        );
        assert_eq!(
            crate::lock(&regexes.cache).len(),
            1,
            "a refused one is not kept"
        );
    }

    #[test]
    fn every_op_tests_what_its_name_says() {
        assert!(check(json!({ "op": "equals", "value": "ok" }), json!("ok")));
        assert!(check(json!({ "op": "equals", "value": 3 }), json!("3")));
        assert!(check(
            json!({ "op": "not_equals", "value": "ok" }),
            json!("no")
        ));
        assert!(check(
            json!({ "op": "contains", "value": "DONE" }),
            json!("all DONE.")
        ));
        assert!(check(
            json!({ "op": "contains", "value": "b" }),
            json!(["a", "b"])
        ));
        assert!(check(
            json!({ "op": "regex", "value": "^v\\d+$" }),
            json!("v12")
        ));
        assert!(!check(
            json!({ "op": "regex", "value": "^v\\d+$" }),
            json!("x12")
        ));
        assert!(check(
            json!({ "op": "exists", "path": "a" }),
            json!({ "a": 0 })
        ));
        assert!(!check(
            json!({ "op": "exists", "path": "b" }),
            json!({ "a": 0 })
        ));
        assert!(check(json!({ "op": "empty" }), json!("  ")));
        assert!(check(json!({ "op": "greater", "value": 2 }), json!(2.5)));
        assert!(check(json!({ "op": "less", "value": "10" }), json!("9")));
    }

    #[test]
    fn paths_are_pointers_or_dotted() {
        let value = json!({ "verdict": { "score": 7, "tags": ["x"] } });
        assert_eq!(at(&value, "/verdict/score"), Some(&json!(7)));
        assert_eq!(at(&value, "verdict.tags.0"), Some(&json!("x")));
        assert_eq!(at(&value, ""), Some(&value));
        assert!(check(
            json!({ "op": "greater", "path": "verdict.score", "value": 5 }),
            value
        ));
    }

    #[test]
    fn value_path_compares_two_parts_of_the_same_value() {
        let pair = json!({ "candidate": { "info": { "id": "exec-4" } }, "winner": "exec-4" });
        let rule =
            |op: &str| json!({ "op": op, "path": "/candidate/info/id", "value_path": "winner" });
        assert!(check(rule("equals"), pair.clone()));
        assert!(!check(rule("not_equals"), pair.clone()));
        let other = json!({ "candidate": { "info": { "id": "exec-3" } }, "winner": "exec-4" });
        assert!(!check(rule("equals"), other.clone()));
        assert!(check(rule("not_equals"), other));
        // Numbers and patterns read the other side the same way.
        let scores = json!({ "score": 8, "threshold": "7", "name": "v12", "pattern": "^v\\d+$" });
        assert!(check(
            json!({ "op": "greater", "path": "score", "value_path": "threshold" }),
            scores.clone()
        ));
        assert!(check(
            json!({ "op": "regex", "path": "name", "value_path": "pattern" }),
            scores.clone()
        ));
        // A `value_path` that points at nothing fails the rule, whatever the op.
        assert!(!check(
            json!({ "op": "not_equals", "path": "score", "value_path": "missing" }),
            scores
        ));
    }

    #[test]
    fn a_bad_op_or_pattern_is_refused() {
        assert!(Rule::parse(json!({ "op": "like" })).is_err());
        let rule = Rule::parse(json!({ "op": "regex", "value": "(" })).unwrap();
        let regexes = Regexes::default();
        assert!(rule.test(&json!("x"), &matcher(&regexes)).is_err());
        assert!(Rule::optional(Some(&json!({}))).unwrap().is_none());
        assert!(Rule::list(Some(&json!("x"))).is_err());
    }
}
