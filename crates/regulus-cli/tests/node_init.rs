//! `regulus node init` end to end (docs/18-custom-nodes.md): the command writes a Python
//! node with the SDK beside it into an existing project and registers it as a
//! `[[modules.local]]`; an untrusted project lists it as `blocked` and runs nothing; once
//! trusted, the core asks the SDK for the manifest, runs a profile through the node and the
//! value comes back transformed; vetoing or dropping the module leaves the rest usable.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regulus_core::{local, Config};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
/// Marks the processes of one test run, to find them in `/proc`.
const MARK: &str = "REGULUS_NODE_INIT_TEST";

/// Whether `python3` runs here; the tests that need it are skipped, with a word, if not.
fn python() -> bool {
    let found = Command::new("python3")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !found {
        eprintln!("skipped: python3 is not available to run the generated node");
    }
    found
}

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

/// A temporary home and an existing project folder `demo` with `project_file`, a profile
/// through `demo/caps` and one through the built-in greeter, and a core serving them.
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
        "caps-pipe",
        "caps",
        "demo/caps@0.1.0",
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
    std::fs::write(folder.join(".regulus/profiles/caps-pipe.toml"), caps).unwrap();
    std::fs::write(folder.join(".regulus/profiles/greet-pipe.toml"), greet).unwrap();
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

/// Starts `profile` and sends `value` to its `in`; the events it caused.
fn run_and_send(home: &Path, profile: &str, value: &str) -> String {
    let run = regulus(home, &["run", profile, "--detach"]);
    assert!(run.status.success(), "{}", text(&run));
    let execution = text(&run).split_whitespace().next().unwrap().to_string();
    let sent = regulus(home, &["send", &execution, "in", value]);
    assert!(sent.status.success(), "{}", text(&sent));
    text(&sent)
}

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
        "id = \"caps\"\n",
        &format!("id = \"caps\"\nenv = {{ {MARK} = {mark:?} }}\n"),
    );
    assert_ne!(marked, text, "the entry is there to mark: {text}");
    std::fs::write(file, marked).unwrap();
}

#[test]
fn a_generated_node_runs_once_trusted_and_its_removal_leaves_the_rest_working() {
    if !python() {
        return;
    }
    let mark = format!("caps-{}", std::process::id());
    with_core("node-init", PROJECT, |home, folder| {
        let path = folder.to_str().unwrap();

        // Write the node into the existing project; nothing runs yet.
        let init = regulus(home, &["node", "init", path, "--id", "caps"]);
        assert!(init.status.success(), "{}", text(&init));
        let said = text(&init);
        assert!(said.contains("created tools/caps/run.py"), "{said}");
        assert!(said.contains("created tools/caps/regulus.py"), "{said}");
        assert!(said.contains("--trust"), "it says how to run it: {said}");
        assert!(!said.contains("warning"), "{said}");
        let file = std::fs::read_to_string(folder.join("regulus.toml")).unwrap();
        assert!(file.starts_with(PROJECT), "comments and keys kept: {file}");
        assert!(
            file.contains("command = [\"python3\", \"tools/caps/run.py\"]"),
            "{file}"
        );
        let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
        assert!(!config.contains("trusted"), "never trusted: {config}");

        // Again: refused, nothing changed.
        let again = regulus(home, &["node", "init", path, "--id", "caps"]);
        assert!(!again.status.success());
        assert!(text(&again).contains("already"), "{}", text(&again));
        assert_eq!(
            std::fs::read_to_string(folder.join("regulus.toml")).unwrap(),
            file
        );
        mark_entry(folder, &mark);

        // Untrusted: listed as blocked, no process.
        let opened = regulus(home, &["project", "open", path]);
        assert!(opened.status.success(), "{}", text(&opened));
        let line = listed(home, "demo/caps").expect("demo/caps listed");
        assert!(line.contains("blocked"), "{line}");
        assert!(marked(&mark).is_empty(), "nothing runs untrusted");
        let refused = regulus(home, &["run", "caps-pipe"]);
        assert!(!refused.status.success(), "{}", text(&refused));

        // Trusted: one process, its manifest from the SDK, the value transformed.
        let trusted = regulus(home, &["project", "trust", path]);
        assert!(trusted.status.success(), "{}", text(&trusted));
        assert_eq!(marked(&mark).len(), 1, "trusting starts it, once");
        let line = listed(home, "demo/caps@0.1.0").expect("demo/caps@0.1.0 listed");
        assert!(line.contains("idle"), "{line}");
        let show = text(&regulus(home, &["module", "show", "demo/caps"]));
        assert!(show.contains("type = \"caps\""), "{show}");
        assert!(show.contains("id = \"prefix\""), "{show}");

        let events = run_and_send(home, "caps-pipe", "hello there");
        assert!(
            events.contains("n.out emitted text@1 \"> HELLO THERE\""),
            "{events}"
        );
        assert!(
            events.contains("self.out emitted text@1 \"> HELLO THERE\""),
            "{events}"
        );
        assert!(!events.contains("failed"), "{events}");

        // Vetoed: its profile is refused, the built-in greeter still runs.
        let text_now = std::fs::read_to_string(folder.join("regulus.toml")).unwrap();
        std::fs::write(
            folder.join("regulus.toml"),
            format!("{text_now}\n[modules]\ndisable = [\"demo/caps\"]\n"),
        )
        .unwrap();
        let refused = regulus(home, &["run", "caps-pipe"]);
        assert!(!refused.status.success());
        assert!(
            text(&refused).contains("disabled in project demo"),
            "{}",
            text(&refused)
        );
        let events = run_and_send(home, "greet-pipe", "Ada");
        assert!(
            events.contains("self.out emitted text@1 \"Hello, Ada!\""),
            "{events}"
        );

        // Gone with its project: the process stops, the core and the rest go on.
        let closed = regulus(home, &["project", "close", "demo"]);
        assert!(closed.status.success(), "{}", text(&closed));
        assert!(marked(&mark).is_empty(), "the node's process is gone");
        assert!(listed(home, "demo/caps").is_none());
        let hello = listed(home, "regulus/hello").expect("regulus/hello listed");
        // Usable: idle, or active with the greeter's execution still running.
        assert!(
            hello.contains("idle") || hello.contains("active"),
            "{hello}"
        );
    });
}

#[test]
fn node_init_refuses_what_it_cannot_do_cleanly() {
    let root = std::env::temp_dir().join(format!("regulus-node-refuse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let folder: PathBuf = root.join("plain");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.to_str().unwrap();

    // Not a project: no `regulus.toml`, nothing written. No core is needed for that.
    let out = regulus(&home, &["node", "init", path, "--id", "caps"]);
    assert!(!out.status.success());
    assert!(
        text(&out).contains("regulus project open"),
        "{}",
        text(&out)
    );
    assert!(!folder.join("tools").exists());

    // An id that is not a plain name.
    std::fs::write(folder.join("regulus.toml"), "").unwrap();
    for bad in ["../up", "Caps", "a/b", "x@1"] {
        let out = regulus(&home, &["node", "init", path, "--id", bad]);
        assert!(!out.status.success(), "{bad}: {}", text(&out));
        assert!(text(&out).contains("node id"), "{}", text(&out));
    }
    assert!(!folder.join("tools").exists());
    assert_eq!(
        std::fs::read_to_string(folder.join("regulus.toml")).unwrap(),
        ""
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// The value after `key` on the line of `text` that starts with it (`revision  <rev>`).
fn field(text: &str, key: &str) -> String {
    text.lines()
        .find_map(|l| l.strip_prefix(key))
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("no `{key}` line in:\n{text}"))
        .to_string()
}

/// The authoring commands against a real core (docs/18-custom-nodes.md, RUN/API.md): read
/// the source, check it without running it, save it with a revision (a stale one is
/// refused), reload without restarting the core, read the shape, find the ports a profile
/// kept stale and sync them; live executions and wires that would be dropped need an
/// explicit word.
#[test]
fn the_authoring_commands_drive_a_real_core() {
    if !python() {
        return;
    }
    with_core("node-authoring", PROJECT, |home, folder| {
        let path = folder.to_str().unwrap();
        let init = regulus(home, &["node", "init", path, "--id", "caps"]);
        assert!(init.status.success(), "{}", text(&init));
        let opened = regulus(home, &["project", "open", path, "--trust"]);
        assert!(opened.status.success(), "{}", text(&opened));
        let line = listed(home, "demo/caps@0.1.0").expect("demo/caps@0.1.0 listed");
        assert!(line.contains("idle"), "{line}");

        // Read: the text on stdout, where it lives and its revision on stderr.
        let source = regulus(home, &["node", "source", "demo", "caps"]);
        assert!(source.status.success(), "{}", text(&source));
        let original = String::from_utf8_lossy(&source.stdout).into_owned();
        assert!(original.contains("@rg.node("), "{original}");
        let first = field(&String::from_utf8_lossy(&source.stderr), "revision");

        // Check without running: the file on disk is fine; a broken text is an error at a
        // line and column, and the command exits non-zero.
        let clean = regulus(home, &["node", "check", "demo", "caps"]);
        assert!(clean.status.success(), "{}", text(&clean));
        assert!(text(&clean).contains("no errors"), "{}", text(&clean));
        let broken = folder.join("broken.py");
        std::fs::write(&broken, "def handle(:\n").unwrap();
        let bad = regulus(
            home,
            &[
                "node",
                "check",
                "demo",
                "caps",
                "--file",
                broken.to_str().unwrap(),
            ],
        );
        assert!(!bad.status.success(), "{}", text(&bad));
        assert!(
            text(&bad).contains("error[python.syntax]"),
            "{}",
            text(&bad)
        );

        // Save an edit that adds an output port. Saving applies nothing.
        let one = "outputs=[rg.then_out(\"out\", \"text@1\")],";
        let two = "outputs=[rg.then_out(\"out\", \"text@1\"), rg.then_out(\"extra\", \"text@1\")],";
        assert!(original.contains(one), "{original}");
        let edited = folder.join("edited.py");
        std::fs::write(&edited, original.replace(one, two)).unwrap();
        let edited_path = edited.to_str().unwrap();
        let saved = regulus(
            home,
            &[
                "node",
                "save",
                "demo",
                "caps",
                edited_path,
                "--revision",
                &first,
            ],
        );
        assert!(saved.status.success(), "{}", text(&saved));
        assert!(
            text(&saved).contains("saved tools/caps/run.py"),
            "{}",
            text(&saved)
        );
        assert!(text(&saved).contains("project reload"), "{}", text(&saved));
        // Without a base it does not guess; with the old revision it is refused and says so.
        let guess = regulus(home, &["node", "save", "demo", "caps", edited_path]);
        assert!(!guess.status.success());
        assert!(text(&guess).contains("--revision"), "{}", text(&guess));
        let stale = regulus(
            home,
            &[
                "node",
                "save",
                "demo",
                "caps",
                edited_path,
                "--revision",
                &first,
            ],
        );
        assert!(!stale.status.success(), "{}", text(&stale));
        assert!(
            text(&stale).contains("changed since you read it"),
            "{}",
            text(&stale)
        );
        let on_disk = std::fs::read_to_string(folder.join("tools/caps/run.py")).unwrap();
        assert!(on_disk.contains("\"extra\""), "the first save is on disk");
        let differs = regulus(home, &["node", "check", "demo", "caps"]);
        assert!(
            text(&differs).contains("differs from what is loaded"),
            "{}",
            text(&differs)
        );

        // Reload one module: the new process replaces the old one; the core stays.
        let reload = regulus(home, &["project", "reload", "demo", "--module", "caps"]);
        assert!(reload.status.success(), "{}", text(&reload));
        assert!(text(&reload).contains("replaced"), "{}", text(&reload));
        let shape = regulus(home, &["module", "shape", "demo/caps", "caps"]);
        assert!(shape.status.success(), "{}", text(&shape));
        assert!(text(&shape).contains("extra"), "{}", text(&shape));

        // The profile kept the old ports: stale. The preview writes nothing; applying
        // saves, and the finding is gone.
        let profile_file = folder.join(".regulus/profiles/caps-pipe.toml");
        let before = std::fs::read_to_string(&profile_file).unwrap();
        let check = regulus(home, &["profile", "check", "caps-pipe"]);
        assert!(
            text(&check).contains("node.ports_stale"),
            "{}",
            text(&check)
        );
        let plan = regulus(home, &["profile", "sync", "caps-pipe"]);
        assert!(plan.status.success(), "{}", text(&plan));
        assert!(text(&plan).contains("+ out extra"), "{}", text(&plan));
        assert!(text(&plan).contains("nothing saved"), "{}", text(&plan));
        assert_eq!(std::fs::read_to_string(&profile_file).unwrap(), before);
        let applied = regulus(home, &["profile", "sync", "caps-pipe", "--apply"]);
        assert!(applied.status.success(), "{}", text(&applied));
        let check = regulus(home, &["profile", "check", "caps-pipe"]);
        assert!(
            !text(&check).contains("node.ports_stale"),
            "{}",
            text(&check)
        );
        let events = run_and_send(home, "caps-pipe", "hi");
        assert!(
            events.contains("\"> HI\""),
            "the synced profile still runs: {events}"
        );

        // A live execution keeps the module from being replaced, unless told to stop it.
        let current = regulus(home, &["node", "source", "demo", "caps"]);
        let second = field(&String::from_utf8_lossy(&current.stderr), "revision");
        let renamed = String::from_utf8_lossy(&current.stdout).replace(
            "rg.then_out(\"out\", \"text@1\"), ",
            "rg.then_out(\"result\", \"text@1\"), ",
        );
        std::fs::write(&edited, renamed).unwrap();
        let saved = regulus(
            home,
            &[
                "node",
                "save",
                "demo",
                "caps",
                edited_path,
                "--revision",
                &second,
            ],
        );
        assert!(saved.status.success(), "{}", text(&saved));
        let refused = regulus(home, &["project", "reload", "demo", "--module", "caps"]);
        assert!(!refused.status.success(), "{}", text(&refused));
        assert!(
            text(&refused).contains("live executions"),
            "{}",
            text(&refused)
        );
        assert!(
            text(&refused).contains("--stop-executions"),
            "{}",
            text(&refused)
        );
        let forced = regulus(
            home,
            &[
                "project",
                "reload",
                "demo",
                "--module",
                "caps",
                "--stop-executions",
            ],
        );
        assert!(forced.status.success(), "{}", text(&forced));
        assert!(text(&forced).contains("stopped"), "{}", text(&forced));

        // Removing the port a wire ends on: the plan lists the wire, and applying it
        // needs --drop-wires.
        let plan = regulus(home, &["profile", "sync", "caps-pipe"]);
        assert!(text(&plan).contains("would be removed"), "{}", text(&plan));
        let before = std::fs::read_to_string(&profile_file).unwrap();
        let refused = regulus(home, &["profile", "sync", "caps-pipe", "--apply"]);
        assert!(!refused.status.success(), "{}", text(&refused));
        assert!(
            text(&refused).contains("--drop-wires"),
            "{}",
            text(&refused)
        );
        assert_eq!(std::fs::read_to_string(&profile_file).unwrap(), before);
        let accepted = regulus(
            home,
            &["profile", "sync", "caps-pipe", "--apply", "--drop-wires"],
        );
        assert!(accepted.status.success(), "{}", text(&accepted));
        assert_ne!(std::fs::read_to_string(&profile_file).unwrap(), before);
    });
}
