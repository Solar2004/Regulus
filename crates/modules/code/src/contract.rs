//! The words this module shares with whoever talks to it: the `code.workspace@1`
//! capability, the `code.inspect` method and the shapes both answer with
//! (docs/14-code.md §4-5).
//!
//! The capability lives here and not under `crates/contracts` because this module is its
//! only server and its only consumer so far; it moves there when a second module serves
//! or consumes it. Adding an optional field keeps `@1`; changing a shape means
//! `code.workspace@2`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A folder to work in: a node that offers it answers [`DESCRIBE`], [`CALL`] and [`LOOK`].
pub const CAPABILITY: &str = "code.workspace@1";

/// The folder and what may be done in it: `NodeCall` → [`Description`].
pub const DESCRIBE: &str = "code.workspace.describe";
/// Runs one operation: `NodeCall` with `tool@1` `Call` args → `tool@1` `Outcome`.
pub const CALL: &str = "code.workspace.call";
/// A read-only look: `NodeCall` with [`Look`] args → the answer of that action.
pub const LOOK: &str = "code.workspace.inspect";

/// The same look from the socket, at a live workspace node: [`InspectParams`].
pub const INSPECT: &str = "code.inspect";

/// The operations of a workspace, by the names agents call them.
pub mod tool_name {
    pub const LIST_FILES: &str = "code_list_files";
    pub const READ_FILE: &str = "code_read_file";
    pub const SEARCH: &str = "code_search";
    pub const GIT_STATUS: &str = "code_git_status";
    pub const GIT_DIFF: &str = "code_git_diff";
    pub const WRITE_FILE: &str = "code_write_file";
    pub const EDIT_FILE: &str = "code_edit_file";
    pub const RUN_COMMAND: &str = "code_run_command";
    /// Every name above, reads first.
    pub const ALL: &[&str] = &[
        LIST_FILES,
        READ_FILE,
        SEARCH,
        GIT_STATUS,
        GIT_DIFF,
        WRITE_FILE,
        EDIT_FILE,
        RUN_COMMAND,
    ];
}

/// What a look may ask for.
pub mod action {
    pub const SUMMARY: &str = "summary";
    pub const LIST: &str = "list";
    pub const READ: &str = "read";
    pub const SEARCH: &str = "search";
    pub const DIFF: &str = "diff";
    pub const ALL: &[&str] = &[SUMMARY, LIST, READ, SEARCH, DIFF];
}

/// What an operation does to the folder, so a box can offer some of them without knowing
/// their names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// Changes nothing.
    Read,
    /// Changes files inside the root.
    Write,
    /// Runs a program: not bounded by the root.
    Execute,
}

/// One operation: a `tool@1` spec plus what it does to the folder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Operation {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments.
    pub input: Value,
    pub access: Access,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Description {
    /// The folder, absolute, with every link resolved.
    pub root: String,
    pub operations: Vec<Operation>,
}

/// What to look at.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Look {
    /// One of [`action`].
    pub action: String,
    /// Relative to the root. `list`: the folder (default the root). `read`: the file
    /// (required). `search`: where to look (default everywhere). `diff`: only this path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// `search`: the text to find, literally, whatever its case.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

/// The params of [`INSPECT`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InspectParams {
    pub execution_id: String,
    /// A live `workspace` node of that execution.
    pub node_id: String,
    #[serde(flatten)]
    pub look: Look,
}

/// `summary`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub root: String,
    /// `null` when the folder is not a repository git can read; [`Summary::git_reason`]
    /// then says why.
    pub git: Option<Git>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_reason: Option<String>,
    /// The operations the workspace offers, by name.
    pub tools: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Git {
    /// The branch checked out; `null` on a detached HEAD.
    pub branch: Option<String>,
    /// `git status --porcelain=v1` of the root's subtree, paths relative to the root:
    /// `XY path` per line, `XY old -> new` for a rename, empty when clean.
    pub status: String,
    /// The commit checked out; `null` before the first commit.
    pub head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub behind: Option<u64>,
    /// The lines of `status`, parsed.
    pub changes: Vec<Change>,
    /// There were more changes than the limit: `status` and `changes` are cut.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Change {
    pub path: String,
    /// The two status letters, index then work tree (`" M"`, `"A "`, `"??"`…).
    pub code: String,
    /// What a renamed or copied path was called.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    /// Relative to the root, `/` between names.
    pub path: String,
    pub kind: Kind,
    /// Bytes, for a file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// `list`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Listing {
    pub root: String,
    /// The folder listed, relative to the root; empty for the root itself.
    pub path: String,
    /// Its entries, by path.
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

/// `read`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileText {
    pub root: String,
    pub path: String,
    /// The file from its start, as far as the limit lets.
    pub text: String,
    /// The file goes on past `text`.
    pub truncated: bool,
    /// The whole file, in bytes.
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Match {
    pub path: String,
    /// From 1.
    pub line: u64,
    /// The line, without its end and cut to the limit.
    pub text: String,
}

/// `search`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Found {
    pub root: String,
    pub matches: Vec<Match>,
    /// The search stopped early: at the most matches, files or time it may take.
    pub truncated: bool,
}

/// `diff`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diff {
    pub root: String,
    /// The tracked changes against `HEAD`, staged or not, as one unified diff. Empty with
    /// nothing changed, and outside a repository.
    pub text: String,
    pub truncated: bool,
    /// Why there is no diff to give, when git cannot read the folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_reason: Option<String>,
}
