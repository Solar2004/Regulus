//! `convert`: the compatibility box. It changes what a value is so that two ends that do
//! not fit can be wired: text and JSON, arrays and lines, any value into a template.
//!
//! Conversions are a table, and `auto` is another table from (input, output) capability
//! to conversion: a new one is a row, not a branch.

use regulus_core::ModuleError;
use regulus_proto::{capability, NodeEvent};
use serde_json::{json, Value};

use crate::rule::{self, text};
use crate::Here;

const OUT: &str = "out";
const AUTO: &str = "auto";
const SAME: &str = "same";
const TEMPLATE: &str = "template";
const YES: &str = "yes";
/// `{{.}}` in a template is the whole value.
const OPEN: &str = "{{";
const CLOSE: &str = "}}";

/// A node's settings as a conversion reads them.
pub(crate) struct Options {
    pub template: String,
    pub separator: String,
    pub pretty: bool,
}

type Run = fn(Value, &Options) -> Result<Value, String>;

/// Every conversion by name.
const CONVERSIONS: &[(&str, Run)] = &[
    (SAME, |v, _| Ok(v)),
    ("text_to_json", text_to_json),
    ("json_to_text", |v, o| {
        Ok(Value::String(to_text(&v, o.pretty)))
    }),
    ("join", join),
    ("split", |v, o| {
        let text = text(&v);
        Ok(json!(text.split(o.separator.as_str()).collect::<Vec<_>>()))
    }),
    (TEMPLATE, |v, o| Ok(Value::String(fill(&o.template, &v)))),
];

/// What `auto` picks for a value going from one capability to another. Pairs not here
/// pass the value as it is (`same`).
const AUTO_TABLE: &[(&str, &str, &str)] = &[
    (capability::TEXT, capability::JSON, "text_to_json"),
    (capability::JSON, capability::TEXT, "json_to_text"),
    (capability::PATH, capability::TEXT, SAME),
    (capability::TEXT, capability::PATH, SAME),
];

fn to_text(value: &Value, pretty: bool) -> String {
    match value {
        Value::String(s) => s.clone(),
        other if pretty => serde_json::to_string_pretty(other).unwrap_or_default(),
        other => other.to_string(),
    }
}

/// Text that reads as JSON becomes that JSON; any other text stays a JSON string.
fn text_to_json(value: Value, _: &Options) -> Result<Value, String> {
    Ok(match value {
        Value::String(s) => serde_json::from_str(s.trim()).unwrap_or(Value::String(s)),
        other => other,
    })
}

fn join(value: Value, options: &Options) -> Result<Value, String> {
    match value {
        Value::Array(items) => Ok(Value::String(
            items
                .iter()
                .map(|item| to_text(item, options.pretty))
                .collect::<Vec<_>>()
                .join(&options.separator),
        )),
        other => Ok(Value::String(to_text(&other, options.pretty))),
    }
}

/// Fills `{{field}}`, `{{a.b}}`, `{{/a/0}}` and `{{.}}` from `value`. What is missing
/// becomes empty; text that is not a placeholder stays as is.
pub(crate) fn fill(template: &str, value: &Value) -> String {
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find(OPEN) {
        out.push_str(&rest[..start]);
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find(CLOSE) else {
            out.push_str(&rest[start..]);
            return out;
        };
        let path = after[..end].trim();
        if let Some(found) = rule::at(value, path) {
            out.push_str(&text(found));
        }
        rest = &after[end + CLOSE.len()..];
    }
    out.push_str(rest);
    out
}

/// The conversion a node runs: its `conversion`, or, on `auto`, its template if it has
/// one, else the row of [`AUTO_TABLE`] for its capabilities.
fn pick<'a>(conversion: &'a str, template: &str, from: &str, to: &str) -> &'a str {
    if conversion != AUTO && !conversion.is_empty() {
        return conversion;
    }
    if !template.is_empty() {
        return TEMPLATE;
    }
    AUTO_TABLE
        .iter()
        .find(|(f, t, _)| *f == from && *t == to)
        .map_or(SAME, |(_, _, name)| name)
}

pub(crate) fn convert(name: &str, value: Value, options: &Options) -> Result<Value, ModuleError> {
    let (_, run) = CONVERSIONS
        .iter()
        .find(|(n, _)| *n == name)
        .ok_or_else(|| {
            let known: Vec<&str> = CONVERSIONS.iter().map(|(n, _)| *n).collect();
            ModuleError::InvalidParams(format!(
                "unknown conversion `{name}` (known: {AUTO}, {})",
                known.join(", ")
            ))
        })?;
    run(value, options).map_err(ModuleError::InvalidParams)
}

pub(crate) fn event(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    let options = Options {
        template: here.text("template"),
        separator: here.text("separator"),
        pretty: here.text("pretty") == YES,
    };
    let conversion = here.text("conversion");
    let to = here.capability(OUT).unwrap_or_default();
    let name = pick(&conversion, &options.template, &event.capability, to);
    let value = convert(name, event.value, &options)?;
    here.emit(OUT, value)?;
    Ok(json!({ "conversion": name }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> Options {
        Options {
            template: String::new(),
            separator: "\n".into(),
            pretty: false,
        }
    }

    #[test]
    fn auto_picks_by_capabilities_and_a_template_wins() {
        assert_eq!(pick(AUTO, "", "text@1", "json@1"), "text_to_json");
        assert_eq!(pick(AUTO, "", "json@1", "text@1"), "json_to_text");
        assert_eq!(pick(AUTO, "", "image@1", "text@1"), SAME);
        assert_eq!(pick(AUTO, "{{.}}", "text@1", "json@1"), TEMPLATE);
        assert_eq!(pick("join", "{{.}}", "json@1", "text@1"), "join");
    }

    #[test]
    fn each_conversion_does_what_it_says() {
        let o = options();
        assert_eq!(
            convert("text_to_json", json!(" {\"a\":1} "), &o).unwrap(),
            json!({ "a": 1 })
        );
        assert_eq!(
            convert("text_to_json", json!("plain"), &o).unwrap(),
            json!("plain")
        );
        assert_eq!(
            convert("json_to_text", json!({ "a": 1 }), &o).unwrap(),
            json!("{\"a\":1}")
        );
        assert_eq!(
            convert("join", json!(["a", 2, "c"]), &o).unwrap(),
            json!("a\n2\nc")
        );
        assert_eq!(
            convert("split", json!("a\nb"), &o).unwrap(),
            json!(["a", "b"])
        );
        assert!(convert("rot13", json!("x"), &o).is_err());
    }

    #[test]
    fn templates_fill_fields_paths_and_the_whole_value() {
        let value = json!({ "name": "Ana", "turn": { "n": 2 }, "tags": ["x"] });
        assert_eq!(
            fill("{{name}} at {{turn.n}} {{/tags/0}}{{missing}}!", &value),
            "Ana at 2 x!"
        );
        assert_eq!(fill("> {{ . }}", &json!("hi")), "> hi");
        assert_eq!(
            fill("open {{ never closed", &json!(1)),
            "open {{ never closed"
        );
    }
}
