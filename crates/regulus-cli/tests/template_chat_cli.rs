//! The chat CLI template (`templates/chat-cli.toml`, docs/13-blocks.md §5 row 8), applied
//! through the real `regulus` program and a real core in a temporary home, with each of
//! its two routers: both profiles pass `profile.check` without errors, and it refuses to
//! apply without `workdir`. Nothing runs: its agents need opencode, so the real run is in
//! the template's header.

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

/// The router the join feeds in the written profile: what the hole `router` chose.
fn router_of(copy: &str) -> String {
    let profile: toml::Value = toml::from_str(copy).unwrap();
    let wires = profile["workflows"][0]["wires"].as_array().unwrap();
    let wire = wires
        .iter()
        .find(|w| w["id"].as_str() == Some("join-router"))
        .unwrap();
    wire["to"]["node_id"].as_str().unwrap().to_owned()
}

fn check_has_no_errors(home: &Path, id: &str) {
    let params = format!(r#"{{"id":"{id}"}}"#);
    let check = regulus(home, &["call", "profile.check", &params]);
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

fn steps(home: &Path) {
    let missing = regulus(home, &["template", "apply", "chat-cli"]);
    assert!(!missing.status.success());
    assert!(text(&missing).contains("`workdir`"), "{}", text(&missing));
    assert!(!home.join("profiles/chat-cli.toml").exists());

    let workdir = format!("workdir={}", home.join("project").display());
    let applied = regulus(home, &["template", "apply", "chat-cli", "--set", &workdir]);
    assert!(applied.status.success(), "{}", text(&applied));
    let copy = std::fs::read_to_string(home.join("profiles/chat-cli.toml")).unwrap();
    assert!(
        !copy.lines().any(|line| line.starts_with("[template")),
        "{copy}"
    );
    assert_eq!(router_of(&copy), "supervisor");
    assert!(copy.contains(r#"preset = "ollama""#), "{copy}");
    assert!(copy.contains(r#"profile = "isolated""#), "{copy}");
    assert!(copy.contains("{{/message}}"), "{copy}");
    check_has_no_errors(home, "chat-cli");

    let keywords = regulus(
        home,
        &[
            "template",
            "apply",
            "chat-cli",
            "--set",
            &workdir,
            "--set",
            "router=keywords",
            "--id",
            "chat-kw",
        ],
    );
    assert!(keywords.status.success(), "{}", text(&keywords));
    let copy = std::fs::read_to_string(home.join("profiles/chat-kw.toml")).unwrap();
    assert_eq!(router_of(&copy), "keywords");
    check_has_no_errors(home, "chat-kw");
}

#[test]
fn the_chat_cli_template_applies_and_checks() {
    let home =
        std::env::temp_dir().join(format!("regulus-template-chat-cli-{}", std::process::id()));
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
