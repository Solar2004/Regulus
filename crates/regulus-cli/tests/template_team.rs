//! The repository's `templates/team.toml` (docs/13-blocks.md §5 row 6), applied through
//! the real `regulus` program and a real core in a temporary home: it writes a profile
//! that `profile.check` accepts with no diagnostic at all, every node of it is wired and
//! the key wires are there, and it refuses to apply without each of its required vars.
//! No agent runs here: that needs opencode (the run is in the header).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regulus_core::{local, Config};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const TEMPLATE: &str = "team";

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

/// The wires of every workflow in `copy`, as `("node.port", "node.port")`, and the nodes
/// no wire touches, as `"workflow/node"`.
fn topology(copy: &str) -> (Vec<(String, String)>, Vec<String>) {
    let profile: toml::Value = toml::from_str(copy).unwrap();
    let end = |end: &toml::Value| {
        format!(
            "{}.{}",
            end["node_id"].as_str().unwrap(),
            end["port_id"].as_str().unwrap()
        )
    };
    let mut wires = Vec::new();
    let mut lonely = Vec::new();
    for workflow in profile["workflows"].as_array().unwrap() {
        let mine: Vec<(String, String)> = workflow["wires"]
            .as_array()
            .unwrap()
            .iter()
            .map(|wire| (end(&wire["from"]), end(&wire["to"])))
            .collect();
        for node in workflow["nodes"].as_array().unwrap() {
            let id = node["id"].as_str().unwrap();
            let prefix = format!("{id}.");
            if !mine
                .iter()
                .any(|(from, to)| from.starts_with(&prefix) || to.starts_with(&prefix))
            {
                lonely.push(format!("{}/{id}", workflow["id"].as_str().unwrap()));
            }
        }
        wires.extend(mine);
    }
    (wires, lonely)
}

fn assert_wired(copy: &str, expected: &[(&str, &str)]) {
    let (wires, lonely) = topology(copy);
    assert!(lonely.is_empty(), "nodes without a wire: {lonely:?}");
    for (from, to) in expected {
        assert!(
            wires.iter().any(|(f, t)| f == from && t == to),
            "no wire {from} → {to}"
        );
    }
}

fn steps(home: &Path) {
    for dir in ["repo", "work"] {
        std::fs::create_dir_all(home.join(dir)).unwrap();
    }
    let repo = format!("repo={}", home.join("repo").display());
    let work = format!("workdir={}", home.join("work").display());
    let tests = "test_command=cargo test".to_string();

    let ls = regulus(home, &["template", "ls"]);
    assert!(ls.status.success(), "{}", text(&ls));
    assert!(
        text(&ls).contains("[needs: repo, test_command, workdir]"),
        "{}",
        text(&ls)
    );

    let required = [&repo, &work, &tests];
    for (left_out, name) in ["`repo`", "`workdir`", "`test_command`"].iter().enumerate() {
        let mut args = vec!["template", "apply", TEMPLATE];
        for (i, set) in required.iter().enumerate() {
            if i != left_out {
                args.extend(["--set", set.as_str()]);
            }
        }
        let out = regulus(home, &args);
        assert!(!out.status.success());
        let wanted = format!("template `team` needs a value for var {name}");
        assert!(text(&out).contains(&wanted), "{}", text(&out));
    }
    assert!(!home.join("profiles/team.toml").exists());

    let applied = regulus(
        home,
        &[
            "template",
            "apply",
            TEMPLATE,
            "--set",
            &repo,
            "--set",
            &work,
            "--set",
            &tests,
            "--set",
            "queue=check",
        ],
    );
    assert!(applied.status.success(), "{}", text(&applied));
    let copy = std::fs::read_to_string(home.join("profiles/team.toml")).unwrap();
    assert!(!copy.contains("\n[template"), "{copy}");
    assert!(copy.contains(r#"queue = "check""#), "{copy}");
    assert!(
        copy.contains(r#"model = "opencode/glm-5.3-flash-free""#),
        "{copy}"
    );
    assert_eq!(
        copy.matches(r#"permissions = "allow""#).count(),
        3,
        "{copy}"
    );
    let root = format!(r#"root = "{}/polecat-2""#, home.join("work").display());
    assert!(copy.contains(&root), "{copy}");
    // The gate keeps its own `{{input}}` and runs the test line in the worktree; the
    // switches keep their JSON Pointers.
    assert!(copy.contains(r#"&& cargo test", "{{input}}"]"#), "{copy}");
    assert!(copy.contains(r#"path = "/merged/branch""#), "{copy}");

    assert_wired(
        &copy,
        &[
            ("ws_1.ready", "polecat_1.workdir"),
            ("ws_2.ready", "polecat_2.workdir"),
            ("mayor.reply", "queue.add"),
            ("claim_1.task", "ws_1.create"),
            ("claim_1.task", "join_1.task"),
            ("ws_1.ready", "join_1.path"),
            ("info_1.value", "refinery.in"),
            ("refinery.check", "check_path.in"),
            ("gate.pass", "refinery.pass"),
            ("refinery.merged", "route_merged.in"),
            ("route_merged.polecat_2", "claim_2.complete"),
            ("route_failed.polecat_1", "claim_1.fail"),
            ("witness_1.stuck", "self.stuck"),
            ("ws_1.error", "ws_failed_1.in"),
            ("ws_failed_2.out", "claim_2.fail"),
            ("queue.error", "self.error"),
            ("refinery.error", "self.error"),
        ],
    );

    let check = regulus(home, &["call", "profile.check", r#"{"id":"team"}"#]);
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(
        String::from_utf8_lossy(&check.stdout).trim(),
        "[]",
        "profile.check: {}",
        text(&check)
    );
}

#[test]
fn the_team_template_applies_and_checks() {
    let home = std::env::temp_dir().join(format!("regulus-template-team-{}", std::process::id()));
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
