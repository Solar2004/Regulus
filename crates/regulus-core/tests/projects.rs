//! Projects (docs/11-composition.md §13): folders with a tolerant `regulus.toml`, opened
//! and closed live and remembered in `config.toml` `[projects] paths`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use regulus_core::projects::Projects;
use regulus_core::service::ProjectHome;
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

struct Setup {
    dir: PathBuf,
    core: Arc<Core>,
}

impl Drop for Setup {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Setup {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "regulus-core-projects-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        let echo = Echo(regulus_core::parse_manifest(MANIFEST).unwrap());
        let core = Core::build(vec![Box::new(echo)], &[], ProfileStore::default());
        core.set_projects(
            Projects::default(),
            ProjectHome {
                config: dir.join("home/config.toml"),
                home: dir.join("home"),
            },
        );
        Self { dir, core }
    }

    fn folder(&self, name: &str) -> PathBuf {
        let path = self.dir.join(name);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, (i32, String)> {
        self.core
            .dispatch(method, params)
            .map_err(|e| (e.code, e.message))
    }

    fn config(&self) -> toml::Table {
        toml::from_str(&fs::read_to_string(self.dir.join("home/config.toml")).unwrap()).unwrap()
    }
}

fn open(setup: &Setup, path: &Path) -> Value {
    setup
        .call(method::PROJECT_OPEN, json!({ "path": path }))
        .unwrap()
}

#[test]
fn opening_a_folder_gives_it_a_file_and_remembers_it() {
    let s = Setup::new("open");
    let demo = s.folder("demo");
    let opened = open(&s, &demo);
    assert_eq!(opened["id"], "demo");
    assert_eq!(opened["name"], "demo");
    assert_eq!(
        fs::read_to_string(demo.join("regulus.toml")).unwrap(),
        "[project]\nname = \"demo\"\n"
    );
    let paths = s.config()["projects"]["paths"].as_array().unwrap().clone();
    assert_eq!(paths.len(), 1);

    // Again: nothing changes, not even config.toml.
    assert_eq!(open(&s, &demo)["id"], "demo");
    assert_eq!(s.config()["projects"]["paths"].as_array().unwrap().len(), 1);

    let list = s.call(method::PROJECT_LIST, Value::Null).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(
        list[0]["path"],
        demo.canonicalize().unwrap().display().to_string()
    );

    let details = s
        .call(method::PROJECT_GET, json!({ "id": "demo" }))
        .unwrap();
    assert!(details["profiles"][0]
        .as_str()
        .unwrap()
        .ends_with(".regulus/profiles"));
    assert_eq!(details["content"]["project"]["name"], "demo");
}

#[test]
fn unknown_keys_and_local_modules_are_warnings() {
    let s = Setup::new("warn");
    let demo = s.folder("demo");
    fs::write(
        demo.join("regulus.toml"),
        "[project]\nname = \"Demo\"\nfoo = \"bar\"\n[modules]\ndisable = [\"web\"]\n\
         [[modules.local]]\ncommand = [\"run\"]\n[client]\nstart_view = \"operate\"\n",
    )
    .unwrap();
    let opened = open(&s, &demo);
    assert_eq!(opened["name"], "Demo");
    let warnings = opened["warnings"].to_string();
    assert!(warnings.contains("project.foo"), "{warnings}");
    assert!(warnings.contains("not supported yet"), "{warnings}");
    assert!(
        warnings.contains("`web`, which is not loaded"),
        "{warnings}"
    );
    let details = s
        .call(method::PROJECT_GET, json!({ "id": "demo" }))
        .unwrap();
    assert_eq!(details["client"]["start_view"], "operate");
    assert_eq!(details["content"]["project"]["foo"], "bar");
    assert_eq!(details["local"][0]["command"][0], "run");
}

#[test]
fn closing_forgets_the_folder_and_deletes_nothing() {
    let s = Setup::new("close");
    let demo = s.folder("demo");
    open(&s, &demo);
    let closed = s
        .call(method::PROJECT_CLOSE, json!({ "id": "demo" }))
        .unwrap();
    assert_eq!(closed["id"], "demo");
    assert!(demo.join("regulus.toml").exists());
    assert!(s.config()["projects"]["paths"]
        .as_array()
        .unwrap()
        .is_empty());
    let list = s.call(method::PROJECT_LIST, Value::Null).unwrap();
    assert!(list.as_array().unwrap().is_empty());
    let again = s.call(method::PROJECT_CLOSE, json!({ "id": "demo" }));
    assert_eq!(again.unwrap_err().0, code::PROJECT_NOT_FOUND);
}

#[test]
fn ids_must_be_unique_and_safe_and_folders_must_exist() {
    let s = Setup::new("ids");
    open(&s, &s.folder("a/demo"));
    let twin = s.call(method::PROJECT_OPEN, json!({ "path": s.folder("b/demo") }));
    assert_eq!(twin.unwrap_err().0, code::PROJECT_EXISTS);

    let bad = s.folder("bad");
    fs::write(bad.join("regulus.toml"), "[project]\nid = \"../x\"\n").unwrap();
    let refused = s.call(method::PROJECT_OPEN, json!({ "path": bad }));
    assert_eq!(refused.unwrap_err().0, code::PROJECT_INVALID);

    let missing = s.call(method::PROJECT_OPEN, json!({ "path": s.dir.join("nope") }));
    assert_eq!(missing.unwrap_err().0, code::PROJECT_NOT_FOUND);
    assert_eq!(s.config()["projects"]["paths"].as_array().unwrap().len(), 1);

    let get = s.call(method::PROJECT_GET, json!({ "id": "other" }));
    assert_eq!(get.unwrap_err().0, code::PROJECT_NOT_FOUND);
}

#[test]
fn opened_projects_load_again_from_config() {
    let s = Setup::new("reload");
    let demo = s.folder("demo");
    open(&s, &demo);
    let config = regulus_core::Config::load(&s.dir.join("home")).unwrap();
    let (projects, warnings) = Projects::load(&config.projects.paths);
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        projects.iter().map(|p| p.id.as_str()).collect::<Vec<_>>(),
        ["demo"]
    );

    fs::remove_dir_all(&demo).unwrap();
    let (projects, warnings) = Projects::load(&config.projects.paths);
    assert_eq!(projects.iter().count(), 0);
    assert!(warnings[0].contains("is not open"), "{warnings:?}");
}

const PROFILE: &str = r#"id = "{id}"
main_workflow_id = "main"

[[workflows]]
id = "main"
kind = "main"

  [[workflows.nodes]]
  id = "echo"
  type = "echo"
  module_id = "test/echo@0.1.0"
"#;

fn profile_text(id: &str) -> String {
    PROFILE.replace("{id}", id)
}

const TEMPLATE: &str = r#"id = "{{id}}"
main_workflow_id = "main"

[template]
name = "{name}"

[template.vars]
id = { default = "{name}" }

[[workflows]]
id = "main"
kind = "main"

  [[workflows.nodes]]
  id = "echo"
  type = "echo"
  module_id = "test/echo@0.1.0"
"#;

fn template_text(name: &str) -> String {
    TEMPLATE.replace("{name}", name)
}

fn profiles(s: &Setup) -> Vec<(String, Value)> {
    let list = s.call(method::PROFILE_LIST, json!({})).unwrap();
    list.as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p["id"].as_str().unwrap().to_string(),
                p["project_id"].clone(),
            )
        })
        .collect()
}

#[test]
fn a_projects_profiles_load_with_its_id_and_leave_when_it_closes() {
    let s = Setup::new("profiles");
    let demo = s.folder("demo");
    let dir = demo.join(".regulus/profiles");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("mine.toml"), profile_text("mine")).unwrap();
    fs::write(
        dir.join("other.toml"),
        profile_text("other").replace(
            "main_workflow_id",
            "project_id = \"elsewhere\"\nmain_workflow_id",
        ),
    )
    .unwrap();
    let opened = open(&s, &demo);
    assert!(
        opened["warnings"]
            .to_string()
            .contains("says project_id `elsewhere`"),
        "{opened}"
    );
    assert_eq!(
        profiles(&s),
        [
            ("mine".into(), json!("demo")),
            ("other".into(), json!("demo"))
        ]
    );
    let filtered = s
        .call(method::PROFILE_LIST, json!({ "project_id": "demo" }))
        .unwrap();
    assert_eq!(filtered.as_array().unwrap().len(), 2);

    // Saved with its project_id, a profile goes to the project's folder.
    let mut saved: Value = toml::from_str::<toml::Table>(&profile_text("new"))
        .map(|t| serde_json::to_value(t).unwrap())
        .unwrap();
    saved["project_id"] = json!("demo");
    let written = s
        .call(method::PROFILE_SAVE, json!({ "profile": saved }))
        .unwrap();
    assert_eq!(written["path"], dir.join("new.toml").display().to_string());

    let deleted = s
        .call(method::PROFILE_DELETE, json!({ "id": "new" }))
        .unwrap();
    assert_eq!(deleted["profile_id"], "new");
    assert!(!dir.join("new.toml").exists());

    s.call(method::PROJECT_CLOSE, json!({ "id": "demo" }))
        .unwrap();
    assert!(profiles(&s).is_empty());
    assert!(dir.join("mine.toml").exists());
}

#[test]
fn project_templates_are_listed_and_applied_into_the_project() {
    let s = Setup::new("templates");
    let global = s.folder("global-templates");
    fs::write(global.join("shared.toml"), template_text("shared")).unwrap();
    s.core.set_template_places(regulus_core::templates::Places {
        templates: vec![global],
        profiles_dir: Some(s.dir.join("home/profiles")),
        profile_paths: vec![s.dir.join("home/profiles")],
        home: Some(s.dir.join("home")),
    });
    let demo = s.folder("demo");
    fs::create_dir_all(demo.join(".regulus/templates")).unwrap();
    fs::write(
        demo.join(".regulus/templates/local.toml"),
        template_text("local"),
    )
    .unwrap();
    open(&s, &demo);

    let list = s.call(method::TEMPLATE_LIST, Value::Null).unwrap();
    let names: Vec<(&str, &Value)> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| (t["name"].as_str().unwrap(), &t["project_id"]))
        .collect();
    assert_eq!(names, [("shared", &Value::Null), ("local", &json!("demo"))]);
    let got = s
        .call(method::TEMPLATE_GET, json!({ "template": "local" }))
        .unwrap();
    assert!(got["path"].as_str().unwrap().contains(".regulus/templates"));

    // A global template, applied into the project.
    let applied = s
        .call(
            method::TEMPLATE_APPLY,
            json!({ "template": "shared", "project_id": "demo" }),
        )
        .unwrap();
    let written = demo.join(".regulus/profiles/shared.toml");
    assert_eq!(applied["path"], written.display().to_string());
    assert!(fs::read_to_string(&written)
        .unwrap()
        .contains("project_id = \"demo\""));

    // A project's template goes to its project without saying so; a global one stays global.
    let local = s
        .call(method::TEMPLATE_APPLY, json!({ "template": "local" }))
        .unwrap();
    assert!(local["path"]
        .as_str()
        .unwrap()
        .contains(".regulus/profiles"));
    let plain = s
        .call(
            method::TEMPLATE_APPLY,
            json!({ "template": "shared", "vars": { "id": "plain" } }),
        )
        .unwrap();
    assert_eq!(
        plain["path"],
        s.dir.join("home/profiles/plain.toml").display().to_string()
    );
    let mut found = profiles(&s);
    found.sort_by(|a, b| a.0.cmp(&b.0));
    assert_eq!(
        found,
        [
            ("local".into(), json!("demo")),
            ("plain".into(), Value::Null),
            ("shared".into(), json!("demo"))
        ]
    );

    let unknown = s.call(
        method::TEMPLATE_APPLY,
        json!({ "template": "shared", "project_id": "nope" }),
    );
    assert_eq!(unknown.unwrap_err().0, code::PROJECT_NOT_FOUND);
}

#[test]
fn a_vetoed_module_is_disabled_in_check_and_refused_on_start() {
    let s = Setup::new("veto");
    let demo = s.folder("demo");
    let dir = demo.join(".regulus/profiles");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("mine.toml"), profile_text("mine")).unwrap();
    open(&s, &demo);

    // Not vetoed yet: it starts.
    let started = s
        .call(method::EXECUTION_START, json!({ "profile_id": "mine" }))
        .unwrap();
    assert_eq!(started["project_id"], "demo");
    s.call(method::EXECUTION_STOP, json!({ "id": started["id"] }))
        .unwrap();

    // An edit to regulus.toml counts without reopening.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(
        demo.join("regulus.toml"),
        "[modules]\ndisable = [\"test/echo\"]\n",
    )
    .unwrap();
    let check = s
        .call(method::PROFILE_CHECK, json!({ "id": "mine" }))
        .unwrap();
    let disabled: Vec<&Value> = check
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "node.module_disabled")
        .collect();
    assert_eq!(disabled.len(), 1, "{check}");
    assert_eq!(disabled[0]["severity"], "warning");
    assert_eq!(disabled[0]["node_id"], "echo");
    assert!(disabled[0]["message"]
        .as_str()
        .unwrap()
        .starts_with("disabled in project demo"));
    let refused = s
        .call(method::EXECUTION_START, json!({ "profile_id": "mine" }))
        .unwrap_err();
    assert_eq!(refused.0, code::MODULE_DISABLED);
    assert!(
        refused.1.contains("disabled in project demo"),
        "{}",
        refused.1
    );

    // `enable` without it vetoes it too; with it, it runs again.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(
        demo.join("regulus.toml"),
        "[modules]\nenable = [\"x/other\"]\n",
    )
    .unwrap();
    let refused = s.call(method::EXECUTION_START, json!({ "profile_id": "mine" }));
    assert_eq!(refused.unwrap_err().0, code::MODULE_DISABLED);
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(
        demo.join("regulus.toml"),
        "[modules]\nenable = [\"test/echo@0.1.0\"]\n",
    )
    .unwrap();
    s.call(method::EXECUTION_START, json!({ "profile_id": "mine" }))
        .unwrap();
}

#[test]
fn the_veto_holds_when_regulus_toml_turns_bad_or_goes_away_after_opening() {
    let s = Setup::new("hold");
    let demo = s.folder("demo");
    let dir = demo.join(".regulus/profiles");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("mine.toml"), profile_text("mine")).unwrap();
    fs::write(
        demo.join("regulus.toml"),
        "[modules]\ndisable = [\"test/echo\"]\n",
    )
    .unwrap();
    open(&s, &demo);
    let bad: [Option<&str>; 3] = [Some("[modules]\ndisable = 3\n"), Some("[modules\n"), None];
    for text in bad {
        std::thread::sleep(std::time::Duration::from_millis(20));
        match text {
            Some(text) => fs::write(demo.join("regulus.toml"), text).unwrap(),
            None => fs::remove_file(demo.join("regulus.toml")).unwrap(),
        }
        let details = s
            .call(method::PROJECT_GET, json!({ "id": "demo" }))
            .unwrap();
        assert!(
            details["warnings"]
                .to_string()
                .contains("last good regulus.toml"),
            "{text:?}: {details}"
        );
        assert_eq!(details["disable"], json!(["test/echo"]));
        let check = s
            .call(method::PROFILE_CHECK, json!({ "id": "mine" }))
            .unwrap();
        assert!(
            check.to_string().contains("node.module_disabled"),
            "{text:?}: {check}"
        );
        let refused = s.call(method::EXECUTION_START, json!({ "profile_id": "mine" }));
        assert_eq!(refused.unwrap_err().0, code::MODULE_DISABLED, "{text:?}");
    }
    let missing = s
        .call(method::PROJECT_GET, json!({ "id": "demo" }))
        .unwrap();
    let missing = missing["warnings"].to_string();
    assert!(missing.contains("regulus.toml missing"), "{missing}");
}

#[test]
fn a_template_of_another_project_is_not_applied_into_this_one() {
    let s = Setup::new("other-template");
    let a = s.folder("a");
    let b = s.folder("b");
    fs::create_dir_all(b.join(".regulus/templates")).unwrap();
    fs::write(
        b.join(".regulus/templates/only-b.toml"),
        template_text("only-b"),
    )
    .unwrap();
    open(&s, &a);
    open(&s, &b);
    let refused = s.call(
        method::TEMPLATE_APPLY,
        json!({ "template": "only-b", "project_id": "a" }),
    );
    assert_eq!(refused.unwrap_err().0, code::TEMPLATE_NOT_FOUND);
    let applied = s
        .call(method::TEMPLATE_APPLY, json!({ "template": "only-b" }))
        .unwrap();
    assert!(applied["path"]
        .as_str()
        .unwrap()
        .starts_with(&b.canonicalize().unwrap().display().to_string()));
}

#[test]
fn open_refuses_any_file_but_regulus_toml_and_a_broken_config() {
    let s = Setup::new("refuse");
    let demo = s.folder("demo");
    fs::write(demo.join("notes.txt"), "x").unwrap();
    let refused = s.call(
        method::PROJECT_OPEN,
        json!({ "path": demo.join("notes.txt") }),
    );
    assert_eq!(refused.unwrap_err().0, code::PROJECT_NOT_FOUND);
    assert!(!demo.join("regulus.toml").exists());

    fs::write(s.dir.join("home/config.toml"), "[server\n").unwrap();
    let refused = s.call(method::PROJECT_OPEN, json!({ "path": demo }));
    assert_eq!(refused.unwrap_err().0, code::MODULE_FAILED);
    assert!(!demo.join("regulus.toml").exists());
}

#[test]
fn a_regulus_toml_that_is_not_toml_is_refused_on_open_and_listed_as_failed_on_start() {
    let s = Setup::new("not-toml");
    let demo = s.folder("demo");
    fs::write(
        demo.join("regulus.toml"),
        "[modules\ndisable = [\"test/echo\"]\n",
    )
    .unwrap();
    let (code_, message) = s
        .call(method::PROJECT_OPEN, json!({ "path": demo }))
        .unwrap_err();
    assert_eq!(code_, code::PROJECT_INVALID);
    assert!(message.contains("is not valid TOML"), "{message}");
    assert!(!s.dir.join("home/config.toml").exists());

    // On start: a folder in [projects] paths whose file does not parse is listed with its
    // error and loads nothing; the core goes on.
    let dir = demo.join(".regulus/profiles");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("mine.toml"), profile_text("mine")).unwrap();
    fs::write(
        s.dir.join("home/config.toml"),
        format!("[projects]\npaths = [{:?}]\n", demo.display().to_string()),
    )
    .unwrap();
    let config = regulus_core::Config::load(&s.dir.join("home")).unwrap();
    let (core, warnings) = regulus_core::assemble(&config, Vec::new());
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("is not open") && w.contains("not valid TOML")),
        "{warnings:?}"
    );
    let list = core.dispatch(method::PROJECT_LIST, Value::Null).unwrap();
    assert_eq!(list[0]["id"], "demo");
    assert!(list[0]["error"]
        .as_str()
        .unwrap()
        .contains("not valid TOML"));
    let profiles = core.dispatch(method::PROFILE_LIST, json!({})).unwrap();
    assert!(profiles.as_array().unwrap().is_empty(), "{profiles}");
    let get = core.dispatch(method::PROJECT_GET, json!({ "id": "demo" }));
    assert_eq!(get.unwrap_err().code, code::PROJECT_NOT_FOUND);

    // Fixed, open tries again and it loads; or close forgets it.
    fs::write(demo.join("regulus.toml"), "[modules]\n").unwrap();
    let opened = core
        .dispatch(method::PROJECT_OPEN, json!({ "path": demo }))
        .unwrap();
    assert!(opened.get("error").is_none(), "{opened}");
    let list = core.dispatch(method::PROJECT_LIST, Value::Null).unwrap();
    assert_eq!(list.as_array().unwrap().len(), 1, "{list}");
    let profiles = core.dispatch(method::PROFILE_LIST, json!({})).unwrap();
    assert_eq!(profiles.as_array().unwrap().len(), 1, "{profiles}");

    fs::write(demo.join("regulus.toml"), "[modules\n").unwrap();
    let (core, _) = regulus_core::assemble(&config, Vec::new());
    let closed = core
        .dispatch(method::PROJECT_CLOSE, json!({ "id": "demo" }))
        .unwrap();
    assert!(closed["error"].is_string());
    assert!(core
        .dispatch(method::PROJECT_LIST, Value::Null)
        .unwrap()
        .as_array()
        .unwrap()
        .is_empty());
    let config: toml::Table =
        toml::from_str(&fs::read_to_string(s.dir.join("home/config.toml")).unwrap()).unwrap();
    assert!(config["projects"]["paths"].as_array().unwrap().is_empty());
}

#[test]
fn config_client_puts_the_project_on_top_of_config_toml() {
    let s = Setup::new("client");
    let global: toml::Table =
        toml::from_str("refresh_ms = 500\nintro = false\n[tui]\nevent_page = 50\n").unwrap();
    s.core.set_client(
        global,
        vec!["config.toml: unknown key `client.x`; ignored".into()],
    );

    let plain = s.call(method::CONFIG_CLIENT, json!({})).unwrap();
    assert_eq!(plain["client"]["refresh_ms"], 500);
    assert_eq!(plain["client"]["tui"]["event_page"], 50);
    assert_eq!(plain["sources"]["refresh_ms"], "config.toml");
    assert_eq!(plain["warnings"].as_array().unwrap().len(), 1);

    let demo = s.folder("demo");
    fs::write(
        demo.join("regulus.toml"),
        "[client]\nrefresh_ms = 250\nshiny = \"yes\"\n[client.tui]\nvalue_max = 80\n",
    )
    .unwrap();
    open(&s, &demo);
    let layered = s
        .call(method::CONFIG_CLIENT, json!({ "project_id": "demo" }))
        .unwrap();
    assert_eq!(layered["client"]["refresh_ms"], 250);
    assert_eq!(layered["client"]["intro"], false);
    assert_eq!(layered["client"]["shiny"], "yes");
    assert_eq!(layered["client"]["tui"]["event_page"], 50);
    assert_eq!(layered["client"]["tui"]["value_max"], 80);
    assert_eq!(layered["sources"]["refresh_ms"], "regulus.toml");
    assert_eq!(layered["sources"]["tui.event_page"], "config.toml");

    // Without a project nothing of it shows.
    assert_eq!(s.call(method::CONFIG_CLIENT, json!({})).unwrap(), plain);
    let (code, _) = s
        .call(method::CONFIG_CLIENT, json!({ "project_id": "nope" }))
        .unwrap_err();
    assert_eq!(code, code::PROJECT_NOT_FOUND);
}

#[test]
fn config_client_without_config_toml_is_empty() {
    let s = Setup::new("client-empty");
    let answer = s.call(method::CONFIG_CLIENT, json!({})).unwrap();
    assert_eq!(
        answer,
        json!({ "client": {}, "sources": {}, "warnings": [] })
    );
}
