//! Where a node's files may be. A path from the profile is the user's word; a path that
//! arrives as a value may come from anything upstream, an agent's output included, so it
//! stays under the node's base folder unless the node says `allow_outside = yes`. The
//! check follows symlinks and `..` before it compares: a link inside that points out is
//! outside.

use std::path::{Component, Path, PathBuf};

use regulus_core::ModuleError;

use crate::failed;

/// `path` with every symlink and `..` resolved. A path that does not exist yet resolves
/// through its nearest existing folder; what is missing below it must be plain names.
pub(crate) fn real(path: &Path) -> Result<PathBuf, ModuleError> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut found) => {
                found.extend(missing.iter().rev());
                return Ok(found);
            }
            Err(error) => {
                let name = match existing.components().next_back() {
                    Some(Component::Normal(name)) => name,
                    _ => return Err(failed(path, error)),
                };
                missing.push(name);
                existing = existing.parent().ok_or_else(|| failed(path, error))?;
            }
        }
    }
}

/// `path` resolved, if it lies under `base` resolved; an error that says so otherwise.
pub(crate) fn confine(base: &Path, path: &Path) -> Result<PathBuf, ModuleError> {
    let base = base.canonicalize().map_err(|e| failed(base, e))?;
    let found = real(path)?;
    if found.starts_with(&base) {
        Ok(found)
    } else {
        Err(ModuleError::Failed(format!(
            "{} is outside {} and the node does not allow it (`allow_outside`)",
            path.display(),
            base.display()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn dot_dots_and_links_that_leave_the_base_are_refused() {
        let root = std::env::temp_dir().join(format!("regulus-place-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let base = root.join("base");
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(root.join("secret"), "x").unwrap();
        fs::write(base.join("ok.md"), "x").unwrap();
        assert!(confine(&base, &base.join("ok.md")).is_ok());
        assert!(
            confine(&base, &base.join("sub/new/file.md")).is_ok(),
            "not there yet"
        );
        assert!(confine(&base, &base.join("../secret")).is_err());
        assert!(confine(&base, &base.join("sub/../../secret")).is_err());
        assert!(confine(&base, &base.join("nope/../../secret")).is_err());
        assert!(confine(&base, Path::new("/etc/passwd")).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("secret"), base.join("link")).unwrap();
            assert!(
                confine(&base, &base.join("link")).is_err(),
                "a link out is out"
            );
        }
    }
}
