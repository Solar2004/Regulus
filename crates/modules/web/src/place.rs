//! Where a node's files may be: a path that arrives as a value stays under the node's
//! base folder (links and `..` resolved) unless the node says `allow_outside = yes`.

use std::path::{Component, Path, PathBuf};

/// `path` with every symlink and `..` resolved; what does not exist yet resolves through
/// its nearest existing folder and must be plain names.
pub(crate) fn real(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut found) => {
                found.extend(missing.iter().rev());
                return Ok(found);
            }
            Err(error) => {
                let Some(Component::Normal(name)) = existing.components().next_back() else {
                    return Err(format!("{}: {error}", path.display()));
                };
                missing.push(name);
                existing = existing
                    .parent()
                    .ok_or_else(|| format!("{}: {error}", path.display()))?;
            }
        }
    }
}

/// `path` resolved, if it lies under `base` resolved.
pub(crate) fn confine(base: &Path, path: &Path) -> Result<PathBuf, String> {
    let root = base
        .canonicalize()
        .map_err(|e| format!("{}: {e}", base.display()))?;
    let found = real(path)?;
    if found.starts_with(&root) {
        Ok(found)
    } else {
        Err(format!(
            "{} is outside {} and the node does not allow it (`allow_outside`)",
            path.display(),
            root.display()
        ))
    }
}
