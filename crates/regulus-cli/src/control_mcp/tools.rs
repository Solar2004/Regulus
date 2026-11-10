//! The tools the control server offers: one table, so the list an agent sees, the
//! arguments it is held to and the core method each one reaches cannot drift apart.
//!
//! There is no tool that forwards an arbitrary method, and none for secrets, project
//! trust or opening projects: those stay with the user (docs/17-control-mcp.md §4).

use serde_json::{json, Map, Value};

/// What a tool can do to the host, as MCP annotations say it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    /// Looks, changes nothing.
    Read,
    /// Changes the host. `destructive`: it can replace or end something that existed.
    Write { destructive: bool, idempotent: bool },
}

/// What a tool's call is checked against the denied modules (`--deny-module`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Guard {
    /// Nothing to check.
    None,
    /// The `profile` argument, whole.
    ProfileArg,
    /// The template named by the `template` argument (and the values of `vars`).
    Template,
    /// The stored profile named by the `profile_id` argument.
    StoredProfile,
}

/// What a tool does when called.
pub enum Op {
    /// The core, its protocol and a count of modules and executions.
    Status,
    /// The recipe for writing a local module (text, nothing is written or run).
    Guide,
    /// One core method. `rename` maps an argument to the parameter the core calls it;
    /// `names_only` is an argument that must be a name, never a path; `execution` marks a
    /// method that returns an execution: one that came back `failed` is a tool error, and
    /// nodes it left off are explained.
    Rpc {
        method: &'static str,
        rename: &'static [(&'static str, &'static str)],
        names_only: Option<&'static str>,
        execution: bool,
        guard: Guard,
    },
}

pub struct Tool {
    pub name: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub access: Access,
    /// Whether a workflow it starts or feeds may reach outside the machine (web, models).
    pub open_world: bool,
    /// Said after the core's own message when the call fails.
    pub hint: &'static str,
    pub schema: Value,
    pub op: Op,
}

impl Tool {
    pub fn writes(&self) -> bool {
        matches!(self.access, Access::Write { .. })
    }

    /// The entry of `tools/list`.
    pub fn listed(&self) -> Value {
        let mut annotations = json!({ "title": self.title, "openWorldHint": self.open_world });
        match self.access {
            Access::Read => annotations["readOnlyHint"] = json!(true),
            Access::Write {
                destructive,
                idempotent,
            } => {
                annotations["readOnlyHint"] = json!(false);
                annotations["destructiveHint"] = json!(destructive);
                annotations["idempotentHint"] = json!(idempotent);
            }
        }
        json!({
            "name": self.name,
            "title": self.title,
            "description": self.description,
            "inputSchema": self.schema,
            "annotations": annotations,
        })
    }

    /// Checks `args` against the schema and returns them with the defaults filled in. Only
    /// the shape is checked here (known names, required ones, JSON types); what the values
    /// mean is the core's to say.
    pub fn prepare(&self, args: Value) -> Result<Map<String, Value>, String> {
        let mut args = match args {
            Value::Null => Map::new(),
            Value::Object(map) => map,
            other => return Err(format!("arguments must be a JSON object, got {other}")),
        };
        let properties = self.schema["properties"].as_object();
        let known = |key: &str| properties.is_some_and(|p| p.contains_key(key));
        if let Some(extra) = args.keys().find(|key| !known(key)) {
            let accepted: Vec<&str> = properties
                .into_iter()
                .flatten()
                .map(|(key, _)| key.as_str())
                .collect();
            let accepted = if accepted.is_empty() {
                "none".to_string()
            } else {
                accepted.join(", ")
            };
            return Err(format!(
                "unknown argument `{extra}` for `{}` (accepted: {accepted})",
                self.name
            ));
        }
        for key in self.schema["required"].as_array().into_iter().flatten() {
            let key = key.as_str().unwrap_or_default();
            if !args.contains_key(key) {
                return Err(format!(
                    "missing required argument `{key}` for `{}`",
                    self.name
                ));
            }
        }
        let mut integers = Vec::new();
        for (key, property) in properties.into_iter().flatten() {
            match args.get(key) {
                Some(value) => {
                    if let Some(wanted) = property.get("type") {
                        if !fits(wanted, value) {
                            return Err(format!(
                                "argument `{key}` must be {}, got {}",
                                describe(wanted),
                                kind(value)
                            ));
                        }
                    }
                    if let Some(options) = property.get("enum").and_then(Value::as_array) {
                        if !options.contains(value) {
                            return Err(format!(
                                "argument `{key}` must be one of {}, got {value}",
                                Value::Array(options.clone())
                            ));
                        }
                    }
                    if let Some(whole) =
                        integral(value).filter(|_| !value.is_i64() && !value.is_u64())
                    {
                        // The core reads integers; 5.0 goes as 5.
                        integers.push((key.clone(), whole));
                    }
                }
                None => {
                    if let Some(default) = property.get("default") {
                        args.insert(key.clone(), default.clone());
                    }
                }
            }
        }
        for (key, whole) in integers {
            args.insert(key, json!(whole));
        }
        Ok(args)
    }
}

fn fits(wanted: &Value, value: &Value) -> bool {
    match wanted {
        Value::Array(alternatives) => alternatives.iter().any(|w| fits(w, value)),
        Value::String(name) => match name.as_str() {
            "string" => value.is_string(),
            "boolean" => value.is_boolean(),
            "integer" => value.is_u64() || value.is_i64() || integral(value).is_some(),
            "number" => value.is_number(),
            "object" => value.is_object(),
            "array" => value.is_array(),
            "null" => value.is_null(),
            _ => true,
        },
        _ => true,
    }
}

/// A number written as a float that is a whole number (`5.0`), as an integer.
fn integral(value: &Value) -> Option<i64> {
    let float = value.as_f64()?;
    (float.fract() == 0.0 && float.abs() < 9.0e15).then_some(float as i64)
}

fn describe(wanted: &Value) -> String {
    match wanted {
        Value::Array(alternatives) => alternatives
            .iter()
            .map(describe)
            .collect::<Vec<_>>()
            .join(" or "),
        Value::String(name) => format!("a(n) {name}"),
        other => other.to_string(),
    }
}

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    }
}

/// An argument that must name something the core already lists, not point at a file.
pub fn check_name(tool: &str, key: &str, value: &Value) -> Result<(), String> {
    let text = value.as_str().unwrap_or_default();
    if text.contains('/') || text.contains('\\') || text.ends_with(".toml") {
        return Err(format!(
            "`{key}` of `{tool}` is a name from `template_list`, not a path (got `{text}`)"
        ));
    }
    Ok(())
}

fn object(properties: Value, required: &[&str]) -> Value {
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

fn text(description: &str) -> Value {
    json!({ "type": "string", "description": description })
}

fn project_filter() -> Value {
    object(
        json!({ "project_id": text("Only this open project's entries; omit for all.") }),
        &[],
    )
}

fn port_schema() -> Value {
    json!({
        "type": "object",
        "description": "An input or output of a node or workflow.",
        "properties": {
            "id": { "type": "string" },
            "name": { "type": "string" },
            "capability": { "type": "string", "description": "Value type such as text@1; wires must match." },
            "direction": { "enum": ["input", "output"] },
            "kind": { "enum": ["then", "uses"], "description": "then = events on the bus; uses = service link." },
            "required": { "type": "boolean" },
            "cardinality": { "enum": ["one", "many"] },
            "lazy": { "type": "boolean" },
        },
        "required": ["id", "capability", "direction", "kind"],
    })
}

fn port_ref() -> Value {
    json!({
        "type": "object",
        "properties": {
            "node_id": { "type": "string", "description": "A node id, or `self` for a port of the workflow." },
            "port_id": { "type": "string" },
        },
        "required": ["node_id", "port_id"],
    })
}

/// A whole profile, as `profile_get` returns it and `profile_save` takes it. Written by
/// hand from `regulus_proto::Profile`; the core is the one that validates.
fn profile_schema() -> Value {
    json!({
        "type": "object",
        "description": "A whole profile, as profile_get returns it. id becomes the file name.",
        "properties": {
            "id": { "type": "string" },
            "name": { "type": "string" },
            "project_id": { "type": ["string", "null"], "description": "Open project it belongs to; null = global." },
            "main_workflow_id": { "type": "string" },
            "workflows": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "kind": { "enum": ["main", "extra", "module"] },
                        "ports": { "type": "array", "items": port_schema(), "description": "The workflow's own inputs/outputs; inner wires reach them as node `self`." },
                        "nodes": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "type": { "type": "string", "description": "A node type from module_manifest." },
                                    "name": { "type": "string" },
                                    "module_id": { "type": "string", "description": "author/module or author/module@version, from module_list." },
                                    "config": { "type": "object", "description": "Values of the node type's fields; `secret:<name>` references a secret." },
                                    "ports": { "type": "array", "items": port_schema() },
                                    "internal_workflow_id": { "type": "string", "description": "A workflow of this profile placed as a box." },
                                    "position": { "type": "object", "properties": { "x": { "type": "number" }, "y": { "type": "number" } } },
                                },
                                "required": ["id", "type"],
                            },
                        },
                        "wires": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": { "type": "string" },
                                    "kind": { "enum": ["then", "uses"] },
                                    "from": port_ref(),
                                    "to": port_ref(),
                                },
                                "required": ["id", "kind", "from", "to"],
                            },
                        },
                    },
                    "required": ["id", "kind"],
                },
            },
        },
        "required": ["id", "main_workflow_id", "workflows"],
    })
}

const NO_PATHS: &str = "Pass a name from template_list.";

fn read(
    name: &'static str,
    title: &'static str,
    description: &'static str,
    hint: &'static str,
    schema: Value,
    method: &'static str,
) -> Tool {
    Tool {
        name,
        title,
        description,
        access: Access::Read,
        open_world: false,
        hint,
        schema,
        op: Op::Rpc {
            method,
            rename: &[],
            names_only: None,
            execution: false,
            guard: Guard::None,
        },
    }
}

/// Every tool, in the order an agent sees them.
pub fn catalog(default_events: usize) -> Vec<Tool> {
    use regulus_proto::method;
    let id_of = |what: &str| object(json!({ "id": text(what) }), &["id"]);
    vec![
        Tool {
            name: "status",
            title: "Core status",
            description: "Start here: the core and protocol versions, whether this server is \
                          read-only, how many modules are in each state (and which are broken, \
                          blocked or incompatible) and how many executions are live.",
            access: Access::Read,
            open_world: false,
            hint: "Is the core running? Start it with `regulus --home <home> serve`.",
            schema: object(json!({}), &[]),
            op: Op::Status,
        },
        read(
            "module_list",
            "List modules",
            "Every module the core loaded, with its state (idle, active, disabled, broken, \
             incompatible or blocked, with the reason) and the node types it brings. Local \
             modules of a project show as `<project>/<id>`.",
            "",
            object(json!({}), &[]),
            method::MODULE_LIST,
        ),
        read(
            "module_manifest",
            "Module manifest",
            "The manifest of one module (node types, ports, fields, RPC methods) or, without \
             `id`, of all of them. This is what to read before wiring a node into a profile.",
            "List the ids with module_list; use author/module, with or without @version.",
            object(
                json!({ "id": text("Module id, e.g. regulus/hello; omit for every module.") }),
                &[],
            ),
            method::MODULE_MANIFEST,
        ),
        read(
            "project_list",
            "List projects",
            "The open projects (folders with a regulus.toml), with warnings or the error that \
             kept one from opening.",
            "",
            object(json!({}), &[]),
            method::PROJECT_LIST,
        ),
        read(
            "project_get",
            "Show a project",
            "One open project: its folder, profile and template folders, vetoed modules, local \
             module entries and regulus.toml content.",
            "List the ids with project_list.",
            id_of("Project id."),
            method::PROJECT_GET,
        ),
        read(
            "profile_list",
            "List profiles",
            "The loaded profiles (id, name, project).",
            "",
            project_filter(),
            method::PROFILE_LIST,
        ),
        read(
            "profile_get",
            "Show a profile",
            "A whole profile: its workflows, nodes, ports and wires, in the shape profile_save takes.",
            "List the ids with profile_list.",
            id_of("Profile id."),
            method::PROFILE_GET,
        ),
        read(
            "profile_check",
            "Check a profile",
            "The core's diagnostics for a loaded profile: errors, warnings and information \
             (unknown modules, missing ports, mismatched wires, required inputs left open, \
             vetoed or unavailable modules).",
            "List the ids with profile_list.",
            id_of("Profile id."),
            method::PROFILE_CHECK,
        ),
        Tool {
            name: "profile_save",
            title: "Create or update a profile",
            description: "Has the core check a whole profile and, only if it has no errors, \
                          write it to the profiles folder and load it. Errors (a wire to a port \
                          that does not exist, mismatched capabilities, duplicate ids...) refuse \
                          it and nothing is written. Warnings and information come back in \
                          `diagnostics` and do not refuse it: a node of a module that is not \
                          installed is saved (`node.module_not_installed`) but will not run. A \
                          profile that already exists is replaced only with force.",
            access: Access::Write {
                destructive: true,
                idempotent: true,
            },
            open_world: false,
            hint: "Fix what the message lists and save again. Module ids, node types and \
                   ports are in module_list/module_manifest.",
            schema: object(
                json!({
                    "profile": profile_schema(),
                    "force": { "type": "boolean", "default": false, "description": "Replace a profile with the same id." },
                }),
                &["profile"],
            ),
            op: Op::Rpc {
                method: method::PROFILE_SAVE,
                rename: &[],
                names_only: None,
                execution: false,
                guard: Guard::ProfileArg,
            },
        },
        read(
            "template_list",
            "List templates",
            "The templates available: name, description, the variables each one needs and the \
             modules it requires. One that cannot be read is listed with its error.",
            "",
            object(json!({}), &[]),
            method::TEMPLATE_LIST,
        ),
        Tool {
            name: "template_get",
            title: "Show a template",
            description: "The text of one template, as it is on disk.",
            access: Access::Read,
            open_world: false,
            hint: NO_PATHS,
            schema: object(json!({ "template": text("Template name.") }), &["template"]),
            op: Op::Rpc {
                method: method::TEMPLATE_GET,
                rename: &[],
                names_only: Some("template"),
                execution: false,
                guard: Guard::None,
            },
        },
        Tool {
            name: "template_apply",
            title: "Apply a template",
            description: "Renders a template with its variables into a profile, checks it and \
                          loads it. Fails when a required variable is missing or a module the \
                          template requires is not installed. An existing profile is replaced \
                          only with force.",
            access: Access::Write {
                destructive: true,
                idempotent: false,
            },
            open_world: false,
            hint: "See the variables and required modules with template_list; modules with module_list.",
            schema: object(
                json!({
                    "template": text("Template name from template_list."),
                    "vars": { "type": "object", "default": {}, "description": "Values for the template's variables, by name." },
                    "id": text("Profile id to write; defaults to the template's."),
                    "project_id": text("Open project to write the profile into."),
                    "force": { "type": "boolean", "default": false, "description": "Replace a profile with the same id." },
                }),
                &["template"],
            ),
            op: Op::Rpc {
                method: method::TEMPLATE_APPLY,
                rename: &[],
                names_only: Some("template"),
                execution: false,
                guard: Guard::Template,
            },
        },
        Tool {
            name: "workflow_start",
            title: "Start a workflow",
            description: "Starts an execution of a profile's workflow (its main one unless \
                          workflow_id says otherwise) and returns it. One that fails to start \
                          comes back as an error with the reason. Nodes left off because a \
                          required input has nothing usable wired into it are listed in \
                          `disabled` and explained in `notes`; the rest runs.",
            access: Access::Write {
                destructive: false,
                idempotent: false,
            },
            open_world: true,
            hint: "profile_check shows what the core finds in the profile; module_list shows \
                   whether each module is usable (broken, blocked, incompatible or vetoed ones \
                   are not).",
            schema: object(
                json!({
                    "profile_id": text("Profile id from profile_list."),
                    "workflow_id": text("Workflow of the profile; omit for its main workflow."),
                }),
                &["profile_id"],
            ),
            op: Op::Rpc {
                method: method::EXECUTION_START,
                rename: &[],
                names_only: None,
                execution: true,
                guard: Guard::StoredProfile,
            },
        },
        Tool {
            name: "workflow_send",
            title: "Send a value into a workflow",
            description: "Puts a value on an input port of the workflow a live execution runs, \
                          as if it came from outside. Returns the `event` number: event_list \
                          from there shows what the value caused.",
            access: Access::Write {
                destructive: false,
                idempotent: false,
            },
            open_world: true,
            hint: "Ports are in profile_get (the workflow's `ports`); the execution must be \
                   live (execution_list).",
            schema: object(
                json!({
                    "execution_id": text("Execution id from workflow_start or execution_list."),
                    "port": text("An input port id of the workflow."),
                    "value": { "description": "Any JSON; its type must fit the port's capability (text@1 takes a string)." },
                }),
                &["execution_id", "port", "value"],
            ),
            op: Op::Rpc {
                method: method::WORKFLOW_SEND,
                rename: &[],
                names_only: None,
                execution: false,
                guard: Guard::None,
            },
        },
        Tool {
            name: "workflow_stop",
            title: "Stop an execution",
            description: "Stops a live execution (and the children it started) and returns it.",
            access: Access::Write {
                destructive: true,
                idempotent: true,
            },
            open_world: false,
            hint: "List live executions with execution_list.",
            schema: object(
                json!({ "execution_id": text("Execution id.") }),
                &["execution_id"],
            ),
            op: Op::Rpc {
                method: method::EXECUTION_STOP,
                rename: &[("execution_id", "id")],
                names_only: None,
                execution: false,
                guard: Guard::None,
            },
        },
        read(
            "execution_list",
            "List executions",
            "The executions: id, profile, workflow, project and state (starting, running, \
             stopped, failed with its reason, interrupted).",
            "",
            project_filter(),
            method::EXECUTION_LIST,
        ),
        read(
            "execution_tree",
            "Execution tree",
            "An execution and the children it started, as deep as they go.",
            "List executions with execution_list.",
            object(
                json!({ "execution_id": text("Execution id.") }),
                &["execution_id"],
            ),
            method::EXECUTION_TREE,
        ),
        read(
            "event_list",
            "List events",
            "What happened on the bus: values emitted, delivered or failed, in order. Pass the \
             returned `next` as `since` to continue; `pending` > 0 means deliveries are still \
             queued.",
            "List executions with execution_list.",
            object(
                json!({
                    "execution_id": text("Only this execution's events."),
                    "project_id": text("Only this project's events."),
                    "since": { "type": "integer", "default": 0, "description": "Events after this seq; 0 = from the oldest kept." },
                    "limit": { "type": "integer", "default": default_events, "description": "At most this many events." },
                }),
                &[],
            ),
            method::EVENT_LIST,
        ),
        Tool {
            name: "module_authoring_guide",
            title: "How to write a module",
            description: "The recipe for adding a module: manifest, the stdio protocol, a \
                          skeleton, declaring it in a project's regulus.toml, and how to check \
                          the result with these tools. Text only; trusting the project is left \
                          to the user.",
            access: Access::Read,
            open_world: false,
            hint: "",
            schema: object(json!({}), &[]),
            op: Op::Guide,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find<'a>(tools: &'a [Tool], name: &str) -> &'a Tool {
        tools.iter().find(|t| t.name == name).unwrap()
    }

    #[test]
    fn names_are_unique_and_schemas_are_objects() {
        let tools = catalog(100);
        let mut names: Vec<&str> = tools.iter().map(|t| t.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), tools.len());
        for tool in &tools {
            assert_eq!(tool.schema["type"], "object", "{}", tool.name);
            assert!(!tool.description.is_empty() && !tool.title.is_empty());
            let listed = tool.listed();
            assert_eq!(listed["annotations"]["readOnlyHint"], !tool.writes());
            if tool.writes() {
                assert!(!tool.hint.is_empty(), "{} needs a hint", tool.name);
            }
        }
    }

    #[test]
    fn no_tool_reaches_secrets_trust_or_an_arbitrary_method() {
        let tools = catalog(100);
        for tool in &tools {
            if let Op::Rpc { method, .. } = &tool.op {
                assert!(
                    !method.starts_with("secret.")
                        && !method.starts_with("project.trust")
                        && !method.starts_with("project.open")
                        && !method.starts_with("hook.")
                        && !method.starts_with("profile.delete"),
                    "{} reaches {method}",
                    tool.name
                );
            }
            assert_ne!(tool.name, "call");
        }
    }

    #[test]
    fn arguments_are_held_to_the_schema() {
        let tools = catalog(100);
        let events = find(&tools, "event_list");
        let args = events.prepare(json!({ "execution_id": "exec-1" })).unwrap();
        assert_eq!(args["since"], 0);
        assert_eq!(args["limit"], 100);
        assert!(events
            .prepare(json!({ "bogus": 1 }))
            .unwrap_err()
            .contains("unknown argument `bogus`"));
        assert!(events
            .prepare(json!({ "limit": "ten" }))
            .unwrap_err()
            .contains("`limit` must be a(n) integer"));
        let whole = events.prepare(json!({ "limit": 5.0 })).unwrap();
        assert_eq!(whole["limit"], json!(5));
        assert!(events.prepare(json!({ "limit": 5.5 })).is_err());
        let get = find(&tools, "profile_get");
        assert!(get
            .prepare(json!({}))
            .unwrap_err()
            .contains("missing required argument `id`"));
        assert!(get.prepare(json!([1])).unwrap_err().contains("JSON object"));
        let send = find(&tools, "workflow_send");
        assert!(send
            .prepare(json!({ "execution_id": "e", "port": "in", "value": null }))
            .is_ok());
        let save = find(&tools, "profile_save");
        assert!(save
            .prepare(json!({ "profile": "not an object" }))
            .unwrap_err()
            .contains("`profile` must be a(n) object"));
    }

    #[test]
    fn templates_are_named_never_pathed() {
        assert!(check_name("template_get", "template", &json!("greeting")).is_ok());
        assert!(check_name("template_get", "template", &json!("../x")).is_err());
        assert!(check_name("template_get", "template", &json!("x.toml")).is_err());
    }
}
