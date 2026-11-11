//! `regulus node init` and `regulus node build`: custom nodes as local modules of a project
//! (docs/03-modules.md §6, docs/18-custom-nodes.md).
//!
//! There is one writer of a project's `[[modules.local]]` entries, the core's
//! `regulus_core::authoring`: a Python node goes through its `init_python` (the same code
//! as the `node.init` RPC), and the Rust node written here registers its entry with the
//! same `plan_registration`, so comments are kept, a taken id is refused and an edit made
//! meanwhile is never lost. What is Rust-specific lives here: the crate that `--lang rust`
//! writes, and `build`.
//!
//! Neither runs anything or trusts anything: the code only runs once the user trusts the
//! project (`regulus project open <folder> --trust`). `build` runs the project's code
//! (build scripts, procedural macros), so it refuses a project that is not trusted.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use regulus_core::authoring;

/// The file that makes a folder a project.
const PROJECT_FILE: &str = "regulus.toml";
/// The folder of the project the nodes go in, one folder per node.
pub use regulus_core::authoring::TOOLS_DIR;
/// The files of a Rust node, with `__ID__` and `__NAME__` filled in. One source for what
/// `--lang rust` writes and for `examples/local-modules/rust-stdio/` (a test keeps them
/// equal).
const RUST_CARGO: &str = include_str!("../assets/node/rust/Cargo.toml.in");
const RUST_MAIN: &str = include_str!("../assets/node/rust/main.rs.in");
const RUST_MANIFEST: &str = include_str!("../assets/node/rust/regulus-module.toml.in");
const RUST_GITIGNORE: &str = include_str!("../assets/node/rust/gitignore.in");
/// The Rust helper that speaks the protocol (`sdk/rust/regulus.rs`), copied as it is beside
/// the node's `main.rs`: the binary carries it, so no Regulus checkout is needed.
const RUST_SDK: &str = include_str!("../../../sdk/rust/regulus.rs");
/// The folder `cargo` builds into, inside `tools/<id>/`, whatever `CARGO_TARGET_DIR` says
/// where the user is: the registered `command` points here.
const TARGET_DIR: &str = "target";

/// What language a node is written in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Lang {
    /// `tools/<id>/run.py` over the SDK in `sdk/python/regulus.py`.
    #[default]
    Python,
    /// A Cargo crate in `tools/<id>/` that speaks the protocol over stdio.
    Rust,
}

/// What `regulus node init` was asked for.
#[derive(Debug, Clone)]
pub struct InitAsk {
    /// The project folder: it must have a `regulus.toml`.
    pub folder: PathBuf,
    /// The module id, and the node type of the starter node.
    pub id: String,
    /// What the node is written in.
    pub lang: Lang,
    /// The Python program the core starts (`python3`, or a venv's interpreter). Python only.
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

/// The Cargo package, and so the program, of a Rust node: `node-<id>`. The prefix keeps an
/// id that starts with a digit valid for Cargo.
pub fn rust_program(id: &str) -> String {
    format!("node-{id}")
}

/// Where `cargo build --release` leaves the program of the Rust node `id`, relative to the
/// project: what the registered `command` runs.
fn rust_command(id: &str) -> String {
    format!("{TOOLS_DIR}/{id}/{TARGET_DIR}/release/{}", rust_program(id))
}

/// The files of a Rust node under `tools/<id>/`: relative path and text.
fn rust_files(id: &str) -> Vec<(&'static str, String)> {
    let name = authoring::display_name(id);
    let fill = |text: &str| text.replace("__ID__", id).replace("__NAME__", &name);
    vec![
        ("Cargo.toml", fill(RUST_CARGO)),
        ("regulus-module.toml", fill(RUST_MANIFEST)),
        (".gitignore", fill(RUST_GITIGNORE)),
        ("src/main.rs", fill(RUST_MAIN)),
        ("src/regulus.rs", RUST_SDK.to_owned()),
    ]
}

/// The `[[modules.local]]` entry of a Rust node, as a table and as the text appended.
fn rust_entry(id: &str) -> (toml::Table, String) {
    let command = rust_command(id);
    let mut table = toml::Table::new();
    table.insert("id".into(), toml::Value::String(id.into()));
    table.insert(
        "command".into(),
        toml::Value::Array(vec![toml::Value::String(command.clone())]),
    );
    let quote = |s: &str| toml::Value::String(s.into()).to_string();
    let text = format!(
        "[[modules.local]]\n\
         # Written by `regulus node init --lang rust`; build it with `regulus node build`, \
         it runs only once the project is trusted.\n\
         id = {}\n\
         command = [{}]\n",
        quote(id),
        quote(&command),
    );
    (table, text)
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

/// `regulus node init`: see the module docs.
pub fn init(ask: &InitAsk) -> Result<Initialized> {
    match ask.lang {
        Lang::Python => {
            let done = authoring::init_python(&authoring::InitAsk {
                folder: ask.folder.clone(),
                id: ask.id.clone(),
                python: ask.python.clone(),
            })?;
            Ok(Initialized {
                root: done.root,
                project: done.project,
                files: done.files,
                rewritten: done.rewritten,
                notes: done.notes,
            })
        }
        Lang::Rust => init_rust(ask),
    }
}

/// A new Rust node: the crate in `tools/<id>/`, registered in `regulus.toml`. Every check
/// runs before the first write; the files are created new, never over something that
/// exists; `regulus.toml` is replaced in one rename after them, only if it did not change;
/// if anything fails, what was written is removed.
fn init_rust(ask: &InitAsk) -> Result<Initialized> {
    authoring::check_node_id(&ask.id)?;
    let root = ask
        .folder
        .canonicalize()
        .with_context(|| format!("cannot open the folder {}", ask.folder.display()))?;
    if !root.is_dir() {
        bail!("{} is not a folder", root.display());
    }
    let (entry, appended) = rust_entry(&ask.id);
    let plan = authoring::plan_registration(&root, &ask.id, entry, &appended)?;

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
    let files = rust_files(&ask.id);
    let paths: Vec<PathBuf> = files.iter().map(|(rel, _)| dir.join(rel)).collect();
    let written = files
        .iter()
        .zip(&paths)
        .try_for_each(|((_, text), path)| {
            if let Some(parent) = path.parent().filter(|p| *p != dir) {
                fs::create_dir_all(parent)
                    .with_context(|| format!("cannot create {}", parent.display()))?;
            }
            create_new(path, text)
        })
        .and_then(|()| plan.commit().map_err(Into::into));
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
        files: paths,
        rewritten: plan.rewritten,
        notes: plan.notes,
    })
}

/// What `regulus node build` was asked for.
#[derive(Debug, Clone)]
pub struct BuildAsk {
    /// The project folder.
    pub folder: PathBuf,
    /// The module id registered by `regulus node init --lang rust`.
    pub id: String,
    /// The `config.toml` that lists the trusted projects, and the home it belongs to.
    pub config: PathBuf,
    pub home: PathBuf,
    /// The `cargo` to run: the program name or path.
    pub cargo: std::ffi::OsString,
}

/// What `regulus node build` did.
#[derive(Debug, Clone)]
pub struct Built {
    /// The project folder, absolute.
    pub root: PathBuf,
    /// The program `cargo` built.
    pub program: PathBuf,
    /// Whether the entry's `command` runs that program. When it does not, the module is
    /// built but the core will not start the program that was built.
    pub registered: bool,
}

/// `regulus node build`: compiles the Rust crate of the node `id` with `cargo build
/// --release`, for this machine, into `tools/<id>/target` whatever `CARGO_TARGET_DIR` or
/// `CARGO_BUILD_TARGET` say. It runs code the project brings (build scripts, procedural
/// macros), so it needs the same trust as starting the module; it reads nothing of the
/// module itself and starts nothing. Cargo says where it put the program; one that is not
/// where the registered `command` points (a `build.target` in a Cargo config file builds
/// for another target into `target/<triple>/`) is refused, never registered. Reload the
/// module afterwards so the core runs the new program.
pub fn build(ask: &BuildAsk) -> Result<Built> {
    authoring::check_node_id(&ask.id)?;
    let root = ask
        .folder
        .canonicalize()
        .with_context(|| format!("cannot open the folder {}", ask.folder.display()))?;
    let file = root.join(PROJECT_FILE);
    let text = fs::read_to_string(&file).with_context(|| {
        format!(
            "{} is not a project: it has no {PROJECT_FILE}",
            root.display()
        )
    })?;
    let raw: toml::Table =
        toml::from_str(&text).with_context(|| format!("{} is not valid TOML", file.display()))?;
    let registered = raw
        .get("modules")
        .and_then(|m| m.get("local"))
        .and_then(toml::Value::as_array)
        .and_then(|list| {
            list.iter()
                .find(|m| m.get("id").and_then(toml::Value::as_str) == Some(ask.id.as_str()))
        })
        .with_context(|| {
            format!(
                "{PROJECT_FILE} has no [[modules.local]] with id `{}`; \
                 `regulus node init --lang rust` writes one",
                ask.id
            )
        })?;
    let dir = root.join(TOOLS_DIR).join(&ask.id);
    if !dir.join("Cargo.toml").is_file() || !inside(&root, &dir) {
        bail!(
            "{} has no Cargo.toml inside the project: there is nothing to build (a Python \
             node needs no build)",
            dir.display()
        );
    }
    if !regulus_core::projects::is_trusted(&ask.config, &ask.home, &root) {
        bail!(
            "building runs the project's own code (build scripts, procedural macros), so \
             {} must be trusted first: regulus project trust {}",
            root.display(),
            root.display()
        );
    }

    // Cargo's messages go to stdout as JSON (read below); its progress and diagnostics
    // still reach the user on stderr.
    let output = std::process::Command::new(&ask.cargo)
        .args([
            "build",
            "--release",
            "--message-format=json-render-diagnostics",
            "--manifest-path",
        ])
        .arg(dir.join("Cargo.toml"))
        .arg("--target-dir")
        .arg(dir.join(TARGET_DIR))
        .env_remove("CARGO_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET_DIR")
        .env_remove("CARGO_BUILD_TARGET")
        .current_dir(&dir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::inherit())
        .output()
        .with_context(|| format!("cannot run {}", ask.cargo.to_string_lossy()))?;
    if !output.status.success() {
        bail!(
            "cargo build failed ({}); nothing was registered or changed",
            output.status
        );
    }

    let name = rust_program(&ask.id);
    let program = dir.join(TARGET_DIR).join("release").join(&name);
    let built = built_programs(&output.stdout);
    let Some((_, made)) = built.iter().find(|(bin, _)| *bin == name) else {
        let names: Vec<&str> = built.iter().map(|(bin, _)| bin.as_str()).collect();
        bail!(
            "cargo built no program named `{name}` (it built: {}): the package in {} must be \
             named `{name}`",
            if names.is_empty() {
                "none".into()
            } else {
                names.join(", ")
            },
            dir.join("Cargo.toml").display()
        );
    };
    let same = made
        .canonicalize()
        .is_ok_and(|real| Some(real) == program.canonicalize().ok());
    if !same || !program.is_file() {
        bail!(
            "cargo built {}, not {}: a Cargo `build.target` (in a .cargo/config.toml of the \
             project, of a folder above it or of your home) builds for another target into \
             target/<triple>/. The core runs the program at {} on this machine: build this \
             crate without that setting. Nothing was registered or changed",
            made.display(),
            program.display(),
            program.display()
        );
    }
    // The entry runs the program the core is told about, relative to the project (or to its
    // `cwd`): say so when it is not the one just built.
    let command = registered
        .get("command")
        .and_then(toml::Value::as_array)
        .and_then(|c| c.first())
        .and_then(toml::Value::as_str)
        .unwrap_or_default();
    let base = registered
        .get("cwd")
        .and_then(toml::Value::as_str)
        .map_or_else(|| root.clone(), |cwd| root.join(cwd));
    let runs = base
        .join(command)
        .canonicalize()
        .is_ok_and(|real| Some(real) == program.canonicalize().ok());
    Ok(Built {
        root,
        program,
        registered: runs,
    })
}

/// The programs a `cargo build --message-format=json…` says it built: each binary
/// target's name and executable. Lines that are not such messages are skipped.
fn built_programs(stdout: &[u8]) -> Vec<(String, PathBuf)> {
    stdout
        .split(|b| *b == b'\n')
        .filter_map(|line| serde_json::from_slice::<serde_json::Value>(line).ok())
        .filter(|message| message["reason"] == "compiler-artifact")
        .filter(|message| {
            message["target"]["kind"]
                .as_array()
                .is_some_and(|kinds| kinds.iter().any(|kind| kind == "bin"))
        })
        .filter_map(|message| {
            let name = message["target"]["name"].as_str()?.to_owned();
            let executable = message["executable"].as_str()?;
            Some((name, PathBuf::from(executable)))
        })
        .collect()
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
            lang: Lang::Python,
            python: "python3".into(),
        }
    }

    fn ask_rust(folder: &Path, id: &str) -> InitAsk {
        InitAsk {
            lang: Lang::Rust,
            ..ask(folder, id)
        }
    }

    #[test]
    fn a_python_node_is_written_by_the_shared_writer_and_listed_run_py_first() {
        let root = project("python", "# mine\n[project]\nname = \"Demo\"\n");
        let done = init(&ask(&root, "caps")).unwrap();
        let shown: Vec<_> = done
            .files
            .iter()
            .map(|f| {
                f.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(shown, ["tools/caps/run.py", "tools/caps/regulus.py"]);
        assert!(!done.rewritten);
        let file = fs::read_to_string(root.join(PROJECT_FILE)).unwrap();
        assert!(file.starts_with("# mine\n"), "{file}");
        assert!(
            file.contains("command = [\"python3\", \"tools/caps/run.py\"]"),
            "{file}"
        );
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rust_node_is_a_crate_and_its_command_runs_the_release_build() {
        let file = "# my project\n[project]\nname = \"Demo\"\n";
        let root = project("rust", file);
        let done = init(&ask_rust(&root, "9lives")).unwrap();
        assert!(!done.rewritten);
        let shown: Vec<_> = done
            .files
            .iter()
            .map(|f| {
                f.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(
            shown,
            [
                "tools/9lives/Cargo.toml",
                "tools/9lives/regulus-module.toml",
                "tools/9lives/.gitignore",
                "tools/9lives/src/main.rs",
                "tools/9lives/src/regulus.rs"
            ]
        );
        let text = fs::read_to_string(root.join(PROJECT_FILE)).unwrap();
        assert!(text.starts_with(file), "{text}");
        let table: toml::Table = toml::from_str(&text).unwrap();
        let local = table["modules"]["local"].as_array().unwrap();
        assert_eq!(
            local[0]["command"].as_array().unwrap(),
            &[toml::Value::String(
                "tools/9lives/target/release/node-9lives".into()
            )]
        );
        let cargo = fs::read_to_string(root.join("tools/9lives/Cargo.toml")).unwrap();
        assert!(cargo.contains("name = \"node-9lives\""), "{cargo}");
        let manifest = fs::read_to_string(root.join("tools/9lives/regulus-module.toml")).unwrap();
        assert!(manifest.contains("type = \"9lives\""), "{manifest}");
        let main = fs::read_to_string(root.join("tools/9lives/src/main.rs")).unwrap();
        assert!(main.contains(".node(\"9lives\", upper)"), "{main}");
        assert!(main.contains("mod regulus;"), "{main}");
        let sdk = fs::read_to_string(root.join("tools/9lives/src/regulus.rs")).unwrap();
        assert_eq!(sdk, RUST_SDK, "the helper is copied as it is");
        for written in [&cargo, &manifest, &main] {
            assert!(
                !written.contains("__ID__") && !written.contains("__NAME__"),
                "{written}"
            );
        }
        assert!(!root.join("tools/9lives/run.py").exists());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_rust_node_refuses_what_it_cannot_do_cleanly_and_writes_nothing() {
        // A taken id (the shared writer's rule): neither the folder nor the file change.
        let file = "[[modules.local]]\nid = \"caps\"\ncommand = [\"sh\"]\n";
        let root = project("rust-taken", file);
        let error = init(&ask_rust(&root, "caps")).unwrap_err().to_string();
        assert!(error.contains("already has"), "{error}");
        assert!(!root.join(TOOLS_DIR).exists());
        assert_eq!(fs::read_to_string(root.join(PROJECT_FILE)).unwrap(), file);
        let _ = fs::remove_dir_all(&root);

        // Files in the way: never overwritten, the registration is not written.
        let root = project("rust-exists", "");
        fs::create_dir_all(root.join("tools/caps")).unwrap();
        fs::write(root.join("tools/caps/Cargo.toml"), "mine").unwrap();
        let error = init(&ask_rust(&root, "caps")).unwrap_err().to_string();
        assert!(error.contains("already exists"), "{error}");
        assert_eq!(
            fs::read_to_string(root.join("tools/caps/Cargo.toml")).unwrap(),
            "mine"
        );
        assert_eq!(fs::read_to_string(root.join(PROJECT_FILE)).unwrap(), "");
        let _ = fs::remove_dir_all(&root);

        // Not a project, or an id that is not a plain name: nothing is created.
        let root = project("rust-none", "");
        fs::remove_file(root.join(PROJECT_FILE)).unwrap();
        let error = init(&ask_rust(&root, "caps")).unwrap_err().to_string();
        assert!(error.contains("regulus project open"), "{error}");
        assert!(!root.join(TOOLS_DIR).exists());
        let root2 = project("rust-id", "");
        for bad in ["../up", "Caps", "a/b", "x@1", ""] {
            assert!(init(&ask_rust(&root2, bad)).is_err(), "{bad:?}");
        }
        assert!(!root2.join(TOOLS_DIR).exists());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&root2);
    }

    #[cfg(unix)]
    #[test]
    fn a_tools_link_out_of_the_project_is_refused_for_rust_too() {
        let root = project("rust-link", "");
        let outside =
            root.with_file_name(format!("regulus-node-unit-outside-{}", std::process::id()));
        fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, root.join(TOOLS_DIR)).unwrap();
        let error = init(&ask_rust(&root, "caps")).unwrap_err().to_string();
        assert!(error.contains("inside the project"), "{error}");
        assert!(fs::read_dir(&outside).unwrap().next().is_none());
        let _ = fs::remove_dir_all(&root);
        let _ = fs::remove_dir_all(&outside);
    }

    /// The committed example is what `--lang rust` writes for its id: one source.
    #[test]
    fn the_rust_example_is_what_node_init_writes() {
        let example =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/local-modules/rust-stdio");
        assert_eq!(
            rust_command("rust-stdio"),
            "tools/rust-stdio/target/release/node-rust-stdio"
        );
        for (relative, text) in rust_files("rust-stdio") {
            let committed = fs::read_to_string(example.join(relative))
                .unwrap_or_else(|e| panic!("{relative} of the example: {e}"));
            assert_eq!(committed, text, "{relative} drifted from the template");
        }
    }

    /// `regulus node build` of the Rust node `y` in a trusted project, with a `cargo` that
    /// says it built the program `bin` at `at` (under `--target-dir`), and makes it.
    #[cfg(unix)]
    fn build_with_fake_cargo(name: &str, bin: &str, at: &str) -> Result<Built> {
        use std::os::unix::fs::PermissionsExt;
        let root = project(name, "");
        init(&ask_rust(&root, "y")).unwrap();
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let config = home.join("config.toml");
        let trusted = format!(
            "[projects]\ntrusted = [{:?}]\n",
            root.canonicalize().unwrap()
        );
        fs::write(&config, trusted).unwrap();
        let cargo = root.join("fake-cargo");
        let script = format!(
            r#"#!/bin/sh
while [ "$1" != "--target-dir" ]; do shift; done
exe="$2/{at}"
mkdir -p "$(dirname "$exe")" && : > "$exe"
echo '{{"reason":"compiler-artifact","target":{{"kind":["lib"],"name":"dep"}},"executable":null}}'
echo "{{\"reason\":\"compiler-artifact\",\"target\":{{\"kind\":[\"bin\"],\"name\":\"{bin}\"}},\"executable\":\"$exe\"}}"
echo '{{"reason":"build-finished","success":true}}'
"#
        );
        fs::write(&cargo, script).unwrap();
        fs::set_permissions(&cargo, fs::Permissions::from_mode(0o755)).unwrap();
        let built = build(&BuildAsk {
            folder: root.clone(),
            id: "y".into(),
            config,
            home,
            cargo: cargo.into(),
        });
        let _ = fs::remove_dir_all(&root);
        built
    }

    /// Where Cargo put the program decides, not where it is expected: a build for another
    /// target (`build.target` in a Cargo config) or a renamed package is refused by name.
    #[cfg(unix)]
    #[test]
    fn building_trusts_where_cargo_says_it_put_the_program() {
        let built = build_with_fake_cargo("build-native", "node-y", "release/node-y").unwrap();
        assert!(built.program.ends_with("tools/y/target/release/node-y"));
        assert!(built.registered);

        let cross = "x86_64-unknown-linux-gnu/release/node-y";
        let error = build_with_fake_cargo("build-cross", "node-y", cross)
            .unwrap_err()
            .to_string();
        assert!(error.contains("build.target"), "{error}");
        assert!(error.contains(cross), "{error}");
        assert!(!error.contains("must be named"), "{error}");

        let error = build_with_fake_cargo("build-named", "other", "release/other")
            .unwrap_err()
            .to_string();
        assert!(error.contains("it built: other"), "{error}");
        assert!(error.contains("must be named `node-y`"), "{error}");
    }

    #[test]
    fn building_needs_a_registered_rust_node_and_a_trusted_project() {
        let root = project("build", "");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let request = BuildAsk {
            folder: root.clone(),
            id: "x".into(),
            config: home.join("config.toml"),
            home: home.clone(),
            cargo: "cargo-that-does-not-exist".into(),
        };
        // Nothing registered.
        let error = build(&request).unwrap_err().to_string();
        assert!(error.contains("no [[modules.local]]"), "{error}");
        // A Python node: no crate to build.
        init(&ask(&root, "x")).unwrap();
        let error = build(&request).unwrap_err().to_string();
        assert!(error.contains("nothing to build"), "{error}");
        // A Rust node in an untrusted project: refused before anything runs; the program
        // named does not even exist.
        init(&ask_rust(&root, "y")).unwrap();
        let error = build(&BuildAsk {
            id: "y".into(),
            ..request.clone()
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("regulus project trust"), "{error}");
        assert!(!root.join("tools/y/target").exists());
        let _ = fs::remove_dir_all(&root);
    }
}
