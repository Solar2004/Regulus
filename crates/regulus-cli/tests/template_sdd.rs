//! The SDD template (`templates/sdd.toml`, docs/13-blocks.md §5 row 3), applied through
//! the real `regulus` program and a real core in a temporary home: it writes a profile
//! that `profile.check` finds no error in, and it refuses to apply without `workdir`.
//! Nothing runs: its agents need opencode, so the real run is in the template's header.

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

/// The diagnostics of `profile.check` for `id`: none at all, warnings included.
fn assert_check_is_clean(home: &Path, id: &str) {
    let params = format!(r#"{{"id":"{id}"}}"#);
    let check = regulus(home, &["call", "profile.check", &params]);
    assert!(check.status.success(), "{}", text(&check));
    let diagnostics: serde_json::Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(diagnostics, serde_json::json!([]), "{diagnostics}");
}

/// The written profile keeps its topology: each `(workflow, wire)` exists, and every node
/// of every workflow but those in `idle` has at least one wire.
fn assert_topology(copy: &str, wires: &[(&str, &str)], idle: &[&str]) {
    let profile: toml::Value = toml::from_str(copy).unwrap();
    let workflows = profile["workflows"].as_array().unwrap();
    for (workflow, wire) in wires {
        let found = workflows
            .iter()
            .filter(|w| w["id"].as_str() == Some(*workflow))
            .flat_map(|w| w["wires"].as_array().unwrap())
            .any(|w| w["id"].as_str() == Some(*wire));
        assert!(found, "no wire `{wire}` in workflow `{workflow}`");
    }
    for workflow in workflows {
        let wired: Vec<&str> = workflow["wires"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|w| [&w["from"]["node_id"], &w["to"]["node_id"]])
            .filter_map(|n| n.as_str())
            .collect();
        for node in workflow["nodes"].as_array().unwrap() {
            let id = node["id"].as_str().unwrap();
            if idle.contains(&id) {
                continue;
            }
            assert!(
                wired.contains(&id),
                "node `{id}` of workflow `{}` has no wire",
                workflow["id"].as_str().unwrap()
            );
        }
    }
}

fn steps(home: &Path) {
    let missing = regulus(home, &["template", "apply", "sdd"]);
    assert!(!missing.status.success());
    assert!(text(&missing).contains("`workdir`"), "{}", text(&missing));
    assert!(!home.join("profiles/sdd.toml").exists());

    let project = home.join("project");
    let workdir = format!("workdir={}", project.display());
    let applied = regulus(
        home,
        &[
            "template",
            "apply",
            "sdd",
            "--set",
            &workdir,
            "--set",
            "test_command=test -f hello.txt",
            "--set",
            "max_turns=2",
        ],
    );
    assert!(applied.status.success(), "{}", text(&applied));
    let written = home.join("profiles/sdd.toml");
    let copy = std::fs::read_to_string(&written).unwrap();
    assert!(
        !copy.lines().any(|line| line.starts_with("[template")),
        "{copy}"
    );
    assert!(copy.contains("test -f hello.txt"), "{copy}");
    assert!(
        copy.contains(r#"model = "opencode/glm-5.3-flash-free""#),
        "{copy}"
    );
    assert!(copy.contains(r#"path = "specs/tasks.md""#), "{copy}");
    assert!(copy.contains("{{/tasks}}"), "{copy}");

    assert_topology(
        &copy,
        &[
            ("sdd", "gate-back"),
            ("sdd", "agent-back"),
            ("sdd", "tasks-start"),
            ("persona", "agent-engine"),
            ("persona", "agent-out"),
        ],
        &[],
    );
    assert_check_is_clean(home, "sdd");
}

#[test]
fn the_sdd_template_applies_and_checks() {
    let home = std::env::temp_dir().join(format!("regulus-template-sdd-{}", std::process::id()));
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
