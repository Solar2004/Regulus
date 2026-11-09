//! The repository's `templates/delegation.toml` (docs/13-blocks.md §5 row 9), applied
//! through the real `regulus` program and a real core in a temporary home: it writes a
//! profile that `profile.check` accepts with no diagnostic at all, every node of it is
//! wired and the key wires are there, and it refuses to apply without `workdir`. No agent
//! runs here: that needs opencode; the delegate itself runs end to end with a fake agent
//! in `crates/regulus-acp/tests/delegation.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regulus_core::{local, Config};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const TEMPLATE: &str = "delegation";

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
    let work = format!("workdir={}", home.display());
    let ls = regulus(home, &["template", "ls"]);
    assert!(ls.status.success(), "{}", text(&ls));
    assert!(text(&ls).contains("[needs: workdir]"), "{}", text(&ls));

    let out = regulus(home, &["template", "apply", TEMPLATE]);
    assert!(!out.status.success());
    let wanted = "template `delegation` needs a value for var `workdir`";
    assert!(text(&out).contains(wanted), "{}", text(&out));
    assert!(!home.join("profiles/delegation.toml").exists());

    let applied = regulus(
        home,
        &[
            "template",
            "apply",
            TEMPLATE,
            "--set",
            &work,
            "--set",
            "max_workers=2",
        ],
    );
    assert!(applied.status.success(), "{}", text(&applied));
    let copy = std::fs::read_to_string(home.join("profiles/delegation.toml")).unwrap();
    assert!(!copy.contains("\n[template"), "{copy}");
    assert!(copy.contains("max_workers = 2 }"), "{copy}");
    assert!(copy.contains("At most 2 workers"), "{copy}");
    assert!(
        copy.contains(r#"workers = { worker = "worker" }"#),
        "{copy}"
    );
    assert_eq!(
        copy.matches(r#"permissions = "allow", turn_timeout_ms"#)
            .count(),
        2,
        "{copy}"
    );

    assert_wired(
        &copy,
        &[
            ("self.goal", "prompt.in"),
            ("prompt.out", "coordinator.prompt"),
            ("tools.toolset", "coordinator.tools"),
            ("team.tools", "tools.tools"),
            ("mcp.engine", "tools.engine"),
            ("opencode.engine", "team.engine"),
            ("self.tasks", "team.create"),
            ("team.result", "self.result"),
            ("self.engine", "worker.engine"),
            ("self.prompt", "worker.prompt"),
            ("worker.reply", "self.reply"),
        ],
    );

    let check = regulus(home, &["call", "profile.check", r#"{"id":"delegation"}"#]);
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(
        String::from_utf8_lossy(&check.stdout).trim(),
        "[]",
        "profile.check: {}",
        text(&check)
    );
}

#[test]
fn the_delegation_template_applies_and_checks() {
    let home = std::env::temp_dir().join(format!(
        "regulus-template-delegation-{}",
        std::process::id()
    ));
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
