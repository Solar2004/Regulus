//! Projects end to end (docs/11-composition.md §13): open a folder, read its tolerant
//! `regulus.toml`, apply the repository's `greeting` template into it, veto a module, and
//! close it, through the real `regulus` program and a real core in a temporary home.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regulus_core::{local, Config};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");

fn templates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates")
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

fn steps(home: &Path, folder: &Path) {
    let opened = regulus(home, &["project", "open", folder.to_str().unwrap()]);
    assert!(opened.status.success(), "{}", text(&opened));
    assert!(text(&opened).contains("demo"), "{}", text(&opened));
    assert!(folder.join("regulus.toml").exists());

    let ls = regulus(home, &["project", "ls"]);
    assert!(text(&ls).contains("demo"), "{}", text(&ls));

    std::fs::write(
        folder.join("regulus.toml"),
        "[project]\nname = \"Demo\"\nfoo = \"bar\"\n[modules]\ndisable = [\"regulus/web\"]\n",
    )
    .unwrap();
    let show = regulus(home, &["project", "show", "demo"]);
    assert!(show.status.success(), "{}", text(&show));
    assert!(text(&show).contains("Demo"), "{}", text(&show));
    assert!(
        text(&show).contains("disable   regulus/web"),
        "{}",
        text(&show)
    );
    assert!(
        text(&show).contains("warning: regulus.toml: unknown key `project.foo`"),
        "{}",
        text(&show)
    );

    let applied = regulus(
        home,
        &[
            "template",
            "apply",
            "greeting",
            "--set",
            "who=Ana",
            "--project",
            "demo",
        ],
    );
    assert!(applied.status.success(), "{}", text(&applied));
    let written = folder.join(".regulus/profiles/greeting.toml");
    assert!(std::fs::read_to_string(&written)
        .unwrap()
        .contains("project_id = \"demo\""));

    let profiles = regulus(home, &["profile", "ls"]);
    assert!(
        text(&profiles).contains("Greeting for Ana  [project demo]"),
        "{}",
        text(&profiles)
    );

    let run = regulus(home, &["run", "greeting"]);
    assert!(run.status.success(), "{}", text(&run));
    assert!(text(&run).contains("Hello, Ana!"), "{}", text(&run));

    std::fs::write(
        folder.join("regulus.toml"),
        "[project]\nname = \"Demo\"\n[modules]\ndisable = [\"regulus/hello\"]\n",
    )
    .unwrap();
    let refused = regulus(home, &["run", "greeting"]);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("disabled in project demo"),
        "{}",
        text(&refused)
    );

    let closed = regulus(home, &["project", "close", "demo"]);
    assert!(closed.status.success(), "{}", text(&closed));
    let profiles = regulus(home, &["profile", "ls"]);
    assert!(!text(&profiles).contains("greeting"), "{}", text(&profiles));
    assert!(written.exists());
    let config = std::fs::read_to_string(home.join("config.toml")).unwrap();
    assert!(config.contains("[templates]"), "{config}");
    assert!(!config.contains("demo"), "{config}");
}

#[test]
fn a_project_opens_takes_a_template_vetoes_a_module_and_closes() {
    let root = std::env::temp_dir().join(format!("regulus-projects-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    let folder = root.join("demo");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&folder).unwrap();
    let config = format!(
        "[server]\nport = 0\n\n[templates]\npaths = [{:?}]\n",
        templates_dir().display().to_string()
    );
    std::fs::write(home.join("config.toml"), config).unwrap();
    let config = Config::load(&home).unwrap();
    let started = local::start(&config, Path::new(PROGRAM));
    let result = std::panic::catch_unwind(|| {
        started.unwrap();
        steps(&home, &folder);
    });
    stop_cores(&home);
    let _ = std::fs::remove_dir_all(&root);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
