//! JSON helpers the nodes share: paths, canonical form, hashes and keys.

use blake2::{Blake2b512, Digest};
use serde_json::Value;

/// The value at `path`: a JSON pointer (`/a/0/b`) or a dotted path (`a.0.b`). An empty
/// path is the value itself.
pub(crate) fn at<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let path = path.trim();
    if path.is_empty() {
        return Some(value);
    }
    if path.starts_with('/') {
        return value.pointer(path);
    }
    path.split('.').try_fold(value, |here, step| match here {
        Value::Object(map) => map.get(step),
        Value::Array(list) => step.parse::<usize>().ok().and_then(|n| list.get(n)),
        _ => None,
    })
}

/// Compact JSON with object keys in sorted order, so equal values always spell the same
/// whatever order their keys were built in.
pub(crate) fn canonical(value: &Value) -> String {
    let mut out = String::new();
    write_canonical(value, &mut out);
    out
}

fn write_canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (n, key) in keys.into_iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                write_canonical(&map[key], out);
            }
            out.push('}');
        }
        Value::Array(list) => {
            out.push('[');
            for (n, item) in list.iter().enumerate() {
                if n > 0 {
                    out.push(',');
                }
                write_canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

/// 128 bits of the canonical form's hash, in hex.
pub(crate) fn digest(value: &Value) -> String {
    Blake2b512::digest(canonical(value).as_bytes())
        .iter()
        .take(16)
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A key from a scalar: text as is, numbers and booleans spelled.
pub(crate) fn key_text(value: &Value) -> Result<String, String> {
    let key = match value {
        Value::String(text) => text.clone(),
        Value::Number(_) | Value::Bool(_) => value.to_string(),
        _ => return Err("a key must be a text, a number or a boolean".into()),
    };
    if key.trim().is_empty() {
        return Err("a key cannot be empty".into());
    }
    if key.chars().any(char::is_control) {
        return Err("a key cannot hold control characters".into());
    }
    Ok(key)
}

/// The record's key: the value at `key_path` when it is set, else `fixed` when it is
/// set, else a hash of the record.
pub(crate) fn key_of(record: &Value, key_path: &str, fixed: &str) -> Result<String, String> {
    if !key_path.trim().is_empty() {
        let found = at(record, key_path).ok_or_else(|| format!("no key at `{key_path}`"))?;
        return key_text(found);
    }
    if !fixed.trim().is_empty() {
        return Ok(fixed.to_string());
    }
    Ok(digest(record))
}

pub(crate) fn cut(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn paths_follow_pointers_and_dots() {
        let value = json!({ "a": { "b": [10, { "c": "x" }] } });
        assert_eq!(at(&value, "/a/b/1/c"), Some(&json!("x")));
        assert_eq!(at(&value, "a.b.0"), Some(&json!(10)));
        assert_eq!(at(&value, "a.z"), None);
        assert_eq!(at(&value, ""), Some(&value));
    }

    #[test]
    fn equal_values_hash_alike_whatever_the_key_order() {
        let one: Value = serde_json::from_str(r#"{"b":1,"a":{"y":2,"x":[1,2]}}"#).unwrap();
        let other: Value = serde_json::from_str(r#"{"a":{"x":[1,2],"y":2},"b":1}"#).unwrap();
        assert_eq!(canonical(&one), r#"{"a":{"x":[1,2],"y":2},"b":1}"#);
        assert_eq!(digest(&one), digest(&other));
        assert_ne!(digest(&one), digest(&json!({ "a": 1 })));
        assert_eq!(digest(&one).len(), 32);
    }

    #[test]
    fn keys_come_from_the_path_the_constant_or_the_content() {
        let record = json!({ "id": 7, "name": "n" });
        assert_eq!(key_of(&record, "/id", "").unwrap(), "7");
        assert_eq!(key_of(&record, "", "page").unwrap(), "page");
        assert_eq!(key_of(&record, "", "").unwrap(), digest(&record));
        assert!(key_of(&record, "/missing", "").is_err());
        assert!(key_of(&json!({ "id": [1] }), "/id", "").is_err());
        assert!(key_of(&json!({ "id": " " }), "/id", "").is_err());
    }
}
