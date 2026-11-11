//! A Rust custom node end to end (docs/18-custom-nodes.md §6): `regulus node init --lang
//! rust` writes a Cargo crate into an existing project and registers it as a
//! `[[modules.local]]`; building it needs a trusted project; the core then runs the program
//! as a process, asks it for the manifest, and a profile comes back with the value
//! transformed; closing the project leaves no process. The crate's own tests pass too.
//! Rebuilt with more node types and reloaded, it shows what its helper (`sdk/rust/regulus.rs`)
//! promises against the real core: a panic fails one call, a slow event of one execution
//! does not hold another's.
//!
//! The build is real (`cargo build --release` of a crate that depends on `serde_json`), so
//! it runs offline: the workspace that compiled this test already has that crate in
//! Cargo's cache.
//!
//! The helper's own tests, over in-memory stdio, are compiled in here (`sdk::`).

#[path = "../../../sdk/rust/regulus.rs"]
mod regulus;
#[path = "../../../sdk/rust/tests.rs"]
mod sdk;

use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use regulus_core::{local, Config};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
/// Marks the processes of one test run, to find them in `/proc`.
const MARK: &str = "REGULUS_NODE_INIT_TEST";

fn regulus(home: &Path, args: &[&str]) -> Output {
    Command::new(PROGRAM)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The ids of the processes whose environment has `MARK=<mark>`.
fn marked(mark: &str) -> Vec<String> {
    let wanted = format!("{MARK}={mark}");
    std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| {
            std::fs::read(entry.path().join("environ"))
                .is_ok_and(|env| env.split(|b| *b == 0).any(|var| var == wanted.as_bytes()))
        })
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

/// Stops every core serving `home`; the started core is detached, so nobody else will.
fn stop_cores(home: &Path) {
    let home = home.to_string_lossy().into_owned();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = cmdline
            .split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        if args.contains(&home) && args.iter().any(|a| a == "serve") {
            let pid = entry.file_name().to_string_lossy().into_owned();
            let _ = Command::new("kill").arg(pid).status();
        }
    }
}

/// A one-node pipe through `node` of `module`: `self.in` → node `in` → node `out` →
/// `self.out`, with `config` on the node.
fn pipe(id: &str, node_type: &str, module: &str, ports: [&str; 2], config: &str) -> String {
    let [input, output] = ports;
    format!(
        r#"
id = "{id}"
name = "{id}"
main_workflow_id = "main"

[[workflows]]
id = "main"
name = "Pipe"
kind = "main"
ports = [
  {{ id = "in", capability = "text@1", direction = "input", kind = "then" }},
  {{ id = "out", capability = "text@1", direction = "output", kind = "then" }},
]

  [[workflows.nodes]]
  id = "n"
  name = "Node"
  type = "{node_type}"
  module_id = "{module}"
  config = {config}
  ports = [
    {{ id = "{input}", capability = "text@1", direction = "input", kind = "then", cardinality = "many" }},
    {{ id = "{output}", capability = "text@1", direction = "output", kind = "then" }},
  ]

  [[workflows.wires]]
  id = "w1"
  kind = "then"
  from = {{ node_id = "self", port_id = "in" }}
  to = {{ node_id = "n", port_id = "{input}" }}

  [[workflows.wires]]
  id = "w2"
  kind = "then"
  from = {{ node_id = "n", port_id = "{output}" }}
  to = {{ node_id = "self", port_id = "out" }}
"#
    )
}

/// `self.in` → each of `nodes` (`(id, type, config)`, ports `in` and `out`) of `module` in
/// turn → `self.out`.
fn chain(id: &str, module: &str, nodes: &[(&str, &str, &str)]) -> String {
    let mut text = format!(
        r#"
id = "{id}"
name = "{id}"
main_workflow_id = "main"

[[workflows]]
id = "main"
name = "Chain"
kind = "main"
ports = [
  {{ id = "in", capability = "text@1", direction = "input", kind = "then" }},
  {{ id = "out", capability = "text@1", direction = "output", kind = "then" }},
]
"#
    );
    for (node, node_type, config) in nodes {
        text += &format!(
            r#"
  [[workflows.nodes]]
  id = "{node}"
  name = "{node}"
  type = "{node_type}"
  module_id = "{module}"
  config = {config}
  ports = [
    {{ id = "in", capability = "text@1", direction = "input", kind = "then", cardinality = "many" }},
    {{ id = "out", capability = "text@1", direction = "output", kind = "then" }},
  ]
"#
        );
    }
    let ends: Vec<&str> = std::iter::once("self")
        .chain(nodes.iter().map(|node| node.0))
        .chain(std::iter::once("self"))
        .collect();
    for (index, pair) in ends.windows(2).enumerate() {
        let from = if index == 0 { "in" } else { "out" };
        let to = if index + 2 == ends.len() { "out" } else { "in" };
        text += &format!(
            r#"
  [[workflows.wires]]
  id = "w{index}"
  kind = "then"
  from = {{ node_id = "{}", port_id = "{from}" }}
  to = {{ node_id = "{}", port_id = "{to}" }}
"#,
            pair[0], pair[1]
        );
    }
    text
}

/// The file the `wait` node of `wait-pipe` waits for, in the project `folder`.
fn release_flag(folder: &Path) -> std::path::PathBuf {
    folder.join("release-flag")
}

/// A temporary home and an existing project folder `demo` with `project_file`, profiles
/// through `demo/rusty` (the last two need node types the test adds later) and one through
/// the built-in greeter, and a core serving them.
fn with_core(name: &str, project_file: &str, test: impl FnOnce(&Path, &Path)) {
    let root = std::env::temp_dir().join(format!("regulus-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let folder = root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(folder.join(".regulus/profiles")).unwrap();
    std::fs::write(home.join("config.toml"), "[server]\nport = 0\n").unwrap();
    std::fs::write(folder.join("regulus.toml"), project_file).unwrap();
    let caps = pipe(
        "rusty-pipe",
        "rusty",
        "demo/rusty@0.1.0",
        ["in", "out"],
        "{ prefix = \"> \" }",
    );
    let greet = pipe(
        "greet-pipe",
        "greeter",
        "regulus/hello",
        ["echo", "reply"],
        "{}",
    );
    std::fs::write(folder.join(".regulus/profiles/rusty-pipe.toml"), caps).unwrap();
    std::fs::write(folder.join(".regulus/profiles/greet-pipe.toml"), greet).unwrap();
    let module = "demo/rusty@0.1.0";
    let flag = format!("{{ flag = {:?} }}", release_flag(&folder).to_str().unwrap());
    let more = [
        (
            "twice-pipe",
            chain(
                "twice-pipe",
                module,
                &[
                    ("a", "rusty", "{ prefix = \"a:\" }"),
                    ("b", "rusty", "{ prefix = \"b:\" }"),
                ],
            ),
        ),
        (
            "boom-pipe",
            chain("boom-pipe", module, &[("n", "boom", "{}")]),
        ),
        (
            "wait-pipe",
            chain("wait-pipe", module, &[("n", "wait", &flag)]),
        ),
    ];
    for (profile, text) in more {
        let file = folder.join(format!(".regulus/profiles/{profile}.toml"));
        std::fs::write(file, text).unwrap();
    }
    let config = Config::load(&home).unwrap();
    let started = local::start(&config, Path::new(PROGRAM));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        started.unwrap();
        test(&home, &folder);
    }));
    stop_cores(&home);
    let _ = std::fs::remove_dir_all(&root);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

/// Starts `profile`; its execution id.
fn start(home: &Path, profile: &str) -> String {
    let run = regulus(home, &["run", profile, "--detach"]);
    assert!(run.status.success(), "{}", text(&run));
    text(&run).split_whitespace().next().unwrap().to_string()
}

/// Sends `value` to the `in` of `execution`; the events it caused.
fn send(home: &Path, execution: &str, value: &str) -> String {
    let sent = regulus(home, &["send", execution, "in", value]);
    assert!(sent.status.success(), "{}", text(&sent));
    text(&sent)
}

/// Starts `profile` and sends `value` to its `in`; the events it caused.
fn run_and_send(home: &Path, profile: &str, value: &str) -> String {
    send(home, &start(home, profile), value)
}

/// Waits up to 30 s for `done`.
fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The node's program again with two node types more, as a user would write them: `boom`
/// panics on "boom"; `wait` says it is waiting, then holds its value until the file its
/// `flag` field names exists.
const MORE_NODES: &str = r#"mod regulus;

use std::path::Path;
use std::time::{Duration, Instant};

use regulus::{Context, Error, Module};
use serde_json::Value;

fn main() {
    Module::new(include_str!("../regulus-module.toml"))
        .node("rusty", upper)
        .node("boom", boom)
        .node("wait", wait)
        .run();
}

fn upper(ctx: &Context, _port: &str, value: Value) -> Result<(), Error> {
    let text = value
        .as_str()
        .ok_or_else(|| Error::invalid("text@1 must be a string"))?;
    let prefix = ctx.config("prefix").as_str().unwrap_or("");
    ctx.emit("out", format!("{prefix}{}", text.to_uppercase()))
}

fn boom(ctx: &Context, _port: &str, value: Value) -> Result<(), Error> {
    if value == "boom" {
        panic!("asked to panic");
    }
    ctx.emit("out", value)
}

fn wait(ctx: &Context, _port: &str, value: Value) -> Result<(), Error> {
    let flag = ctx
        .config("flag")
        .as_str()
        .ok_or_else(|| Error::invalid("flag names a file"))?;
    std::fs::write(format!("{flag}.waiting"), "")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while !Path::new(flag).exists() {
        if Instant::now() > deadline {
            return Err(Error::failed("never released"));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    ctx.emit("out", value)
}
"#;

/// The node types `MORE_NODES` adds, appended to the crate's `regulus-module.toml`.
const MORE_MANIFEST: &str = r#"
[[node]]
type = "boom"
name = "Boom"

  [[node.port]]
  id          = "in"
  capability  = "text@1"
  direction   = "input"
  kind        = "then"
  cardinality = "many"

  [[node.port]]
  id         = "out"
  capability = "text@1"
  direction  = "output"
  kind       = "then"

[[node]]
type = "wait"
name = "Wait"

  [[node.port]]
  id          = "in"
  capability  = "text@1"
  direction   = "input"
  kind        = "then"
  cardinality = "many"

  [[node.port]]
  id         = "out"
  capability = "text@1"
  direction  = "output"
  kind       = "then"

  [[node.field]]
  id    = "flag"
  type  = "text"
  label = "Flag file"
"#;

/// The line of `module ls` for `id`.
fn listed(home: &Path, id: &str) -> Option<String> {
    let ls = regulus(home, &["module", "ls"]);
    assert!(ls.status.success(), "{}", text(&ls));
    text(&ls)
        .lines()
        .find(|l| l.starts_with(id))
        .map(str::to_string)
}

/// The project file written for the test: a comment and an unknown key the command keeps,
/// and an `env` the test adds to the entry to find the process.
const PROJECT: &str = "# The demo project.\n[project]\nname = \"Demo\"\nflavour = \"kept\"  # not a key the core knows\n";

/// Marks the `caps` entry's process with `mark`, as a user would add `env` by hand.
fn mark_entry(folder: &Path, mark: &str) {
    let file = folder.join("regulus.toml");
    let text = std::fs::read_to_string(&file).unwrap();
    let marked = text.replace(
        "id = \"rusty\"\n",
        &format!("id = \"rusty\"\nenv = {{ {MARK} = {mark:?} }}\n"),
    );
    assert_ne!(marked, text, "the entry is there to mark: {text}");
    std::fs::write(file, marked).unwrap();
}

/// `regulus` with `CARGO_NET_OFFLINE`: the build of the node must not need the network.
fn regulus_offline(home: &Path, args: &[&str]) -> Output {
    Command::new(PROGRAM)
        .env("CARGO_NET_OFFLINE", "true")
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn a_rust_node_is_written_built_trusted_and_run_as_a_process() {
    let mark = format!("rusty-{}", std::process::id());
    with_core("rust-node", PROJECT, |home, folder| {
        let path = folder.to_str().unwrap();

        // Rust takes no `--python`; nothing was written for the refusal.
        let refused = regulus(
            home,
            &[
                "node", "init", path, "--id", "rusty", "--lang", "rust", "--python", "p",
            ],
        );
        assert!(!refused.status.success());
        assert!(
            text(&refused).contains("only for --lang python"),
            "{}",
            text(&refused)
        );
        assert!(!folder.join("tools").exists());

        // Write the crate; nothing runs, nothing is trusted, nothing is built.
        let init = regulus(
            home,
            &["node", "init", path, "--id", "rusty", "--lang", "rust"],
        );
        assert!(init.status.success(), "{}", text(&init));
        let said = text(&init);
        for file in [
            "Cargo.toml",
            "regulus-module.toml",
            ".gitignore",
            "src/main.rs",
            "src/regulus.rs",
        ] {
            assert!(
                said.contains(&format!("created tools/rusty/{file}")),
                "{said}"
            );
        }
        assert!(
            said.contains("regulus node build"),
            "it says how to build: {said}"
        );
        assert!(!said.contains("warning"), "{said}");
        let file = std::fs::read_to_string(folder.join("regulus.toml")).unwrap();
        assert!(file.starts_with(PROJECT), "comments and keys kept: {file}");
        assert!(
            file.contains("command = [\"tools/rusty/target/release/node-rusty\"]"),
            "{file}"
        );
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(!config.contains("trusted"), "never trusted: {config}");
        assert!(!folder.join("tools/rusty/target").exists());
        mark_entry(folder, &mark);

        // Untrusted: blocked, no process, and building is refused before cargo runs.
        let opened = regulus(home, &["project", "open", path]);
        assert!(opened.status.success(), "{}", text(&opened));
        let line = listed(home, "demo/rusty").expect("demo/rusty listed");
        assert!(line.contains("blocked"), "{line}");
        let build = regulus_offline(home, &["node", "build", path, "--id", "rusty"]);
        assert!(!build.status.success(), "{}", text(&build));
        assert!(
            text(&build).contains("regulus project trust"),
            "{}",
            text(&build)
        );
        assert!(
            !folder.join("tools/rusty/target").exists(),
            "cargo never ran"
        );
        assert!(marked(&mark).is_empty(), "nothing runs untrusted");

        // Trusted but not built: the module is broken, and says so; nothing else is.
        let trusted = regulus(home, &["project", "trust", path]);
        assert!(trusted.status.success(), "{}", text(&trusted));
        let line = listed(home, "demo/rusty").expect("demo/rusty listed");
        assert!(line.contains("broken"), "no program yet: {line}");

        // Build it: the program is where the entry points.
        let build = regulus_offline(home, &["node", "build", path, "--id", "rusty"]);
        assert!(build.status.success(), "{}", text(&build));
        let said = text(&build);
        assert!(
            said.contains("built tools/rusty/target/release/node-rusty"),
            "{said}"
        );
        assert!(
            !said.contains("warning"),
            "the command runs what was built: {said}"
        );
        assert!(folder
            .join("tools/rusty/target/release/node-rusty")
            .is_file());

        // Reopen: one process, its manifest from the crate, the value transformed.
        let closed = regulus(home, &["project", "close", "demo"]);
        assert!(closed.status.success(), "{}", text(&closed));
        let opened = regulus(home, &["project", "open", path]);
        assert!(opened.status.success(), "{}", text(&opened));
        assert_eq!(
            marked(&mark).len(),
            1,
            "opening a trusted project starts it, once"
        );
        let line = listed(home, "demo/rusty@0.1.0").expect("demo/rusty@0.1.0 listed");
        assert!(line.contains("idle"), "{line}");
        let show = text(&regulus(home, &["module", "show", "demo/rusty"]));
        assert!(show.contains("type = \"rusty\""), "{show}");
        assert!(show.contains("id = \"prefix\""), "{show}");

        let events = run_and_send(home, "rusty-pipe", "hello there");
        assert!(
            events.contains("n.out emitted text@1 \"> HELLO THERE\""),
            "{events}"
        );
        assert!(
            events.contains("self.out emitted text@1 \"> HELLO THERE\""),
            "{events}"
        );
        assert!(!events.contains("failed"), "{events}");

        // The crate's own tests pass (they ship with the template).
        let tested = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args(["test", "--offline", "--manifest-path"])
            .arg(folder.join("tools/rusty/Cargo.toml"))
            .arg("--target-dir")
            .arg(folder.join("tools/rusty/target"))
            .env_remove("CARGO_TARGET_DIR")
            .output()
            .unwrap();
        assert!(tested.status.success(), "{}", text(&tested));
        assert!(text(&tested).contains("3 passed"), "{}", text(&tested));

        // Two nodes of the type in one execution keep their own config.
        let events = run_and_send(home, "twice-pipe", "hi");
        assert!(
            events.contains("self.out emitted text@1 \"b:A:HI\""),
            "{events}"
        );

        // A value the node refuses fails that call, visibly; the execution goes on.
        let execution = start(home, "rusty-pipe");
        let refused = send(home, &execution, "3");
        assert!(refused.contains("n.in failed"), "{refused}");
        assert!(refused.contains("text@1 must be a string"), "{refused}");
        let events = send(home, &execution, "again");
        assert!(
            events.contains("self.out emitted text@1 \"> AGAIN\""),
            "{events}"
        );

        // More node types: rebuilt, then reloaded without restarting the core.
        std::fs::write(folder.join("tools/rusty/src/main.rs"), MORE_NODES).unwrap();
        let manifest = folder.join("tools/rusty/regulus-module.toml");
        let declared = std::fs::read_to_string(&manifest).unwrap() + MORE_MANIFEST;
        std::fs::write(&manifest, declared).unwrap();
        let build = regulus_offline(home, &["node", "build", path, "--id", "rusty"]);
        assert!(build.status.success(), "{}", text(&build));
        assert!(!text(&build).contains("warning"), "{}", text(&build));
        let reload = regulus(
            home,
            &[
                "project",
                "reload",
                "demo",
                "--module",
                "rusty",
                "--stop-executions",
            ],
        );
        assert!(reload.status.success(), "{}", text(&reload));
        wait_until("one process after the reload", || marked(&mark).len() == 1);
        let show = text(&regulus(home, &["module", "show", "demo/rusty"]));
        assert!(show.contains("type = \"wait\""), "{show}");

        // A panic fails its call only: the module and the execution go on.
        let execution = start(home, "boom-pipe");
        let events = send(home, &execution, "boom");
        assert!(
            events.contains("n.in failed") && events.contains("the node panicked: asked to panic"),
            "{events}"
        );
        let events = send(home, &execution, "fine");
        assert!(
            events.contains("self.out emitted text@1 \"fine\""),
            "{events}"
        );
        let line = listed(home, "demo/rusty").expect("demo/rusty listed");
        assert!(!line.contains("broken"), "{line}");
        assert_eq!(marked(&mark).len(), 1, "the same process");

        // A slow event of one execution does not hold the event of another.
        let flag = release_flag(folder);
        let slow = start(home, "wait-pipe");
        let queued = regulus(home, &["send", &slow, "in", "slow", "--detach"]);
        assert!(queued.status.success(), "{}", text(&queued));
        let waiting = flag.with_extension("waiting");
        wait_until("the wait node holds its value", || waiting.exists());
        let events = run_and_send(home, "rusty-pipe", "quick");
        assert!(
            events.contains("self.out emitted text@1 \"> QUICK\""),
            "{events}"
        );
        let held = text(&regulus(home, &["events", "--execution", &slow]));
        assert!(!held.contains("self.out emitted"), "still held: {held}");
        std::fs::write(&flag, "").unwrap();
        wait_until("the slow value goes on once released", || {
            text(&regulus(home, &["events", "--execution", &slow]))
                .contains("self.out emitted text@1 \"slow\"")
        });

        // A setting the helper cannot use stops the program before it serves, saying why.
        let program = folder.join("tools/rusty/target/release/node-rusty");
        let bad = Command::new(program)
            .env("REGULUS_MAX_WORKERS", "0")
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(bad.status.code(), Some(2), "{}", text(&bad));
        assert!(
            text(&bad).contains("REGULUS_MAX_WORKERS must be a whole number above 0"),
            "{}",
            text(&bad)
        );

        // Gone with its project: the process stops, the core and the rest go on.
        let closed = regulus(home, &["project", "close", "demo"]);
        assert!(closed.status.success(), "{}", text(&closed));
        assert!(marked(&mark).is_empty(), "the node's process is gone");
        assert!(listed(home, "demo/rusty").is_none());
        let hello = listed(home, "regulus/hello").expect("regulus/hello listed");
        assert!(
            hello.contains("idle") || hello.contains("active"),
            "{hello}"
        );
    });
}

#[test]
fn building_a_python_node_or_a_missing_one_is_refused_plainly() {
    let root = std::env::temp_dir().join(format!("regulus-node-nobuild-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let folder = root.join("plain");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("regulus.toml"), "").unwrap();
    let path = folder.to_str().unwrap();

    let none = regulus(&home, &["node", "build", path, "--id", "ghost"]);
    assert!(!none.status.success());
    assert!(
        text(&none).contains("no [[modules.local]]"),
        "{}",
        text(&none)
    );

    // A Python node is registered but has no crate: nothing to build, even if trusted.
    let init = regulus(&home, &["node", "init", path, "--id", "py"]);
    assert!(init.status.success(), "{}", text(&init));
    // Trusted by the file the core reads, so no core has to run for this.
    std::fs::write(
        home.join("config.toml"),
        format!(
            "[projects]\ntrusted = [{:?}]\n",
            folder.canonicalize().unwrap()
        ),
    )
    .unwrap();
    let build = regulus(&home, &["node", "build", path, "--id", "py"]);
    assert!(!build.status.success());
    assert!(
        text(&build).contains("nothing to build"),
        "{}",
        text(&build)
    );
    assert!(!folder.join("tools/py/target").exists());
    let _ = std::fs::remove_dir_all(&root);
}
