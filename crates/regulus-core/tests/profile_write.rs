//! What the app needs to edit (docs/11-composition.md §7): `module.manifest`, and
//! `profile.save` / `profile.delete` writing and removing profile files under the
//! configured `[profiles] paths`, live, without a restart.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use regulus_core::templates::Places;
use regulus_core::{Core, Module, ModuleCtx, ModuleError, ProfileStore};
use regulus_proto::{code, method, Manifest};
use serde_json::{json, Value};

const MANIFEST: &str = r#"
[module]
id = "test/echo@0.1.0"
name = "Echo"

[[node]]
type = "echo"
name = "Echo"

  [[node.port]]
  id = "out"
  capability = "text@1"
  direction = "output"
  kind = "then"

  [[node.field]]
  id = "mode"
  type = "select"
  default = "loud"
  options = ["loud", "quiet"]
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

/// A profile with one echo node and a box: a node that holds the workflow `inner`, whose
/// ports the box takes as its own when it declares none.
fn profile(id: &str, mode: &str) -> Value {
    json!({
        "id": id,
        "name": format!("Profile {id}"),
        "main_workflow_id": "main",
        "workflows": [
            {
                "id": "main",
                "kind": "main",
                "nodes": [
                    {
                        "id": "e",
                        "type": "echo",
                        "module_id": "test/echo@0.1.0",
                        "config": { "mode": mode, "list": [1, 2.5, "x"], "deep": { "a": true } },
                        "position": { "x": 10, "y": 20.5 },
                        "ports": [
                            { "id": "out", "capability": "text@1", "direction": "output", "kind": "then" }
                        ]
                    },
                    { "id": "box", "type": "workflow", "internal_workflow_id": "inner" }
                ]
            },
            {
                "id": "inner",
                "kind": "extra",
                "ports": [
                    { "id": "go", "capability": "text@1", "direction": "input", "kind": "then" }
                ]
            }
        ]
    })
}

struct Home {
    root: PathBuf,
    core: Arc<Core>,
}

impl Home {
    /// A core whose `[profiles] paths` are `<root>/profiles` and `<root>/more`, with
    /// `files` already written there before it starts.
    fn new(name: &str, files: &[(&str, &str)]) -> Self {
        let root = std::env::temp_dir().join(format!("regulus-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let paths = vec![root.join("profiles"), root.join("more")];
        for path in &paths {
            fs::create_dir_all(path).unwrap();
        }
        for (file, text) in files {
            fs::write(root.join(file), text).unwrap();
        }
        let (profiles, warnings) = ProfileStore::load(&paths);
        assert!(warnings.is_empty(), "{warnings:?}");
        let manifest = regulus_core::parse_manifest(MANIFEST).unwrap();
        let modules: Vec<Box<dyn Module>> = vec![Box::new(Echo(manifest))];
        let core = Core::build(modules, &[], profiles);
        core.set_template_places(Places {
            templates: Vec::new(),
            profiles_dir: Some(paths[0].clone()),
            profile_paths: paths,
            home: Some(root.clone()),
        });
        Self { root, core }
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, (i32, String)> {
        self.core
            .dispatch(name, params)
            .map_err(|e| (e.code, e.message))
    }

    fn save(&self, profile: Value, force: bool) -> Result<Value, (i32, String)> {
        self.call(
            method::PROFILE_SAVE,
            json!({ "profile": profile, "force": force }),
        )
    }

    fn get(&self, id: &str) -> Result<Value, (i32, String)> {
        self.call(method::PROFILE_GET, json!({ "id": id }))
    }

    fn file(&self, name: &str) -> PathBuf {
        self.root.join(name)
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn error_code(result: Result<Value, (i32, String)>) -> (i32, String) {
    result.expect_err("expected an error")
}

#[test]
fn manifests_come_out_with_the_names_the_toml_uses() {
    let home = Home::new("manifest", &[]);
    let one = home
        .call(method::MODULE_MANIFEST, json!({ "id": "test/echo" }))
        .unwrap();
    assert_eq!(one["module"]["id"], "test/echo@0.1.0");
    assert_eq!(one["node"][0]["type"], "echo");
    assert_eq!(one["node"][0]["port"][0]["capability"], "text@1");
    assert_eq!(one["node"][0]["field"][0]["default"], "loud");
    assert_eq!(
        one["node"][0]["field"][0]["options"],
        json!(["loud", "quiet"])
    );
    let all = home.call(method::MODULE_MANIFEST, json!({})).unwrap();
    assert_eq!(all, json!([one]));
    let (code, message) = error_code(home.call(method::MODULE_MANIFEST, json!({ "id": "x/y" })));
    assert_eq!(code, code::INVALID_PARAMS);
    assert!(message.contains("x/y"), "{message}");
}

#[test]
fn a_new_profile_is_saved_loaded_and_read_back_the_same() {
    let home = Home::new("save-new", &[]);
    let saved = home.save(profile("p", "loud"), false).unwrap();
    let path = home.file("profiles/p.toml");
    assert_eq!(saved["profile_id"], "p");
    assert_eq!(saved["path"], path.display().to_string());
    let text = fs::read_to_string(&path).unwrap();
    assert!(text.starts_with("# Saved by Regulus"), "{text}");
    // The box keeps no copy of its workflow's ports: it follows them.
    let first = home.get("p").unwrap();
    assert_eq!(first["workflows"][0]["nodes"][1]["ports"][0]["id"], "go");
    let reread: regulus_proto::Profile = toml::from_str(&text).unwrap();
    assert!(reread.workflows[0].nodes[1].ports.is_empty(), "{text}");

    // get → save → get gives the same profile, through the file.
    home.save(first.clone(), true).unwrap();
    assert_eq!(home.get("p").unwrap(), first);
    let (profiles, warnings) = ProfileStore::load(&[home.file("profiles")]);
    assert!(warnings.is_empty(), "{warnings:?}");
    let mut loaded = serde_json::to_value(profiles.get("p").unwrap().as_ref()).unwrap();
    // Loading fills the box's ports the way the core does when it serves the profile.
    loaded["workflows"][0]["nodes"][1]["ports"] =
        first["workflows"][0]["nodes"][1]["ports"].clone();
    assert_eq!(loaded, first);
    let list = home.call(method::PROFILE_LIST, json!({})).unwrap();
    assert_eq!(list[0]["id"], "p");
}

#[test]
fn an_existing_profile_needs_force_and_its_own_file() {
    let many = "[[profiles]]\nid = \"a\"\nmain_workflow_id = \"m\"\n\
                [[profiles.workflows]]\nid = \"m\"\nkind = \"main\"\n\n\
                [[profiles]]\nid = \"b\"\nmain_workflow_id = \"m\"\n\
                [[profiles.workflows]]\nid = \"m\"\nkind = \"main\"\n";
    let elsewhere =
        "id = \"far\"\nmain_workflow_id = \"m\"\n[[workflows]]\nid = \"m\"\nkind = \"main\"\n";
    let home = Home::new(
        "save-existing",
        &[("profiles/ab.toml", many), ("more/far.toml", elsewhere)],
    );
    home.save(profile("p", "loud"), false).unwrap();
    let (code, message) = error_code(home.save(profile("p", "quiet"), false));
    assert_eq!(code, code::PROFILE_EXISTS);
    assert!(message.contains("force"), "{message}");
    assert_eq!(
        home.get("p").unwrap()["workflows"][0]["nodes"][0]["config"]["mode"],
        "loud"
    );
    home.save(profile("p", "quiet"), true).unwrap();
    assert_eq!(
        home.get("p").unwrap()["workflows"][0]["nodes"][0]["config"]["mode"],
        "quiet"
    );
    assert!(fs::read_to_string(home.file("profiles/p.toml"))
        .unwrap()
        .contains("quiet"));

    // A profile that shares its file, or lives outside the first path, says where.
    let (code, message) = error_code(home.save(profile("a", "loud"), true));
    assert_eq!(code, code::PROFILE_EXISTS);
    assert!(message.contains("ab.toml"), "{message}");
    let (code, message) = error_code(home.save(profile("far", "loud"), true));
    assert_eq!(code, code::PROFILE_EXISTS);
    assert!(message.contains("more/far.toml"), "{message}");
    assert!(!home.file("profiles/far.toml").exists());
    assert_eq!(
        fs::read_to_string(home.file("profiles/ab.toml")).unwrap(),
        many
    );
}

#[test]
fn a_profile_with_errors_a_bad_id_or_a_null_is_not_written() {
    let home = Home::new("save-invalid", &[]);
    let mut broken = profile("p", "loud");
    broken["main_workflow_id"] = json!("nowhere");
    let (code, message) = error_code(home.save(broken, false));
    assert_eq!(code, code::PROFILE_INVALID);
    assert!(message.contains("profile.main_missing"), "{message}");
    let (code, _) = error_code(home.save(profile("../x", "loud"), false));
    assert_eq!(code, code::INVALID_PARAMS);
    let mut null = profile("p", "loud");
    null["workflows"][0]["nodes"][0]["config"]["deep"]["a"] = Value::Null;
    let (code, message) = error_code(home.save(null, false));
    assert_eq!(code, code::INVALID_PARAMS);
    assert!(message.contains("config.deep.a"), "{message}");
    let (code, _) = error_code(home.call(method::PROFILE_SAVE, json!({ "profile": { "id": 1 } })));
    assert_eq!(code, code::INVALID_PARAMS);
    assert_eq!(fs::read_dir(home.file("profiles")).unwrap().count(), 0);
    assert!(!home.file("x.toml").exists());
}

#[test]
fn a_profile_is_deleted_only_when_nothing_of_it_runs() {
    let many = "[[profiles]]\nid = \"a\"\nmain_workflow_id = \"m\"\n\
                [[profiles.workflows]]\nid = \"m\"\nkind = \"main\"\n\n\
                [[profiles]]\nid = \"b\"\nmain_workflow_id = \"m\"\n\
                [[profiles.workflows]]\nid = \"m\"\nkind = \"main\"\n";
    let home = Home::new("delete", &[("profiles/ab.toml", many)]);
    home.save(profile("p", "loud"), false).unwrap();
    let execution = home
        .call(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(execution["state"]["status"], "running");
    let (code, message) = error_code(home.call(method::PROFILE_DELETE, json!({ "id": "p" })));
    assert_eq!(code, code::PROFILE_IN_USE);
    assert!(
        message.contains(execution["id"].as_str().unwrap()),
        "{message}"
    );
    assert!(home.file("profiles/p.toml").exists());

    home.call(method::EXECUTION_STOP, json!({ "id": execution["id"] }))
        .unwrap();
    let deleted = home
        .call(method::PROFILE_DELETE, json!({ "id": "p" }))
        .unwrap();
    assert_eq!(deleted["profile_id"], "p");
    assert!(!home.file("profiles/p.toml").exists());
    assert_eq!(error_code(home.get("p")).0, code::INVALID_PARAMS);
    let (code, _) = error_code(home.call(method::PROFILE_DELETE, json!({ "id": "p" })));
    assert_eq!(code, code::INVALID_PARAMS);

    // A file with other profiles stays, and so does the profile.
    let (code, message) = error_code(home.call(method::PROFILE_DELETE, json!({ "id": "a" })));
    assert_eq!(code, code::PROFILE_EXISTS);
    assert!(message.contains("`b`"), "{message}");
    assert!(home.get("a").is_ok());
    assert_eq!(
        fs::read_to_string(home.file("profiles/ab.toml")).unwrap(),
        many
    );
}

#[test]
fn a_profile_listed_by_itself_in_the_paths_is_not_deleted() {
    let root = std::env::temp_dir().join(format!("regulus-listed-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let file = root.join("solo.toml");
    fs::write(
        &file,
        "id = \"solo\"\nmain_workflow_id = \"m\"\n[[workflows]]\nid = \"m\"\nkind = \"main\"\n",
    )
    .unwrap();
    let (profiles, _) = ProfileStore::load(std::slice::from_ref(&file));
    let core = Core::build(Vec::new(), &[], profiles);
    core.set_template_places(Places {
        profiles_dir: Some(file.clone()),
        profile_paths: vec![file.clone()],
        ..Places::default()
    });
    let error = core
        .dispatch(method::PROFILE_DELETE, json!({ "id": "solo" }))
        .unwrap_err();
    assert_eq!(error.code, code::PROFILE_EXISTS);
    assert!(Path::new(&file).exists());
    let _ = fs::remove_dir_all(&root);
}
