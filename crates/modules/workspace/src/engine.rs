//! What makes a workspace: an engine that creates the folder of one execution, merges it
//! back and archives it. Two are built in, `git` (a worktree per execution, through the
//! `git` program) and `folder` (a plain folder, no git); a node wired into `engine`
//! (`workspace.engine@1`) replaces them without touching this crate.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use regulus_core::{Endpoint, ModuleCtx};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::fsx;

/// The capability of a pluggable engine: its node serves [`ENGINE_METHOD`].
pub const ENGINE_CAPABILITY: &str = "workspace.engine@1";
/// `NodeCall` with `{ op: "create", spec }` → [`Info`]; `{ op: "merge", info, options }`
/// → `{ merged }` or `{ conflict }`; `{ op: "archive", info, options }` → what it did.
pub const ENGINE_METHOD: &str = "workspace.engine";

/// What a workspace should be, from the node's fields.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Spec {
    /// Its name: the execution's id.
    pub id: String,
    /// The folder it lives in.
    pub path: PathBuf,
    pub repo: Option<PathBuf>,
    /// Branch to start from and merge into; empty: the repo's current branch.
    pub base: String,
    pub branch: String,
    /// Create `repo` (git init, one empty commit) if it does not exist.
    pub init: bool,
    /// The file in the repo that lists untracked files to copy in.
    pub include_file: String,
    /// `folder`: what to copy in; also where to merge if `target` is empty.
    pub seed: Option<PathBuf>,
    pub target: Option<PathBuf>,
    /// Where `folder` puts archived folders.
    pub archive_dir: PathBuf,
    pub options: Options,
}

/// How to commit and merge.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Options {
    pub merge_mode: String,
    pub message: String,
    pub author_name: String,
    pub author_email: String,
    pub timeout_ms: u64,
}

/// A workspace that exists: what `info` sends and what the merge queue takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Info {
    pub id: String,
    pub engine: String,
    pub path: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_dir: Option<PathBuf>,
}

pub enum Merge {
    Done(Value),
    /// It could not merge cleanly; nothing changed in the base.
    Conflict(Value),
}

pub trait Engine {
    fn create(&self, spec: &Spec) -> Result<Info, String>;
    fn merge(&self, info: &Info, options: &Options) -> Result<Merge, String>;
    fn archive(&self, info: &Info, options: &Options) -> Result<Value, String>;
}

pub const GIT: &str = "git";
pub const FOLDER: &str = "folder";

/// The built-in engine called `name`.
pub fn built_in(name: &str) -> Result<Box<dyn Engine>, String> {
    match name {
        GIT => Ok(Box::new(Git)),
        FOLDER => Ok(Box::new(Folder)),
        other => Err(format!("`engine` is git or folder, not `{other}`")),
    }
}

/// A node wired into `engine`.
pub struct Wired<'a> {
    pub ctx: &'a ModuleCtx,
    pub endpoint: Endpoint,
}

impl Wired<'_> {
    fn ask(&self, args: Value) -> Result<Value, String> {
        self.ctx
            .call(&self.endpoint, ENGINE_METHOD, args)
            .map_err(|e| format!("engine: {e}"))
    }
}

impl Engine for Wired<'_> {
    fn create(&self, spec: &Spec) -> Result<Info, String> {
        let answer = self.ask(json!({ "op": "create", "spec": spec }))?;
        serde_json::from_value(answer).map_err(|e| format!("engine: not an Info: {e}"))
    }

    fn merge(&self, info: &Info, options: &Options) -> Result<Merge, String> {
        let answer = self.ask(json!({ "op": "merge", "info": info, "options": options }))?;
        if let Some(conflict) = answer.get("conflict") {
            return Ok(Merge::Conflict(conflict.clone()));
        }
        Ok(Merge::Done(answer.get("merged").cloned().unwrap_or(answer)))
    }

    fn archive(&self, info: &Info, options: &Options) -> Result<Value, String> {
        self.ask(json!({ "op": "archive", "info": info, "options": options }))
    }
}

/// Most characters of a program's error kept in a message.
const MAX_ERROR_CHARS: usize = 400;
/// How often a running `git` is looked at while it may time out.
const POLL: Duration = Duration::from_millis(5);

/// Runs `git` in `dir`, at most `timeout_ms` (0: no limit); stdout trimmed, or why not.
pub fn git(dir: &Path, args: &[&str], options: &Options) -> Result<String, String> {
    let mut command = Command::new("git");
    command.arg("-C").arg(dir);
    if !options.author_name.is_empty() {
        command.args(["-c", &format!("user.name={}", options.author_name)]);
    }
    if !options.author_email.is_empty() {
        command.args(["-c", &format!("user.email={}", options.author_email)]);
    }
    command
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let shown = format!("git {}", args.join(" "));
    let mut child = command.spawn().map_err(|e| format!("{shown}: {e}"))?;
    let mut out = child.stdout.take().expect("piped");
    let mut err = child.stderr.take().expect("piped");
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = out.read_to_string(&mut text);
        text
    });
    let errors = std::thread::spawn(move || {
        let mut text = String::new();
        let _ = err.read_to_string(&mut text);
        text
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(e) => return Err(format!("{shown}: {e}")),
        }
        if options.timeout_ms > 0 && started.elapsed() >= Duration::from_millis(options.timeout_ms)
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("{shown}: no answer in {} ms", options.timeout_ms));
        }
        std::thread::sleep(POLL);
    };
    let stdout = reader.join().unwrap_or_default();
    let stderr = errors.join().unwrap_or_default();
    if status.success() {
        Ok(stdout.trim().to_string())
    } else {
        let message = if stderr.trim().is_empty() {
            stdout
        } else {
            stderr
        };
        let message: String = message.trim().chars().take(MAX_ERROR_CHARS).collect();
        Err(format!("{shown}: {message}"))
    }
}

/// A worktree per execution of a base repository.
pub struct Git;

impl Git {
    fn repo(spec: &Spec) -> Result<PathBuf, String> {
        let repo = spec
            .repo
            .clone()
            .ok_or("the git engine needs `repo`, a git repository")?;
        if !repo.exists() && spec.init {
            fs::create_dir_all(&repo).map_err(|e| format!("{}: {e}", repo.display()))?;
            git(&repo, &["init", "-q"], &spec.options)?;
            git(
                &repo,
                &["commit", "-q", "--allow-empty", "-m", "regulus: init"],
                &spec.options,
            )?;
        }
        git(&repo, &["rev-parse", "--show-toplevel"], &spec.options)
            .map(PathBuf::from)
            .map_err(|e| format!("{} is not a git repository: {e}", repo.display()))
    }

    /// Copies what `.worktreeinclude` lists (globs from the repo's root, `#` comments)
    /// into the new worktree, if it is not there already. Nothing outside the repo.
    fn include(repo: &Path, worktree: &Path, file: &str) -> Result<u64, String> {
        if file.is_empty() {
            return Ok(0);
        }
        let Ok(list) = fs::read_to_string(repo.join(file)) else {
            return Ok(0);
        };
        let mut copied = 0;
        for line in list.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            let pattern = repo.join(line.trim_start_matches('/'));
            let Ok(found) = glob::glob(&pattern.to_string_lossy()) else {
                continue;
            };
            for source in found.flatten() {
                let Ok(inside) = fsx::confine(repo, &source) else {
                    continue;
                };
                let Ok(relative) = inside.strip_prefix(repo) else {
                    continue;
                };
                if relative.starts_with(".git") {
                    continue;
                }
                let target = worktree.join(relative);
                if !target.exists() {
                    copied += fsx::copy_tree(&source, &target)?;
                }
            }
        }
        Ok(copied)
    }

    /// Commits whatever changed in the worktree, so a merge or an archive loses nothing.
    fn commit_all(path: &Path, options: &Options) -> Result<bool, String> {
        git(path, &["add", "-A"], options)?;
        if git(path, &["status", "--porcelain"], options)?.is_empty() {
            return Ok(false);
        }
        git(path, &["commit", "-q", "-m", &options.message], options)?;
        Ok(true)
    }
}

impl Engine for Git {
    fn create(&self, spec: &Spec) -> Result<Info, String> {
        let repo = Self::repo(spec)?;
        let base = if spec.base.is_empty() {
            git(&repo, &["symbolic-ref", "--short", "HEAD"], &spec.options)?
        } else {
            spec.base.clone()
        };
        let path = &spec.path;
        let shown = path.to_string_lossy().to_string();
        if path.exists() {
            git(path, &["rev-parse", "--is-inside-work-tree"], &spec.options)
                .map_err(|e| format!("{shown} exists and is not a worktree: {e}"))?;
        } else {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            let branch_ref = format!("refs/heads/{}", spec.branch);
            let exists = git(
                &repo,
                &["rev-parse", "--verify", "--quiet", &branch_ref],
                &spec.options,
            )
            .is_ok();
            if exists {
                git(
                    &repo,
                    &["worktree", "add", "-q", &shown, &spec.branch],
                    &spec.options,
                )?;
            } else {
                git(
                    &repo,
                    &["worktree", "add", "-q", "-b", &spec.branch, &shown, &base],
                    &spec.options,
                )?;
            }
            Self::include(&repo, path, &spec.include_file)?;
        }
        Ok(Info {
            id: spec.id.clone(),
            engine: GIT.into(),
            path: path.clone(),
            repo: Some(repo),
            branch: Some(spec.branch.clone()),
            base: Some(base),
            target: None,
            archive_dir: None,
        })
    }

    fn merge(&self, info: &Info, options: &Options) -> Result<Merge, String> {
        let (Some(repo), Some(branch), Some(base)) = (&info.repo, &info.branch, &info.base) else {
            return Err("a git workspace needs `repo`, `branch` and `base`".into());
        };
        let committed = Self::commit_all(&info.path, options)?;
        let current = git(repo, &["symbolic-ref", "--short", "HEAD"], options)?;
        if &current != base {
            return Err(format!(
                "{} has `{current}` checked out, not `{base}`: the merge happens in the base \
                 branch's checkout",
                repo.display()
            ));
        }
        let merged = match options.merge_mode.as_str() {
            "ff" => git(repo, &["merge", "-q", "--ff-only", branch], options),
            "squash" => git(repo, &["merge", "-q", "--squash", branch], options).and_then(|_| {
                if git(repo, &["diff", "--cached", "--quiet"], options).is_err() {
                    git(repo, &["commit", "-q", "-m", &options.message], options)?;
                }
                Ok(String::new())
            }),
            _ => git(
                repo,
                &[
                    "merge",
                    "-q",
                    "--no-ff",
                    "--no-edit",
                    "-m",
                    &options.message,
                    branch,
                ],
                options,
            ),
        };
        if let Err(error) = merged {
            let files =
                git(repo, &["diff", "--name-only", "--diff-filter=U"], options).unwrap_or_default();
            if files.is_empty() && options.merge_mode != "ff" {
                return Err(error);
            }
            // Leave the base as it was.
            let undo = if options.merge_mode == "squash" {
                ["reset", "--merge"]
            } else {
                ["merge", "--abort"]
            };
            let _ = git(repo, &undo, options);
            let files: Vec<&str> = files.lines().collect();
            return Ok(Merge::Conflict(json!({
                "id": info.id, "branch": branch, "base": base, "files": files, "message": error,
            })));
        }
        let commit = git(repo, &["rev-parse", "HEAD"], options)?;
        Ok(Merge::Done(json!({
            "id": info.id, "branch": branch, "base": base, "commit": commit,
            "committed_changes": committed,
        })))
    }

    fn archive(&self, info: &Info, options: &Options) -> Result<Value, String> {
        let repo = info.repo.as_ref().ok_or("a git workspace needs `repo`")?;
        if info.path.exists() {
            Self::commit_all(&info.path, options)?;
            let shown = info.path.to_string_lossy().to_string();
            git(repo, &["worktree", "remove", "--force", &shown], options)?;
        }
        Ok(json!({ "id": info.id, "path": info.path, "branch": info.branch }))
    }
}

/// A plain folder: empty or a copy of `seed`; merging copies it onto `target`.
pub struct Folder;

impl Engine for Folder {
    fn create(&self, spec: &Spec) -> Result<Info, String> {
        let path = &spec.path;
        if !path.exists() {
            match &spec.seed {
                Some(seed) => {
                    fsx::copy_tree(seed, path)?;
                }
                None => fs::create_dir_all(path).map_err(|e| format!("{}: {e}", path.display()))?,
            }
        }
        Ok(Info {
            id: spec.id.clone(),
            engine: FOLDER.into(),
            path: path.clone(),
            repo: None,
            branch: None,
            base: None,
            target: spec.target.clone().or_else(|| spec.seed.clone()),
            archive_dir: Some(spec.archive_dir.clone()),
        })
    }

    fn merge(&self, info: &Info, _: &Options) -> Result<Merge, String> {
        let target = info
            .target
            .as_ref()
            .ok_or("the folder engine merges onto `target` (or `seed`); neither is set")?;
        let files = fsx::copy_tree(&info.path, target)?;
        Ok(Merge::Done(
            json!({ "id": info.id, "target": target, "files": files }),
        ))
    }

    fn archive(&self, info: &Info, _: &Options) -> Result<Value, String> {
        let dir = info
            .archive_dir
            .as_ref()
            .ok_or("the folder engine needs where to archive")?;
        fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let mut to = dir.join(&info.id);
        let mut n = 1;
        while to.exists() {
            n += 1;
            to = dir.join(format!("{}-{n}", info.id));
        }
        if info.path.exists() {
            fs::rename(&info.path, &to).map_err(|e| format!("{}: {e}", to.display()))?;
        }
        Ok(json!({ "id": info.id, "path": to }))
    }
}
