//! `template.save` (docs/11-composition.md §7): a profile, or one workflow of it with the
//! workflows it places as boxes, written as a template that lists, reads and applies.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use regulus_core::templates::Places;
use regulus_core::{Core, Module, ModuleCtx, ModuleError, ProfileStore};
use regulus_proto::{code, method, Manifest, Profile, WorkflowKind};
use serde_json::{json, Value};

const MANIFEST: &str = r#"
[module]
id = "test/echo@0.3.1"
name = "Echo"

[[node]]
type = "echo"
name = "Echo"
"#;

struct Echo(Manifest);

impl Module for Echo {
    fn manifest(&self) -> &Manifest {
        &self.0
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, method: &str, _: Value) -> Result<Value, ModuleError> {
        match method {
            method::NODE_START | method::NODE_STOP => Ok(Value::Null),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

struct Home {
    root: PathBuf,
    core: Arc<Core>,
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text_port(id: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": "text@1", "direction": direction, "kind": "then" })
}

/// `main` runs an echo whose mode is the hole `{{mode}}` and places `outer` as a box;
/// `outer` places `inner`; `spare` is placed nowhere.
fn profile() -> Value {
    let echo = |id: &str, mode: &str| {
        json!({ "id": id, "type": "echo", "module_id": "test/echo@0.3.1",
                "config": { "mode": mode } })
    };
    json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main",
              "nodes": [echo("e", "{{mode}}"),
                        { "id": "box", "type": "workflow", "internal_workflow_id": "outer" }] },
            { "id": "outer", "kind": "extra", "ports": [text_port("go", "input")],
              "nodes": [{ "id": "in", "type": "workflow", "internal_workflow_id": "inner" }] },
            { "id": "inner", "kind": "extra", "ports": [text_port("go", "input")],
              "nodes": [echo("deep", "loud")] },
            { "id": "spare", "kind": "extra", "nodes": [echo("s", "loud")] },
        ]
    })
}

impl Home {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "regulus-template-save-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let profiles = root.join("profiles");
        fs::create_dir_all(&profiles).unwrap();
        let manifest = regulus_core::parse_manifest(MANIFEST).unwrap();
        let modules: Vec<Box<dyn Module>> = vec![Box::new(Echo(manifest))];
        let core = Core::build(modules, &[], ProfileStore::default());
        core.set_template_places(Places {
            templates: vec![root.join("templates")],
            profiles_dir: Some(profiles.clone()),
            profile_paths: vec![profiles],
            home: Some(root.clone()),
        });
        let home = Self { root, core };
        home.call(method::PROFILE_SAVE, json!({ "profile": profile() }))
            .unwrap();
        home
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, (i32, String)> {
        self.core
            .dispatch(name, params)
            .map_err(|e| (e.code, e.message))
    }

    fn save(&self, params: Value) -> Result<Value, (i32, String)> {
        self.call(method::TEMPLATE_SAVE, params)
    }

    fn profile(&self, id: &str) -> Profile {
        serde_json::from_value(self.call(method::PROFILE_GET, json!({ "id": id })).unwrap())
            .unwrap()
    }
}

#[test]
fn a_saved_profile_lists_reads_and_applies_as_a_template() {
    let home = Home::new("whole");
    let saved = home
        .save(json!({
            "name": "kit", "profile_id": "p", "description": "An echo kit",
            "vars": { "mode": { "description": "how it echoes", "default": "loud" } },
        }))
        .unwrap();
    let path = home.root.join("templates/kit.toml");
    assert_eq!(saved, json!({ "path": path.display().to_string() }));

    let list = home.call(method::TEMPLATE_LIST, json!({})).unwrap();
    assert_eq!(list[0]["name"], "kit");
    assert_eq!(list[0]["description"], "An echo kit");
    assert_eq!(list[0]["vars"][0]["name"], "mode");
    assert_eq!(list[0]["vars"][0]["default"], "loud");
    assert_eq!(
        list[0]["requires"],
        json!([{ "module": "test/echo", "version": ">=0.3" }])
    );
    assert!(list[0].get("error").is_none(), "{list}");
    let text = home
        .call(method::TEMPLATE_GET, json!({ "template": "kit" }))
        .unwrap();
    assert!(
        text["text"].as_str().unwrap().contains("[template]"),
        "{text}"
    );

    home.call(
        method::TEMPLATE_APPLY,
        json!({ "template": "kit", "id": "copy", "vars": { "mode": "quiet" } }),
    )
    .unwrap();
    let copy = home.profile("copy");
    let ids: Vec<&str> = copy.workflows.iter().map(|w| w.id.as_str()).collect();
    assert_eq!(ids, ["main", "outer", "inner", "spare"]);
    assert_eq!(copy.workflows[0].nodes[0].config["mode"], "quiet");
    assert_eq!(copy.project_id, None);
}

#[test]
fn a_saved_workflow_takes_the_boxes_it_places_and_becomes_the_main_one() {
    let home = Home::new("workflow");
    home.save(json!({ "name": "outer-kit", "profile_id": "p", "workflow_id": "outer" }))
        .unwrap();
    home.call(method::TEMPLATE_APPLY, json!({ "template": "outer-kit" }))
        .unwrap();
    let applied = home.profile("outer-kit");
    assert_eq!(applied.main_workflow_id, "outer");
    let kinds: Vec<(&str, WorkflowKind)> = applied
        .workflows
        .iter()
        .map(|w| (w.id.as_str(), w.kind))
        .collect();
    assert_eq!(
        kinds,
        [
            ("outer", WorkflowKind::Main),
            ("inner", WorkflowKind::Extra)
        ]
    );
}

#[test]
fn an_existing_file_needs_force_and_bad_input_is_refused() {
    let home = Home::new("refused");
    let params = json!({ "name": "kit", "profile_id": "p",
                         "vars": { "mode": { "description": "m", "default": "loud" } } });
    home.save(params.clone()).unwrap();
    let (code, message) = home.save(params.clone()).unwrap_err();
    assert_eq!(code, code::PROFILE_EXISTS, "{message}");
    let mut forced = params.clone();
    forced["force"] = json!(true);
    forced["description"] = json!("again");
    home.save(forced).unwrap();
    let list = home.call(method::TEMPLATE_LIST, json!({})).unwrap();
    assert_eq!(list[0]["description"], "again");

    // Not a template there: left alone, even with force.
    let other = home.root.join("notes.toml");
    fs::write(&other, "just = \"notes\"\n").unwrap();
    let (code, _) = home
        .save(
            json!({ "name": "kit", "profile_id": "p", "path": "notes.toml", "force": true,
                      "vars": { "mode": { "description": "m" } } }),
        )
        .unwrap_err();
    assert_eq!(code, code::PROFILE_EXISTS);
    assert_eq!(fs::read_to_string(&other).unwrap(), "just = \"notes\"\n");

    for bad in [
        json!({ "name": "a/b", "profile_id": "p" }),
        json!({ "name": "kit2", "profile_id": "nope" }),
        json!({ "name": "kit2", "profile_id": "p", "workflow_id": "nope" }),
        // `{{mode}}` is in the profile but not declared: applying could never work.
        json!({ "name": "kit2", "profile_id": "p" }),
        json!({ "name": "kit2", "profile_id": "p", "vars": { "1x": { "description": "" } } }),
    ] {
        let (code, message) = home.save(bad.clone()).unwrap_err();
        assert_eq!(code, code::TEMPLATE_INVALID, "{bad}: {message}");
    }
    assert!(!home.root.join("templates/kit2.toml").exists());
}

#[test]
fn a_path_may_name_the_file_or_its_folder() {
    let home = Home::new("path");
    let vars = json!({ "mode": { "description": "m", "default": "loud" } });
    let saved = home
        .save(json!({ "name": "kit", "profile_id": "p", "vars": vars, "path": "elsewhere" }))
        .unwrap();
    let folder = home.root.join("elsewhere/kit.toml");
    assert_eq!(saved["path"], folder.display().to_string());
    let saved = home
        .save(json!({ "name": "kit", "profile_id": "p", "vars": vars, "path": "one/my.toml" }))
        .unwrap();
    assert_eq!(
        saved["path"],
        home.root.join("one/my.toml").display().to_string()
    );
    assert!(home.root.join("one/my.toml").is_file());
}
