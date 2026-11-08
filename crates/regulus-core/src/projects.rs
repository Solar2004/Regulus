//! Projects (docs/11-composition.md §13): a folder with a `regulus.toml`. Everything in the
//! file is optional and the file tolerates what it does not understand: unknown keys are
//! kept and reported, never fatal. The folders that are open live in `config.toml`,
//! `[projects] paths`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use regulus_proto::{ProjectDetails, ProjectSummary};
use serde::{Deserialize, Serialize};

use crate::registry::base_id;

/// The file that makes a folder a project.
pub const PROJECT_FILE: &str = "regulus.toml";
/// Where a project keeps its profiles when its file names no `[project] profiles`.
const DEFAULT_PROFILES: &str = ".regulus/profiles";
/// Where a project keeps its templates when its file names no `[project] templates`.
const DEFAULT_TEMPLATES: &str = ".regulus/templates";
/// The table and key of `config.toml` that list the open project folders.
const CONFIG_TABLE: &str = "projects";
const CONFIG_PATHS: &str = "paths";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProjectError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Exists(String),
    #[error("{0}")]
    Io(String),
}

/// `regulus.toml` as read. Keys the core does not know stay in `extra`, where they were.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectFile {
    pub project: ProjectTable,
    pub modules: ModulesTable,
    /// Free: handed to clients as it is.
    pub client: toml::Table,
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProjectTable {
    /// Defaults to the folder's name.
    pub id: Option<String>,
    pub name: Option<String>,
    /// Files or directories with profiles, relative to the file. Empty means
    /// `.regulus/profiles`.
    pub profiles: Vec<PathBuf>,
    /// Files or directories with templates, relative to the file. Empty means
    /// `.regulus/templates`.
    pub templates: Vec<PathBuf>,
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ModulesTable {
    /// Module ids (`author/module`, any version) whose nodes do not run in this project.
    pub disable: Vec<String>,
    /// If present, only these modules run in this project.
    pub enable: Option<Vec<String>>,
    /// Modules of this project only, run out of process. Kept, not loaded yet.
    pub local: Vec<LocalModule>,
    #[serde(flatten)]
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalModule {
    pub manifest: Option<PathBuf>,
    pub command: Vec<String>,
    #[serde(flatten)]
    pub extra: toml::Table,
}

/// An open project: its folder, its file and what the core made of it.
#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// The folder, as it was opened (absolute).
    pub root: PathBuf,
    pub file: ProjectFile,
    /// `[project] profiles` and `templates`, resolved against the folder.
    pub profiles: Vec<PathBuf>,
    pub templates: Vec<PathBuf>,
    /// What the core did not understand or could not use. Never fatal.
    pub warnings: Vec<String>,
}

fn unknown(warnings: &mut Vec<String>, at: &str, extra: &toml::Table) {
    for key in extra.keys() {
        warnings.push(format!(
            "{PROJECT_FILE}: unknown key `{at}{key}`; kept and ignored"
        ));
    }
}

/// A project id is a profile id: one plain name that is safe as a file name.
pub fn check_id(id: &str) -> Result<(), ProjectError> {
    crate::templates::file_name(id).map(|_| ()).map_err(|_| {
        ProjectError::Invalid(format!(
            "project id `{id}` must be one plain name, without `/`, `\\`, a leading `.` or \
             control characters; set `[project] id` in {PROJECT_FILE}"
        ))
    })
}

fn resolve(root: &Path, paths: &[PathBuf], default: &str) -> Vec<PathBuf> {
    if paths.is_empty() {
        return vec![root.join(default)];
    }
    paths.iter().map(|p| root.join(p)).collect()
}

impl Project {
    /// Reads the project in `root`. A file that is missing or cannot be read leaves every
    /// default in place and says why; only an id that cannot be used is an error.
    pub fn load(root: &Path) -> Result<Self, ProjectError> {
        let path = root.join(PROJECT_FILE);
        let mut warnings = Vec::new();
        let file = match fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<ProjectFile>(&text) {
                Ok(file) => file,
                Err(e) => {
                    warnings.push(format!(
                        "{} cannot be read, so every default applies: {e}",
                        path.display()
                    ));
                    ProjectFile::default()
                }
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => ProjectFile::default(),
            Err(e) => {
                warnings.push(format!(
                    "cannot read {}, so every default applies: {e}",
                    path.display()
                ));
                ProjectFile::default()
            }
        };
        unknown(&mut warnings, "", &file.extra);
        unknown(&mut warnings, "project.", &file.project.extra);
        unknown(&mut warnings, "modules.", &file.modules.extra);
        for (i, local) in file.modules.local.iter().enumerate() {
            unknown(&mut warnings, &format!("modules.local[{i}]."), &local.extra);
        }
        if !file.modules.local.is_empty() {
            warnings.push(format!(
                "{PROJECT_FILE}: local modules are not supported yet; {} [[modules.local]] \
                 kept, not loaded",
                file.modules.local.len()
            ));
        }
        let id = match &file.project.id {
            Some(id) => id.clone(),
            None => root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        check_id(&id)?;
        Ok(Self {
            name: file.project.name.clone().unwrap_or_else(|| id.clone()),
            id,
            root: root.to_path_buf(),
            profiles: resolve(root, &file.project.profiles, DEFAULT_PROFILES),
            templates: resolve(root, &file.project.templates, DEFAULT_TEMPLATES),
            file,
            warnings,
        })
    }

    /// Whether this project keeps a module's nodes from running: `disable` names it, or
    /// `enable` is there and does not. By id, with or without the version.
    pub fn vetoes(&self, module_id: &str) -> bool {
        let wanted = base_id(module_id);
        let named = |list: &[String]| list.iter().any(|m| base_id(m) == wanted);
        named(&self.file.modules.disable)
            || self
                .file
                .modules
                .enable
                .as_deref()
                .is_some_and(|enable| !named(enable))
    }

    /// The warnings, plus a `disable` or `enable` entry that names no loaded module.
    pub fn warnings(&self, is_installed: impl Fn(&str) -> bool) -> Vec<String> {
        let mut warnings = self.warnings.clone();
        let modules = &self.file.modules;
        let lists = [
            ("disable", Some(&modules.disable)),
            ("enable", modules.enable.as_ref()),
        ];
        for (key, list) in lists {
            for module in list.into_iter().flatten().filter(|m| !is_installed(m)) {
                warnings.push(format!(
                    "{PROJECT_FILE}: modules.{key} names `{module}`, which is not loaded; \
                     modules are named by their manifest id (`author/module`)"
                ));
            }
        }
        warnings
    }

    pub fn summary(&self, warnings: Vec<String>) -> ProjectSummary {
        ProjectSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            path: self.root.display().to_string(),
            warnings,
        }
    }

    pub fn details(&self, warnings: Vec<String>) -> ProjectDetails {
        let shown = |paths: &[PathBuf]| paths.iter().map(|p| p.display().to_string()).collect();
        ProjectDetails {
            id: self.id.clone(),
            name: self.name.clone(),
            path: self.root.display().to_string(),
            file: self.root.join(PROJECT_FILE).display().to_string(),
            profiles: shown(&self.profiles),
            templates: shown(&self.templates),
            disable: self.file.modules.disable.clone(),
            enable: self.file.modules.enable.clone(),
            local: self.file.modules.local.iter().map(json).collect(),
            client: match json(&self.file.client) {
                serde_json::Value::Object(map) => map,
                _ => serde_json::Map::new(),
            },
            content: json(&self.file),
            warnings,
        }
    }
}

/// What TOML holds, as JSON; it always converts.
fn json(value: &impl Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_default()
}

/// The folder a client means: a relative path is resolved against `base`, and a path to a
/// file (its `regulus.toml`) means the folder holding it. It must exist.
pub fn folder(base: &Path, path: &Path) -> Result<PathBuf, ProjectError> {
    let path = base.join(path);
    let path = if path.is_file() {
        path.parent().map(Path::to_path_buf).unwrap_or(path)
    } else {
        path
    };
    if !path.is_dir() {
        return Err(ProjectError::NotFound(format!(
            "{} is not a folder",
            path.display()
        )));
    }
    path.canonicalize()
        .map_err(|e| ProjectError::Io(format!("cannot resolve {}: {e}", path.display())))
}

/// Gives `root` a minimal `regulus.toml` if it has none: `[project] name`, the folder's
/// name. An existing file is never touched.
pub fn ensure_file(root: &Path) -> Result<(), ProjectError> {
    let path = root.join(PROJECT_FILE);
    if path.exists() {
        return Ok(());
    }
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let text = format!("[project]\nname = {}\n", toml::Value::String(name));
    let mut file = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => return Ok(()),
        Err(e) => {
            return Err(ProjectError::Io(format!(
                "cannot create {}: {e}",
                path.display()
            )))
        }
    };
    io::Write::write_all(&mut file, text.as_bytes())
        .map_err(|e| ProjectError::Io(format!("cannot write {}: {e}", path.display())))
}

/// The open projects, in the order of `[projects] paths`.
#[derive(Debug, Default)]
pub struct Projects {
    open: Vec<Project>,
}

impl Projects {
    /// Opens every folder in `paths` (already resolved). A folder that is gone, an id that
    /// cannot be used or one already taken is skipped with a warning; the path stays in
    /// `config.toml`.
    pub fn load(paths: &[PathBuf]) -> (Self, Vec<String>) {
        let mut projects = Self::default();
        let mut warnings = Vec::new();
        for path in paths {
            let opened = folder(Path::new(""), path)
                .and_then(|root| Project::load(&root))
                .and_then(|project| projects.insert(project));
            if let Err(error) = opened {
                warnings.push(format!("project {} is not open: {error}", path.display()));
            }
        }
        (projects, warnings)
    }

    /// Adds an open project, unless its id or its folder is open already.
    pub fn insert(&mut self, project: Project) -> Result<(), ProjectError> {
        if let Some(other) = self
            .open
            .iter()
            .find(|p| p.id == project.id || p.root == project.root)
        {
            return Err(ProjectError::Exists(format!(
                "project `{}` is already open at {}",
                other.id,
                other.root.display()
            )));
        }
        self.open.push(project);
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Option<Project> {
        let position = self.open.iter().position(|p| p.id == id)?;
        Some(self.open.remove(position))
    }

    pub fn get(&self, id: &str) -> Option<&Project> {
        self.open.iter().find(|p| p.id == id)
    }

    pub fn by_root(&self, root: &Path) -> Option<&Project> {
        self.open.iter().find(|p| p.root == root)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Project> {
        self.open.iter()
    }
}

/// Reads `config.toml` as a table, or an empty one if it does not exist.
fn read_config(path: &Path) -> Result<toml::Table, ProjectError> {
    match fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map_err(|e| ProjectError::Io(format!("cannot edit {}: {e}", path.display()))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(toml::Table::new()),
        Err(e) => Err(ProjectError::Io(format!(
            "cannot read {}: {e}",
            path.display()
        ))),
    }
}

/// Writes `table` over `path` through a temporary file, so a reader never sees half of it.
fn write_config(path: &Path, table: &toml::Table) -> Result<(), ProjectError> {
    let text = toml::to_string(table)
        .map_err(|e| ProjectError::Io(format!("cannot write {}: {e}", path.display())))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.{}.tmp", std::process::id()));
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)
            .map_err(|e| ProjectError::Io(format!("cannot create {}: {e}", dir.display())))?;
    }
    fs::write(&temp, text)
        .and_then(|()| fs::rename(&temp, path))
        .map_err(|e| {
            let _ = fs::remove_file(&temp);
            ProjectError::Io(format!("cannot write {}: {e}", path.display()))
        })
}

/// The `[projects] paths` array of a `config.toml` table, created if missing. Anything
/// else there is refused rather than overwritten.
fn paths_of<'a>(
    table: &'a mut toml::Table,
    path: &Path,
) -> Result<&'a mut Vec<toml::Value>, ProjectError> {
    let wrong = || {
        ProjectError::Io(format!(
            "{}: `{CONFIG_TABLE}.{CONFIG_PATHS}` is not a list of paths; fix it by hand",
            path.display()
        ))
    };
    let projects = table
        .entry(CONFIG_TABLE)
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .ok_or_else(wrong)?;
    projects
        .entry(CONFIG_PATHS)
        .or_insert_with(|| toml::Value::Array(Vec::new()))
        .as_array_mut()
        .ok_or_else(wrong)
}

/// Whether a `[projects] paths` entry, resolved against `home`, is the folder `root`.
fn same_folder(home: &Path, entry: &toml::Value, root: &Path) -> bool {
    let Some(entry) = entry.as_str() else {
        return false;
    };
    let entry = home.join(entry);
    entry == root || entry.canonicalize().is_ok_and(|e| e == root)
}

/// Adds `root` to `[projects] paths` in the `config.toml` at `path`, unless it is there.
/// Only that key changes; the rest of the file is read and written back as it was (as TOML:
/// comments are not kept).
pub fn remember(path: &Path, home: &Path, root: &Path) -> Result<(), ProjectError> {
    let mut table = read_config(path)?;
    let paths = paths_of(&mut table, path)?;
    if paths.iter().any(|entry| same_folder(home, entry, root)) {
        return Ok(());
    }
    paths.push(toml::Value::String(root.display().to_string()));
    write_config(path, &table)
}

/// Takes `root` out of `[projects] paths` in the `config.toml` at `path`. Nothing on disk
/// but that key changes.
pub fn forget(path: &Path, home: &Path, root: &Path) -> Result<(), ProjectError> {
    let mut table = read_config(path)?;
    let paths = paths_of(&mut table, path)?;
    let before = paths.len();
    paths.retain(|entry| !same_folder(home, entry, root));
    if paths.len() == before {
        return Ok(());
    }
    write_config(path, &table)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("regulus-projects-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_folder_without_a_file_is_a_project_named_after_it() {
        let root = temp("bare").join("demo");
        fs::create_dir_all(&root).unwrap();
        let project = Project::load(&root).unwrap();
        assert_eq!(project.id, "demo");
        assert_eq!(project.name, "demo");
        assert_eq!(project.profiles, vec![root.join(".regulus/profiles")]);
        assert_eq!(project.templates, vec![root.join(".regulus/templates")]);
        assert!(project.warnings.is_empty(), "{:?}", project.warnings);
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn unknown_keys_are_kept_and_reported_never_fatal() {
        let root = temp("unknown");
        fs::write(
            root.join(PROJECT_FILE),
            r#"
color = "blue"
[project]
id = "mine"
name = "Mine"
profiles = ["p"]
foo = "bar"
[modules]
disable = ["regulus/web"]
weird = 1
[[modules.local]]
manifest = "tools/m.toml"
command = ["tools/run"]
also = true
[client]
start_view = "operate"
[future]
x = 1
"#,
        )
        .unwrap();
        let project = Project::load(&root).unwrap();
        assert_eq!(project.id, "mine");
        assert_eq!(project.name, "Mine");
        assert_eq!(project.profiles, vec![root.join("p")]);
        let all = project.warnings.join("\n");
        for key in [
            "`color`",
            "`future`",
            "`project.foo`",
            "`modules.weird`",
            "`modules.local[0].also`",
        ] {
            assert!(all.contains(key), "{key} in {all}");
        }
        assert!(all.contains("local modules are not supported yet"), "{all}");
        assert_eq!(project.file.extra["color"].as_str(), Some("blue"));
        assert_eq!(project.file.project.extra["foo"].as_str(), Some("bar"));
        let details = project.details(project.warnings.clone());
        assert_eq!(details.client["start_view"], "operate");
        assert_eq!(details.content["project"]["foo"], "bar");
        assert_eq!(details.local.len(), 1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_file_leaves_the_defaults_and_says_why() {
        let root = temp("broken").join("demo");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(PROJECT_FILE), "[project\nname=").unwrap();
        let project = Project::load(&root).unwrap();
        assert_eq!(project.id, "demo");
        assert!(
            project.warnings[0].contains("cannot be read"),
            "{:?}",
            project.warnings
        );
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }

    #[test]
    fn an_id_that_is_not_a_file_name_is_refused() {
        let root = temp("badid");
        fs::write(root.join(PROJECT_FILE), "[project]\nid = \"../up\"\n").unwrap();
        assert!(matches!(
            Project::load(&root),
            Err(ProjectError::Invalid(_))
        ));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn disable_and_enable_veto_by_module_id() {
        let root = temp("veto");
        fs::write(
            root.join(PROJECT_FILE),
            "[modules]\ndisable = [\"a/web\"]\n",
        )
        .unwrap();
        let project = Project::load(&root).unwrap();
        assert!(project.vetoes("a/web@0.1.0"));
        assert!(!project.vetoes("a/files@0.1.0"));
        let warnings = project.warnings(|id| base_id(id) == "a/files");
        assert!(
            warnings.iter().any(|w| w.contains("`a/web`")),
            "{warnings:?}"
        );

        fs::write(
            root.join(PROJECT_FILE),
            "[modules]\nenable = [\"a/files\"]\n",
        )
        .unwrap();
        let project = Project::load(&root).unwrap();
        assert!(project.vetoes("a/web"));
        assert!(!project.vetoes("a/files@1.0.0"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn remembering_edits_only_the_paths_and_keeps_the_rest() {
        let home = temp("config");
        let root = temp("config-root");
        let config = home.join("config.toml");
        fs::write(
            &config,
            "[server]\nport = 0\n\n[profiles]\npaths = [\"p\"]\n\n[mystery]\nkeep = true\n",
        )
        .unwrap();
        remember(&config, &home, &root).unwrap();
        remember(&config, &home, &root).unwrap();
        let table: toml::Table = toml::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(table["server"]["port"].as_integer(), Some(0));
        assert_eq!(table["mystery"]["keep"].as_bool(), Some(true));
        let paths = table["projects"]["paths"].as_array().unwrap();
        assert_eq!(paths.len(), 1);
        forget(&config, &home, &root).unwrap();
        let table: toml::Table = toml::from_str(&fs::read_to_string(&config).unwrap()).unwrap();
        assert!(table["projects"]["paths"].as_array().unwrap().is_empty());
        assert_eq!(table["profiles"]["paths"][0].as_str(), Some("p"));
        let _ = fs::remove_dir_all(&home);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_file_writes_a_minimal_file_once() {
        let root = temp("ensure").join("my proj");
        fs::create_dir_all(&root).unwrap();
        ensure_file(&root).unwrap();
        let text = fs::read_to_string(root.join(PROJECT_FILE)).unwrap();
        assert_eq!(text, "[project]\nname = \"my proj\"\n");
        fs::write(root.join(PROJECT_FILE), "# mine\n").unwrap();
        ensure_file(&root).unwrap();
        assert_eq!(
            fs::read_to_string(root.join(PROJECT_FILE)).unwrap(),
            "# mine\n"
        );
        let _ = fs::remove_dir_all(root.parent().unwrap());
    }
}
