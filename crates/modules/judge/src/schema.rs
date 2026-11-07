//! The part of JSON Schema a judge checks answers against: `type`, `enum`, `properties`,
//! `required`, `additionalProperties = false` and `items`. A server that enforces the
//! schema already complies; this catches the ones that do not (local models, tool-call
//! fallbacks, plugged-in engines).

use serde_json::Value;

/// `Ok` if `value` fits `schema`; otherwise where and why it does not.
pub fn check(value: &Value, schema: &Value) -> Result<(), String> {
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

fn at(value: &Value, schema: &Value, path: &str) -> Result<(), String> {
    match schema.get("type") {
        Some(Value::String(name)) if !type_fits(value, name) => {
            return Err(format!("{path} is not {name}: {value}"));
        }
        Some(Value::Array(names))
            if !names
                .iter()
                .filter_map(Value::as_str)
                .any(|n| type_fits(value, n)) =>
        {
            return Err(format!(
                "{path} is none of {}: {value}",
                Value::from(names.clone())
            ));
        }
        _ => {}
    }
    if let Some(Value::Array(allowed)) = schema.get("enum") {
        if !allowed.contains(value) {
            return Err(format!(
                "{path} = {value} is not one of {}",
                Value::from(allowed.clone())
            ));
        }
    }
    if let Value::Object(object) = value {
        if let Some(Value::Array(required)) = schema.get("required") {
            for key in required.iter().filter_map(Value::as_str) {
                if !object.contains_key(key) {
                    return Err(format!("{path}.{key} is missing"));
                }
            }
        }
        let properties = schema.get("properties").and_then(Value::as_object);
        for (key, item) in object {
            match properties.and_then(|p| p.get(key)) {
                Some(sub) => at(item, sub, &format!("{path}.{key}"))?,
                None if schema.get("additionalProperties") == Some(&Value::Bool(false)) => {
                    return Err(format!("{path}.{key} is not allowed"));
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
        let error = |v: Value| check(&v, &schema).unwrap_err();
        assert!(error(json!({ "verdict": "maybe", "feedback": "" })).contains("$.verdict"));
        assert!(error(json!({ "verdict": "pass" })).contains("feedback is missing"));
        assert!(error(json!({ "verdict": "pass", "feedback": "", "x": 1 })).contains("$.x"));
        assert!(
            error(json!({ "verdict": "pass", "feedback": "", "tags": [1] })).contains("$.tags[0]")
        );
        assert!(error(json!("pass")).contains("not object"));
    }
}
