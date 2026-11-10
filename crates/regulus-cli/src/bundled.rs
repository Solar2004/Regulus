//! First-run installation of host-shipped templates. These remain ordinary editable
//! files; an existing file and an explicitly configured template catalog always win.

use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use regulus_core::Config;

/// Install a bundled template only when the default catalog is enabled. Publishing a
/// complete temporary file with a hard link avoids exposing partial TOML or replacing
/// a file another core or the person created in the meantime.
pub(crate) fn install(config: &Config, name: &str, text: &str) -> io::Result<()> {
    let directory = config.home.join("templates");
    if !config.templates.paths.contains(&directory) {
        return Ok(());
    }
    install_in(&directory, name, text)
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
            Err(error) => Err(error),
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
        install(&config, "example.toml", "bundled").unwrap();
        let file = directory.join("templates/example.toml");
        assert_eq!(fs::read_to_string(&file).unwrap(), "bundled");
        fs::write(&file, "customized").unwrap();
        install(&config, "example.toml", "updated bundle").unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "customized");
        config.templates.paths = vec![directory.join("custom-catalog")];
        install(&config, "other.toml", "not opted in").unwrap();
        assert!(!directory.join("templates/other.toml").exists());
        assert_eq!(
            fs::read_dir(directory.join("templates")).unwrap().count(),
            1
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
