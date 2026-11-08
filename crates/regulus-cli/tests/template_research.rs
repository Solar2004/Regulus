//! The repository's `templates/research.toml` (docs/13-blocks.md §5 row 7), applied through
//! the real `regulus` program and a real core in a temporary home: the profile it writes
//! checks with no errors, and a missing required var is refused by name. No agent runs
//! here (that needs opencode); the real run is in the template's header.

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
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let work_var = format!("workdir={}", work.display());

    let ls = regulus(home, &["template", "ls"]);
    assert!(text(&ls).contains("[needs: workdir]"), "{}", text(&ls));

    let missing = regulus(home, &["template", "apply", "research"]);
    assert!(!missing.status.success());
    assert!(
        text(&missing).contains("template `research` needs a value for var `workdir`"),
        "{}",
        text(&missing)
    );
    assert!(!home.join("profiles/research.toml").exists());

    // The judge by default: the quality leaf is `criteria`, the provider has a key.
    let applied = regulus(home, &["template", "apply", "research", "--set", &work_var]);
    assert!(applied.status.success(), "{}", text(&applied));
    let copy = std::fs::read_to_string(home.join("profiles/research.toml")).unwrap();
    assert!(!copy.contains("\n[template"), "{copy}");
    assert!(
        copy.contains(r#"credential = "secret:openrouter""#),
        "{copy}"
    );
    assert!(
        copy.contains(r#"{ id = "quality", weight = 2, criteria = "#),
        "{copy}"
    );
    assert!(copy.contains(r#"workflow = "investigate""#), "{copy}");
    assert!(copy.contains("srsearch={{input}}"), "{copy}");
    no_errors(home);

    // The free stand-in: the quality leaf becomes a gate, the provider has no key.
    let stand_in = regulus(
        home,
        &[
            "template",
            "apply",
            "research",
            "--force",
            "--set",
            &work_var,
            "--set",
            "quality_by=gate",
            "--set",
            "judge_credential=",
            "--set",
            "questions=2",
        ],
    );
    assert!(stand_in.status.success(), "{}", text(&stand_in));
    let copy = std::fs::read_to_string(home.join("profiles/research.toml")).unwrap();
    assert!(
        copy.contains(r#"{ id = "quality", weight = 2, gate = "#),
        "{copy}"
    );
    assert!(copy.contains(r#"credential = """#), "{copy}");
    assert!(copy.contains("count = 2"), "{copy}");
    no_errors(home);
}

/// `profile.check` on the applied copy finds no errors.
fn no_errors(home: &Path) {
    let check = regulus(home, &["call", "profile.check", r#"{"id":"research"}"#]);
    assert!(check.status.success(), "{}", text(&check));
    let diagnostics: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    let errors: Vec<_> = diagnostics
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["severity"] == "error")
        .collect();
    assert!(errors.is_empty(), "{diagnostics}");
}

#[test]
fn the_research_template_applies_and_checks() {
    let home =
        std::env::temp_dir().join(format!("regulus-template-research-{}", std::process::id()));
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
