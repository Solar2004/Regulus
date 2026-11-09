//! `regulus template save` and `regulus exec tree` through the real program and a real
//! core: a profile applied from `templates/greeting.toml` is saved back as a template of
//! its own, which lists and applies; a run shows up as a tree.

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

fn ok(home: &Path, args: &[&str]) -> String {
    let output = regulus(home, args);
    assert!(output.status.success(), "{args:?}: {}", text(&output));
    text(&output)
}

fn steps(home: &Path) {
    ok(home, &["template", "apply", "greeting", "--set", "who=Ana"]);
    let saved = ok(
        home,
        &[
            "template",
            "save",
            "greeting",
            "--name",
            "my-greeting",
            "--description",
            "Ana's greeting",
            "--var",
            "unused=not used yet=1",
        ],
    );
    let written = home.join("mine/my-greeting.toml");
    assert!(saved.contains(&written.display().to_string()), "{saved}");
    let again = regulus(
        home,
        &["template", "save", "greeting", "--name", "my-greeting"],
    );
    assert!(!again.status.success());
    assert!(text(&again).contains("already exists"), "{}", text(&again));
    let bad = regulus(
        home,
        &[
            "template", "save", "greeting", "--name", "x", "--var", "novalue",
        ],
    );
    assert!(!bad.status.success());

    let ls = ok(home, &["template", "ls"]);
    assert!(
        ls.contains("my-greeting") && ls.contains("Ana's greeting"),
        "{ls}"
    );
    let applied = ok(home, &["template", "apply", "my-greeting", "--id", "again"]);
    assert!(applied.contains("profile again"), "{applied}");

    ok(home, &["run", "again", "--detach"]);
    let tree = ok(home, &["exec", "tree", "exec-1"]);
    assert!(tree.starts_with("exec-1  "), "{tree}");
    let unknown = regulus(home, &["exec", "tree", "exec-99"]);
    assert!(!unknown.status.success());
    assert!(
        text(&unknown).contains("Unknown execution"),
        "{}",
        text(&unknown)
    );
}

#[test]
fn a_profile_is_saved_as_a_template_and_a_run_shows_as_a_tree() {
    let home = std::env::temp_dir().join(format!("regulus-template-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    // Saved templates go to the first path, never to the repository's.
    let config = format!(
        "[server]\nport = 0\n\n[templates]\npaths = [{:?}, {:?}]\n",
        home.join("mine").display().to_string(),
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
