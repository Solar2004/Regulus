//! The repository's `templates/research.toml` (docs/13-blocks.md §5 row 7), applied
//! through the real `regulus` program and a real core in a temporary home, in both of
//! its shapes (judge, and the free stand-in): each writes a profile that `profile.check`
//! accepts with no diagnostic at all, every node of it is wired and the key wires are
//! there, and it refuses to apply without its required var. No agent runs here: that
//! needs opencode (the run is in the header).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regulus_core::{local, Config};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const TEMPLATE: &str = "research";

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
    std::fs::create_dir_all(home.join("work")).unwrap();
    let work = format!("workdir={}", home.join("work").display());

    let ls = regulus(home, &["template", "ls"]);
    assert!(ls.status.success(), "{}", text(&ls));
    assert!(text(&ls).contains("[needs: workdir]"), "{}", text(&ls));

    let missing = regulus(home, &["template", "apply", TEMPLATE]);
    assert!(!missing.status.success());
    assert!(
        text(&missing).contains("template `research` needs a value for var `workdir`"),
        "{}",
        text(&missing)
    );
    assert!(!home.join("profiles/research.toml").exists());

    // The judge by default: the quality leaf is `criteria`, the provider has a key.
    let applied = regulus(home, &["template", "apply", TEMPLATE, "--set", &work]);
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
    assert!(
        copy.contains(r#""grep -q http < $0", "{{input}}"]"#),
        "{copy}"
    );
    assert_eq!(
        copy.matches(r#"permissions = "allow""#).count(),
        4,
        "{copy}"
    );
    assert!(copy.contains("count = 3"), "{copy}");
    wired(&copy);
    clean(home);

    // The free stand-in: the quality leaf becomes a gate, the provider has no key.
    let stand_in = regulus(
        home,
        &[
            "template",
            "apply",
            TEMPLATE,
            "--force",
            "--set",
            &work,
            "--set",
            "quality_by=gate",
            "--set",
            "judge_credential=",
            "--set",
            "questions=2",
            "--set",
            "permissions=reject",
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
    assert!(copy.contains("max_items = 2"), "{copy}");
    assert_eq!(
        copy.matches(r#"permissions = "reject""#).count(),
        4,
        "{copy}"
    );
    wired(&copy);
    clean(home);
}

/// Every node of both workflows has a wire, and the stages are chained.
fn wired(copy: &str) {
    assert_wired(
        copy,
        &[
            ("workspace.ready", "experimenter.workdir"),
            ("workspace.ready", "writer.workdir"),
            ("orchestrator.reply", "split.in"),
            ("fanout.out", "lines.in"),
            ("lines.other", "budget.in"),
            ("budget.out", "call.in"),
            ("call.out", "gather.in"),
            ("gather.out", "notes.in"),
            ("notes_write.out", "workspace.write"),
            ("workspace.written", "exp_prompt.in"),
            ("workspace.quota_exceeded", "quota_text.in"),
            ("quota_text.out", "self.error"),
            ("notes_path.value", "job.in"),
            ("job.done", "exp_code.in"),
            ("exp_json.out", "rubric.gate"),
            ("writer_join.out", "writer_prompt.in"),
            ("writer.reply", "report.in"),
            ("report.written", "report_gate.in"),
            ("report_gate.exit", "rc_code.in"),
            ("rc_json.out", "rubric.gate"),
            ("report_text.value", "rubric.in"),
            ("judge_provider.endpoint", "rubric.model"),
            ("rubric.pass", "self.passed"),
            ("source.out", "join.source"),
            ("researcher.reply", "self.out"),
        ],
    );
}

/// `profile.check` on the applied copy says nothing at all.
fn clean(home: &Path) {
    let check = regulus(home, &["call", "profile.check", r#"{"id":"research"}"#]);
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(
        String::from_utf8_lossy(&check.stdout).trim(),
        "[]",
        "profile.check: {}",
        text(&check)
    );
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
