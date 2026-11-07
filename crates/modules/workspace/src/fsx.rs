//! File system helpers: where a path may be, how much a folder holds, copying trees.

use std::fs;
use std::path::{Component, Path, PathBuf};

/// `path` with every symlink and `..` resolved. A path that does not exist yet resolves
/// through its nearest existing folder; what is missing below it must be plain names.
pub fn real(path: &Path) -> Result<PathBuf, String> {
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
                    _ => return Err(format!("{}: {error}", path.display())),
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
pub fn confine(base: &Path, path: &Path) -> Result<PathBuf, String> {
    let base = base
        .canonicalize()
        .map_err(|e| format!("{}: {e}", base.display()))?;
    let found = real(path)?;
    if found.starts_with(&base) {
        Ok(found)
    } else {
        Err(format!(
            "{} is outside {} and the node does not allow it (`allow_outside`)",
            path.display(),
            base.display()
        ))
    }
}

/// Bytes under `dir`, links not followed. Stops counting once past `limit`: the answer is
/// then only known to be above it.
pub fn usage(dir: &Path, limit: u64) -> u64 {
    let mut total = 0u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        let Ok(entries) = fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.path().symlink_metadata() else {
                continue;
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total = total.saturating_add(meta.len());
                if total > limit {
                    return total;
                }
            }
        }
    }
    total
}

/// What a copy does with a symlink it finds in what it copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Links {
    /// Copied as links (a seed from the profile: the user's own).
    Keep,
    /// Left out (a merge or an include: the content may come from anything).
    Skip,
}

/// Copies `from` (a file or a folder) to `to`, overwriting files. Below `to`, an entry
/// that is already a symlink is refused rather than written through: a link planted
/// there must not carry the copy out of `to`. Returns how many entries it copied.
pub fn copy_tree(from: &Path, to: &Path, links: Links) -> Result<u64, String> {
    copy_entry(from, to, links, true)
}

fn copy_entry(from: &Path, to: &Path, links: Links, top: bool) -> Result<u64, String> {
    let fail = |path: &Path, e: std::io::Error| format!("{}: {e}", path.display());
    if !top
        && to
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(format!(
            "{} is a symlink: refused, a copy never writes through one",
            to.display()
        ));
    }
    let meta = from.symlink_metadata().map_err(|e| fail(from, e))?;
    if meta.file_type().is_symlink() {
        if links == Links::Skip {
            return Ok(0);
        }
        let target = fs::read_link(from).map_err(|e| fail(from, e))?;
        if to.symlink_metadata().is_ok() {
            fs::remove_file(to).map_err(|e| fail(to, e))?;
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, to).map_err(|e| fail(to, e))?;
        #[cfg(not(unix))]
        let _ = target;
        return Ok(1);
    }
    if meta.is_file() {
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(|e| fail(parent, e))?;
        }
        fs::copy(from, to).map_err(|e| fail(to, e))?;
        return Ok(1);
    }
    fs::create_dir_all(to).map_err(|e| fail(to, e))?;
    let mut copied = 0;
    for entry in fs::read_dir(from).map_err(|e| fail(from, e))?.flatten() {
        copied += copy_entry(&entry.path(), &to.join(entry.file_name()), links, false)?;
    }
    Ok(copied)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_counts_files_and_stops_past_the_limit_and_paths_stay_inside() {
        let root = std::env::temp_dir().join(format!("regulus-fsx-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("a/b")).unwrap();
        fs::write(root.join("a/one"), [0u8; 10]).unwrap();
        fs::write(root.join("a/b/two"), [0u8; 20]).unwrap();
        assert_eq!(usage(&root, 100), 30);
        assert!(usage(&root, 15) > 15);
        assert_eq!(
            copy_tree(&root.join("a"), &root.join("c"), Links::Keep).unwrap(),
            2
        );
        assert_eq!(fs::read(root.join("c/b/two")).unwrap().len(), 20);
        assert!(confine(&root.join("a"), &root.join("a/new/file")).is_ok());
        assert!(confine(&root.join("a"), &root.join("a/../c/one")).is_err());
        let _ = fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn a_copy_never_writes_through_a_link_in_the_target_nor_copies_links_it_skips() {
        let root = std::env::temp_dir().join(format!("regulus-fsx-link-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("from/evil")).unwrap();
        fs::write(root.join("from/evil/x"), "payload").unwrap();
        fs::create_dir_all(root.join("outside")).unwrap();
        fs::create_dir_all(root.join("to")).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("to/evil")).unwrap();
        assert!(copy_tree(&root.join("from"), &root.join("to"), Links::Skip).is_err());
        assert!(
            !root.join("outside/x").exists(),
            "nothing went through the link"
        );
        // A link in what is copied is left out with `Skip`.
        fs::create_dir_all(root.join("from2")).unwrap();
        std::os::unix::fs::symlink(root.join("outside"), root.join("from2/evil")).unwrap();
        assert_eq!(
            copy_tree(&root.join("from2"), &root.join("to2"), Links::Skip).unwrap(),
            0
        );
        assert!(root.join("to2/evil").symlink_metadata().is_err());
        let _ = fs::remove_dir_all(&root);
    }
}
