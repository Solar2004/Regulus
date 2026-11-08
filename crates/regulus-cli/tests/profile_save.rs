//! Editing profiles end to end (docs/11-composition.md §7): through the real `regulus`
//! program and a real core in a temporary home, `profile save` gives back exactly what
//! `profile.get` returned, for the applied `greeting` template and the `ralph` example; a
//! changed copy runs with its new value; `profile rm` unloads it; `module show` prints a
//! manifest.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use regulus_core::{local, Config};
use serde_json::Value;

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");

fn ralph_example() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../modules/files/examples/ralph.toml")
}

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

fn get(home: &Path, id: &str) -> Value {
    let params = format!("{{\"id\":\"{id}\"}}");
    let out = regulus(home, &["call", "profile.get", &params]);
    assert!(out.status.success(), "{}", text(&out));
    serde_json::from_slice(&out.stdout).unwrap()
}

fn save_stdin(home: &Path, profile: &Value, force: bool) -> Output {
    let mut args = vec!["--home".to_string(), home.display().to_string()];
    args.extend(["profile", "save", "-"].map(String::from));
    if force {
        args.push("--force".into());
    }
    let mut child = Command::new(PROGRAM)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(profile.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

/// `profile.get` → `profile save` → `profile.get` is the same profile.
fn round_trip(home: &Path, id: &str) {
    let before = get(home, id);
    let file = home.join(format!("{id}.json"));
    std::fs::write(&file, before.to_string()).unwrap();
    let file = file.display().to_string();
    let refused = regulus(home, &["profile", "save", &file]);
    assert!(!refused.status.success());
    assert!(
        text(&refused).contains("already exists"),
        "{}",
        text(&refused)
    );
    let saved = regulus(home, &["profile", "save", &file, "--force"]);
    assert!(saved.status.success(), "{}", text(&saved));
    assert!(
        text(&saved).contains(&format!("profiles/{id}.toml")),
        "{}",
        text(&saved)
    );
    assert_eq!(get(home, id), before, "{id} changed through save");
}

fn steps(home: &Path) {
    let show = regulus(home, &["module", "show", "regulus/flow"]);
    assert!(show.status.success(), "{}", text(&show));
    assert!(text(&show).contains("[module]"), "{}", text(&show));
    assert!(text(&show).contains("type = \"call\""), "{}", text(&show));

    let applied = regulus(home, &["template", "apply", "greeting", "--set", "who=Ana"]);
    assert!(applied.status.success(), "{}", text(&applied));
    round_trip(home, "greeting");
    round_trip(home, "ralph");

    let mut changed = get(home, "greeting");
    changed["workflows"][0]["nodes"][0]["config"]["name"] = "Bo".into();
    let refused = save_stdin(home, &changed, false);
    assert!(!refused.status.success());
    let saved = save_stdin(home, &changed, true);
    assert!(saved.status.success(), "{}", text(&saved));
    let run = regulus(home, &["run", "greeting"]);
    assert!(run.status.success(), "{}", text(&run));
    assert!(text(&run).contains("Hello, Bo!"), "{}", text(&run));

    let stopped = regulus(home, &["stop", "exec-1"]);
    assert!(stopped.status.success(), "{}", text(&stopped));
    let removed = regulus(home, &["profile", "rm", "greeting"]);
    assert!(removed.status.success(), "{}", text(&removed));
    assert!(!home.join("profiles/greeting.toml").exists());
    let profiles = regulus(home, &["profile", "ls"]);
    assert!(!text(&profiles).contains("greeting"), "{}", text(&profiles));
    assert!(text(&profiles).contains("ralph"), "{}", text(&profiles));
}

#[test]
fn profiles_save_round_trip_run_and_are_removed() {
    let home = std::env::temp_dir().join(format!("regulus-profile-save-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("profiles")).unwrap();
    std::fs::copy(ralph_example(), home.join("profiles/ralph.toml")).unwrap();
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
