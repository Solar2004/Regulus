//! Applying a template end to end (docs/11-composition.md §7): the repository's
//! `templates/greeting.toml`, through the real `regulus` program and a real core in a
//! temporary home, then running the profile it wrote.

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

fn steps(home: &Path) {
    let ls = regulus(home, &["template", "ls"]);
    assert!(ls.status.success(), "{}", text(&ls));
    assert!(text(&ls).contains("greeting"), "{}", text(&ls));
    assert!(text(&ls).contains("[needs: who]"), "{}", text(&ls));

    let missing = regulus(home, &["template", "apply", "greeting"]);
    assert!(!missing.status.success());
    assert!(text(&missing).contains("`who`"), "{}", text(&missing));

    let applied = regulus(home, &["template", "apply", "greeting", "--set", "who=Ana"]);
    assert!(applied.status.success(), "{}", text(&applied));
    let written = home.join("profiles/greeting.toml");
    assert!(
        text(&applied).contains("profile greeting"),
        "{}",
        text(&applied)
    );
    assert!(text(&applied).contains(&written.display().to_string()));
    let copy = std::fs::read_to_string(&written).unwrap();
    assert!(copy.contains(r#"name = "Ana""#), "{copy}");
    assert!(copy.contains(r#"path = "greeting.txt""#), "{copy}");
    assert!(!copy.contains("[template"), "{copy}");

    let again = regulus(home, &["template", "apply", "greeting", "--set", "who=Bo"]);
    assert!(!again.status.success());
    assert!(text(&again).contains("already exists"), "{}", text(&again));
    assert!(std::fs::read_to_string(&written).unwrap().contains("Ana"));

    let escape = regulus(
        home,
        &[
            "template", "apply", "greeting", "--set", "who=Bo", "--id", "../x",
        ],
    );
    assert!(!escape.status.success());
    assert!(!home.join("x.toml").exists());

    let profiles = regulus(home, &["profile", "ls"]);
    assert!(
        text(&profiles).contains("Greeting for Ana"),
        "{}",
        text(&profiles)
    );

    let run = regulus(home, &["run", "greeting"]);
    assert!(run.status.success(), "{}", text(&run));
    assert!(text(&run).contains("Hello, Ana!"), "{}", text(&run));
    assert!(
        text(&run).contains("note.written emitted"),
        "{}",
        text(&run)
    );
    assert!(
        text(&run).contains("ana.tick → note.in delivered"),
        "{}",
        text(&run)
    );

    let forced = regulus(
        home,
        &[
            "template",
            "apply",
            "greeting",
            "--set",
            "who=Bo",
            "--set",
            "file=bo.txt",
            "--force",
        ],
    );
    assert!(forced.status.success(), "{}", text(&forced));
    let profiles = regulus(home, &["profile", "ls"]);
    assert!(
        text(&profiles).contains("Greeting for Bo"),
        "{}",
        text(&profiles)
    );
}

#[test]
fn the_greeting_template_applies_and_runs() {
    let home = std::env::temp_dir().join(format!("regulus-templates-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let config = format!(
        "[server]\nport = 0\n\n[templates]\npaths = [{:?}]\n",
        templates_dir().display().to_string()
    );
    std::fs::write(home.join("config.toml"), config).unwrap();
    let config = Config::load(&home).unwrap();
    let started = local::start(&config, Path::new(PROGRAM));
    let result = std::panic::catch_unwind(|| {
        started.unwrap();
        steps(&home);
    });
    stop_cores(&home);
    let _ = std::fs::remove_dir_all(&home);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
