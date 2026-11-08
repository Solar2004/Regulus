//! Projects (docs/11-composition.md §13): a folder with a `regulus.toml`. Everything in the
//! file is optional and the file tolerates what it does not understand: unknown keys are
//! kept and reported, never fatal. The folders that are open live in `config.toml`,
//! `[projects] paths`.
//!
//! Once a project is open, a later version of its file that cannot be used (missing,
//! unreadable, or with a key of the wrong type) never replaces the last good one: the veto
//! of `[modules]` must not fall away because of a typo.

use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::SystemTime;

use regulus_proto::{ProjectDetails, ProjectSummary};

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

/// `regulus.toml` as the core understood it. Keys it does not know stay in `extra`.
#[derive(Debug, Clone, Default)]
pub struct ProjectFile {
    pub project: ProjectTable,
    pub modules: ModulesTable,
    /// Free: handed to clients as it is.
    pub client: toml::Table,
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Default)]
pub struct ProjectTable {
    /// Defaults to the folder's name.
    pub id: Option<String>,
    pub name: Option<String>,
    /// Folders or files with profiles, inside the project. Empty means
    /// `.regulus/profiles`.
    pub profiles: Vec<PathBuf>,
    /// Folders or files with templates, inside the project. Empty means
    /// `.regulus/templates`.
    pub templates: Vec<PathBuf>,
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Default)]
pub struct ModulesTable {
    /// Module ids (`author/module`, any version) whose nodes do not run in this project.
    pub disable: Vec<String>,
    /// If present, only these modules run in this project.
    pub enable: Option<Vec<String>>,
    /// Modules of this project only, run out of process. Kept, not loaded yet.
    pub local: Vec<LocalModule>,
    pub extra: toml::Table,
}

#[derive(Debug, Clone, Default)]
pub struct LocalModule {
    pub manifest: Option<PathBuf>,
    pub command: Vec<String>,
    pub extra: toml::Table,
}

/// Reading `regulus.toml`: what was understood, the file as it is, the keys it did not
/// know and the keys it could not use (`problems`: the wrong type).
struct Parsed {
    file: ProjectFile,
    raw: toml::Table,
    warnings: Vec<String>,
    problems: Vec<String>,
}

/// One known key of a table: taken out of it and converted; the wrong type is a problem
/// and the key is dropped.
struct Taker<'a> {
    at: &'a str,
    problems: &'a mut Vec<String>,
}

impl Taker<'_> {
    fn take<T>(
        &mut self,
        table: &mut toml::Table,
        key: &str,
        what: &str,
        convert: impl Fn(toml::Value) -> Option<T>,
    ) -> Option<T> {
        let value = table.remove(key)?;
        let shown = value.to_string();
        let converted = convert(value);
        if converted.is_none() {
            self.problems.push(format!(
                "{PROJECT_FILE}: `{}{key}` must be {what}, not {shown}; ignored",
                self.at
            ));
        }
        converted
    }

    fn text(&mut self, table: &mut toml::Table, key: &str) -> Option<String> {
        self.take(table, key, "text", |v| match v {
            toml::Value::String(s) => Some(s),
            _ => None,
        })
    }

    /// A list of text; one text alone is a list of one.
    fn list(&mut self, table: &mut toml::Table, key: &str) -> Option<Vec<String>> {
        self.take(table, key, "a list of text", |v| match v {
            toml::Value::String(s) => Some(vec![s]),
            toml::Value::Array(items) => items
                .into_iter()
                .map(|i| match i {
                    toml::Value::String(s) => Some(s),
                    _ => None,
                })
                .collect(),
            _ => None,
        })
    }

    fn table(&mut self, table: &mut toml::Table, key: &str) -> Option<toml::Table> {
        self.take(table, key, "a table", |v| match v {
            toml::Value::Table(t) => Some(t),
            _ => None,
        })
    }
}

fn paths(list: Option<Vec<String>>) -> Vec<PathBuf> {
    list.unwrap_or_default()
        .into_iter()
        .map(PathBuf::from)
        .collect()
}

fn unknown(warnings: &mut Vec<String>, at: &str, extra: &toml::Table) {
    for key in extra.keys() {
        warnings.push(format!(
            "{PROJECT_FILE}: unknown key `{at}{key}`; kept and ignored"
        ));
    }
}

/// Reads the text of a `regulus.toml`. Only a file that is not TOML at all is an error.
fn parse(text: &str) -> Result<Parsed, String> {
    let raw: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
    let mut problems = Vec::new();
    let mut top = raw.clone();
    let mut t = Taker {
        at: "",
        problems: &mut problems,
    };
    let mut project = t.table(&mut top, "project").unwrap_or_default();
    let mut modules = t.table(&mut top, "modules").unwrap_or_default();
    let client = t.table(&mut top, "client").unwrap_or_default();
    let mut t = Taker {
        at: "project.",
        problems: &mut problems,
    };
    let project_table = ProjectTable {
        id: t.text(&mut project, "id"),
        name: t.text(&mut project, "name"),
        profiles: paths(t.list(&mut project, "profiles")),
        templates: paths(t.list(&mut project, "templates")),
        extra: project,
    };
    let mut t = Taker {
        at: "modules.",
        problems: &mut problems,
    };
    let disable = t.list(&mut modules, "disable").unwrap_or_default();
    let enable = t.list(&mut modules, "enable");
    let entries = t
        .take(&mut modules, "local", "a list of tables", |v| match v {
            toml::Value::Array(items) => items
                .into_iter()
                .map(|i| match i {
                    toml::Value::Table(t) => Some(t),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>(),
            _ => None,
        })
        .unwrap_or_default();
    let mut local = Vec::with_capacity(entries.len());
    for (i, mut entry) in entries.into_iter().enumerate() {
        let at = format!("modules.local[{i}].");
        let mut t = Taker {
            at: &at,
            problems: &mut problems,
        };
        local.push(LocalModule {
            manifest: t.text(&mut entry, "manifest").map(PathBuf::from),
            command: t.list(&mut entry, "command").unwrap_or_default(),
            extra: entry,
        });
    }
    let file = ProjectFile {
        project: project_table,
        modules: ModulesTable {
            disable,
            enable,
            local,
            extra: modules,
        },
        client,
        extra: top,
    };
    let mut warnings = Vec::new();
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
    Ok(Parsed {
        file,
        raw,
        warnings,
        problems,
    })
}

/// What reading `root`'s `regulus.toml` gave.
enum Read {
    Missing,
    /// Unreadable or not TOML: why.
    Broken(String),
    Parsed(Box<Parsed>),
}

fn read(root: &Path) -> Read {
    let path = root.join(PROJECT_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => match parse(&text) {
            Ok(parsed) => Read::Parsed(Box::new(parsed)),
            Err(e) => Read::Broken(format!("{} is not valid TOML: {e}", path.display())),
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => Read::Missing,
        Err(e) => Read::Broken(format!("cannot read {}: {e}", path.display())),
    }
}

/// An open project: its folder, its file and what the core made of it.
#[derive(Debug, Clone)]
pub struct Project {
    pub id: String,
    pub name: String,
    /// The folder, as it was opened (absolute and canonical).
    pub root: PathBuf,
    pub file: ProjectFile,
    /// The file as it was read, unknown keys included; empty if there was none.
    pub raw: toml::Table,
    /// `[project] profiles` and `templates`, resolved inside the folder.
    pub profiles: Vec<PathBuf>,
    pub templates: Vec<PathBuf>,
    /// What the core did not understand or could not use in the file. Never fatal.
    pub warnings: Vec<String>,
    /// What loading its profiles found.
    pub notes: Vec<String>,
    /// Why the file on disk now is not the one in use, if it is not.
    pub stale: Option<String>,
    /// When the file was last changed as the core read it; `None` if it had none.
    pub modified: Option<SystemTime>,
}

/// When `root`'s `regulus.toml` was last changed, if it can be told.
fn modified(root: &Path) -> Option<SystemTime> {
    fs::metadata(root.join(PROJECT_FILE))
        .and_then(|m| m.modified())
        .ok()
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

/// The `profiles` or `templates` entries of `key`, inside `root`: an absolute path, one
/// that climbs (`..`) or one that leads out through a link is refused with a warning, since
/// the core writes and deletes profiles there. None left means the default.
fn resolve(
    root: &Path,
    key: &str,
    paths: &[PathBuf],
    default: &str,
    warnings: &mut Vec<String>,
) -> Vec<PathBuf> {
    let mut inside = Vec::new();
    for path in paths {
        let plain = path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        let joined = root.join(path);
        let escapes = joined
            .canonicalize()
            .is_ok_and(|real| !real.starts_with(root));
        if plain && !escapes && !path.as_os_str().is_empty() {
            inside.push(joined);
        } else {
            warnings.push(format!(
                "{PROJECT_FILE}: `project.{key}` entry `{}` is not inside the project; \
                 ignored (use a path relative to the folder, without `..`)",
                path.display()
            ));
        }
    }
    if inside.is_empty() {
        inside.push(root.join(default));
    }
    inside
}

impl Project {
    /// Reads the project in `root`. A file that is missing, unreadable or not TOML leaves
    /// every default in place and says why; a key of the wrong type is ignored and said.
    /// Only an id that cannot be used is an error.
    pub fn load(root: &Path) -> Result<Self, ProjectError> {
        let changed = modified(root);
        let parsed = match read(root) {
            Read::Parsed(parsed) => *parsed,
            Read::Missing => Parsed {
                file: ProjectFile::default(),
                raw: toml::Table::new(),
                warnings: vec![format!("{PROJECT_FILE} missing; every default applies")],
                problems: Vec::new(),
            },
            Read::Broken(why) => Parsed {
                file: ProjectFile::default(),
                raw: toml::Table::new(),
                warnings: vec![format!("{why}; every default applies")],
                problems: Vec::new(),
            },
        };
        Self::build(root, parsed, changed)
    }

    fn build(
        root: &Path,
        parsed: Parsed,
        changed: Option<SystemTime>,
    ) -> Result<Self, ProjectError> {
        let Parsed {
            file,
            raw,
            mut warnings,
            problems,
        } = parsed;
        warnings.extend(problems);
        let id = match &file.project.id {
            Some(id) => id.clone(),
            None => root
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        check_id(&id)?;
        let profiles = resolve(
            root,
            "profiles",
            &file.project.profiles,
            DEFAULT_PROFILES,
            &mut warnings,
        );
        let templates = resolve(
            root,
            "templates",
            &file.project.templates,
            DEFAULT_TEMPLATES,
            &mut warnings,
        );
        Ok(Self {
            name: file.project.name.clone().unwrap_or_else(|| id.clone()),
            id,
            root: root.to_path_buf(),
            profiles,
            templates,
            file,
            raw,
            warnings,
            notes: Vec::new(),
            stale: None,
            modified: changed,
        })
    }

    /// Whether its file changed since the core read it.
    pub fn is_stale(&self) -> bool {
        modified(&self.root) != self.modified
    }

    /// Reads the file again after it changed. A version that cannot be used (missing,
    /// unreadable, not TOML, a key of the wrong type, another id) leaves this one in use
    /// and says why; a good one takes its place.
    fn refreshed(&self) -> Self {
        let changed = modified(&self.root);
        let keep = |why: String| Self {
            stale: Some(format!("{why}; still using the last good {PROJECT_FILE}")),
            modified: changed,
            ..self.clone()
        };
        let parsed = match read(&self.root) {
            Read::Missing => return keep(format!("{PROJECT_FILE} missing")),
            Read::Broken(why) => return keep(why),
            Read::Parsed(parsed) if !parsed.problems.is_empty() => {
                return keep(parsed.problems.join("; "))
            }
            Read::Parsed(parsed) => *parsed,
        };
        match Self::build(&self.root, parsed, changed) {
            Ok(fresh) if fresh.id == self.id => Self {
                notes: self.notes.clone(),
                ..fresh
            },
            Ok(fresh) => keep(format!(
                "{PROJECT_FILE} now says id `{}`; close and open the project to use it",
                fresh.id
            )),
            Err(error) => keep(error.to_string()),
        }
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
        warnings.extend(self.stale.iter().cloned());
        warnings.extend(self.notes.iter().cloned());
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
        let local = self
            .raw
            .get("modules")
            .and_then(|m| m.get("local"))
            .and_then(toml::Value::as_array)
            .map(|items| items.iter().map(json).collect())
            .unwrap_or_default();
        ProjectDetails {
            id: self.id.clone(),
            name: self.name.clone(),
            path: self.root.display().to_string(),
            file: self.root.join(PROJECT_FILE).display().to_string(),
            profiles: shown(&self.profiles),
            templates: shown(&self.templates),
            disable: self.file.modules.disable.clone(),
            enable: self.file.modules.enable.clone(),
            local,
            client: match json(&self.file.client) {
                serde_json::Value::Object(map) => map,
                _ => serde_json::Map::new(),
            },
            content: json(&self.raw),
            warnings,
        }
    }
}

/// What TOML holds, as JSON; it always converts.
fn json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).unwrap_or_default()
}

/// The folder a client means: a relative path is resolved against `base`, and a path to
/// its `regulus.toml` means the folder holding it; any other file is refused. It must
/// exist.
pub fn folder(base: &Path, path: &Path) -> Result<PathBuf, ProjectError> {
    let path = base.join(path);
    let path = if path.is_file() {
        if path.file_name() != Some(PROJECT_FILE.as_ref()) {
            return Err(ProjectError::NotFound(format!(
                "{} is a file; open a folder or its {PROJECT_FILE}",
                path.display()
            )));
        }
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
    open: Vec<Arc<Project>>,
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
        self.open.push(Arc::new(project));
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Option<Arc<Project>> {
        let position = self.open.iter().position(|p| p.id == id)?;
        Some(self.open.remove(position))
    }

    pub fn get(&self, id: &str) -> Option<Arc<Project>> {
        self.open.iter().find(|p| p.id == id).cloned()
    }

    pub fn by_root(&self, root: &Path) -> Option<Arc<Project>> {
        self.open.iter().find(|p| p.root == root).cloned()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Project> {
        self.open.iter().map(Arc::as_ref)
    }

    /// Adds what loading a project's profiles found to its notes.
    pub fn note(&mut self, id: &str, notes: Vec<String>) {
        if let Some(project) = self.open.iter_mut().find(|p| p.id == id) {
            Arc::make_mut(project).notes.extend(notes);
        }
    }

    /// Reads again every file that changed since it was read, so an edit to
    /// `regulus.toml` counts without reopening ([`Project`] keeps the last good one).
    /// Profiles folders are read on open only; templates folders, on every use.
    pub fn refresh(&mut self) {
        for project in self.open.iter_mut().filter(|p| p.is_stale()) {
            *project = Arc::new(project.refreshed());
        }
    }
}

/// Reads `config.toml` as a table, or an empty one if it does not exist.
pub fn read_config(path: &Path) -> Result<toml::Table, ProjectError> {
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
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let temp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
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
        assert_eq!(
            project.warnings,
            ["regulus.toml missing; every default applies"]
        );
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
            project.warnings[0].contains("is not valid TOML"),
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
    fn a_key_of_the_wrong_type_is_ignored_and_one_text_is_a_list_of_one() {
        let root = temp("types");
        fs::write(
            root.join(PROJECT_FILE),
            "[project]\nprofiles = 3\nname = 7\n[modules]\ndisable = \"a/web\"\nenable = [1]\n",
        )
        .unwrap();
        let project = Project::load(&root).unwrap();
        assert!(project.vetoes("a/web@0.1.0"));
        assert_eq!(project.file.modules.enable, None);
        assert_eq!(project.profiles, vec![root.join(".regulus/profiles")]);
        let all = project.warnings.join("\n");
        for key in [
            "`project.profiles` must be",
            "`project.name` must be",
            "`modules.enable` must be",
        ] {
            assert!(all.contains(key), "{key} in {all}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn profiles_and_templates_stay_inside_the_project() {
        let root = temp("inside");
        let outside = temp("inside-out");
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        fs::write(
            root.join(PROJECT_FILE),
            format!(
                "[project]\nprofiles = [\"/etc\", \"../../x\", \"a/../../b\", \"link\", \"ok\"]\n\
                 templates = [\"{}\"]\n",
                outside.display()
            ),
        )
        .unwrap();
        let root = root.canonicalize().unwrap();
        let project = Project::load(&root).unwrap();
        assert_eq!(project.profiles, vec![root.join("ok")]);
        assert_eq!(project.templates, vec![root.join(".regulus/templates")]);
        let refused = project
            .warnings
            .iter()
            .filter(|w| w.contains("is not inside the project"))
            .count();
        assert_eq!(refused, 5, "{:?}", project.warnings);
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn a_file_that_turns_bad_after_opening_keeps_the_last_good_one() {
        let root = temp("refresh");
        let file = root.join(PROJECT_FILE);
        fs::write(&file, "[modules]\ndisable = [\"a/web\"]\n").unwrap();
        let mut projects = Projects::default();
        projects.insert(Project::load(&root).unwrap()).unwrap();
        let bad = [
            "[modules]\ndisable = 3\n",
            "[modules\n",
            "[project]\nid = \"other\"\n",
        ];
        for text in bad {
            std::thread::sleep(std::time::Duration::from_millis(15));
            fs::write(&file, text).unwrap();
            projects.refresh();
            let project = projects.get(&root_id(&root)).unwrap();
            assert!(project.vetoes("a/web"), "after {text:?}");
            assert!(
                project.stale.as_deref().unwrap().contains("last good"),
                "{text:?}"
            );
        }
        fs::remove_file(&file).unwrap();
        projects.refresh();
        let project = projects.get(&root_id(&root)).unwrap();
        assert!(project.vetoes("a/web"));
        assert!(project.stale.as_deref().unwrap().contains("missing"));
        fs::write(&file, "[modules]\n").unwrap();
        projects.refresh();
        let project = projects.get(&root_id(&root)).unwrap();
        assert!(!project.vetoes("a/web"));
        assert_eq!(project.stale, None);
        let _ = fs::remove_dir_all(&root);
    }

    fn root_id(root: &Path) -> String {
        root.file_name().unwrap().to_string_lossy().into_owned()
    }

    #[test]
    fn only_a_folder_or_its_regulus_toml_opens() {
        let root = temp("folder");
        fs::write(root.join("notes.txt"), "x").unwrap();
        assert!(folder(Path::new(""), &root.join("notes.txt")).is_err());
        fs::write(root.join(PROJECT_FILE), "").unwrap();
        let real = root.canonicalize().unwrap();
        assert_eq!(
            folder(Path::new(""), &root.join(PROJECT_FILE)).unwrap(),
            real
        );
        assert_eq!(folder(Path::new(""), &root).unwrap(), real);
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
