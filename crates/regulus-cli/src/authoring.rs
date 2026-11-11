//! What the authoring commands print and decide (docs/18-custom-nodes.md): `regulus node
//! source|save|check`, `project reload`, `profile sync|check` and `module shape`. The core
//! does the work and owns the rules (source revisions, the non-executing syntax check, the
//! one shape resolver, what a reload does to live executions); this module only turns its
//! JSON answers into lines and exit decisions. They are plain functions over JSON, so the
//! tests need no core.

use serde_json::Value;

/// The JSON-RPC code of a refused reload or init because live executions use the module.
pub const MODULE_IN_USE: i32 = -32014;
/// The JSON-RPC code of a save over a file that changed since it was read.
pub const SOURCE_CONFLICT: i32 = -32015;

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn list<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value
        .get(key)
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// The lines for a `SourceCheck` of `path`, and how many diagnostics are errors.
/// `project` and `module` name the reload that would apply the file.
pub fn check_lines(path: &str, project: &str, module: &str, check: &Value) -> (Vec<String>, usize) {
    let mut lines = Vec::new();
    let mut errors = 0;
    for d in list(check, "diagnostics") {
        let severity = text(d, "severity");
        if severity == "error" {
            errors += 1;
        }
        let at = match (
            d.get("line").and_then(Value::as_u64),
            d.get("column").and_then(Value::as_u64),
        ) {
            (Some(line), Some(column)) => format!(":{line}:{column}"),
            (Some(line), None) => format!(":{line}"),
            _ => String::new(),
        };
        lines.push(format!(
            "{path}{at}: {severity}[{}]: {}",
            text(d, "code"),
            text(d, "message")
        ));
    }
    if check.get("checked").and_then(Value::as_bool) == Some(false) {
        lines.push(format!("{path}: not checked (see the warning above)"));
    }
    if check.get("complete").and_then(Value::as_bool) == Some(false) {
        lines.push(format!(
            "{path}: the node types could only be read in part: an argument is not a literal"
        ));
    }
    if check.get("matches_loaded").and_then(Value::as_bool) == Some(false) {
        lines.push(format!(
            "{path}: differs from what is loaded; apply it with: regulus project reload {project} --module {module}"
        ));
    }
    (lines, errors)
}

/// The lines for the live executions that keep a module from being replaced, from the
/// `data` of a `MODULE_IN_USE` error.
pub fn in_use_lines(data: &Value) -> Vec<String> {
    let mut lines = vec!["nothing changed: live executions use the module:".to_string()];
    for execution in list(data, "executions") {
        let nodes: Vec<&str> = list(execution, "nodes")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        lines.push(format!(
            "  {} (profile {}, workflow {}): nodes {}",
            text(execution, "id"),
            text(execution, "profile_id"),
            text(execution, "workflow_id"),
            nodes.join(", ")
        ));
    }
    lines.push("stop them first, or add --stop-executions".into());
    lines
}

/// The lines for a `ProjectReloaded`, and whether every module is now running its current
/// files. A module that was `kept`, `failed`, `blocked` or `skipped` is not.
pub fn reload_lines(project: &str, reloaded: &Value) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut ok = true;
    for m in list(reloaded, "modules") {
        let outcome = text(m, "outcome");
        let id = match text(m, "id") {
            "" => text(m, "local_id"),
            id => id,
        };
        // A module state is `{ "status": "idle", … }`; a bare string is accepted too.
        let state = match m.get("state") {
            Some(Value::String(status)) => status.as_str(),
            Some(state) => text(state, "status"),
            None => "",
        };
        lines.push(format!("{outcome:<9} {id}  {state}").trim_end().to_string());
        if let Some(error) = m.get("error").and_then(Value::as_str) {
            lines.push(format!("          {error}"));
        }
        if outcome == "blocked" {
            lines.push(format!(
                "          untrusted project: regulus project trust <folder-of-{project}>"
            ));
        }
        let types = m.get("node_types").unwrap_or(&Value::Null);
        for (key, label) in [
            ("added", "added"),
            ("removed", "removed"),
            ("changed", "changed"),
        ] {
            let names: Vec<&str> = list(types, key).iter().filter_map(Value::as_str).collect();
            if !names.is_empty() {
                lines.push(format!(
                    "          node types {label}: {}",
                    names.join(", ")
                ));
            }
        }
        if !matches!(outcome, "started" | "replaced" | "removed") {
            ok = false;
        }
    }
    for id in list(reloaded, "stopped_executions")
        .iter()
        .filter_map(Value::as_str)
    {
        lines.push(format!("stopped   {id}"));
    }
    let mut profiles: Vec<&str> = Vec::new();
    for s in list(reloaded, "stale") {
        let profile = text(s, "profile_id");
        if !profiles.contains(&profile) {
            profiles.push(profile);
        }
    }
    for profile in profiles {
        lines.push(format!(
            "stale ports in profile {profile}: regulus profile sync {profile}"
        ));
    }
    (lines, ok)
}

fn port_line(port: &Value) -> String {
    let direction = match text(port, "direction") {
        "input" => "in ",
        "output" => "out",
        other => other,
    };
    format!(
        "{direction} {:<18} {:<6} {}",
        text(port, "id"),
        text(port, "kind"),
        text(port, "capability")
    )
}

/// The lines for a `ProfileSynced` plan: what each node would gain, lose and change, and
/// which wires would go with it.
pub fn sync_lines(synced: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    let nodes = list(synced, "nodes");
    if nodes.is_empty() {
        lines.push("ports are in sync: nothing to change".into());
    }
    for n in nodes {
        lines.push(format!(
            "{}/{} ({} {})",
            text(n, "workflow_id"),
            text(n, "node_id"),
            text(n, "module_id"),
            text(n, "node_type")
        ));
        for p in list(n, "added") {
            lines.push(format!("  + {}", port_line(p)));
        }
        for p in list(n, "removed") {
            lines.push(format!("  - {}", port_line(p)));
        }
        for c in list(n, "changed") {
            lines.push(format!(
                "  ~ {}  =>  {}",
                port_line(c.get("before").unwrap_or(&Value::Null)),
                port_line(c.get("after").unwrap_or(&Value::Null))
            ));
        }
        for w in list(n, "wires_removed") {
            let wire = w.get("wire").unwrap_or(&Value::Null);
            let end = |key: &str| {
                let e = wire.get(key).unwrap_or(&Value::Null);
                format!("{}.{}", text(e, "node_id"), text(e, "port_id"))
            };
            lines.push(format!(
                "  wire {} ({} -> {}) would be removed: {}",
                text(wire, "id"),
                end("from"),
                end("to"),
                text(w, "reason")
            ));
        }
        for p in list(n, "problems").iter().filter_map(Value::as_str) {
            lines.push(format!("  problem: {p}"));
        }
    }
    lines
}

/// Why `--apply` must not go on, or `None` when it may: wires would be removed and
/// `--drop-wires` was not given.
pub fn apply_refusal(synced: &Value, drop_wires: bool) -> Option<String> {
    if synced.get("drops_wires").and_then(Value::as_bool) == Some(true) && !drop_wires {
        return Some(
            "applying removes wires (listed above); nothing was saved. Add --drop-wires to accept that"
                .into(),
        );
    }
    None
}

/// The lines for a `NodeShape`.
pub fn shape_lines(shape: &Value) -> Vec<String> {
    let mut lines: Vec<String> = list(shape, "ports").iter().map(port_line).collect();
    let hidden: Vec<&str> = list(shape, "hidden_fields")
        .iter()
        .filter_map(Value::as_str)
        .collect();
    if !hidden.is_empty() {
        lines.push(format!("hidden fields: {}", hidden.join(", ")));
    }
    for p in list(shape, "problems").iter().filter_map(Value::as_str) {
        lines.push(format!("problem: {p}"));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_check_prints_position_code_and_the_reload_to_run() {
        let check = json!({
            "checked": true,
            "complete": true,
            "matches_loaded": false,
            "diagnostics": [
                { "severity": "error", "code": "python.syntax", "message": "invalid syntax", "line": 3, "column": 5 },
                { "severity": "warning", "code": "python.no_run", "message": "no rg.run()" },
            ],
        });
        let (lines, errors) = check_lines("tools/caps/run.py", "demo", "caps", &check);
        assert_eq!(errors, 1);
        assert_eq!(
            lines[0],
            "tools/caps/run.py:3:5: error[python.syntax]: invalid syntax"
        );
        assert_eq!(
            lines[1],
            "tools/caps/run.py: warning[python.no_run]: no rg.run()"
        );
        assert!(lines[2].ends_with("regulus project reload demo --module caps"));
        let (lines, errors) = check_lines("x.py", "p", "m", &json!({ "checked": false }));
        assert_eq!(errors, 0);
        assert!(lines[0].contains("not checked"), "{lines:?}");
    }

    #[test]
    fn a_reload_is_ok_only_when_every_module_runs_the_new_files() {
        let reloaded = json!({
            "modules": [
                { "local_id": "a", "id": "demo/a@0.1.0", "outcome": "replaced", "state": "idle",
                  "node_types": { "added": ["t"], "removed": [], "changed": ["a"] } },
                { "local_id": "b", "id": "demo/b@0.1.0", "outcome": "kept",
                  "state": { "status": "active", "since": 1 }, "error": "manifest: bad json" },
            ],
            "stopped_executions": ["exec-3"],
            "stale": [
                { "profile_id": "p", "workflow_id": "main", "node_id": "n1" },
                { "profile_id": "p", "workflow_id": "main", "node_id": "n2" },
            ],
        });
        let (lines, ok) = reload_lines("demo", &reloaded);
        assert!(!ok);
        assert!(
            lines
                .iter()
                .any(|l| l.starts_with("replaced  demo/a@0.1.0")),
            "{lines:?}"
        );
        assert!(lines.iter().any(|l| l.contains("added: t")), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.contains("manifest: bad json")),
            "{lines:?}"
        );
        assert!(lines.contains(&"stopped   exec-3".to_string()), "{lines:?}");
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with("stale ports"))
                .count(),
            1,
            "one line per profile: {lines:?}"
        );
        let started =
            json!({ "modules": [{ "id": "demo/a@0.1.0", "outcome": "started", "state": "idle" }] });
        assert!(reload_lines("demo", &started).1);
        let blocked =
            json!({ "modules": [{ "id": "demo/a", "outcome": "blocked", "state": "blocked" }] });
        let (lines, ok) = reload_lines("demo", &blocked);
        assert!(!ok);
        assert!(
            lines.iter().any(|l| l.contains("regulus project trust")),
            "{lines:?}"
        );
    }

    #[test]
    fn the_executions_that_hold_a_module_are_listed() {
        let lines = in_use_lines(&json!({ "executions": [
            { "id": "exec-1", "profile_id": "p", "workflow_id": "main", "nodes": ["a", "b"] }
        ] }));
        assert!(
            lines[1].contains("exec-1") && lines[1].contains("nodes a, b"),
            "{lines:?}"
        );
        assert!(lines.last().unwrap().contains("--stop-executions"));
    }

    #[test]
    fn a_sync_plan_lists_ports_and_wires_and_refuses_to_drop_wires_unasked() {
        let port = |id: &str| json!({ "id": id, "direction": "input", "kind": "then", "capability": "text@1" });
        let synced = json!({
            "drops_wires": true,
            "nodes": [{
                "workflow_id": "main", "node_id": "caps", "module_id": "demo/caps", "node_type": "caps",
                "added": [port("new")], "removed": [port("old")],
                "changed": [{ "before": port("x"), "after": port("x") }],
                "wires_removed": [{
                    "wire": { "id": "w1", "from": { "node_id": "a", "port_id": "out" }, "to": { "node_id": "caps", "port_id": "old" } },
                    "reason": "port `old` removed",
                }],
            }],
        });
        let lines = sync_lines(&synced);
        assert_eq!(lines[0], "main/caps (demo/caps caps)");
        assert!(
            lines.iter().any(|l| l.starts_with("  + in  new")),
            "{lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("  - in  old")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l
                    .contains("wire w1 (a.out -> caps.old) would be removed: port `old` removed")),
            "{lines:?}"
        );
        assert!(apply_refusal(&synced, false).is_some());
        assert!(apply_refusal(&synced, true).is_none());
        assert!(apply_refusal(&json!({ "drops_wires": false }), false).is_none());
        assert_eq!(
            sync_lines(&json!({ "nodes": [] })),
            ["ports are in sync: nothing to change"]
        );
    }

    #[test]
    fn a_shape_prints_its_ports_hidden_fields_and_problems() {
        let lines = shape_lines(&json!({
            "ports": [{ "id": "in", "direction": "input", "kind": "then", "capability": "text@1" }],
            "hidden_fields": ["advanced"],
            "problems": ["item `a b` is not a valid id"],
        }));
        assert_eq!(lines[0], "in  in                 then   text@1");
        assert_eq!(lines[1], "hidden fields: advanced");
        assert_eq!(lines[2], "problem: item `a b` is not a valid id");
    }
}
