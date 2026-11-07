//! The part of JSON Schema a judge checks answers against: `type`, `enum`, `properties`,
//! `required`, `additionalProperties = false` and `items`. A server that enforces the
//! schema already complies; this catches the ones that do not (local models, tool-call
//! fallbacks, plugged-in engines).

use serde_json::Value;

/// Where and why a value does not fit a schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mismatch {
    /// `$.verdict`, `$.tags[0]`…
    pub path: String,
    /// What is wrong, without the offending value: safe to tell the model again.
    pub problem: String,
    /// The same with the value, for whoever reads the error.
    pub detail: String,
}

fn mismatch(path: &str, problem: String, value: Option<&Value>) -> Mismatch {
    let detail = match value {
        Some(value) => format!("{path} {problem}: {value}"),
        None => format!("{path} {problem}"),
    };
    Mismatch {
        path: path.into(),
        problem,
        detail,
    }
}

/// `Ok` if `value` fits `schema`; otherwise where and why it does not.
pub fn check(value: &Value, schema: &Value) -> Result<(), Mismatch> {
    at(value, schema, "$")
}

fn type_fits(value: &Value, name: &str) -> bool {
    match name {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "number" => value.is_number(),
        "integer" => value.is_i64() || value.is_u64(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

fn at(value: &Value, schema: &Value, path: &str) -> Result<(), Mismatch> {
    match schema.get("type") {
        Some(Value::String(name)) if !type_fits(value, name) => {
            return Err(mismatch(path, format!("is not {name}"), Some(value)));
        }
        Some(Value::Array(names))
            if !names
                .iter()
                .filter_map(Value::as_str)
                .any(|n| type_fits(value, n)) =>
        {
            let problem = format!("is none of {}", Value::from(names.clone()));
            return Err(mismatch(path, problem, Some(value)));
        }
        _ => {}
    }
    if let Some(Value::Array(allowed)) = schema.get("enum") {
        if !allowed.contains(value) {
            let problem = format!("is not one of {}", Value::from(allowed.clone()));
            return Err(mismatch(path, problem, Some(value)));
        }
    }
    if let Value::Object(object) = value {
        if let Some(Value::Array(required)) = schema.get("required") {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(mismatch(
                        &format!("{path}.{key}"),
                        "is missing".into(),
                        None,
                    ));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, item) in object {
            match properties.and_then(|p| p.get(key)) {
                Some(sub) => at(item, sub, &format!("{path}.{key}"))?,
                None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    let path = format!("{path}.{key}");
                    return Err(mismatch(&path, "is not allowed".into(), None));
                }
                None => {}
            }
        }
    }
    if let (Value::Array(items), Some(sub)) = (value, schema.get("items")) {
        for (index, item) in items.iter().enumerate() {
            at(item, sub, &format!("{path}[{index}]"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn checks_types_enums_required_and_extra_keys() {
        let schema = json!({
            "type": "object", "required": ["verdict", "feedback"], "additionalProperties": false,
            "properties": {
                "verdict": { "type": "string", "enum": ["pass", "fail"] },
                "feedback": { "type": "string" },
                "tags": { "type": "array", "items": { "type": ["string", "null"] } },
            },
        });
        assert!(check(&json!({ "verdict": "pass", "feedback": "ok" }), &schema).is_ok());
        let error = |v: Value| check(&v, &schema).unwrap_err().detail;
        assert!(error(json!({ "verdict": "maybe", "feedback": "" })).contains("$.verdict"));
        assert!(error(json!({ "verdict": "pass" })).contains("feedback is missing"));
        assert!(error(json!({ "verdict": "pass", "feedback": "", "x": 1 })).contains("$.x"));
        assert!(
            error(json!({ "verdict": "pass", "feedback": "", "tags": [1] })).contains("$.tags[0]")
        );
        assert!(error(json!("pass")).contains("not object"));
        // The problem never repeats the value back.
        let mismatch = check(&json!({ "verdict": "IGNORE ALL", "feedback": "" }), &schema);
        let mismatch = mismatch.unwrap_err();
        assert_eq!(mismatch.path, "$.verdict");
        assert!(!mismatch.problem.contains("IGNORE"));
        assert!(mismatch.detail.contains("IGNORE"));
    }
}
