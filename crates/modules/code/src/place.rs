//! Where a workspace's file operations may go: inside its root, and nowhere a protected
//! name leads. A path comes from an agent, so it is resolved before it is trusted: `..`
//! and every link are followed first, and what comes out must still be under the root. A
//! link inside that points out is outside.
//!
//! The check is made before the operation, not held during it: a program that swaps a
//! folder for a link between the check and the write is not defended against, and neither
//! is a hard link to a file outside (docs/14-code.md §7).

use std::ffi::OsStr;
use std::path::{Component, Path, PathBuf};

use crate::Fault;

/// A workspace's folder, with every link resolved, and the names nothing enters.
#[derive(Debug, Clone)]
pub(crate) struct Root {
    pub real: PathBuf,
    protect: Vec<String>,
}

/// A path an operation was asked for, found inside the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Place {
    /// With every link resolved. It may not exist yet: what is missing is plain names at
    /// its end.
    pub real: PathBuf,
    /// Relative to the root, `/` between names; empty for the root itself.
    pub rel: String,
}

impl Root {
    /// `folder` as a root: absolute, existing and a folder.
    pub fn open(folder: &str, protect: Vec<String>) -> Result<Self, Fault> {
        let folder = folder.trim();
        if folder.is_empty() {
            return Err(Fault::Failed(
                "the workspace has no root: set its `root` field or send a folder to its `root` input"
                    .into(),
            ));
        }
        let path = Path::new(folder);
        if !path.is_absolute() {
            return Err(Fault::Failed(format!(
                "the workspace root `{folder}` is not an absolute path"
            )));
        }
        let real = path.canonicalize().map_err(|error| {
            Fault::Failed(format!(
                "the workspace root `{folder}` cannot be opened: {error}"
            ))
        })?;
        if !real.is_dir() {
            return Err(Fault::Failed(format!(
                "the workspace root `{folder}` is not a folder"
            )));
        }
        Ok(Self { real, protect })
    }

    /// The root as clients and agents are told it.
    pub fn text(&self) -> String {
        self.real.display().to_string()
    }

    /// Whether no file operation enters `name`. Case does not matter: on a file system
    /// that ignores it, `.GIT` is `.git`.
    pub fn is_protected(&self, name: &str) -> bool {
        self.protect.iter().any(|p| p.eq_ignore_ascii_case(name))
    }

    /// A resolved path as the names under the root, if it is under it and they are text.
    fn names(&self, real: &Path) -> Option<Vec<String>> {
        real.strip_prefix(&self.real)
            .ok()?
            .components()
            .map(|c| match c {
                Component::Normal(name) => name.to_str().map(String::from),
                _ => None,
            })
            .collect()
    }

    /// A resolved path relative to the root, if operations may reach it: under the root
    /// and through no protected name.
    pub fn relative(&self, real: &Path) -> Option<String> {
        let names = self.names(real)?;
        if names.iter().any(|name| self.is_protected(name)) {
            return None;
        }
        Some(names.join("/"))
    }

    /// Where `asked` leads: a path relative to the root, or an absolute one inside it.
    pub fn locate(&self, asked: &str) -> Result<Place, Fault> {
        let text = asked.trim();
        if text.contains('\0') {
            return Err(Fault::Request("a path cannot hold a NUL character".into()));
        }
        let protected = || {
            Fault::Request(format!(
                "`{text}` is protected in this workspace (`protect`)"
            ))
        };
        let path = Path::new(text);
        // The names as asked for, before anything is resolved: `.git` may be a file.
        let asked_protected = !path.is_absolute()
            && path.components().any(|c| match c {
                Component::Normal(name) => name.to_str().is_some_and(|n| self.is_protected(n)),
                _ => false,
            });
        if asked_protected {
            return Err(protected());
        }
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.real.join(path)
        };
        let real = resolve(&joined, text)?;
        let names = self
            .names(&real)
            .ok_or_else(|| Fault::Request(format!("`{text}` is outside the workspace")))?;
        if names.iter().any(|name| self.is_protected(name)) {
            return Err(protected());
        }
        Ok(Place {
            real,
            rel: names.join("/"),
        })
    }
}

/// `path` with every link and `..` resolved. A path that does not exist yet resolves
/// through its nearest existing folder; what is missing below it must be plain names. A
/// link that leads nowhere is refused: writing "under" it would land wherever it points.
fn resolve(path: &Path, asked: &str) -> Result<PathBuf, Fault> {
    let mut existing = path;
    let mut missing: Vec<&OsStr> = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut found) => {
                found.extend(missing.iter().rev());
                return Ok(found);
            }
            Err(_) => {
                let dangling = existing
                    .symlink_metadata()
                    .is_ok_and(|meta| meta.file_type().is_symlink());
                if dangling {
                    return Err(Fault::Request(format!(
                        "`{asked}` goes through a link that leads nowhere"
                    )));
                }
                let not_inside =
                    || Fault::Request(format!("`{asked}` is not a path inside the workspace"));
                let name = match existing.components().next_back() {
                    Some(Component::Normal(name)) => name,
                    _ => return Err(not_inside()),
                };
                missing.push(name);
                existing = existing.parent().ok_or_else(not_inside)?;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("regulus-code-place-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir.canonicalize().unwrap()
    }

    fn request(result: Result<Place, Fault>) -> String {
        match result {
            Err(Fault::Request(message)) => message,
            other => panic!("expected a refused request, got {other:?}"),
        }
    }

    #[test]
    fn a_root_is_an_absolute_existing_folder() {
        let home = temp("root");
        fs::write(home.join("file"), "x").unwrap();
        assert!(Root::open(home.to_str().unwrap(), vec![]).is_ok());
        for bad in [
            "",
            "relative/folder",
            home.join("missing").to_str().unwrap(),
            home.join("file").to_str().unwrap(),
        ] {
            assert!(
                matches!(Root::open(bad, vec![]), Err(Fault::Failed(_))),
                "{bad}"
            );
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn paths_stay_inside_the_root_links_and_dot_dots_resolved() {
        let home = temp("inside");
        let base = home.join("base");
        fs::create_dir_all(base.join("sub")).unwrap();
        fs::write(home.join("secret"), "x").unwrap();
        fs::write(base.join("ok.md"), "x").unwrap();
        let root = Root::open(base.to_str().unwrap(), vec![".git".into()]).unwrap();

        assert_eq!(root.locate("").unwrap().rel, "");
        assert_eq!(root.locate(".").unwrap().rel, "");
        assert_eq!(root.locate("ok.md").unwrap().rel, "ok.md");
        assert_eq!(root.locate(" sub/../ok.md ").unwrap().rel, "ok.md");
        let absolute = base.join("ok.md");
        assert_eq!(
            root.locate(absolute.to_str().unwrap()).unwrap().rel,
            "ok.md"
        );
        let new = root.locate("sub/new/file.md").unwrap();
        assert_eq!(new.rel, "sub/new/file.md", "not there yet");
        assert_eq!(new.real, base.join("sub/new/file.md"));

        for outside in [
            "../secret",
            "sub/../../secret",
            "/etc/passwd",
            home.join("secret").to_str().unwrap(),
        ] {
            let message = request(root.locate(outside));
            assert!(message.contains("outside the workspace"), "{message}");
        }
        // `..` through a folder that is not there cannot be resolved, so it is refused.
        assert!(request(root.locate("nope/../../secret")).contains("not a path inside"));
        assert!(request(root.locate("a\0b")).contains("NUL"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;
            symlink(home.join("secret"), base.join("out")).unwrap();
            symlink(&home, base.join("up")).unwrap();
            symlink(base.join("sub"), base.join("in")).unwrap();
            symlink(home.join("nowhere"), base.join("broken")).unwrap();
            assert!(request(root.locate("out")).contains("outside the workspace"));
            assert!(request(root.locate("up/secret")).contains("outside the workspace"));
            assert_eq!(root.locate("in/x.md").unwrap().rel, "sub/x.md", "a link in");
            assert!(request(root.locate("broken")).contains("leads nowhere"));
            assert!(request(root.locate("broken/new.md")).contains("leads nowhere"));
        }
        let _ = fs::remove_dir_all(&home);
    }

    #[test]
    fn protected_names_are_refused_as_asked_and_as_resolved() {
        let home = temp("protect");
        fs::create_dir_all(home.join(".git/hooks")).unwrap();
        fs::create_dir_all(home.join("src/.git")).unwrap();
        let root = Root::open(home.to_str().unwrap(), vec![".git".into()]).unwrap();
        for path in [
            ".git",
            ".git/config",
            ".GIT/config",
            "src/.git/x",
            "src/../.git/hooks/pre-commit",
        ] {
            assert!(request(root.locate(path)).contains("protected"), "{path}");
        }
        assert!(root.locate(".gitignore").is_ok(), "only the whole name");
        assert!(root.relative(&home.join(".git/config")).is_none());
        assert_eq!(root.relative(&home.join("src/a.rs")).unwrap(), "src/a.rs");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(home.join(".git"), home.join("alias")).unwrap();
            assert!(request(root.locate("alias/config")).contains("protected"));
        }
        let _ = fs::remove_dir_all(&home);
    }
}
