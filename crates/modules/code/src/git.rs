//! Git for a workspace: the branch, what changed and the diff, read-only.
//!
//! A repository's configuration can name programs that git runs on its own: a file
//! system monitor, hooks, an external diff, text converters, clean and smudge filters, a
//! pager. A folder an agent works in is not always one the user made, so every run here
//! switches all of those off, asks for no lock (git then never rewrites the index), does
//! not go into submodules and prompts for nothing. What git reads is still the
//! repository the folder says it belongs to.

use std::path::Path;
use std::time::Duration;

use crate::contract;
use crate::run::{exec, Done, Keep, Refused, Spec};

/// Bytes kept of an answer that is a line or two.
const SMALL: usize = 16 * 1024;
/// Bytes kept of the list of configured filter drivers.
const DRIVERS: usize = 256 * 1024;
/// Filter drivers switched off at most; a configuration with more is not trusted.
const MAX_DRIVERS: usize = 256;
/// Bytes kept of what git says went wrong.
const STDERR: usize = 4 * 1024;

/// Settings that win over every configuration file: nothing a file names gets run.
const QUIET: &[(&str, &str)] = &[
    ("core.fsmonitor", "false"),
    ("core.hooksPath", "/dev/null"),
    ("core.pager", "cat"),
    ("diff.external", ""),
    ("diff.ignoreSubmodules", "all"),
    ("status.submoduleSummary", "false"),
    ("core.quotePath", "false"),
    ("color.ui", "false"),
];

/// How this workspace runs git.
#[derive(Debug, Clone)]
pub(crate) struct Git {
    pub program: String,
    pub timeout: Duration,
    /// The variables of the core's environment git inherits (`HOME` gives it the user's
    /// own configuration: ignore rules, `safe.directory`).
    pub env: Vec<(String, String)>,
    /// The repository's own directory must be under the root.
    pub inside_only: bool,
    /// What git's output has to arrive once git ended.
    pub pipe_grace: Duration,
}

/// A repository whose work tree holds the workspace's root.
#[derive(Debug, Clone)]
pub(crate) struct Repo {
    /// The root inside the work tree: `sub/`, or empty when the root is its top.
    prefix: String,
    /// [`QUIET`] plus every configured filter driver emptied.
    overrides: Vec<(String, String)>,
}

fn quiet() -> Vec<(String, String)> {
    QUIET
        .iter()
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// What git said went wrong, in a line.
fn why(done: &Done) -> String {
    if done.timed_out {
        return "git did not answer in time (`git_timeout_ms`)".into();
    }
    let said = String::from_utf8_lossy(&done.stderr);
    if said.contains("not a git repository") {
        return "not a git repository".into();
    }
    if said.contains("dubious ownership") {
        return "git refuses this repository: it belongs to another user (`safe.directory`)".into();
    }
    said.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(|line| line.trim_start_matches("fatal: ").to_string())
        .unwrap_or_else(|| match done.code {
            Some(code) => format!("git exited with code {code}"),
            None => "git was killed".into(),
        })
}

impl Git {
    /// Runs git in `cwd` with `overrides` over every configuration file, keeping `keep`
    /// bytes of what it prints.
    fn run(
        &self,
        cwd: &Path,
        overrides: &[(String, String)],
        args: &[&str],
        keep: usize,
    ) -> Result<Done, String> {
        // Nothing of git's own from the core's environment: no other repository, index
        // or configuration than the folder's.
        let mut env: Vec<(String, String)> = self
            .env
            .iter()
            .filter(|(name, _)| !name.starts_with("GIT_"))
            .cloned()
            .collect();
        let fixed = [
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_TERMINAL_PROMPT", "0"),
            ("GIT_PAGER", "cat"),
            ("PAGER", "cat"),
            ("LC_ALL", "C"),
        ];
        env.extend(fixed.iter().map(|(k, v)| (k.to_string(), v.to_string())));
        env.push(("GIT_CONFIG_COUNT".into(), overrides.len().to_string()));
        for (index, (key, value)) in overrides.iter().enumerate() {
            env.push((format!("GIT_CONFIG_KEY_{index}"), key.clone()));
            env.push((format!("GIT_CONFIG_VALUE_{index}"), value.clone()));
        }
        let mut full = vec!["--no-pager".to_string()];
        full.extend(args.iter().map(|arg| arg.to_string()));
        let spec = Spec {
            program: self.program.clone(),
            args: full,
            cwd: cwd.to_path_buf(),
            env,
            timeout: self.timeout,
            grace: Duration::ZERO,
            pipe_grace: self.pipe_grace,
            stdout: Keep::Head(keep),
            stderr: Keep::Head(STDERR),
        };
        exec(&spec, None).map_err(|refused| match refused {
            Refused::Spawn(error) if error.kind() == std::io::ErrorKind::NotFound => format!(
                "`{}` is not installed (the workspace's `git` field)",
                self.program
            ),
            Refused::Spawn(error) => format!("`{}` could not be run: {error}", self.program),
            Refused::Node(message) => message,
        })
    }

    /// The repository `root` (a resolved folder) is in, if git can read it and its work
    /// tree really holds the root. `core.worktree` can point a repository at any folder:
    /// one whose work tree is somewhere else is refused, so status and diff only ever
    /// show files under the root.
    pub fn open(&self, root: &Path) -> Result<Repo, String> {
        let mut overrides = quiet();
        let found = self.run(
            root,
            &overrides,
            &[
                "rev-parse",
                "--is-inside-work-tree",
                "--show-toplevel",
                "--show-prefix",
                "--absolute-git-dir",
            ],
            SMALL,
        )?;
        if !found.succeeded() {
            return Err(why(&found));
        }
        let said = String::from_utf8_lossy(&found.stdout);
        let mut lines = said.lines();
        let (Some("true"), Some(top), Some(prefix), Some(git_dir)) =
            (lines.next(), lines.next(), lines.next(), lines.next())
        else {
            return Err("not inside a git work tree".into());
        };
        let top = Path::new(top)
            .canonicalize()
            .map_err(|e| format!("the repository's work tree cannot be opened: {e}"))?;
        if top.join(prefix.trim_end_matches('/')) != root {
            return Err(
                "the repository's work tree is not where this folder is (`core.worktree`)".into(),
            );
        }
        if self.inside_only {
            let inside = Path::new(git_dir)
                .canonicalize()
                .is_ok_and(|dir| dir.starts_with(root));
            if !inside {
                return Err(
                    "the repository's own directory is outside the workspace (`git_dir = inside`)"
                        .into(),
                );
            }
        }
        // Filters run whenever git reads a file to compare it. Their names are free, so
        // each one configured, anywhere, is emptied by name.
        let drivers = self.run(
            root,
            &overrides,
            &[
                "config",
                "-z",
                "--name-only",
                "--get-regexp",
                r"^filter\..*\.(clean|smudge|process)$",
            ],
            DRIVERS,
        )?;
        if drivers.timed_out || drivers.stdout_cut {
            return Err("git's filter configuration could not be read".into());
        }
        let names = String::from_utf8_lossy(&drivers.stdout).into_owned();
        let keys: Vec<&str> = names.split('\0').filter(|key| !key.is_empty()).collect();
        if keys.len() > MAX_DRIVERS {
            return Err(format!(
                "git has more than {MAX_DRIVERS} filter settings: not trusted"
            ));
        }
        for key in keys {
            overrides.push((key.to_string(), String::new()));
            if let Some((driver, _)) = key.rsplit_once('.') {
                let required = format!("{driver}.required");
                if !overrides.iter().any(|(k, _)| *k == required) {
                    overrides.push((required, "false".into()));
                }
            }
        }
        Ok(Repo {
            prefix: prefix.to_string(),
            overrides,
        })
    }

    /// The branch and what changed under the root, at most `max` changes.
    pub fn status(
        &self,
        root: &Path,
        repo: &Repo,
        max: usize,
        keep: usize,
    ) -> Result<contract::Git, String> {
        let done = self.run(
            root,
            &repo.overrides,
            &[
                "status",
                "--porcelain=v2",
                "--branch",
                "-z",
                "--untracked-files=normal",
                "--ignore-submodules=all",
                "--",
                ".",
            ],
            keep,
        )?;
        if !done.succeeded() {
            return Err(why(&done));
        }
        let mut status = parse_status(&done.stdout, &repo.prefix, max);
        status.truncated |= done.stdout_cut;
        Ok(status)
    }

    /// The tracked changes under the root (or under `rel`, a path relative to it) against
    /// `HEAD`, staged or not, as one unified diff of at most `max` bytes; and whether
    /// there was more.
    pub fn diff(
        &self,
        root: &Path,
        repo: &Repo,
        rel: Option<&str>,
        max: usize,
    ) -> Result<(String, bool), String> {
        let head = self.run(
            root,
            &repo.overrides,
            &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
            SMALL,
        )?;
        let base = if head.succeeded() {
            "HEAD".to_string()
        } else {
            // Before the first commit there is no HEAD: everything tracked is new, which
            // is a diff against the empty tree.
            let empty = self.run(
                root,
                &repo.overrides,
                &["hash-object", "-t", "tree", "/dev/null"],
                SMALL,
            )?;
            if !empty.succeeded() {
                return Err(why(&empty));
            }
            String::from_utf8_lossy(&empty.stdout).trim().to_string()
        };
        let only = match rel.filter(|rel| !rel.is_empty()) {
            Some(rel) => format!(":(literal){rel}"),
            None => ".".into(),
        };
        let done = self.run(
            root,
            &repo.overrides,
            &[
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--relative",
                "--ignore-submodules=all",
                &base,
                "--",
                &only,
            ],
            max,
        )?;
        if !done.succeeded() {
            return Err(why(&done));
        }
        Ok((
            String::from_utf8_lossy(&done.stdout).into_owned(),
            done.stdout_cut,
        ))
    }
}

/// Reads `git status --porcelain=v2 --branch -z`: the branch lines, then one record per
/// change, NUL between records and, for a rename, between its two paths. Paths come
/// relative to the work tree's top: `prefix` (the root inside it) is taken off.
fn parse_status(raw: &[u8], prefix: &str, max: usize) -> contract::Git {
    let mut git = contract::Git::default();
    let mut lines = Vec::new();
    let text = String::from_utf8_lossy(raw);
    let mut records = text.split('\0');
    while let Some(record) = records.next() {
        if record.is_empty() {
            continue;
        }
        if let Some(header) = record.strip_prefix("# ") {
            match header.split_once(' ') {
                Some(("branch.oid", oid)) if oid != "(initial)" => git.head = Some(oid.into()),
                Some(("branch.head", name)) if name != "(detached)" => {
                    git.branch = Some(name.into())
                }
                Some(("branch.upstream", name)) => git.upstream = Some(name.into()),
                Some(("branch.ab", counts)) => {
                    let mut counts = counts.split(' ');
                    let number = |text: Option<&str>| text.and_then(|t| t.get(1..)?.parse().ok());
                    git.ahead = number(counts.next());
                    git.behind = number(counts.next());
                }
                _ => {}
            }
            continue;
        }
        // `1 XY …7 fields… path`, `2 XY …8 fields… path` + the old path, `u XY …9 fields…
        // path`, `? path`. `.` stands for "unchanged" where v1 has a space.
        let (kind, rest) = record.split_at(1);
        let fields = match kind {
            "1" => 8,
            "2" => 9,
            "u" => 10,
            "?" => 1,
            _ => continue,
        };
        let rest = rest.strip_prefix(' ').unwrap_or(rest);
        let parts: Vec<&str> = rest.splitn(fields, ' ').collect();
        let from = (kind == "2").then(|| records.next()).flatten();
        let (Some(path), true) = (parts.last(), parts.len() == fields) else {
            continue;
        };
        let code = if kind == "?" {
            "??".to_string()
        } else {
            parts[0].replace('.', " ")
        };
        let local = |path: &str| path.strip_prefix(prefix).map(String::from);
        let Some(path) = local(path) else { continue };
        if git.changes.len() >= max {
            git.truncated = true;
            break;
        }
        let from = from.and_then(local);
        lines.push(match &from {
            Some(from) => format!("{code} {from} -> {path}\n"),
            None => format!("{code} {path}\n"),
        });
        git.changes.push(contract::Change { path, code, from });
    }
    git.status = lines.concat();
    git
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_status_is_read_into_branch_and_changes() {
        let raw = b"# branch.oid 1234abcd\0# branch.head main\0# branch.upstream origin/main\0\
# branch.ab +2 -1\0\
1 .M N... 100644 100644 100644 aaa bbb sub/a.txt\0\
2 R. N... 100644 100644 100644 aaa bbb R100 sub/new name.txt\0sub/old.txt\0\
1 A. N... 000000 100644 100644 000 bbb other/c.txt\0\
u UU N... 100644 100644 100644 100644 a b c sub/both.txt\0\
? sub/dir/\0";
        let git = parse_status(raw, "sub/", 10);
        assert_eq!(git.branch.as_deref(), Some("main"));
        assert_eq!(git.head.as_deref(), Some("1234abcd"));
        assert_eq!(git.upstream.as_deref(), Some("origin/main"));
        assert_eq!((git.ahead, git.behind), (Some(2), Some(1)));
        assert_eq!(
            git.status, " M a.txt\nR  old.txt -> new name.txt\nUU both.txt\n?? dir/\n",
            "paths are relative to the root; what is outside it is left out"
        );
        assert_eq!(git.changes.len(), 4);
        assert_eq!(git.changes[1].from.as_deref(), Some("old.txt"));
        assert!(!git.truncated);
    }

    #[test]
    fn a_new_or_detached_repository_and_a_limit_are_told() {
        let fresh = parse_status(b"# branch.oid (initial)\0# branch.head master\0", "", 10);
        assert_eq!(
            (fresh.head, fresh.branch.as_deref()),
            (None, Some("master"))
        );
        assert_eq!(fresh.status, "");
        let detached = parse_status(b"# branch.oid abc\0# branch.head (detached)\0", "", 10);
        assert_eq!(
            (detached.head.as_deref(), detached.branch),
            (Some("abc"), None)
        );
        let many = parse_status(b"? a\0? b\0? c\0", "", 2);
        assert_eq!(many.status, "?? a\n?? b\n");
        assert!(many.truncated);
    }

    #[test]
    fn what_git_says_is_told_in_a_line() {
        let said = |stderr: &str| {
            why(&Done {
                code: Some(128),
                stderr: stderr.as_bytes().to_vec(),
                ..Done::default()
            })
        };
        assert_eq!(
            said("fatal: not a git repository (or any of the parent directories): .git\n"),
            "not a git repository"
        );
        assert!(
            said("fatal: detected dubious ownership in repository at '/x'\n")
                .contains("another user")
        );
        assert_eq!(said("\nfatal: bad object HEAD\n"), "bad object HEAD");
        assert_eq!(said(""), "git exited with code 128");
        let late = why(&Done {
            timed_out: true,
            ..Done::default()
        });
        assert!(late.contains("in time"), "{late}");
    }
}
