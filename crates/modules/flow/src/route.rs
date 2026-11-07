//! `switch` and `fanout`: where one value goes.

use regulus_core::ModuleError;
use regulus_proto::{Direction, NodeEvent};
use serde_json::Value;

use crate::rule::{self, Rule};
use crate::Here;

/// `switch` modes.
const RULES: &str = "rules";
const ENUM: &str = "enum";
/// `match = all`: every case that matches, not only the first.
const ALL: &str = "all";
/// `fanout` mode that sends each element of an array on its own.
const SPLIT: &str = "split";

pub(crate) fn switch(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    let outputs = here.then_ports(Direction::Output);
    let mut ports = match here.text("mode").as_str() {
        ENUM => by_enum(
            &event.value,
            &here.text("path"),
            &outputs,
            &here.text("fallback"),
        ),
        RULES | "" => {
            let cases = Rule::list(here.setting("cases"))?;
            by_rules(&event.value, &cases, here.text("match") == ALL, &outputs)?
        }
        other => {
            return Err(ModuleError::InvalidParams(format!(
                "switch mode is `{RULES}` or `{ENUM}`, not `{other}`"
            )))
        }
    };
    let fallback = here.text("fallback");
    if ports.is_empty() && outputs.contains(&fallback) {
        ports.push(fallback);
    }
    here.send(
        ports
            .into_iter()
            .map(|p| (p, event.value.clone()))
            .collect(),
    )
}

/// The output named by the value at `path`, if the node has it (and it is not the
/// fallback, which is only for what matches nothing).
fn by_enum(value: &Value, path: &str, outputs: &[String], fallback: &str) -> Vec<String> {
    rule::at(value, path)
        .map(rule::text)
        .filter(|name| name != fallback && outputs.contains(name))
        .into_iter()
        .collect()
}

/// The outputs of the cases `value` meets, in order; only the first unless `all`.
fn by_rules(
    value: &Value,
    cases: &[Rule],
    all: bool,
    outputs: &[String],
) -> Result<Vec<String>, ModuleError> {
    let mut ports = Vec::new();
    for case in cases {
        if !outputs.contains(&case.port) {
            return Err(ModuleError::InvalidParams(format!(
                "a case goes to `{}`, which is not a \"then\" output of the node",
                case.port
            )));
        }
        if case.test(value)? && !ports.contains(&case.port) {
            ports.push(case.port.clone());
            if !all {
                break;
            }
        }
    }
    Ok(ports)
}

pub(crate) fn fanout(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    let outputs = here.then_ports(Direction::Output);
    here.send(spread(event.value, here.text("mode") == SPLIT, &outputs))
}

/// The value to every output; split, each element of an array to every output in turn.
fn spread(value: Value, split: bool, outputs: &[String]) -> Vec<(String, Value)> {
    let values = match value {
        Value::Array(items) if split => items,
        other => vec![other],
    };
    values
        .into_iter()
        .flat_map(|v| outputs.iter().map(move |p| (p.clone(), v.clone())))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ports(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn rules_route_to_the_first_match_or_to_all() {
        let cases = Rule::list(Some(&json!([
            { "port": "pass", "op": "contains", "value": "PASS" },
            { "port": "long", "op": "regex", "value": ".{10,}" },
        ])))
        .unwrap();
        let outs = ports(&["pass", "long", "other"]);
        let value = json!("tests PASS fine");
        assert_eq!(by_rules(&value, &cases, false, &outs).unwrap(), ["pass"]);
        assert_eq!(
            by_rules(&value, &cases, true, &outs).unwrap(),
            ["pass", "long"]
        );
        assert!(by_rules(&json!("x"), &cases, true, &outs)
            .unwrap()
            .is_empty());
        let missing = by_rules(&value, &cases, false, &ports(&["long"]));
        assert!(missing.is_err());
    }

    #[test]
    fn enum_mode_names_the_output_from_the_value() {
        let outs = ports(&["approve", "reject", "other"]);
        let value = json!({ "verdict": "reject" });
        assert_eq!(by_enum(&value, "verdict", &outs, "other"), ["reject"]);
        assert!(by_enum(&json!({ "verdict": "maybe" }), "verdict", &outs, "other").is_empty());
        assert!(by_enum(&json!("other"), "", &outs, "other").is_empty());
    }

    #[test]
    fn fanout_copies_or_splits() {
        let outs = ports(&["a", "b"]);
        assert_eq!(
            spread(json!("x"), false, &outs),
            [("a".into(), json!("x")), ("b".into(), json!("x"))]
        );
        let split = spread(json!([1, 2]), true, &ports(&["a"]));
        assert_eq!(split, [("a".into(), json!(1)), ("a".into(), json!(2))]);
        // Splitting something that is not an array sends it whole.
        assert_eq!(spread(json!("x"), true, &ports(&["a"])).len(), 1);
    }
}
