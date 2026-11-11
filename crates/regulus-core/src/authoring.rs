//! Writing the nodes of a project (docs/18-custom-nodes.md): a new Python node and its
//! `[[modules.local]]` entry, and the one file of a local module a client may read and
//! write. Nothing here runs project code: the Python checker is the host's interpreter
//! (`[local_modules] python` of `config.toml`), isolated, reading the source as data with
//! the core's own copy of the SDK.
//!
//! Both `regulus node init` and `node.init` go through [`init_python`], and any other
//! language's scaffolder registers its entry through [`plan_registration`]: one writer of
//! `regulus.toml` entries.

use std::fs;
use std::io::{Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use regulus_proto::LocalSource;
use serde_json::Value;

use crate::config::LocalModulesConfig;
use crate::projects::{Project, PROJECT_FILE};

/// The folder of a project the nodes go in, one folder per node.
pub const TOOLS_DIR: &str = "tools";
/// The script the core runs, and the SDK it imports, inside `tools/<id>/`.
pub const RUN_FILE: &str = "run.py";
pub const SDK_FILE: &str = "regulus.py";
/// The Python SDK (standard library only), copied beside every `run.py`, and the static
/// reader the checker runs.
pub const SDK: &str = include_str!("../../../sdk/python/regulus.py");
/// The starter `run.py`; `__ID__` and `__NAME__` are filled in.
const RUN_TEMPLATE: &str = include_str!("../../../sdk/python/templates/run.py");
/// The longest node id taken: it ends up in paths, module ids and node types.
const MAX_ID: usize = 64;
/// The language of the sources the core can check.
pub const PYTHON: &str = "python";
/// What a Python source file ends with.
const PY_SUFFIX: &str = ".py";
/// What the checker runs after the SDK: it reads the source on stdin and writes JSON.
const INSPECT_CALL: &str = "\n_inspect_main()\n";
/// The only variable of the core's environment the checker gets.
const CHECK_ENV: &str = "PATH";
/// How often a running checker is looked at.
const POLL: Duration = Duration::from_millis(5);
/// What the checker may write beyond four times the source (its answer quotes the
/// source's declarations, never much more), and how much of its stderr is kept to say why
/// it failed. Bounds on a host process, not settings.
const ANSWER_SLACK: u64 = 64 * 1024;
const STDERR_KEPT: u64 = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AuthoringError {
    /// A value that cannot be used (an id, a program name).
    #[error("{0}")]
    Invalid(String),
    /// What it would create exists already; nothing was written.
    #[error("{0}")]
    Exists(String),
    /// No such module or file, or one the core may not touch.
    #[error("{0}")]
    Unavailable(String),
    /// The file changed since the revision the client read; nothing was written.
    #[error("{message}")]
    Conflict {
        message: String,
        /// The revision now; `None` if there is no file.
        revision: Option<String>,
        path: String,
    },
    #[error("{0}")]
    Io(String),
}

type Result<T> = std::result::Result<T, AuthoringError>;

fn io(what: &str, path: &Path, error: std::io::Error) -> AuthoringError {
    AuthoringError::Io(format!("cannot {what} {}: {error}", path.display()))
}

/// A node id `node init` takes: a plain lowercase name that is safe as a folder, a module
/// id (`<project>/<id>`) and a node type. Stricter than `[[modules.local]]` on purpose.
pub fn check_node_id(id: &str) -> Result<()> {
    let first_ok = id
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok = id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !first_ok || !rest_ok || id.len() > MAX_ID {
        return Err(AuthoringError::Invalid(format!(
            "node id `{id}` must be 1 to {MAX_ID} characters of a-z, 0-9, `-` or `_`, \
             starting with a letter or a digit"
        )));
    }
    Ok(())
}

/// The starter node's display name: `word-count` is `Word count`.
pub fn display_name(id: &str) -> String {
    let spaced = id.replace(['-', '_'], " ");
    let mut chars = spaced.chars();
    chars
        .next()
        .map(|c| c.to_ascii_uppercase().to_string() + chars.as_str())
        .unwrap_or_default()
}

/// The starter `run.py` for `id`.
pub fn run_script(id: &str) -> String {
    RUN_TEMPLATE
        .replace("__ID__", id)
        .replace("__NAME__", &display_name(id))
}

/// The `[[modules.local]]` entry of a Python node, as a table and as the text appended.
pub fn python_entry(id: &str, python: &str) -> (toml::Table, String) {
    let script = format!("{TOOLS_DIR}/{id}/{RUN_FILE}");
    let mut table = toml::Table::new();
    table.insert("id".into(), toml::Value::String(id.into()));
    table.insert(
        "command".into(),
        toml::Value::Array(vec![
            toml::Value::String(python.into()),
            toml::Value::String(script.clone()),
        ]),
    );
    let quote = |s: &str| toml::Value::String(s.into()).to_string();
    let text = format!(
        "[[modules.local]]\n\
         # Written by `regulus node init`; runs only once the project is trusted.\n\
         id = {}\n\
         command = [{}, {}]\n",
        quote(id),
        quote(python),
        quote(&script),
    );
    (table, text)
}

/// `raw` with `entry` added at the end of `modules.local`. Refuses a file whose `modules` or
/// `modules.local` is not what the core reads, and an id taken.
fn with_entry(raw: &toml::Table, id: &str, entry: toml::Table) -> Result<toml::Table> {
    let mut table = raw.clone();
    let modules = table
        .entry("modules")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(modules) = modules.as_table_mut() else {
        return Err(AuthoringError::Invalid(format!(
            "{PROJECT_FILE}: `modules` must be a table; fix it before adding a node"
        )));
    };
    let local = modules
        .entry("local")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let Some(local) = local
        .as_array_mut()
        .filter(|l| l.iter().all(toml::Value::is_table))
    else {
        return Err(AuthoringError::Invalid(format!(
            "{PROJECT_FILE}: `modules.local` must be a list of tables; fix it before adding a \
             node"
        )));
    };
    let taken = local
        .iter()
        .any(|m| m.get("id").and_then(toml::Value::as_str) == Some(id));
    if taken {
        return Err(AuthoringError::Exists(format!(
            "{PROJECT_FILE} already has a [[modules.local]] with id `{id}`; choose another id"
        )));
    }
    local.push(toml::Value::Table(entry));
    Ok(table)
}

/// The new text of `regulus.toml`: the entry appended to the text as it is, so comments and
/// layout stay, when that reads back as exactly `expected`; else the whole table written
/// again (unknown keys kept, comments lost). The flag says which.
fn new_text(text: &str, appended: &str, expected: &toml::Table) -> Result<(String, bool)> {
    let separator = if text.is_empty() || text.ends_with("\n\n") {
        ""
    } else if text.ends_with('\n') {
        "\n"
    } else {
        "\n\n"
    };
    let candidate = format!("{text}{separator}{appended}");
    if toml::from_str::<toml::Table>(&candidate).is_ok_and(|t| &t == expected) {
        return Ok((candidate, false));
    }
    let cannot =
        |e: &dyn std::fmt::Display| AuthoringError::Io(format!("cannot write {PROJECT_FILE}: {e}"));
    let whole = toml::to_string(expected).map_err(|e| cannot(&e))?;
    let back: toml::Table = toml::from_str(&whole).map_err(|e| cannot(&e))?;
    if &back != expected {
        return Err(AuthoringError::Io(format!(
            "cannot write {PROJECT_FILE} without changing what it says; add the entry by hand"
        )));
    }
    Ok((whole, true))
}

/// Writes `text` to `path`, which must not exist.
fn create_new(path: &Path, text: &[u8]) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                AuthoringError::Exists(format!("{} already exists", path.display()))
            } else {
                io("create", path, e)
            }
        })?;
    file.write_all(text)
        .and_then(|()| file.sync_all())
        .map_err(|e| io("write", path, e))
}

/// Replaces `path` with `text` through a new file beside it and one rename, keeping the
/// permissions of the file it replaces, so a reader never sees half of it.
fn replace(path: &Path, text: &[u8]) -> Result<()> {
    static WRITES: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        WRITES.fetch_add(1, Ordering::Relaxed)
    ));
    let written = create_new(&temp, text)
        .and_then(|()| match fs::metadata(path) {
            Ok(meta) => {
                fs::set_permissions(&temp, meta.permissions()).map_err(|e| io("write", &temp, e))
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(io("read", path, e)),
        })
        .and_then(|()| fs::rename(&temp, path).map_err(|e| io("replace", path, e)));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// An entry ready to go into a project's `regulus.toml`: checked against the file as it
/// is, written by [`Registration::commit`] only if the file did not change since.
#[derive(Debug, Clone)]
pub struct Registration {
    file: PathBuf,
    before: String,
    after: String,
    /// The file is written again as a whole: its comments are lost.
    pub rewritten: bool,
    /// The project id the core will give it: `[project] id`, else the folder's name.
    pub project: String,
    /// What in `[modules]` keeps the module from running even once trusted.
    pub notes: Vec<String>,
}

impl Registration {
    /// Replaces `regulus.toml` with the entry added, in one rename, if it is still the file
    /// the plan read; else nothing is written.
    pub fn commit(&self) -> Result<()> {
        let now = fs::read_to_string(&self.file).map_err(|e| io("read", &self.file, e))?;
        if now != self.before {
            return Err(AuthoringError::Conflict {
                message: format!(
                    "{} changed while the node was written; nothing was registered, try again",
                    self.file.display()
                ),
                revision: Some(revision(now.as_bytes())),
                path: PROJECT_FILE.into(),
            });
        }
        replace(&self.file, self.after.as_bytes())
    }
}

/// Plans adding `entry` (`appended`: the same entry as text, with comments) for module `id`
/// to the `regulus.toml` of the project in `root` (canonical). Refuses a folder that is not
/// a project, a file that is a link or not TOML, a `modules` of the wrong type and an id
/// taken. Writes nothing.
pub fn plan_registration(
    root: &Path,
    id: &str,
    entry: toml::Table,
    appended: &str,
) -> Result<Registration> {
    let file = root.join(PROJECT_FILE);
    let before = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Err(AuthoringError::Unavailable(format!(
                "{} is not a project: it has no {PROJECT_FILE}. Make it one with `regulus \
                 project open {}`, then try again",
                root.display(),
                root.display()
            )))
        }
        Err(e) => return Err(io("read", &file, e)),
    };
    let link = fs::symlink_metadata(&file)
        .map_err(|e| io("read", &file, e))?
        .file_type()
        .is_symlink();
    if link {
        return Err(AuthoringError::Invalid(format!(
            "{} is a link; edit the file it points to by hand",
            file.display()
        )));
    }
    let raw: toml::Table = toml::from_str(&before).map_err(|e| {
        AuthoringError::Invalid(format!("{} is not valid TOML: {e}", file.display()))
    })?;
    let expected = with_entry(&raw, id, entry)?;
    let (after, rewritten) = new_text(&before, appended, &expected)?;
    let project = raw
        .get("project")
        .and_then(|p| p.get("id"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    Ok(Registration {
        notes: vetoes(&raw, &project, id),
        file,
        before,
        after,
        rewritten,
        project,
    })
}

/// What in `[modules]` keeps `<project>/<id>` from running even once trusted.
fn vetoes(raw: &toml::Table, project: &str, id: &str) -> Vec<String> {
    let module = format!("{project}/{id}");
    let base = |m: &str| m.split('@').next().unwrap_or(m).to_string();
    let list = |key: &str| -> Option<Vec<String>> {
        let value = raw.get("modules")?.get(key)?;
        match value {
            toml::Value::Array(items) => Some(
                items
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(base)
                    .collect(),
            ),
            toml::Value::String(one) => Some(vec![base(one)]),
            _ => None,
        }
    };
    let mut notes = Vec::new();
    if list("disable").is_some_and(|d| d.contains(&module)) {
        notes.push(format!(
            "`[modules] disable` lists {module}: its nodes will not run in this project"
        ));
    }
    if list("enable").is_some_and(|e| !e.contains(&module)) {
        notes.push(format!(
            "`[modules] enable` does not list {module}: add it there for its nodes to run"
        ));
    }
    notes
}

/// What `node init` is asked for.
#[derive(Debug, Clone)]
pub struct InitAsk {
    /// The project folder: it must have a `regulus.toml`.
    pub folder: PathBuf,
    /// The module id, and the node type of the starter node.
    pub id: String,
    /// The Python program the core starts (`python3`, or a venv's interpreter).
    pub python: String,
}

/// What `node init` did.
#[derive(Debug, Clone)]
pub struct Initialized {
    /// The project folder, absolute.
    pub root: PathBuf,
    /// The project id the core will give it.
    pub project: String,
    /// The files written, absolute: `run.py` first.
    pub files: Vec<PathBuf>,
    /// Whether `regulus.toml` was written again as a whole (comments lost).
    pub rewritten: bool,
    /// Things worth saying that did not stop it (the module vetoed, for one).
    pub notes: Vec<String>,
}

/// Whether `path` stays inside `root` once links are followed (`root` is canonical).
fn inside(root: &Path, path: &Path) -> bool {
    path.canonicalize().is_ok_and(|real| real.starts_with(root))
}

/// A new Python node: `tools/<id>/run.py` and the SDK beside it, registered in the project's
/// `regulus.toml`. Every check runs before the first write; the files are created new, never
/// over something that exists; `regulus.toml` is replaced in one rename after them, only if
/// it did not change; if anything fails, what was written is removed. Runs nothing.
pub fn init_python(ask: &InitAsk) -> Result<Initialized> {
    check_node_id(&ask.id)?;
    if ask.python.trim().is_empty() || ask.python.chars().any(char::is_control) {
        return Err(AuthoringError::Invalid(
            "the Python program must be a name or a path".into(),
        ));
    }
    let root = ask.folder.canonicalize().map_err(|e| {
        AuthoringError::Unavailable(format!(
            "cannot open the folder {}: {e}",
            ask.folder.display()
        ))
    })?;
    if !root.is_dir() {
        return Err(AuthoringError::Unavailable(format!(
            "{} is not a folder",
            root.display()
        )));
    }
    let (entry, appended) = python_entry(&ask.id, &ask.python);
    let plan = plan_registration(&root, &ask.id, entry, &appended)?;
    let tools = root.join(TOOLS_DIR);
    if fs::symlink_metadata(&tools).is_ok() && (!tools.is_dir() || !inside(&root, &tools)) {
        return Err(AuthoringError::Invalid(format!(
            "{} must be a folder inside the project, not a file or a link out of it",
            tools.display()
        )));
    }
    let dir = tools.join(&ask.id);
    if fs::symlink_metadata(&dir).is_ok() {
        return Err(AuthoringError::Exists(format!(
            "{} already exists; nothing was written (choose another id, or remove it)",
            dir.display()
        )));
    }
    let made_tools = !tools.exists();
    fs::create_dir_all(&tools).map_err(|e| io("create", &tools, e))?;
    // `create_dir`, not `_all`: if something took the name since the check, stop here.
    if let Err(error) = fs::create_dir(&dir) {
        if made_tools {
            let _ = fs::remove_dir(&tools);
        }
        return Err(if error.kind() == std::io::ErrorKind::AlreadyExists {
            AuthoringError::Exists(format!("{} already exists", dir.display()))
        } else {
            io("create", &dir, error)
        });
    }
    let run = dir.join(RUN_FILE);
    let sdk = dir.join(SDK_FILE);
    let written = create_new(&sdk, SDK.as_bytes())
        .and_then(|()| create_new(&run, run_script(&ask.id).as_bytes()))
        .and_then(|()| plan.commit());
    if let Err(error) = written {
        let _ = fs::remove_dir_all(&dir);
        if made_tools {
            let _ = fs::remove_dir(&tools);
        }
        return Err(error);
    }
    Ok(Initialized {
        root,
        project: plan.project.clone(),
        files: vec![run, sdk],
        rewritten: plan.rewritten,
        notes: plan.notes,
    })
}

// --- the source of a local module --------------------------------------------------------

/// The editable file of one local module: where it is, relative to the project and
/// absolute.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFile {
    /// Its id in `[[modules.local]]`.
    pub module: String,
    pub relative: PathBuf,
    pub absolute: PathBuf,
}

/// The local id a client means: `caps`, or `<project>/caps[@version]`.
pub fn local_id<'a>(project: &str, module: &'a str) -> &'a str {
    let module = module.split_once('@').map_or(module, |(base, _)| base);
    module
        .strip_prefix(project)
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(module)
}

fn plain(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
}

/// Joins relative paths and drops the `.` parts.
fn tidy(path: PathBuf) -> PathBuf {
    path.components()
        .filter(|c| !matches!(c, Component::CurDir))
        .collect()
}

/// The Python file of local module `module` of `project`: the first argument of its
/// `command` that ends in `.py` (relative to its `cwd`), or the program itself if it is a
/// relative path ending in `.py`. It must stay inside the project once links are followed
/// and must not be a link itself. The file need not exist yet.
pub fn source_of(project: &Project, module: &str) -> Result<SourceFile> {
    let id = local_id(&project.id, module);
    let entry = project
        .file
        .modules
        .local
        .iter()
        .find(|local| local.id.as_deref() == Some(id))
        .ok_or_else(|| {
            AuthoringError::Unavailable(format!(
                "project `{}` has no [[modules.local]] with id `{id}`",
                project.id
            ))
        })?;
    if let Some(why) = entry.unusable() {
        return Err(AuthoringError::Unavailable(format!(
            "[[modules.local]] {why}"
        )));
    }
    let cwd = tidy(entry.cwd.clone().unwrap_or_default());
    let is_py = |s: &&String| s.ends_with(PY_SUFFIX);
    let relative = match entry.command[1..].iter().find(is_py) {
        Some(script) => cwd.join(script),
        None => match entry.command.first().filter(is_py) {
            // A bare name is looked up on PATH, not in the project.
            Some(program) if Path::new(program).components().count() > 1 => PathBuf::from(program),
            _ => {
                return Err(AuthoringError::Unavailable(format!(
                    "`{id}` runs `{}`: there is no Python file in its command to edit here",
                    entry.command.join(" ")
                )))
            }
        },
    };
    if !plain(&relative) {
        return Err(AuthoringError::Unavailable(format!(
            "`{id}`: `{}` is not a path inside the project (use one relative to it, without \
             `..`)",
            relative.display()
        )));
    }
    let relative = tidy(relative);
    let absolute = project.root.join(&relative);
    let parent = absolute.parent().unwrap_or(&project.root);
    if parent.exists() && !inside(&project.root, parent) {
        return Err(AuthoringError::Unavailable(format!(
            "`{id}`: {} leads out of the project through a link",
            relative.display()
        )));
    }
    if fs::symlink_metadata(&absolute).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(AuthoringError::Unavailable(format!(
            "`{id}`: {} is a link or not a file; edit it outside Regulus",
            relative.display()
        )));
    }
    Ok(SourceFile {
        module: id.to_string(),
        relative,
        absolute,
    })
}

/// Every local module of `project` with an id, and where its editable file is or why it
/// has none.
pub fn sources(project: &Project) -> Vec<LocalSource> {
    project
        .file
        .modules
        .local
        .iter()
        .filter_map(|local| local.id.clone())
        .map(|id| match source_of(project, &id) {
            Ok(file) => LocalSource {
                module: id,
                path: Some(file.relative.display().to_string()),
                error: None,
            },
            Err(error) => LocalSource {
                module: id,
                path: None,
                error: Some(error.to_string()),
            },
        })
        .collect()
}

/// An opaque revision of some bytes: FNV-1a (64 bits) and the length. Equal bytes, equal
/// revision, on every machine and version.
pub fn revision(bytes: &[u8]) -> String {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let hash = bytes.iter().fold(OFFSET, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(PRIME)
    });
    format!("{hash:016x}-{}", bytes.len())
}

fn too_large(what: &str, len: usize, max: usize) -> AuthoringError {
    AuthoringError::Unavailable(format!(
        "{what} is {len} bytes; the core edits sources of at most {max} \
         ([local_modules] max_source_bytes)"
    ))
}

/// The text of a source and its revision; `None` if the file does not exist.
pub fn read_source(file: &SourceFile, max: usize) -> Result<Option<(String, String)>> {
    let bytes = match fs::read(&file.absolute) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io("read", &file.absolute, e)),
    };
    if bytes.len() > max {
        return Err(too_large(
            &file.relative.display().to_string(),
            bytes.len(),
            max,
        ));
    }
    let revision = revision(&bytes);
    let text = String::from_utf8(bytes).map_err(|_| {
        AuthoringError::Unavailable(format!(
            "{} is not UTF-8; edit it outside Regulus",
            file.relative.display()
        ))
    })?;
    Ok(Some((text, revision)))
}

/// Writes `text` over a source, if its revision is still `expected` (`None`: no file) or
/// with `force`. Atomic (a new file and one rename), keeping the file's permissions. Returns
/// the new revision.
pub fn write_source(
    file: &SourceFile,
    text: &str,
    expected: Option<&str>,
    force: bool,
    max: usize,
) -> Result<String> {
    if text.len() > max {
        return Err(too_large("the text", text.len(), max));
    }
    let now = match fs::read(&file.absolute) {
        Ok(bytes) => Some(revision(&bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(io("read", &file.absolute, e)),
    };
    if !force && now.as_deref() != expected {
        let message = match &now {
            Some(_) if expected.is_none() => format!(
                "{} exists already; read it first, or save with force to replace it",
                file.relative.display()
            ),
            Some(_) => format!(
                "{} changed since it was read; read it again, or save with force to replace it",
                file.relative.display()
            ),
            None => format!(
                "{} is gone since it was read; save with force to write it again",
                file.relative.display()
            ),
        };
        return Err(AuthoringError::Conflict {
            message,
            revision: now,
            path: file.relative.display().to_string(),
        });
    }
    if let Some(parent) = file.absolute.parent() {
        if !parent.is_dir() {
            return Err(AuthoringError::Unavailable(format!(
                "{} has no folder {}; create it first",
                file.relative.display(),
                parent.display()
            )));
        }
    }
    // Looked at again right before: a link put there since is never written through.
    if fs::symlink_metadata(&file.absolute).is_ok_and(|m| !m.file_type().is_file()) {
        return Err(AuthoringError::Unavailable(format!(
            "{} is a link or not a file; edit it outside Regulus",
            file.relative.display()
        )));
    }
    replace(&file.absolute, text.as_bytes())?;
    Ok(revision(text.as_bytes()))
}

// --- checking a source without running it ------------------------------------------------

/// What the checker said, as JSON, or why it could not say anything.
#[derive(Debug, Clone, PartialEq)]
pub enum Checked {
    Ran(Value),
    Unavailable(String),
}

/// Reads `text` as Python without running it: the host's interpreter (`config.python`),
/// isolated (`-I -S`: no user site, no `site`, no current folder on the path), in `cwd`,
/// with only `PATH` from the environment, runs the core's SDK, whose `_inspect_main` parses
/// the text it reads on stdin with `ast` and writes one JSON object. Killed after
/// `check_timeout_ms`.
pub fn check_python(config: &LocalModulesConfig, cwd: &Path, text: &str) -> Checked {
    if text.len() > config.max_source_bytes {
        return Checked::Unavailable(
            too_large("the text", text.len(), config.max_source_bytes).to_string(),
        );
    }
    let script = format!("{SDK}{INSPECT_CALL}");
    let mut command = Command::new(&config.python);
    command
        .args(["-I", "-S", "-c", &script])
        .current_dir(cwd)
        .env_clear()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(path) = std::env::var_os(CHECK_ENV) {
        command.env(CHECK_ENV, path);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(e) => {
            return Checked::Unavailable(format!(
                "cannot run `{}` to check the source ([local_modules] python): {e}",
                config.python
            ))
        }
    };
    let (Some(mut stdin), Some(mut stdout), Some(mut stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        let _ = child.kill();
        let _ = child.wait();
        return Checked::Unavailable("the checker has no pipes".into());
    };
    let input = text.as_bytes().to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    // Bounded: the answer is the source's declarations, never much bigger than the source.
    let limit = (config.max_source_bytes as u64).saturating_mul(4) + ANSWER_SLACK;
    let reader = std::thread::spawn(move || {
        let mut out = Vec::new();
        let _ = (&mut stdout).take(limit).read_to_end(&mut out);
        out
    });
    let errors = std::thread::spawn(move || {
        let mut err = Vec::new();
        let _ = (&mut stderr).take(STDERR_KEPT).read_to_end(&mut err);
        err
    });
    let deadline = Instant::now() + Duration::from_millis(config.check_timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(POLL),
            _ => break None,
        }
    };
    if status.is_none() {
        kill_group(&mut child);
    }
    let _ = writer.join();
    let out = reader.join().unwrap_or_default();
    let err = errors.join().unwrap_or_default();
    let Some(status) = status else {
        return Checked::Unavailable(format!(
            "the check took longer than {} ms ([local_modules] check_timeout_ms)",
            config.check_timeout_ms
        ));
    };
    match serde_json::from_slice::<Value>(&out) {
        Ok(value) if value.is_object() => Checked::Ran(value),
        _ => {
            let err = String::from_utf8_lossy(&err);
            let last = err
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
            Checked::Unavailable(format!(
                "`{}` could not check the source ({status}){}{last}",
                config.python,
                if last.is_empty() { "" } else { ": " }
            ))
        }
    }
}

fn kill_group(child: &mut std::process::Child) {
    #[cfg(unix)]
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        if group > 1 {
            // SAFETY: `killpg` only sends a signal to the group the checker leads
            // (`process_group(0)`); it reads no memory of ours.
            unsafe {
                libc::killpg(group, libc::SIGKILL);
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// The `__version__` a copy of the SDK declares, if it can be read.
pub fn sdk_version(text: &str) -> Option<&str> {
    text.lines().find_map(|line| {
        let rest = line.strip_prefix("__version__")?.trim_start();
        let rest = rest.strip_prefix('=')?.trim();
        rest.strip_prefix('"')?.split('"').next()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("regulus-authoring-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root.canonicalize().unwrap()
    }

    fn project(root: &Path, toml: &str) -> Project {
        fs::write(root.join(PROJECT_FILE), toml).unwrap();
        Project::load(root).unwrap()
    }

    #[test]
    fn ids_are_plain_lowercase_names() {
        for good in ["caps", "word-count", "a_1", "9lives"] {
            check_node_id(good).unwrap();
        }
        let long = "a".repeat(MAX_ID + 1);
        for bad in [
            "", "..", ".x", "-x", "a/b", "a\\b", "a@1", "Caps", "a b", "é", "a.b", &long,
        ] {
            assert!(check_node_id(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn init_appends_keeping_comments_and_never_overwrites() {
        let root = temp("init");
        let file = "# mine\n[project]\nname = \"Demo\"\nodd = 1\n";
        fs::write(root.join(PROJECT_FILE), file).unwrap();
        let ask = InitAsk {
            folder: root.clone(),
            id: "caps".into(),
            python: "python3".into(),
        };
        let done = init_python(&ask).unwrap();
        assert!(!done.rewritten);
        let text = fs::read_to_string(root.join(PROJECT_FILE)).unwrap();
        assert!(text.starts_with(file), "{text}");
        assert_eq!(
            fs::read_to_string(root.join("tools/caps/regulus.py")).unwrap(),
            SDK
        );
        let run = fs::read_to_string(root.join("tools/caps/run.py")).unwrap();
        assert!(run.contains("\"caps\"") && !run.contains("__ID__"), "{run}");
        let again = init_python(&ask).unwrap_err();
        assert!(matches!(again, AuthoringError::Exists(_)), "{again:?}");
        assert_eq!(fs::read_to_string(root.join(PROJECT_FILE)).unwrap(), text);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_source_is_the_py_of_the_command_and_nothing_else() {
        let root = temp("source");
        let project = project(
            &root,
            r#"
[[modules.local]]
id = "caps"
command = ["python3", "tools/caps/run.py"]
[[modules.local]]
id = "in-cwd"
command = ["python3", "-u", "./main.py"]
cwd = "tools/x"
[[modules.local]]
id = "binary"
command = ["tools/b/target/release/b"]
[[modules.local]]
id = "climb"
command = ["python3", "../../etc/x.py"]
[[modules.local]]
id = "direct"
command = ["tools/d/run.py"]
"#,
        );
        assert_eq!(
            source_of(&project, "caps").unwrap().relative,
            PathBuf::from("tools/caps/run.py")
        );
        let full = format!("{}/caps@0.1.0", project.id);
        assert_eq!(source_of(&project, &full).unwrap().module, "caps");
        assert_eq!(
            source_of(&project, "in-cwd").unwrap().relative,
            PathBuf::from("tools/x/main.py")
        );
        assert_eq!(
            source_of(&project, "direct").unwrap().relative,
            PathBuf::from("tools/d/run.py")
        );
        for refused in ["binary", "climb", "ghost"] {
            assert!(
                matches!(
                    source_of(&project, refused),
                    Err(AuthoringError::Unavailable(_))
                ),
                "{refused}"
            );
        }
        let listed = sources(&project);
        assert_eq!(listed.len(), 5);
        assert!(listed[2].path.is_none() && listed[2].error.is_some());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn links_are_never_followed_out_or_written_through() {
        let root = temp("links");
        let outside = temp("links-out");
        fs::write(outside.join("run.py"), "x = 1\n").unwrap();
        std::os::unix::fs::symlink(&outside, root.join("tools")).unwrap();
        fs::create_dir_all(root.join("own")).unwrap();
        std::os::unix::fs::symlink(outside.join("run.py"), root.join("own/run.py")).unwrap();
        let project = project(
            &root,
            "[[modules.local]]\nid = \"a\"\ncommand = [\"python3\", \"tools/run.py\"]\n\
             [[modules.local]]\nid = \"b\"\ncommand = [\"python3\", \"own/run.py\"]\n",
        );
        for id in ["a", "b"] {
            assert!(
                matches!(source_of(&project, id), Err(AuthoringError::Unavailable(_))),
                "{id}"
            );
        }
        assert_eq!(
            fs::read_to_string(outside.join("run.py")).unwrap(),
            "x = 1\n"
        );
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn saves_refuse_a_stale_revision_and_keep_permissions() {
        let root = temp("save");
        fs::create_dir_all(root.join("tools/caps")).unwrap();
        let project = project(
            &root,
            "[[modules.local]]\nid = \"caps\"\ncommand = [\"python3\", \"tools/caps/run.py\"]\n",
        );
        let file = source_of(&project, "caps").unwrap();
        // No file yet: `None` creates it; a revision does not.
        assert!(read_source(&file, 1024).unwrap().is_none());
        let first = write_source(&file, "a = 1\n", None, false, 1024).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file.absolute, fs::Permissions::from_mode(0o751)).unwrap();
        }
        let (text, read) = read_source(&file, 1024).unwrap().unwrap();
        assert_eq!((text.as_str(), read.as_str()), ("a = 1\n", first.as_str()));
        let second = write_source(&file, "a = 2\n", Some(&first), false, 1024).unwrap();
        assert_ne!(first, second);
        match write_source(&file, "a = 3\n", Some(&first), false, 1024) {
            Err(AuthoringError::Conflict { revision, .. }) => {
                assert_eq!(revision.as_deref(), Some(second.as_str()))
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(
            write_source(&file, "a = 3\n", None, false, 1024),
            Err(AuthoringError::Conflict { .. })
        ));
        write_source(&file, "a = 4\n", Some("stale"), true, 1024).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&file.absolute).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o751);
        }
        assert!(matches!(
            write_source(&file, &"x".repeat(2000), None, true, 1024),
            Err(AuthoringError::Unavailable(_))
        ));
        fs::write(&file.absolute, [0xff, 0xfe]).unwrap();
        assert!(matches!(
            read_source(&file, 1024),
            Err(AuthoringError::Unavailable(_))
        ));
        let leftovers = fs::read_dir(root.join("tools/caps"))
            .unwrap()
            .filter(|e| {
                e.as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn revisions_are_stable() {
        assert_eq!(revision(b""), "cbf29ce484222325-0");
        assert_eq!(revision(b"a"), "af63dc4c8601ec8c-1");
    }

    #[test]
    fn the_sdk_says_its_version() {
        assert!(sdk_version(SDK).is_some_and(|v| !v.is_empty()));
        assert_eq!(sdk_version("x = 1\n__version__ = \"9.9\"\n"), Some("9.9"));
        assert_eq!(sdk_version("x = 1\n"), None);
    }
}
