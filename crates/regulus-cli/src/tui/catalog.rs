//! What the Edit view offers from the manifests: the palette of node types with the state
//! of their module, and each node's config as rows the inspector edits. The same rules as
//! the app (`frontend/src/composition/editor/palette.ts`, `fields.ts`).

use regulus_proto::{
    Field, Manifest, ModuleState, ModuleSummary, Node, NodeType, Position, Profile, Workflow,
};
use serde_json::{Map, Value};

use super::document::{base_module_id, unique_id};

/// How the module behind a piece stands in `module.list` (docs/04-client.md §3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PieceState {
    Installed,
    Disabled,
    Broken,
    Incompatible,
    NotInstalled,
}

impl PieceState {
    /// Symbol and word: a state is never color alone.
    pub fn label(self) -> (&'static str, &'static str) {
        match self {
            Self::Installed => ("✓", "installed"),
            Self::Disabled => ("✗", "disabled"),
            Self::Broken => ("▲", "broken"),
            Self::Incompatible => ("▲", "incompatible"),
            Self::NotInstalled => ("□", "not installed"),
        }
    }

    /// Whether a node of it may be added: one that is not installed is shown, not added.
    pub fn addable(self) -> bool {
        self != Self::NotInstalled
    }
}

pub fn find_module<'a>(modules: &'a [ModuleSummary], id: &str) -> Option<&'a ModuleSummary> {
    modules.iter().find(|m| m.id == id).or_else(|| {
        modules
            .iter()
            .find(|m| base_module_id(&m.id) == base_module_id(id))
    })
}

/// Absent from `module.list` is "not installed", never an error. No module: a resource or
/// a box, which is always there.
pub fn module_state_of(modules: &[ModuleSummary], module_id: Option<&str>) -> PieceState {
    let Some(id) = module_id else {
        return PieceState::Installed;
    };
    match find_module(modules, id).map(|m| &m.state) {
        None => PieceState::NotInstalled,
        Some(ModuleState::Active { .. } | ModuleState::Idle { .. }) => PieceState::Installed,
        Some(ModuleState::Disabled { .. }) => PieceState::Disabled,
        Some(ModuleState::Broken { .. } | ModuleState::Blocked { .. }) => PieceState::Broken,
        Some(ModuleState::Incompatible { .. }) => PieceState::Incompatible,
    }
}

/// What a palette entry places.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Piece {
    /// A node type of a module.
    Type {
        module_id: String,
        node_type: String,
    },
    /// Another workflow of the profile, placed as a box.
    Box { workflow_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaletteEntry {
    pub piece: Piece,
    pub name: String,
    /// The module's name, or `workflow` for a box.
    pub group: String,
    pub detail: String,
    pub state: PieceState,
}

/// The workflows that can be placed as a box inside `workflow`: any other with ports.
pub fn boxable<'a>(profile: &'a Profile, workflow: &str) -> Vec<&'a Workflow> {
    profile
        .workflows
        .iter()
        .filter(|w| w.id != workflow && !w.ports.is_empty())
        .collect()
}

/// Every node type of every manifest, with its module's state, then the boxes.
pub fn palette(
    manifests: &[Manifest],
    modules: &[ModuleSummary],
    profile: &Profile,
    workflow: &str,
) -> Vec<PaletteEntry> {
    let mut entries: Vec<PaletteEntry> = manifests
        .iter()
        .flat_map(|manifest| {
            let state = module_state_of(modules, Some(&manifest.module.id));
            manifest.nodes.iter().map(move |node| PaletteEntry {
                piece: Piece::Type {
                    module_id: manifest.module.id.clone(),
                    node_type: node.node_type.clone(),
                },
                name: if node.name.is_empty() {
                    node.node_type.clone()
                } else {
                    node.name.clone()
                },
                group: manifest.module.name.clone(),
                detail: format!("{} · {}", node.node_type, manifest.module.id),
                state,
            })
        })
        .collect();
    entries.extend(
        boxable(profile, workflow)
            .into_iter()
            .map(|w| PaletteEntry {
                piece: Piece::Box {
                    workflow_id: w.id.clone(),
                },
                name: if w.name.is_empty() {
                    w.id.clone()
                } else {
                    w.name.clone()
                },
                group: "workflow".into(),
                detail: format!("box · {}", w.id),
                state: PieceState::Installed,
            }),
    );
    entries
}

/// Entries whose name, detail or group contains every word of the query, case aside.
pub fn filter_palette<'a>(entries: &'a [PaletteEntry], query: &str) -> Vec<&'a PaletteEntry> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    entries
        .iter()
        .filter(|entry| {
            let text = format!("{} {} {}", entry.name, entry.detail, entry.group).to_lowercase();
            words.iter().all(|word| text.contains(word))
        })
        .collect()
}

pub fn manifest_of<'a>(manifests: &'a [Manifest], module_id: &str) -> Option<&'a Manifest> {
    manifests
        .iter()
        .find(|m| m.module.id == module_id)
        .or_else(|| {
            manifests
                .iter()
                .find(|m| base_module_id(&m.module.id) == base_module_id(module_id))
        })
}

/// The manifest type of a node already in a profile.
pub fn type_of<'a>(manifests: &'a [Manifest], node: &Node) -> Option<&'a NodeType> {
    manifest_of(manifests, node.module_id.as_deref()?)?.node_type(&node.node_type)
}

/// A config holding every default the manifest gives (a node just added).
pub fn defaults_of(fields: &[Field]) -> Map<String, Value> {
    fields
        .iter()
        .filter_map(|f| {
            f.default
                .clone()
                .filter(|v| !v.is_null())
                .map(|v| (f.id.clone(), v))
        })
        .collect()
}

/// A node of a palette entry: its ports, its fields' defaults as config, a free id.
pub fn node_for(
    manifests: &[Manifest],
    profile: &Profile,
    piece: &Piece,
    position: Position,
    taken: &[&str],
) -> Option<Node> {
    let taken = taken.iter().copied();
    match piece {
        Piece::Type {
            module_id,
            node_type,
        } => {
            let manifest = manifest_of(manifests, module_id)?;
            let kind = manifest.node_type(node_type)?;
            Some(Node {
                id: unique_id(&kind.node_type, taken, "node"),
                node_type: kind.node_type.clone(),
                name: if kind.name.is_empty() {
                    kind.node_type.clone()
                } else {
                    kind.name.clone()
                },
                module_id: Some(manifest.module.id.clone()),
                resource: None,
                ports: kind.ports.clone(),
                config: defaults_of(&kind.fields),
                position,
                internal_workflow_id: None,
                exposed_parameters: Vec::new(),
            })
        }
        Piece::Box { workflow_id } => {
            let inner = profile.workflow(workflow_id)?;
            Some(Node {
                id: unique_id(&inner.id, taken, "node"),
                node_type: "workflow".into(),
                name: if inner.name.is_empty() {
                    inner.id.clone()
                } else {
                    inner.name.clone()
                },
                module_id: None,
                resource: None,
                ports: Vec::new(),
                config: Map::new(),
                position,
                internal_workflow_id: Some(inner.id.clone()),
                exposed_parameters: Vec::new(),
            })
        }
    }
}

/// The control a field gets; a type this client does not know is raw text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Text,
    Number,
    Select,
    /// A JSON object, typed as JSON.
    Map,
    /// The id of a workflow of the profile.
    Workflow,
    Raw,
}

const BOOLEAN_TYPES: [&str; 2] = ["boolean", "bool"];

pub fn control_for(field: &Field) -> Control {
    let boolean = BOOLEAN_TYPES.contains(&field.field_type.as_str())
        || matches!(field.default, Some(Value::Bool(_)));
    match field.field_type.as_str() {
        regulus_proto::FIELD_WORKFLOW => Control::Workflow,
        "select" => Control::Select,
        _ if boolean || !field.options.is_empty() => Control::Select,
        "number" => Control::Number,
        "text" => Control::Text,
        "map" => Control::Map,
        _ => Control::Raw,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FieldRow {
    pub id: String,
    /// The manifest type, or `None` for a key the manifest does not declare.
    pub field_type: Option<String>,
    pub control: Control,
    /// What the node has; `None` when its config does not set it.
    pub value: Option<Value>,
    pub default: Option<Value>,
    pub options: Vec<Value>,
    /// No value of its own, or the same as the default.
    pub is_default: bool,
}

/// One row per manifest field, in manifest order, then one per config key it does not
/// declare. `workflows` are the options of a workflow field.
pub fn field_rows(
    fields: &[Field],
    config: &Map<String, Value>,
    workflows: &[String],
) -> Vec<FieldRow> {
    let mut rows: Vec<FieldRow> = fields
        .iter()
        .map(|field| {
            let value = config.get(&field.id).cloned();
            let default = field.default.clone().filter(|v| !v.is_null());
            let control = control_for(field);
            let options = match control {
                Control::Workflow => workflows.iter().map(|w| Value::from(w.as_str())).collect(),
                _ if !field.options.is_empty() => field.options.clone(),
                Control::Select => vec![Value::Bool(true), Value::Bool(false)],
                _ => Vec::new(),
            };
            FieldRow {
                id: field.id.clone(),
                field_type: Some(field.field_type.clone()),
                control,
                is_default: value.is_none() || value == default,
                value,
                default,
                options,
            }
        })
        .collect();
    for (id, value) in config {
        if fields.iter().any(|f| &f.id == id) {
            continue;
        }
        rows.push(FieldRow {
            id: id.clone(),
            field_type: None,
            control: Control::Raw,
            value: Some(value.clone()),
            default: None,
            options: Vec::new(),
            is_default: false,
        });
    }
    rows
}

/// How a value reads in a text control: strings as they are, the rest as JSON.
pub fn display(value: Option<&Value>) -> String {
    match value {
        None => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

/// Turns what was typed into the value to store, keeping the type the field expects.
pub fn coerce(row: &FieldRow, input: &str) -> Result<Value, String> {
    match row.control {
        Control::Number => input
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|n| n.is_finite())
            .and_then(|n| {
                if n.fract() == 0.0 && n.abs() < 9.0e15 {
                    Some(Value::from(n as i64))
                } else {
                    serde_json::Number::from_f64(n).map(Value::Number)
                }
            })
            .ok_or_else(|| format!("`{input}` is not a number")),
        Control::Select | Control::Workflow => row
            .options
            .iter()
            .find(|option| display(Some(option)) == input)
            .cloned()
            .ok_or_else(|| format!("`{input}` is not one of the options")),
        Control::Map => match serde_json::from_str::<Value>(input) {
            Ok(value @ Value::Object(_)) => Ok(value),
            _ => Err("a map is a JSON object, such as {\"key\": \"value\"}".into()),
        },
        Control::Raw => {
            let current = row.value.as_ref().or(row.default.as_ref());
            match current {
                None | Some(Value::String(_)) => Ok(Value::from(input)),
                Some(_) => match serde_json::from_str::<Value>(input) {
                    Ok(Value::Null) | Err(_) => Err(format!("`{input}` is not JSON")),
                    Ok(value) => Ok(value),
                },
            }
        }
        Control::Text => Ok(Value::from(input)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn manifest() -> Manifest {
        serde_json::from_value(json!({
            "module": { "id": "regulus/hello@0.1.0", "name": "Hello" },
            "node": [{
                "type": "greeter", "name": "Greeter",
                "port": [{ "id": "tick", "capability": "text@1", "direction": "output", "kind": "then" }],
                "field": [
                    { "id": "who", "type": "text", "default": "world" },
                    { "id": "times", "type": "number", "default": 1 },
                    { "id": "loud", "type": "bool", "default": false },
                    { "id": "mood", "type": "select", "options": ["calm", "loud"] },
                    { "id": "shape", "type": "color" }
                ]
            }]
        }))
        .unwrap()
    }

    fn module(id: &str, state: ModuleState) -> ModuleSummary {
        ModuleSummary {
            id: id.into(),
            name: id.into(),
            node_types: vec![],
            workflow_id: None,
            state,
        }
    }

    #[test]
    fn the_palette_says_how_each_module_stands() {
        let profile = super::super::document::new_profile("p", "P", "Main");
        let manifests = [manifest()];
        let entries = palette(&manifests, &[], &profile, "main");
        assert_eq!(entries[0].state, PieceState::NotInstalled);
        assert!(!entries[0].state.addable());
        let modules = [module(
            "regulus/hello@0.1.0",
            ModuleState::Idle { last_used: None },
        )];
        assert_eq!(
            palette(&manifests, &modules, &profile, "main")[0].state,
            PieceState::Installed
        );
        let broken = [module(
            "regulus/hello@0.2.0",
            ModuleState::Broken { reason: "x".into() },
        )];
        assert_eq!(
            palette(&manifests, &broken, &profile, "main")[0].state,
            PieceState::Broken
        );
        assert_eq!(filter_palette(&entries, "hello greet").len(), 1);
        assert!(filter_palette(&entries, "nothing").is_empty());
    }

    #[test]
    fn a_node_from_the_palette_has_ports_and_defaults() {
        let profile = super::super::document::new_profile("p", "P", "Main");
        let piece = Piece::Type {
            module_id: "regulus/hello@0.1.0".into(),
            node_type: "greeter".into(),
        };
        let node = node_for(
            &[manifest()],
            &profile,
            &piece,
            Position::default(),
            &["greeter"],
        )
        .unwrap();
        assert_eq!(node.id, "greeter-2");
        assert_eq!(node.ports.len(), 1);
        assert_eq!(node.config["who"], "world");
        assert_eq!(node.config["loud"], false);
        assert!(!node.config.contains_key("mood"));
    }

    #[test]
    fn fields_get_a_control_and_coerce_what_is_typed() {
        let kind = manifest().nodes[0].clone();
        let mut config = defaults_of(&kind.fields);
        config.insert("extra".into(), json!([1, 2]));
        let rows = field_rows(&kind.fields, &config, &[]);
        let controls: Vec<Control> = rows.iter().map(|r| r.control).collect();
        assert_eq!(
            controls,
            vec![
                Control::Text,
                Control::Number,
                Control::Select,
                Control::Select,
                Control::Raw,
                Control::Raw
            ]
        );
        assert!(rows[0].is_default);
        assert_eq!(rows[5].field_type, None);
        assert_eq!(coerce(&rows[1], "3").unwrap(), json!(3));
        assert_eq!(coerce(&rows[1], "2.5").unwrap(), json!(2.5));
        assert!(coerce(&rows[1], "x").is_err());
        assert_eq!(coerce(&rows[2], "true").unwrap(), json!(true));
        assert!(coerce(&rows[3], "angry").is_err());
        assert_eq!(coerce(&rows[4], "red").unwrap(), json!("red"));
        assert_eq!(coerce(&rows[5], "[3]").unwrap(), json!([3]));
        assert!(coerce(&rows[5], "nope").is_err());
    }
}
