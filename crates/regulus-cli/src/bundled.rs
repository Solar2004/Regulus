//! First-run installation of host-shipped templates. These remain ordinary editable
//! files; an existing file and an explicitly configured template catalog always win.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use regulus_core::Config;

/// The templates this host ships, as (file name, text): every file in `templates/`, so an
/// installed binary has what the repository has. A test fails when the two drift apart.
const TEMPLATES: &[(&str, &str)] = &[
    (
        "best-of-n.toml",
        include_str!("../../../templates/best-of-n.toml"),
    ),
    (
        "chat-cli.toml",
        include_str!("../../../templates/chat-cli.toml"),
    ),
    (
        "delegation.toml",
        include_str!("../../../templates/delegation.toml"),
    ),
    (
        "evaluator-optimizer.toml",
        include_str!("../../../templates/evaluator-optimizer.toml"),
    ),
    (
        "greeting.toml",
        include_str!("../../../templates/greeting.toml"),
    ),
    (
        "native-harness.toml",
        include_str!("../../../templates/native-harness.toml"),
    ),
    ("pxl.toml", include_str!("../../../templates/pxl.toml")),
    (
        "rag-chat.toml",
        include_str!("../../../templates/rag-chat.toml"),
    ),
    (
        "rag-eval.toml",
        include_str!("../../../templates/rag-eval.toml"),
    ),
    (
        "rag-lab.toml",
        include_str!("../../../templates/rag-lab.toml"),
    ),
    ("ralph.toml", include_str!("../../../templates/ralph.toml")),
    (
        "regulus-code.toml",
        include_str!("../../../templates/regulus-code.toml"),
    ),
    (
        "research-evidence.toml",
        include_str!("../../../templates/research-evidence.toml"),
    ),
    (
        "research.toml",
        include_str!("../../../templates/research.toml"),
    ),
    ("sdd.toml", include_str!("../../../templates/sdd.toml")),
    ("team.toml", include_str!("../../../templates/team.toml")),
    (
        "topic-watch.toml",
        include_str!("../../../templates/topic-watch.toml"),
    ),
];

/// The file of the catalog that remembers which shipped templates were seeded, one name per
/// line. A name in it is never seeded again, so a template the person deleted stays deleted;
/// and nothing shipped later overwrites a file: updates are not applied to existing copies.
const SEEDED: &str = ".regulus-seeded";

/// Seeds every shipped template into the default catalog, each name once. A write that
/// fails is reported and skipped: it never stops the core or the other templates.
pub(crate) fn seed(config: &Config) {
    seed_list(config, TEMPLATES);
}

fn seed_list(config: &Config, templates: &[(&str, &str)]) {
    let directory = config.home.join("templates");
    if !config.templates.paths.contains(&directory) {
        return;
    }
    let record = directory.join(SEEDED);
    let seeded = fs::read_to_string(&record).unwrap_or_default();
    for (name, text) in templates {
        if seeded.lines().any(|line| line == *name) {
            continue;
        }
        let outcome = install_in(&directory, name, text).and_then(|()| {
            let mut file = fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&record)?;
            writeln!(file, "{name}")
        });
        if let Err(error) = outcome {
            eprintln!("regulus: could not install bundled template {name}: {error}");
        }
    }
}

fn install_in(directory: &Path, name: &str, text: &str) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let destination = directory.join(name);
    if destination.symlink_metadata().is_ok() {
        return Ok(());
    }
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let temporary = directory.join(format!(".{name}.{}.{nonce}.tmp", std::process::id()));
    let outcome = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        match fs::hard_link(&temporary, &destination) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
            // A file system without hard links: write the destination itself, still
            // without ever replacing a file that is there.
            Err(_) => match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&destination)
            {
                Ok(mut file) => file.write_all(text.as_bytes()),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
                Err(error) => Err(error),
            },
        }
    })();
    let _ = fs::remove_file(temporary);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_install_is_editable_and_never_replaces_a_customized_template() {
        let directory = std::env::temp_dir().join(format!(
            "regulus-bundled-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut config = Config::load(&directory).unwrap();
        seed_list(&config, &[("example.toml", "bundled")]);
        let file = directory.join("templates/example.toml");
        assert_eq!(fs::read_to_string(&file).unwrap(), "bundled");
        fs::write(&file, "customized").unwrap();
        seed_list(&config, &[("example.toml", "updated bundle")]);
        assert_eq!(fs::read_to_string(&file).unwrap(), "customized");
        config.templates.paths = vec![directory.join("custom-catalog")];
        seed_list(&config, &[("other.toml", "not opted in")]);
        assert!(!directory.join("templates/other.toml").exists());
        assert_eq!(
            fs::read_dir(directory.join("templates")).unwrap().count(),
            2
        );
        fs::remove_dir_all(directory).unwrap();
    }

    /// Every file of `templates/` is shipped, byte for byte, and nothing else is: a new
    /// template that is not added to `TEMPLATES` never reaches an installed binary.
    #[test]
    fn the_shipped_list_is_exactly_the_templates_folder() {
        let folder = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates");
        let mut on_disk: Vec<String> = fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.ends_with(".toml"))
            .collect();
        on_disk.sort();
        let shipped: Vec<String> = TEMPLATES.iter().map(|(name, _)| name.to_string()).collect();
        let mut sorted = shipped.clone();
        sorted.sort();
        assert_eq!(sorted, shipped, "keep TEMPLATES sorted");
        assert_eq!(on_disk, shipped, "TEMPLATES and templates/ differ");
        for (name, text) in TEMPLATES {
            assert_eq!(
                &fs::read_to_string(folder.join(name)).unwrap(),
                text,
                "{name}"
            );
        }
    }

    #[test]
    fn seeding_installs_every_shipped_template_once_and_skips_a_custom_catalog() {
        let directory = std::env::temp_dir().join(format!("regulus-seed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let mut config = Config::load(&directory).unwrap();
        seed(&config);
        for (name, text) in TEMPLATES {
            let file = directory.join("templates").join(name);
            assert_eq!(&fs::read_to_string(&file).unwrap(), text, "{name}");
            assert!(text.contains("[template]"), "{name} declares a template");
        }
        fs::write(directory.join("templates/regulus-code.toml"), "mine").unwrap();
        seed(&config);
        assert_eq!(
            fs::read_to_string(directory.join("templates/regulus-code.toml")).unwrap(),
            "mine"
        );
        // A catalog the person chose is left alone, and an unwritable one is not fatal.
        let custom = std::env::temp_dir().join(format!("regulus-seed-c-{}", std::process::id()));
        let _ = fs::remove_dir_all(&custom);
        config.home = custom.clone();
        config.templates.paths = vec![custom.join("elsewhere")];
        seed(&config);
        assert!(!custom.join("templates").exists());
        fs::create_dir_all(&custom).unwrap();
        fs::write(custom.join("templates"), "a file, not a folder").unwrap();
        config.templates.paths = vec![custom.join("templates")];
        seed(&config);
        let _ = fs::remove_dir_all(&directory);
        let _ = fs::remove_dir_all(&custom);
    }

    #[test]
    fn a_deleted_template_stays_deleted_and_a_new_one_is_still_seeded() {
        let directory =
            std::env::temp_dir().join(format!("regulus-seed-once-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        let config = Config::load(&directory).unwrap();
        let templates = directory.join("templates");
        seed_list(&config, &[("a.toml", "a"), ("b.toml", "b")]);
        assert!(templates.join("a.toml").is_file() && templates.join("b.toml").is_file());
        fs::remove_file(templates.join("a.toml")).unwrap();
        fs::write(templates.join("b.toml"), "edited").unwrap();
        seed_list(
            &config,
            &[("a.toml", "a2"), ("b.toml", "b2"), ("c.toml", "c")],
        );
        assert!(!templates.join("a.toml").exists(), "deleted stays deleted");
        assert_eq!(
            fs::read_to_string(templates.join("b.toml")).unwrap(),
            "edited"
        );
        assert_eq!(fs::read_to_string(templates.join("c.toml")).unwrap(), "c");
        // An existing copy is marked seeded too, so deleting it later is respected.
        fs::write(templates.join("d.toml"), "mine").unwrap();
        seed_list(&config, &[("d.toml", "d")]);
        fs::remove_file(templates.join("d.toml")).unwrap();
        seed_list(&config, &[("d.toml", "d")]);
        assert!(!templates.join("d.toml").exists());
        let _ = fs::remove_dir_all(&directory);
    }
}
