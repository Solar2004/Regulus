//! Local modules end to end (docs/03-modules.md §6, docs/11-composition.md §13): a project
//! declares the Python `echo` example as `[[modules.local]]`; the core runs it as a
//! process, lists it as `<project>/echo`, runs a profile through it, and stops the process
//! when the project closes. A module whose process dies at once is broken, with its stderr,
//! and nothing else is.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regulus_core::{local, Config};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
/// Marks the processes of one test run, to find them in `/proc`.
const MARK: &str = "REGULUS_LOCAL_MODULE_TEST";

fn example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/local-modules/echo/echo.py")
        .canonicalize()
        .unwrap()
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

const PROFILE: &str = r#"
id = "pipe"
name = "Echo then shout"
main_workflow_id = "main"

[[workflows]]
id = "main"
name = "Pipe"
kind = "main"
ports = [
  { id = "in", capability = "text@1", direction = "input", kind = "then" },
  { id = "out", capability = "text@1", direction = "output", kind = "then" },
]

  [[workflows.nodes]]
  id = "e"
  name = "Echo"
  type = "echo"
  module_id = "demo/echo@0.1.0"
  ports = [
    { id = "in", capability = "text@1", direction = "input", kind = "then", cardinality = "many" },
    { id = "out", capability = "text@1", direction = "output", kind = "then" },
  ]

  [[workflows.nodes]]
  id = "s"
  name = "Shout"
  type = "shout"
  module_id = "demo/echo"
  ports = [
    { id = "in", capability = "text@1", direction = "input", kind = "then", cardinality = "many" },
    { id = "out", capability = "text@1", direction = "output", kind = "then" },
  ]

  [[workflows.wires]]
  id = "w1"
  kind = "then"
  from = { node_id = "self", port_id = "in" }
  to = { node_id = "e", port_id = "in" }

  [[workflows.wires]]
  id = "w2"
  kind = "then"
  from = { node_id = "e", port_id = "out" }
  to = { node_id = "s", port_id = "in" }

  [[workflows.wires]]
  id = "w3"
  kind = "then"
  from = { node_id = "s", port_id = "out" }
  to = { node_id = "self", port_id = "out" }
"#;

/// A temporary home and project folder, and a core serving them, for `test`.
fn with_core(name: &str, project_file: &str, test: impl FnOnce(&Path, &Path)) {
    let root = std::env::temp_dir().join(format!("regulus-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let folder = root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(folder.join(".regulus/profiles")).unwrap();
    std::fs::write(home.join("config.toml"), "[server]\nport = 0\n").unwrap();
    std::fs::write(folder.join("regulus.toml"), project_file).unwrap();
    std::fs::write(folder.join(".regulus/profiles/pipe.toml"), PROFILE).unwrap();
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

#[test]
fn a_local_module_runs_a_profile_and_stops_with_its_project() {
    let mark = format!("echo-{}", std::process::id());
    let file = format!(
        "[[modules.local]]\nid = \"echo\"\ncommand = [\"python3\", {:?}]\nenv = {{ {MARK} = {mark:?} }}\n",
        example().display().to_string()
    );
    with_core("local-echo", &file, |home, folder| {
        let opened = regulus(home, &["project", "open", folder.to_str().unwrap()]);
        assert!(opened.status.success(), "{}", text(&opened));
        assert_eq!(marked(&mark).len(), 1, "one echo process runs");

        let ls = regulus(home, &["module", "ls"]);
        let line = text(&ls)
            .lines()
            .find(|l| l.starts_with("demo/echo@0.1.0"))
            .map(str::to_string);
        let line = line.unwrap_or_else(|| panic!("demo/echo listed: {}", text(&ls)));
        assert!(line.contains("idle"), "{line}");

        let show = regulus(home, &["module", "show", "demo/echo"]);
        assert!(text(&show).contains("type = \"shout\""), "{}", text(&show));

        let run = regulus(home, &["run", "pipe", "--detach"]);
        assert!(run.status.success(), "{}", text(&run));
        let execution = text(&run).split_whitespace().next().unwrap().to_string();
        assert!(text(&run).contains("running"), "{}", text(&run));

        let sent = regulus(home, &["send", &execution, "in", "hello there"]);
        let events = text(&sent);
        assert!(sent.status.success(), "{events}");
        assert!(
            events.contains("e.out emitted text@1 \"hello there\""),
            "{events}"
        );
        assert!(
            events.contains("s.out emitted text@1 \"HELLO THERE\""),
            "{events}"
        );
        assert!(
            events.contains("self.out emitted text@1 \"HELLO THERE\""),
            "{events}"
        );
        assert!(!events.contains("failed"), "{events}");

        // Vetoed like any module, by its id without version.
        let file = std::fs::read_to_string(folder.join("regulus.toml")).unwrap();
        std::fs::write(
            folder.join("regulus.toml"),
            format!("{file}\n[modules]\ndisable = [\"demo/echo\"]\n"),
        )
        .unwrap();
        let refused = regulus(home, &["run", "pipe"]);
        assert!(!refused.status.success());
        assert!(
            text(&refused).contains("disabled in project demo"),
            "{}",
            text(&refused)
        );

        let closed = regulus(home, &["project", "close", "demo"]);
        assert!(closed.status.success(), "{}", text(&closed));
        assert!(marked(&mark).is_empty(), "the echo process is gone");
        let ls = regulus(home, &["module", "ls"]);
        assert!(ls.status.success(), "{}", text(&ls));
        assert!(!text(&ls).contains("demo/echo"), "{}", text(&ls));
    });
}

#[test]
fn a_local_module_that_dies_is_broken_with_its_stderr_and_the_rest_works() {
    let file = format!(
        "[[modules.local]]\nid = \"crash\"\ncommand = [\"sh\", \"-c\", \"echo boom >&2; exit 3\"]\n\n\
         [[modules.local]]\nid = \"echo\"\ncommand = [\"python3\", {:?}]\n",
        example().display().to_string()
    );
    with_core("local-crash", &file, |home, folder| {
        let opened = regulus(home, &["project", "open", folder.to_str().unwrap()]);
        assert!(opened.status.success(), "{}", text(&opened));

        let ls = regulus(home, &["module", "ls"]);
        assert!(ls.status.success(), "{}", text(&ls));
        let listed = text(&ls);
        let crash = listed
            .lines()
            .find(|l| l.starts_with("demo/crash"))
            .unwrap_or_else(|| panic!("demo/crash listed: {listed}"));
        assert!(crash.contains("broken"), "{crash}");
        assert!(crash.contains("boom"), "{crash}");
        let echo = listed
            .lines()
            .find(|l| l.starts_with("demo/echo"))
            .unwrap_or_else(|| panic!("demo/echo listed: {listed}"));
        assert!(echo.contains("idle"), "{echo}");
        assert!(
            listed
                .lines()
                .any(|l| l.starts_with("regulus/hello") && l.contains("idle")),
            "{listed}"
        );

        let run = regulus(home, &["run", "pipe", "--detach"]);
        assert!(run.status.success(), "{}", text(&run));
    });
}
