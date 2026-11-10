//! A workspace's operations as an agent sees them: what each one is called, what it
//! takes and what it gives back (docs/14-code.md §3). The answers are data (`tool@1`):
//! how they are written for a model is decided where the tools are presented.

use std::time::Duration;

use regulus_contract_tool as tool;
use serde_json::{json, Value};

use crate::contract::{tool_name, Access, Operation};
use crate::fsops::{self, Bounds, Query};
use crate::run::{exec, Keep, Refused, Spec};
use crate::{Fault, Settings, Workspace, CHANGED_PORT, RAN_PORT};

/// Every operation a workspace can have, reads first, with the limits of this one in
/// the words an agent reads.
pub(crate) fn catalog(settings: &Settings) -> Vec<Operation> {
    let text = |description: &str| json!({ "type": "string", "description": description });
    let whole =
        |description: &str| json!({ "type": "integer", "minimum": 1, "description": description });
    let flag = |description: &str| json!({ "type": "boolean", "description": description });
    let operation = |name: &str, access, description: String, input: Value| Operation {
        name: name.into(),
        description,
        input,
        access,
    };
    vec![
        operation(
            tool_name::LIST_FILES,
            Access::Read,
            format!(
                "List the files and folders of a folder of the workspace. Every path is \
                 relative to the workspace root. `depth` 1 (the default) gives the folder's \
                 own entries; more goes that many levels down. At most {} entries: \
                 `truncated` says there were more.",
                settings.max_list_entries
            ),
            json!({ "type": "object", "properties": {
                "path": text("The folder. Default: the workspace root."),
                "depth": whole("Levels to list. Default 1."),
            } }),
        ),
        operation(
            tool_name::READ_FILE,
            Access::Read,
            format!(
                "Read a text file of the workspace. Gives whole lines, at most {} bytes per \
                 call: when `truncated` is true, call again with `offset` set to `end_line` \
                 plus 1.",
                settings.max_read_bytes
            ),
            json!({ "type": "object", "properties": {
                "path": text("The file, relative to the workspace root."),
                "offset": whole("The first line to read, from 1. Default 1."),
                "limit": whole("How many lines to read. Default: as many as fit."),
            }, "required": ["path"] }),
        ),
        operation(
            tool_name::SEARCH,
            Access::Read,
            format!(
                "Find lines in the files of the workspace. `query` is the exact text to \
                 find. At most {} matches, each with its file and line number: `truncated` \
                 says the search stopped early.",
                settings.max_search_matches
            ),
            json!({ "type": "object", "properties": {
                "query": text("The text to find, exactly as written."),
                "path": text("A folder or one file to look in. Default: the whole workspace."),
                "glob": text("Only files whose path matches, like `*.rs` or `src/**/*.ts`."),
                "regex": flag("`query` is a regular expression."),
                "ignore_case": flag("Upper and lower case match each other."),
            }, "required": ["query"] }),
        ),
        operation(
            tool_name::GIT_STATUS,
            Access::Read,
            "The git branch of the workspace and its changed files, as `git status` gives \
             them: two status letters and the path, relative to the workspace root. Says \
             so when the folder is not a git repository."
                .into(),
            json!({ "type": "object", "properties": {} }),
        ),
        operation(
            tool_name::GIT_DIFF,
            Access::Read,
            format!(
                "The changes to tracked files since the last commit, staged or not, as one \
                 unified diff of at most {} bytes. Files git does not track yet are not in \
                 it: {} lists them.",
                settings.max_diff_bytes,
                tool_name::GIT_STATUS
            ),
            json!({ "type": "object", "properties": {
                "path": text("Only this file or folder. Default: the whole workspace."),
            } }),
        ),
        operation(
            tool_name::WRITE_FILE,
            Access::Write,
            format!(
                "Create a file, or replace all of an existing one, with `content`. The \
                 folders above it are created. To change part of a file use {} instead.",
                tool_name::EDIT_FILE
            ),
            json!({ "type": "object", "properties": {
                "path": text("The file, relative to the workspace root."),
                "content": text("The whole content of the file."),
            }, "required": ["path", "content"] }),
        ),
        operation(
            tool_name::EDIT_FILE,
            Access::Write,
            "Change part of a text file: replaces `old_text` with `new_text`. `old_text` \
             must be in the file exactly as written, spaces and line ends included, and \
             only once unless `replace_all` is set; otherwise nothing changes and the \
             answer says why. Read the file first and copy the text from it."
                .into(),
            json!({ "type": "object", "properties": {
                "path": text("The file, relative to the workspace root."),
                "old_text": text("The text to replace, exactly as it is in the file."),
                "new_text": text("What to put in its place."),
                "replace_all": flag("Replace every place `old_text` is in, not just one."),
            }, "required": ["path", "old_text", "new_text"] }),
        ),
        operation(
            tool_name::RUN_COMMAND,
            Access::Execute,
            format!(
                "Run a shell command line in the workspace and wait for it to end. Gives \
                 its exit code, stdout and stderr, each cut to {} bytes keeping the start \
                 and the end. It is stopped after `timeout_ms` (default {}, at most {}), \
                 and whatever it left running in the background is stopped with it: it \
                 cannot start a server and leave it up.",
                settings.max_output_bytes,
                settings.command_timeout.as_millis(),
                settings.max_command_timeout.as_millis()
            ),
            json!({ "type": "object", "properties": {
                "command": text("The command line, as typed in a shell."),
                "cwd": text("The folder to run it in. Default: the workspace root."),
                "timeout_ms": whole("Milliseconds it may take."),
            }, "required": ["command"] }),
        ),
    ]
}

/// A text argument, if it was given.
fn text_arg<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

/// A text argument that must be given; it may be empty.
fn needed<'a>(args: &'a Value, name: &str) -> Result<&'a str, Fault> {
    text_arg(args, name).ok_or_else(|| Fault::Request(format!("`{name}` is required, as text")))
}

/// A text argument that must be given and say something.
fn named<'a>(args: &'a Value, name: &str) -> Result<&'a str, Fault> {
    Some(needed(args, name)?)
        .filter(|text| !text.trim().is_empty())
        .ok_or_else(|| Fault::Request(format!("`{name}` is empty")))
}

/// A whole number, as a number or as the text of one: models write both.
fn number_arg(args: &Value, name: &str) -> Result<Option<u64>, Fault> {
    let number = match args.get(name) {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(number)) => number.as_u64(),
        Some(Value::String(text)) => text.trim().parse().ok(),
        Some(_) => None,
    };
    number
        .map(Some)
        .ok_or_else(|| Fault::Request(format!("`{name}` is not a whole number")))
}

fn flag_arg(args: &Value, name: &str) -> bool {
    match args.get(name) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::String(text)) => text.trim().eq_ignore_ascii_case("true"),
        _ => false,
    }
}

impl Workspace<'_> {
    /// Where a search of this workspace stops.
    pub(crate) fn bounds(&self, matches: usize) -> Bounds {
        Bounds {
            matches,
            line_chars: self.settings.max_line_chars,
            files: self.settings.max_search_files,
            file_bytes: self.settings.max_search_file_bytes,
            time: self.settings.search_timeout,
        }
    }

    /// Runs one operation: its answer, and whether the answer is a failure (a command
    /// that did not exit with 0). A fault is an operation that did not happen.
    pub(crate) fn perform(&self, call: &tool::Call) -> Result<(Value, bool), Fault> {
        let args = &call.args;
        let settings = &self.settings;
        let done = |data: Value| Ok((data, false));
        match call.name.as_str() {
            tool_name::LIST_FILES => {
                let depth = number_arg(args, "depth")?.unwrap_or(1).max(1);
                let listed = fsops::list(
                    &self.root()?,
                    &settings.skip,
                    text_arg(args, "path").unwrap_or(""),
                    usize::try_from(depth).unwrap_or(usize::MAX),
                    settings.max_list_entries,
                )?;
                done(json!({ "path": listed.path, "entries": listed.entries,
                             "truncated": listed.truncated }))
            }
            tool_name::READ_FILE => {
                let lines = fsops::read_lines(
                    &self.root()?,
                    named(args, "path")?,
                    number_arg(args, "offset")?.unwrap_or(1),
                    number_arg(args, "limit")?,
                    settings.max_read_bytes,
                )?;
                done(json!({ "path": lines.path, "start_line": lines.start_line,
                             "end_line": lines.end_line, "size": lines.size,
                             "text": lines.text, "truncated": lines.truncated }))
            }
            tool_name::SEARCH => {
                let query = Query {
                    text: needed(args, "query")?,
                    regex: flag_arg(args, "regex"),
                    ignore_case: flag_arg(args, "ignore_case"),
                    glob: text_arg(args, "glob"),
                    path: text_arg(args, "path").unwrap_or(""),
                };
                let hits = fsops::search(
                    &self.root()?,
                    &settings.skip,
                    &query,
                    &self.bounds(settings.max_search_matches),
                )?;
                done(json!({ "matches": hits.matches, "truncated": hits.truncated }))
            }
            tool_name::GIT_STATUS => {
                let root = self.root()?;
                let git = self.git();
                let status = git.open(&root.real).and_then(|repo| {
                    git.status(
                        &root.real,
                        &repo,
                        settings.max_list_entries,
                        settings.inspect_max_bytes,
                    )
                });
                done(match status {
                    Ok(status) => json!({ "repository": true, "branch": status.branch,
                                          "head": status.head, "upstream": status.upstream,
                                          "ahead": status.ahead, "behind": status.behind,
                                          "status": status.status,
                                          "truncated": status.truncated }),
                    Err(reason) => json!({ "repository": false, "reason": reason }),
                })
            }
            tool_name::GIT_DIFF => {
                let root = self.root()?;
                let only = match text_arg(args, "path").map(str::trim) {
                    Some(path) if !path.is_empty() => Some(root.locate(path)?.rel),
                    _ => None,
                };
                let git = self.git();
                let diff = git.open(&root.real).and_then(|repo| {
                    git.diff(&root.real, &repo, only.as_deref(), settings.max_diff_bytes)
                });
                done(match diff {
                    Ok((text, truncated)) => {
                        json!({ "repository": true, "text": text, "truncated": truncated })
                    }
                    Err(reason) => json!({ "repository": false, "reason": reason }),
                })
            }
            tool_name::WRITE_FILE => {
                let written = fsops::write(
                    &self.root()?,
                    named(args, "path")?,
                    needed(args, "content")?,
                    settings.max_write_bytes,
                )?;
                self.emit(
                    CHANGED_PORT,
                    json!({ "tool": call.name, "path": written.path, "bytes": written.bytes,
                            "created": written.created, "by": call.caller.name }),
                );
                done(json!({ "path": written.path, "bytes": written.bytes,
                             "created": written.created }))
            }
            tool_name::EDIT_FILE => {
                let edited = fsops::edit(
                    &self.root()?,
                    named(args, "path")?,
                    needed(args, "old_text")?,
                    needed(args, "new_text")?,
                    flag_arg(args, "replace_all"),
                    settings.max_write_bytes,
                )?;
                self.emit(
                    CHANGED_PORT,
                    json!({ "tool": call.name, "path": edited.path, "bytes": edited.bytes,
                            "created": false, "by": call.caller.name }),
                );
                done(
                    json!({ "path": edited.path, "replacements": edited.replacements,
                             "bytes": edited.bytes }),
                )
            }
            tool_name::RUN_COMMAND => self.run_command(call),
            other => Err(Fault::Request(format!(
                "this workspace has no tool `{other}`"
            ))),
        }
    }

    /// `code_run_command`: the command line through the workspace's shell, in a folder of
    /// the workspace, with only the environment the workspace gives it.
    fn run_command(&self, call: &tool::Call) -> Result<(Value, bool), Fault> {
        let args = &call.args;
        let settings = &self.settings;
        let line = named(args, "command")?;
        let root = self.root()?;
        let place = root.locate(text_arg(args, "cwd").unwrap_or(""))?;
        if !place.real.is_dir() {
            return Err(Fault::Request(format!(
                "`cwd` is not a folder of the workspace: `{}`",
                place.rel
            )));
        }
        let timeout = number_arg(args, "timeout_ms")?
            .map(Duration::from_millis)
            .unwrap_or(settings.command_timeout)
            .clamp(Duration::from_millis(1), settings.max_command_timeout);
        let (program, first) = settings
            .shell
            .split_first()
            .ok_or_else(|| Fault::Failed("the workspace has no `shell`".into()))?;
        let mut shell_args = first.to_vec();
        shell_args.push(line.to_string());
        let mut env = self.inherited();
        env.extend(settings.env.iter().cloned());
        if !env.iter().any(|(name, _)| name == "PWD") {
            env.push(("PWD".into(), place.real.display().to_string()));
        }
        let spec = Spec {
            program: program.clone(),
            args: shell_args,
            cwd: place.real.clone(),
            env,
            timeout,
            grace: settings.kill_grace,
            pipe_grace: settings.pipe_grace,
            stdout: Keep::Ends(settings.max_output_bytes),
            stderr: Keep::Ends(settings.max_output_bytes),
        };
        let ran = exec(&spec, Some((self.runs(), self.key(), settings.max_running))).map_err(
            |refused| match refused {
                Refused::Node(why) => Fault::Failed(why),
                Refused::Spawn(error) => Fault::Failed(format!(
                    "the shell `{program}` could not be started: {error}"
                )),
            },
        )?;
        if !self.runs().is_running(self.key()) {
            return Err(Fault::Failed(
                "the workspace stopped while the command ran: it was ended".into(),
            ));
        }
        self.emit(
            RAN_PORT,
            json!({ "command": line, "cwd": place.rel, "code": ran.code, "signal": ran.signal,
                    "timed_out": ran.timed_out, "ms": ran.ms, "by": call.caller.name }),
        );
        let failed = !ran.succeeded();
        Ok((
            json!({ "command": line, "cwd": place.rel, "code": ran.code, "signal": ran.signal,
                    "timed_out": ran.timed_out, "ms": ran.ms,
                    "stdout": String::from_utf8_lossy(&ran.stdout),
                    "stderr": String::from_utf8_lossy(&ran.stderr),
                    "truncated": ran.stdout_cut || ran.stderr_cut }),
            failed,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn arguments_are_read_as_models_write_them() {
        let args = json!({ "path": "a.rs", "empty": "", "offset": "12", "limit": 3,
                           "bad": "x", "all": "TRUE", "one": true, "none": null });
        assert_eq!(text_arg(&args, "path"), Some("a.rs"));
        assert_eq!(needed(&args, "empty").unwrap(), "");
        assert!(needed(&args, "missing").is_err());
        assert!(needed(&args, "limit").is_err(), "a number is not text");
        assert!(named(&args, "empty").is_err());
        assert_eq!(number_arg(&args, "offset").unwrap(), Some(12));
        assert_eq!(number_arg(&args, "limit").unwrap(), Some(3));
        assert_eq!(number_arg(&args, "none").unwrap(), None);
        assert_eq!(number_arg(&args, "missing").unwrap(), None);
        assert!(number_arg(&args, "bad").is_err());
        assert!(number_arg(&json!({ "n": -1 }), "n").is_err());
        assert!(number_arg(&json!({ "n": 1.5 }), "n").is_err());
        assert!(flag_arg(&args, "all") && flag_arg(&args, "one"));
        assert!(!flag_arg(&args, "missing") && !flag_arg(&args, "path"));
    }
}
