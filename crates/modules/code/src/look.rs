//! A read-only look at a workspace, for clients (`code.inspect`) and for boxes wired to
//! it (`code.workspace.inspect`): what it is, what is in a folder, a file, where a text
//! is, what changed (docs/14-code.md §5). It changes nothing: files are read here, and
//! git is run with nothing a repository configures (`git.rs`).

use serde::Serialize;
use serde_json::Value;

use crate::contract::{action, Diff, FileText, Found, Listing, Look, Summary};
use crate::fsops::{self, Query};
use crate::{Fault, Workspace};

fn answer(value: impl Serialize) -> Result<Value, Fault> {
    serde_json::to_value(value).map_err(|e| Fault::Failed(e.to_string()))
}

impl Workspace<'_> {
    pub(crate) fn look(&self, look: &Look) -> Result<Value, Fault> {
        // What is wrong with the request is said before the workspace is touched.
        let needs =
            |what: &str| Fault::Request(format!("`{what}` is required for `{}`", look.action));
        let path = look.path.as_deref().unwrap_or("");
        let query = look.query.as_deref().unwrap_or("");
        match look.action.as_str() {
            action::READ if path.trim().is_empty() => return Err(needs("path")),
            action::SEARCH if query.is_empty() => return Err(needs("query")),
            known if action::ALL.contains(&known) => {}
            unknown => {
                return Err(Fault::Request(format!(
                    "unknown action `{unknown}`: it is one of {}",
                    action::ALL.join(", ")
                )))
            }
        }
        let settings = &self.settings;
        let root = self.root()?;
        match look.action.as_str() {
            action::SUMMARY => {
                let git = self.git();
                let status = git.open(&root.real).and_then(|repo| {
                    git.status(
                        &root.real,
                        &repo,
                        settings.inspect_max_entries,
                        settings.inspect_max_bytes,
                    )
                });
                let (git, git_reason) = match status {
                    Ok(status) => (Some(status), None),
                    Err(reason) => (None, Some(reason)),
                };
                answer(Summary {
                    root: root.text(),
                    git,
                    git_reason,
                    tools: settings.tools.clone(),
                })
            }
            action::LIST => {
                let listed =
                    fsops::list(&root, &settings.skip, path, 1, settings.inspect_max_entries)?;
                answer(Listing {
                    root: root.text(),
                    path: listed.path,
                    entries: listed.entries,
                    truncated: listed.truncated,
                })
            }
            action::READ => {
                let head = fsops::read_head(&root, path, settings.inspect_max_bytes)?;
                answer(FileText {
                    root: root.text(),
                    path: head.path,
                    text: head.text,
                    truncated: head.truncated,
                    size: head.size,
                })
            }
            action::SEARCH => {
                // For a person: the text as typed, whatever its case.
                let query = Query {
                    text: query,
                    regex: false,
                    ignore_case: true,
                    glob: None,
                    path,
                };
                let hits = fsops::search(
                    &root,
                    &settings.skip,
                    &query,
                    &self.bounds(settings.inspect_max_entries),
                )?;
                answer(Found {
                    root: root.text(),
                    matches: hits.matches,
                    truncated: hits.truncated,
                })
            }
            _ => {
                let only = if path.trim().is_empty() {
                    None
                } else {
                    Some(root.locate(path)?.rel)
                };
                let git = self.git();
                let diff = git.open(&root.real).and_then(|repo| {
                    git.diff(
                        &root.real,
                        &repo,
                        only.as_deref(),
                        settings.inspect_max_bytes,
                    )
                });
                answer(match diff {
                    Ok((text, truncated)) => Diff {
                        root: root.text(),
                        text,
                        truncated,
                        git_reason: None,
                    },
                    // Not being a repository is an answer, not a failure.
                    Err(reason) => Diff {
                        root: root.text(),
                        text: String::new(),
                        truncated: false,
                        git_reason: Some(reason),
                    },
                })
            }
        }
    }
}
