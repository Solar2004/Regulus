//! `regulus node init`: a custom node in Python, as a local module of a project
//! (docs/03-modules.md §6, docs/18-custom-nodes.md). It writes `tools/<id>/run.py` with
//! the Python SDK beside it and registers the module in the project's `regulus.toml` as a
//! `[[modules.local]]`. It runs nothing and trusts nothing: the code only runs once the
//! user trusts the project (`regulus project open <folder> --trust`).
//!
//! Nothing is half written: every check runs before the first write, the files are
//! created new (never over something that exists), and `regulus.toml` is replaced in one
//! rename only after they are on disk. If that fails, the files written are removed.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

/// The file that makes a folder a project.
const PROJECT_FILE: &str = "regulus.toml";
/// The folder of the project the nodes go in, one folder per node.
pub const TOOLS_DIR: &str = "tools";
/// The script the core runs, and the SDK it imports, inside `tools/<id>/`.
pub const RUN_FILE: &str = "run.py";
pub const SDK_FILE: &str = "regulus.py";
/// The Python SDK (standard library only), copied beside every `run.py`.
pub const SDK: &str = include_str!("../../../sdk/python/regulus.py");
/// The starter `run.py`; `__ID__` and `__NAME__` are filled in.
const RUN_TEMPLATE: &str = include_str!("../assets/node/run.py");
/// The longest id taken: it ends up in paths, module ids and node types.
const MAX_ID: usize = 64;

/// What `regulus node init` was asked for.
#[derive(Debug, Clone)]
pub struct InitAsk {
    /// The project folder: it must have a `regulus.toml`.
    pub folder: PathBuf,
    /// The module id, and the node type of the starter node.
    pub id: String,
    /// The Python program the core starts (`python3`, or a venv's interpreter).
    pub python: String,
}

/// What `regulus node init` did.
#[derive(Debug, Clone)]
pub struct Initialized {
    /// The project folder, absolute.
    pub root: PathBuf,
    /// The project id the core will give it: `[project] id`, else the folder's name.
    pub project: String,
    /// The files written, absolute.
    pub files: Vec<PathBuf>,
    /// Whether `regulus.toml` had to be rewritten as a whole (comments are lost); `false`
    /// when the entry was appended and the rest of the file is as it was.
    pub rewritten: bool,
    /// Things worth saying that did not stop it (the module vetoed, for one).
    pub notes: Vec<String>,
}

/// A module id `node init` takes: a plain lowercase name that is safe as a folder, a
/// module id (`<project>/<id>`) and a node type. Stricter than what `[[modules.local]]`
/// accepts, on purpose: whatever it writes, every client reads.
pub fn check_id(id: &str) -> Result<()> {
    let first_ok = id
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit());
    let rest_ok = id
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_');
    if !first_ok || !rest_ok || id.len() > MAX_ID {
        bail!(
            "node id `{id}` must be 1 to {MAX_ID} characters of a-z, 0-9, `-` or `_`, \
             starting with a letter or a digit"
        );
    }
    Ok(())
}

/// The starter node's display name: `word-count` is `Word count`.
fn display_name(id: &str) -> String {
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

/// The `[[modules.local]]` entry for `id`, as a table and as the text appended.
fn entry(id: &str, python: &str) -> (toml::Table, String) {
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

/// `raw` with `entry` added at the end of `modules.local`. Refuses a file whose
/// `modules` or `modules.local` is not what the core reads, and an id taken.
fn with_entry(raw: &toml::Table, id: &str, entry: toml::Table) -> Result<toml::Table> {
    let mut table = raw.clone();
    let modules = table
        .entry("modules")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()));
    let Some(modules) = modules.as_table_mut() else {
        bail!("{PROJECT_FILE}: `modules` must be a table; fix it before adding a node");
    };
    let local = modules
        .entry("local")
        .or_insert_with(|| toml::Value::Array(Vec::new()));
    let Some(local) = local
        .as_array_mut()
        .filter(|l| l.iter().all(toml::Value::is_table))
    else {
        bail!(
            "{PROJECT_FILE}: `modules.local` must be a list of tables; fix it before adding a node"
        );
    };
    let taken = local
        .iter()
        .any(|m| m.get("id").and_then(toml::Value::as_str) == Some(id));
    if taken {
        bail!("{PROJECT_FILE} already has a [[modules.local]] with id `{id}`; choose another id");
    }
    local.push(toml::Value::Table(entry));
    Ok(table)
}

/// The new text of `regulus.toml`: the entry appended to the text as it is, so comments
/// and layout stay, when that reads back as exactly `expected`; else the whole table
/// written again (unknown keys kept, comments lost). The flag says which.
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
    let whole = toml::to_string(expected).context("cannot write regulus.toml")?;
    let back: toml::Table = toml::from_str(&whole).context("cannot write regulus.toml")?;
    if &back != expected {
        bail!("cannot write {PROJECT_FILE} without changing what it says; add the entry by hand");
    }
    Ok((whole, true))
}

/// Whether `path` stays inside `root` once links are followed (`root` is canonical).
fn inside(root: &Path, path: &Path) -> bool {
    path.canonicalize().is_ok_and(|real| real.starts_with(root))
}

/// Writes `text` to `path`, which must not exist.
fn create_new(path: &Path, text: &str) -> Result<()> {
    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .with_context(|| format!("cannot create {}", path.display()))?;
    file.write_all(text.as_bytes())
        .and_then(|()| file.sync_all())
        .with_context(|| format!("cannot write {}", path.display()))
}

/// Replaces `path` with `text` through a new file beside it and one rename, keeping its
/// permissions, so a reader (the core refreshing the project) never sees half of it.
fn replace(path: &Path, text: &str) -> Result<()> {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let temp = path.with_file_name(format!(".{name}.{}.node-init.tmp", std::process::id()));
    let written = create_new(&temp, text)
        .and_then(|()| {
            let permissions = fs::metadata(path)?.permissions();
            fs::set_permissions(&temp, permissions)?;
            Ok(())
        })
        .and_then(|()| {
            fs::rename(&temp, path).with_context(|| format!("cannot replace {}", path.display()))
        });
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// `regulus node init`: see the module docs.
pub fn init(ask: &InitAsk) -> Result<Initialized> {
    check_id(&ask.id)?;
    if ask.python.trim().is_empty() || ask.python.chars().any(char::is_control) {
        bail!("--python must name a program");
    }
    let root = ask
        .folder
        .canonicalize()
        .with_context(|| format!("cannot open the folder {}", ask.folder.display()))?;
    if !root.is_dir() {
        bail!("{} is not a folder", root.display());
    }
    let file = root.join(PROJECT_FILE);
    let text = match fs::read_to_string(&file) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!(
            "{} is not a project: it has no {PROJECT_FILE}. Make it one with \
             `regulus project open {}`, then run this again",
            root.display(),
            root.display()
        ),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", file.display())),
    };
    if fs::symlink_metadata(&file)?.file_type().is_symlink() {
        bail!(
            "{} is a link; edit the file it points to by hand",
            file.display()
        );
    }
    let raw: toml::Table =
        toml::from_str(&text).with_context(|| format!("{} is not valid TOML", file.display()))?;
    let (table, appended) = entry(&ask.id, &ask.python);
    let expected = with_entry(&raw, &ask.id, table)?;
    let (new_text, rewritten) = new_text(&text, &appended, &expected)?;

    // Where the files go: inside the project, and new.
    let tools = root.join(TOOLS_DIR);
    if fs::symlink_metadata(&tools).is_ok() && (!tools.is_dir() || !inside(&root, &tools)) {
        bail!(
            "{} must be a folder inside the project, not a file or a link out of it",
            tools.display()
        );
    }
    let dir = tools.join(&ask.id);
    if fs::symlink_metadata(&dir).is_ok() {
        bail!(
            "{} already exists; nothing was written (choose another id, or remove it)",
            dir.display()
        );
    }

    let made_tools = !tools.exists();
    fs::create_dir_all(&tools).with_context(|| format!("cannot create {}", tools.display()))?;
    // `create_dir`, not `_all`: if something took the name since the check, stop here,
    // without leaving behind a `tools` folder made just now.
    if let Err(error) = fs::create_dir(&dir) {
        if made_tools {
            let _ = fs::remove_dir(&tools);
        }
        return Err(error).with_context(|| format!("cannot create {}", dir.display()));
    }
    let run = dir.join(RUN_FILE);
    let sdk = dir.join(SDK_FILE);
    let written = create_new(&sdk, SDK)
        .and_then(|()| create_new(&run, &run_script(&ask.id)))
        .and_then(|()| {
            // The file must still be the one checked: an edit since then is not lost.
            let now = fs::read_to_string(&file)
                .with_context(|| format!("cannot read {}", file.display()))?;
            if now != text {
                bail!(
                    "{} changed while the node was written; run this again",
                    file.display()
                );
            }
            replace(&file, &new_text)
        });
    if let Err(error) = written {
        let _ = fs::remove_dir_all(&dir);
        if made_tools {
            let _ = fs::remove_dir(&tools);
        }
        return Err(error);
    }

    let project = raw
        .get("project")
        .and_then(|p| p.get("id"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .or_else(|| root.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_default();
    Ok(Initialized {
        notes: notes(&raw, &project, &ask.id),
        root,
        project,
        files: vec![run, sdk],
        rewritten,
    })
}

/// What in `[modules]` keeps `<project>/<id>` from running even once trusted.
fn notes(raw: &toml::Table, project: &str, id: &str) -> Vec<String> {
    let module = format!("{project}/{id}");
    let base = |m: &str| m.split('@').next().unwrap_or(m).to_string();
    let list = |key: &str| -> Option<Vec<String>> {
        raw.get("modules")?
            .get(key)?
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(toml::Value::as_str)
                    .map(base)
                    .collect()
            })
            .or_else(|| {
                raw.get("modules")?
                    .get(key)?
                    .as_str()
                    .map(|s| vec![base(s)])
            })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str, file: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("regulus-node-unit-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join(PROJECT_FILE), file).unwrap();
        root
    }

    fn ask(folder: &Path, id: &str) -> InitAsk {
        InitAsk {
            folder: folder.to_path_buf(),
            id: id.into(),
            python: "python3".into(),
        }
    }

    #[test]
    fn ids_are_plain_lowercase_names() {
        for good in ["caps", "word-count", "a_1", "9lives"] {
            check_id(good).unwrap();
        }
        let long = "a".repeat(MAX_ID + 1);
        for bad in [
            "", "..", ".x", "-x", "a/b", "a\\b", "a@1", "Caps", "a b", "é", "a.b", &long,
        ] {
            assert!(check_id(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn appends_keeping_comments_and_unknown_keys() {
        let file = "# my project\n[project]\nname = \"Demo\"\nflavour = \"kept\"  # unknown\n\n\
                    [[modules.local]]\nid = \"old\"\ncommand = [\"sh\", \"x\"]\nmystery = 1\n";
        let root = project("append", file);
        let done = init(&ask(&root, "caps")).unwrap();
        assert!(!done.rewritten);
        let text = fs::read_to_string(root.join(PROJECT_FILE)).unwrap();
        assert!(text.starts_with(file), "{text}");
        let table: toml::Table = toml::from_str(&text).unwrap();
        let local = table["modules"]["local"].as_array().unwrap();
        assert_eq!(local.len(), 2);
        assert_eq!(local[0]["mystery"].as_integer(), Some(1));
        assert_eq!(local[1]["id"].as_str(), Some("caps"));
        assert_eq!(
            local[1]["command"].as_array().unwrap()[1].as_str(),
            Some("tools/caps/run.py")
        );
        assert_eq!(
            fs::read_to_string(root.join("tools/caps/regulus.py")).unwrap(),
            SDK
        );
        let run = fs::read_to_string(root.join("tools/caps/run.py")).unwrap();
        assert!(run.contains("\"caps\""), "{run}");
        assert!(
            !run.contains("__ID__") && !run.contains("__NAME__"),
            "{run}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_inline_local_list_is_rewritten_whole_with_its_keys() {
        let file = "[project]\nodd = true\n[modules]\nlocal = [{ id = \"old\", command = [\"sh\"], x = 2 }]\n";
        let root = project("inline", file);
        let done = init(&ask(&root, "caps")).unwrap();
        assert!(done.rewritten);
        let table: toml::Table =
            toml::from_str(&fs::read_to_string(root.join(PROJECT_FILE)).unwrap()).unwrap();
        assert_eq!(table["project"]["odd"].as_bool(), Some(true));
        let local = table["modules"]["local"].as_array().unwrap();
        assert_eq!(local[0]["x"].as_integer(), Some(2));
        assert_eq!(local[1]["id"].as_str(), Some("caps"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn refuses_and_writes_nothing() {
        // A taken id: neither the folder nor the file change.
        let file = "[[modules.local]]\nid = \"caps\"\ncommand = [\"sh\"]\n";
        let root = project("taken", file);
        let error = init(&ask(&root, "caps")).unwrap_err().to_string();
        assert!(error.contains("already has"), "{error}");
        assert!(!root.join(TOOLS_DIR).exists());
        assert_eq!(fs::read_to_string(root.join(PROJECT_FILE)).unwrap(), file);
        let _ = fs::remove_dir_all(&root);

        // Files in the way: never overwritten, the registration is not written.
        let root = project("exists", "");
        fs::create_dir_all(root.join("tools/caps")).unwrap();
        fs::write(root.join("tools/caps/run.py"), "mine").unwrap();
        let error = init(&ask(&root, "caps")).unwrap_err().to_string();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(
            fs::read_to_string(root.join("tools/caps/run.py")).unwrap(),
            "mine"
        );
        assert_eq!(fs::read_to_string(root.join(PROJECT_FILE)).unwrap(), "");
        let _ = fs::remove_dir_all(&root);

        // `modules` of the wrong type: refused before writing anything.
        let root = project("wrong", "modules = 3\n");
        assert!(init(&ask(&root, "caps")).is_err());
        assert!(!root.join(TOOLS_DIR).exists());
        let _ = fs::remove_dir_all(&root);

        // No project file: not a project, nothing created.
        let root = project("none", "");
        fs::remove_file(root.join(PROJECT_FILE)).unwrap();
        let error = init(&ask(&root, "caps")).unwrap_err().to_string();
        assert!(error.contains("regulus project open"), "{error}");
        assert!(!root.join(TOOLS_DIR).exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_tools_link_out_of_the_project_is_refused() {
        let root = project("link", "");
        let outside =
            root.with_file_name(format!("regulus-node-unit-outside-{}", std::process::id()));
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(TOOLS_DIR)).unwrap();
        let error = init(&ask(&root, "caps")).unwrap_err().to_string();
        assert!(error.contains("inside the project"), "{error}");
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    #[test]
    fn says_when_the_project_vetoes_it() {
        let raw: toml::Table = toml::from_str(
            "[modules]\ndisable = [\"demo/caps@0.1.0\"]\nenable = [\"regulus/hello\"]\n",
        )
        .unwrap();
        let notes = notes(&raw, "demo", "caps");
        assert_eq!(notes.len(), 2, "{notes:?}");
    }

    #[test]
    fn display_names_read_as_words() {
        assert_eq!(display_name("word-count"), "Word count");
        assert_eq!(display_name("caps"), "Caps");
    }
}
