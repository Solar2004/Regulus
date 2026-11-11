//! First run end to end: a fresh home with the default template catalog is seeded by
//! `regulus serve` with the templates the host ships, as ordinary editable files; what the
//! person already changed is kept, and a catalog they chose is not touched.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regulus_core::{local, Config};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
/// Templates every host ships (`src/bundled.rs`).
const SHIPPED: &[&str] = &[
    "best-of-n",
    "chat-cli",
    "delegation",
    "evaluator-optimizer",
    "greeting",
    "native-harness",
    "pxl",
    "rag-chat",
    "rag-eval",
    "rag-lab",
    "ralph",
    "regulus-code",
    "research",
    "research-evidence",
    "sdd",
    "team",
    "topic-watch",
];

fn regulus(home: &Path, args: &[&str]) -> Output {
    Command::new(PROGRAM)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap()
}

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
            let _ = Command::new("kill")
                .arg(entry.file_name().to_string_lossy().as_ref())
                .status();
        }
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
}

fn fresh(name: &str, config: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("regulus-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.toml"), config).unwrap();
    (root, home)
}

#[test]
fn a_fresh_home_gets_the_shipped_templates_and_keeps_what_was_edited() {
    let (root, home) = fresh("seed-fresh", "[server]\nport = 0\n");
    local::start(&Config::load(&home).unwrap(), Path::new(PROGRAM)).unwrap();
    let ls = regulus(&home, &["template", "ls"]);
    let listing = String::from_utf8_lossy(&ls.stdout).into_owned();
    for name in SHIPPED {
        assert!(listing.contains(name), "{name} in:\n{listing}");
        assert!(home.join(format!("templates/{name}.toml")).is_file());
    }
    // They are ordinary files: an edit survives the next start.
    let edited = home.join("templates/research-evidence.toml");
    std::fs::write(
        &edited,
        std::fs::read_to_string(&edited).unwrap() + "\n# mine\n",
    )
    .unwrap();
    // A shipped template the person deleted does not come back.
    let deleted = home.join("templates/topic-watch.toml");
    std::fs::remove_file(&deleted).unwrap();
    assert!(
        std::fs::read_to_string(home.join("templates/.regulus-seeded"))
            .unwrap()
            .contains("topic-watch.toml")
    );
    stop_cores(&home);
    local::start(&Config::load(&home).unwrap(), Path::new(PROGRAM)).unwrap();
    assert!(!deleted.exists());
    assert!(std::fs::read_to_string(&edited)
        .unwrap()
        .ends_with("# mine\n"));
    stop_cores(&home);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn a_catalog_the_person_chose_is_not_seeded() {
    let (root, home) = fresh(
        "seed-custom",
        "[server]\nport = 0\n\n[templates]\npaths = [\"mine\"]\n",
    );
    std::fs::create_dir_all(home.join("mine")).unwrap();
    local::start(&Config::load(&home).unwrap(), Path::new(PROGRAM)).unwrap();
    assert!(!home.join("templates").exists());
    assert_eq!(std::fs::read_dir(home.join("mine")).unwrap().count(), 0);
    stop_cores(&home);
    let _ = std::fs::remove_dir_all(root);
}
