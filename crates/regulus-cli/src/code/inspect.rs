//! `code.inspect`, the read-only look at a coding workspace (docs/04-client.md §12): what
//! is asked, how an answer reads, and how `regulus code inspect` prints it. Pure. The
//! answers are read tolerantly: a field that is missing or of another shape shows as
//! nothing rather than failing, so a module that says more keeps working here.

use serde_json::{json, Value};

/// The method, served by whichever module runs the workspace node.
pub const METHOD: &str = "code.inspect";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    Summary,
    List,
    Read,
    Search,
    Diff,
}

impl Action {
    pub const ALL: [Action; 5] = [
        Action::Summary,
        Action::List,
        Action::Read,
        Action::Search,
        Action::Diff,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Action::Summary => "summary",
            Action::List => "list",
            Action::Read => "read",
            Action::Search => "search",
            Action::Diff => "diff",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|action| action.name() == name)
    }
}

/// One question about a workspace.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Ask {
    pub action: Action,
    pub path: Option<String>,
    pub query: Option<String>,
}

impl Ask {
    pub fn summary() -> Self {
        Self::of(Action::Summary)
    }

    pub fn list(path: &str) -> Self {
        Self {
            path: Some(path.to_string()),
            ..Self::of(Action::List)
        }
    }

    pub fn read(path: &str) -> Self {
        Self {
            path: Some(path.to_string()),
            ..Self::of(Action::Read)
        }
    }

    pub fn search(query: &str) -> Self {
        Self {
            query: Some(query.to_string()),
            ..Self::of(Action::Search)
        }
    }

    pub fn diff() -> Self {
        Self::of(Action::Diff)
    }

    fn of(action: Action) -> Self {
        Self {
            action,
            path: None,
            query: None,
        }
    }

    /// What `regulus code inspect <action> [argument] [--path P]` asks. The argument is the
    /// path of `list`, `read` and `diff` and the query of `search`.
    pub fn from_words(
        action: &str,
        argument: Option<String>,
        path: Option<String>,
    ) -> Result<Self, String> {
        let Some(action) = Action::parse(action) else {
            let names: Vec<&str> = Action::ALL.iter().map(|a| a.name()).collect();
            return Err(format!(
                "`{action}` is not something to inspect: {}",
                names.join(", ")
            ));
        };
        let ask = match action {
            Action::Summary => match argument {
                None => Self::of(action),
                Some(extra) => return Err(format!("summary takes nothing, got `{extra}`")),
            },
            Action::Search => Self {
                query: Some(argument.ok_or("search needs what to look for")?),
                path,
                ..Self::of(action)
            },
            Action::Read => Self {
                path: Some(argument.or(path).ok_or("read needs a file")?),
                ..Self::of(action)
            },
            Action::List | Action::Diff => Self {
                path: argument.or(path),
                ..Self::of(action)
            },
        };
        Ok(ask)
    }

    /// The parameters of the call: only what was asked goes in.
    pub fn params(&self, execution: &str, node: &str) -> Value {
        let mut params = json!({
            "execution_id": execution, "node_id": node, "action": self.action.name(),
        });
        if let Some(path) = &self.path {
            params["path"] = Value::from(path.clone());
        }
        if let Some(query) = &self.query {
            params["query"] = Value::from(query.clone());
        }
        params
    }
}

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap_or_default().to_string()
}

/// What git says about the workspace, or why it says nothing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Git {
    /// `None`: no branch (a detached head) or no git.
    pub branch: Option<String>,
    /// One change per line, as `git status --porcelain` writes them. Empty: clean.
    pub changes: Vec<String>,
    /// `Some`: git has nothing to say, and why if the module said.
    pub unavailable: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Summary {
    pub root: String,
    pub git: Git,
    pub tools: Vec<String>,
}

/// A `summary` answer. `git` may be missing or `null` (no repository, no git), say
/// `available: false` with a `reason`, and give `status` as porcelain text, as lines or as
/// `{path, status|code}` rows.
pub fn summary(value: &Value) -> Summary {
    let git = &value["git"];
    let changes: Vec<String> = match &git["status"] {
        Value::String(porcelain) => porcelain
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(str::to_string)
            .collect(),
        Value::Array(rows) => rows
            .iter()
            .filter_map(|row| match row {
                Value::String(line) => Some(line.clone()),
                Value::Object(_) => {
                    let path = row["path"].as_str()?;
                    let code = ["code", "status"]
                        .iter()
                        .find_map(|key| row[*key].as_str())
                        .unwrap_or("?");
                    Some(format!("{code} {path}"))
                }
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    };
    let reason = ["reason", "error"]
        .iter()
        .find_map(|key| git[*key].as_str())
        .map(str::to_string);
    let missing = git.is_null() || git["available"] == false || git["status"].is_null();
    let unavailable = match (missing, reason) {
        (_, Some(reason)) => Some(reason),
        (true, None) => Some("not a git repository, or git is not available".to_string()),
        (false, None) => None,
    };
    Summary {
        root: text(value, "root"),
        git: Git {
            branch: git["branch"].as_str().map(str::to_string),
            changes,
            unavailable,
        },
        tools: value["tools"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|tool| tool.as_str().map(str::to_string))
            .collect(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Relative to the root.
    pub path: String,
    pub directory: bool,
}

impl Entry {
    /// The entry as shown inside the folder `within` (relative to the root): its own name,
    /// with `/` after a folder.
    pub fn label(&self, within: &str) -> String {
        let within = within.trim_matches('/');
        let name = match self.path.strip_prefix(within) {
            Some(rest) if !within.is_empty() => rest.trim_start_matches('/'),
            _ => self.path.as_str(),
        };
        if self.directory {
            format!("{name}/")
        } else {
            name.to_string()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listing {
    pub root: String,
    pub path: String,
    pub entries: Vec<Entry>,
    pub truncated: bool,
}

/// A `list` answer, folders first and then by path, however the module ordered it.
pub fn listing(value: &Value) -> Listing {
    let mut entries: Vec<Entry> = value["entries"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            Some(Entry {
                path: entry["path"].as_str()?.to_string(),
                directory: entry["kind"].as_str() == Some("directory"),
            })
        })
        .collect();
    entries.sort_by(|a, b| (!a.directory, &a.path).cmp(&(!b.directory, &b.path)));
    Listing {
        root: text(value, "root"),
        path: text(value, "path"),
        entries,
        truncated: value["truncated"] == true,
    }
}

/// A `read` or a `diff` answer: text, and whether there was more.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Text {
    pub root: String,
    pub path: String,
    pub text: String,
    pub truncated: bool,
}

pub fn file(value: &Value) -> Text {
    Text {
        root: text(value, "root"),
        path: text(value, "path"),
        text: text(value, "text"),
        truncated: value["truncated"] == true,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Match {
    pub path: String,
    /// From 1.
    pub line: u64,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Found {
    pub root: String,
    pub matches: Vec<Match>,
    pub truncated: bool,
}

pub fn found(value: &Value) -> Found {
    Found {
        root: text(value, "root"),
        matches: value["matches"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| {
                Some(Match {
                    path: m["path"].as_str()?.to_string(),
                    line: m["line"].as_u64().unwrap_or_default(),
                    text: text(m, "text"),
                })
            })
            .collect(),
        truncated: value["truncated"] == true,
    }
}

/// The folder above `path` (relative to the root); the root is its own.
pub fn parent(path: &str) -> String {
    let path = path.trim_matches('/');
    match path.rsplit_once('/') {
        Some((above, _)) => above.to_string(),
        None => String::new(),
    }
}

/// What `regulus code inspect` writes: `out` for stdout, the thing asked for and nothing
/// else, so it pipes; `notes` for stderr.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Printed {
    pub out: String,
    pub notes: Vec<String>,
}

const TRUNCATED: &str = "truncated: there is more than the workspace's limits let through";

fn ends_in_newline(mut text: String) -> String {
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// An answer as the command prints it.
pub fn print(action: Action, value: &Value) -> Printed {
    let mut printed = Printed::default();
    let truncated = match action {
        Action::Summary => {
            let summary = summary(value);
            let mut out = format!("root    {}\n", summary.root);
            match (&summary.git.unavailable, &summary.git.branch) {
                (Some(why), _) => out.push_str(&format!("git     unavailable: {why}\n")),
                (None, Some(branch)) => out.push_str(&format!("branch  {branch}\n")),
                (None, None) => out.push_str("branch  (none: detached head)\n"),
            }
            if summary.git.unavailable.is_none() {
                match summary.git.changes.as_slice() {
                    [] => out.push_str("status  clean\n"),
                    changes => {
                        for (at, change) in changes.iter().enumerate() {
                            let head = if at == 0 { "status " } else { "       " };
                            out.push_str(&format!("{head} {change}\n"));
                        }
                    }
                }
            }
            out.push_str(&format!("tools   {}\n", summary.tools.join(", ")));
            printed.out = out;
            false
        }
        Action::List => {
            let listing = listing(value);
            printed.out = listing
                .entries
                .iter()
                .map(|entry| {
                    let slash = if entry.directory { "/" } else { "" };
                    format!("{}{slash}\n", entry.path)
                })
                .collect();
            if listing.entries.is_empty() {
                printed.notes.push("empty".into());
            }
            listing.truncated
        }
        Action::Read | Action::Diff => {
            let text = file(value);
            if action == Action::Diff && text.text.is_empty() {
                printed.notes.push("no changes to tracked files".into());
            }
            printed.out = ends_in_newline(text.text);
            text.truncated
        }
        Action::Search => {
            let found = found(value);
            printed.out = found
                .matches
                .iter()
                .map(|m| format!("{}:{}: {}\n", m.path, m.line, m.text.trim_end()))
                .collect();
            if found.matches.is_empty() {
                printed.notes.push("no matches".into());
            }
            found.truncated
        }
    };
    if truncated {
        printed.notes.push(TRUNCATED.into());
    }
    printed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_become_a_question_with_only_what_was_asked() {
        let ask = Ask::from_words("list", Some("src".into()), None).unwrap();
        assert_eq!(
            ask.params("exec-1", "workspace"),
            json!({ "execution_id": "exec-1", "node_id": "workspace", "action": "list",
                    "path": "src" })
        );
        let ask = Ask::from_words("summary", None, None).unwrap();
        assert_eq!(
            ask.params("e", "w"),
            json!({ "execution_id": "e", "node_id": "w", "action": "summary" })
        );
        let ask = Ask::from_words("search", Some("fn main".into()), Some("src".into())).unwrap();
        assert_eq!(ask.query.as_deref(), Some("fn main"));
        assert_eq!(ask.path.as_deref(), Some("src"));
        assert_eq!(Ask::from_words("diff", None, None).unwrap(), Ask::diff());
        assert_eq!(
            Ask::from_words("read", None, Some("a.rs".into())).unwrap(),
            Ask::read("a.rs")
        );
        for (action, argument, said) in [
            ("read", None, "read needs a file"),
            ("search", None, "search needs what to look for"),
            ("summary", Some("x"), "summary takes nothing"),
            ("write", None, "`write` is not something to inspect"),
        ] {
            let error = Ask::from_words(action, argument.map(str::to_string), None).unwrap_err();
            assert!(error.contains(said), "{error}");
        }
    }

    #[test]
    fn a_summary_reads_every_shape_git_may_take() {
        let porcelain = summary(&json!({ "root": "/w", "tools": ["code_read_file"],
            "git": { "branch": "main", "status": " M src/a.rs\n?? notes.md\n",
                     "available": true } }));
        assert_eq!(porcelain.root, "/w");
        assert_eq!(porcelain.git.branch.as_deref(), Some("main"));
        assert_eq!(porcelain.git.changes, vec![" M src/a.rs", "?? notes.md"]);
        assert_eq!(porcelain.git.unavailable, None);
        assert_eq!(porcelain.tools, vec!["code_read_file"]);

        let rows = summary(&json!({ "root": "/w", "git": { "branch": null,
            "status": [{ "path": "a.rs", "code": "M" }, "?? b.rs"] } }));
        assert_eq!(rows.git.changes, vec!["M a.rs", "?? b.rs"]);
        assert_eq!(rows.git.branch, None);
        assert_eq!(rows.git.unavailable, None);

        for missing in [
            json!({ "root": "/w", "git": null }),
            json!({ "root": "/w" }),
            json!({ "root": "/w", "git": { "branch": null, "status": null } }),
        ] {
            assert!(summary(&missing).git.unavailable.is_some(), "{missing}");
        }
        let said = summary(&json!({ "root": "/w", "git": { "available": false,
            "status": null, "reason": "git is not installed" } }));
        assert_eq!(
            said.git.unavailable.as_deref(),
            Some("git is not installed")
        );
        // Clean is not unavailable.
        let clean = summary(&json!({ "root": "/w", "git": { "branch": "main", "status": "" } }));
        assert_eq!(clean.git.unavailable, None);
        assert!(clean.git.changes.is_empty());
    }

    #[test]
    fn a_listing_puts_folders_first_and_names_entries_inside_their_folder() {
        let listing = listing(&json!({ "root": "/w", "path": "src", "truncated": true,
            "entries": [{ "path": "src/main.rs", "kind": "file" },
                        { "path": "src/tui", "kind": "directory" },
                        { "path": "src/lib.rs", "kind": "file" }, { "kind": "file" }] }));
        let labels: Vec<String> = listing.entries.iter().map(|e| e.label("src")).collect();
        assert_eq!(labels, vec!["tui/", "lib.rs", "main.rs"]);
        assert!(listing.truncated);
        // At the root nothing is cut off; a deeper entry keeps the rest of its path.
        let entry = Entry {
            path: "src/tui/mod.rs".into(),
            directory: false,
        };
        assert_eq!(entry.label(""), "src/tui/mod.rs");
        assert_eq!(entry.label("src"), "tui/mod.rs");
        assert_eq!(parent("src/tui"), "src");
        assert_eq!(parent("src"), "");
        assert_eq!(parent(""), "");
    }

    #[test]
    fn the_command_prints_what_was_asked_and_notes_the_rest() {
        let summary = print(
            Action::Summary,
            &json!({ "root": "/w", "tools": ["code_list_files", "code_read_file"],
                     "git": { "branch": "main", "status": " M a.rs\n?? b.rs" } }),
        );
        assert_eq!(
            summary.out,
            "root    /w\nbranch  main\nstatus   M a.rs\n        ?? b.rs\n\
             tools   code_list_files, code_read_file\n"
        );
        let bare = print(Action::Summary, &json!({ "root": "/w", "git": null }));
        assert!(
            bare.out
                .contains("git     unavailable: not a git repository"),
            "{}",
            bare.out
        );
        assert!(!bare.out.contains("status"), "{}", bare.out);

        let list = print(
            Action::List,
            &json!({ "path": "", "entries": [{ "path": "a.rs", "kind": "file" },
                     { "path": "src", "kind": "directory" }], "truncated": true }),
        );
        assert_eq!(list.out, "src/\na.rs\n");
        assert_eq!(list.notes, vec![TRUNCATED]);

        let read = print(
            Action::Read,
            &json!({ "path": "a.rs", "text": "fn main() {}" }),
        );
        assert_eq!(read.out, "fn main() {}\n");
        assert!(read.notes.is_empty());

        let search = print(
            Action::Search,
            &json!({ "matches": [{ "path": "a.rs", "line": 3, "text": "  let x = 1;\n" }] }),
        );
        assert_eq!(search.out, "a.rs:3:   let x = 1;\n");
        assert_eq!(
            print(Action::Search, &json!({ "matches": [] })).notes,
            vec!["no matches"]
        );
        let diff = print(Action::Diff, &json!({ "text": "" }));
        assert_eq!(diff.out, "");
        assert_eq!(diff.notes, vec!["no changes to tracked files"]);
    }
}
