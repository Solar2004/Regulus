//! The repository's `templates/best-of-n.toml` (docs/13-blocks.md §5 row 5),
//! applied through the real `regulus` program and a real core in a temporary home: it
//! writes a profile that `profile.check` accepts, and refuses to apply without its
//! required vars. No agent runs here: that needs opencode and a judge (the run is in
//! the header).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regulus_core::{local, Config};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const TEMPLATE: &str = "best-of-n";

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
    for dir in ["repo", "wt"] {
        std::fs::create_dir_all(home.join(dir)).unwrap();
    }
    let repo = format!("repo={}", home.join("repo").display());
    let worktrees = format!("worktrees={}", home.join("wt").display());

    let ls = regulus(home, &["template", "ls"]);
    assert!(ls.status.success(), "{}", text(&ls));
    assert!(
        text(&ls).contains("[needs: judge_model, repo, worktrees]"),
        "{}",
        text(&ls)
    );

    for (missing, sets) in [
        ("`judge_model`", [&repo, &worktrees]),
        ("`repo`", [&worktrees, &"judge_model=m".to_string()]),
        ("`worktrees`", [&repo, &"judge_model=m".to_string()]),
    ] {
        let out = regulus(
            home,
            &[
                "template", "apply", TEMPLATE, "--set", sets[0], "--set", sets[1],
            ],
        );
        assert!(!out.status.success());
        assert!(text(&out).contains(missing), "{}", text(&out));
    }
    assert!(!home.join("profiles/best-of-n.toml").exists());

    let applied = regulus(
        home,
        &[
            "template",
            "apply",
            TEMPLATE,
            "--set",
            &repo,
            "--set",
            &worktrees,
            "--set",
            "judge_model=qwen3:0.6b",
            "--set",
            "n=2",
        ],
    );
    assert!(applied.status.success(), "{}", text(&applied));
    let copy = std::fs::read_to_string(home.join("profiles/best-of-n.toml")).unwrap();
    assert!(copy.contains(r#"model = "qwen3:0.6b""#), "{copy}");
    assert!(
        copy.contains(r#"model = "opencode/glm-5.3-flash-free""#),
        "{copy}"
    );
    assert!(copy.contains("max = 2"), "{copy}");
    assert!(copy.contains("count = 2"), "{copy}");
    assert!(copy.contains(r#""sh", "{{input}}"]"#), "{copy}");
    assert!(copy.contains(r#"workflow = "attempt""#), "{copy}");
    assert!(!copy.contains("\n[template]"), "{copy}");
    assert!(!copy.contains("[template.vars]"), "{copy}");

    let check = regulus(home, &["call", "profile.check", r#"{"id":"best-of-n"}"#]);
    assert!(check.status.success(), "{}", text(&check));
    assert!(
        !text(&check).contains(r#""error""#),
        "profile.check found errors: {}",
        text(&check)
    );
}

#[test]
fn the_best_of_n_template_applies_and_checks() {
    let home = std::env::temp_dir().join(format!("regulus-template-bon-{}", std::process::id()));
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
