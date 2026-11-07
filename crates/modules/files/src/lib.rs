//! Files (docs/13-blocks.md §3.2): `read` turns a file into a prompt, `write` a prompt
//! into a file, `document` injects a file into an agent every turn (as a `hook@1`, the
//! Agent box's `hooks` input), and `watch` sends what changes in a file or a folder.
//!
//! Relative paths resolve against the node's `dir`, or, if it is empty, against the
//! node's own directory (`ModuleCtx::node_dir`).

mod place;
mod watch;

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{
    method, Direction, Hook, HookAnswer, HookEffect, HookFailure, HookRequest, Manifest, NodeCall,
    NodeEvent, WireKind, HOOK_DESCRIBE,
};
use serde_json::{json, Value};

const MANIFEST: &str = include_str!("../regulus-module.toml");
const READ: &str = "read";
const WRITE: &str = "write";
const DOCUMENT: &str = "document";
const WATCH: &str = "watch";
const OUT: &str = "out";
const WRITTEN: &str = "written";
const YES: &str = "yes";
const APPEND: &str = "append";
/// What ends the name of the hidden file `write` fills before renaming it over the target.
const TEMP_SUFFIX: &str = ".regulus-tmp";
/// Characters that make a `path` a glob.
const GLOB_CHARS: &[char] = &['*', '?', '['];

pub struct Files {
    manifest: Manifest,
    ctx: ModuleCtx,
    watching: watch::Watchers,
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn failed(path: &Path, error: impl std::fmt::Display) -> ModuleError {
    ModuleError::Failed(format!("{}: {error}", path.display()))
}

/// One call to one node: the node with its settings, and how to act as it.
pub(crate) struct Here<'a> {
    files: &'a Files,
    call: NodeCall,
    me: NodeRef,
}

impl Here<'_> {
    fn setting(&self, field: &str) -> Option<&Value> {
        self.files.manifest.setting(&self.call.node, field)
    }

    /// A setting as text; nothing as empty.
    fn text(&self, field: &str) -> String {
        match self.setting(field) {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    fn number(&self, field: &str) -> u64 {
        match self.setting(field) {
            Some(Value::Number(n)) => n.as_u64().unwrap_or(0),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn has_output(&self, port: &str) -> bool {
        self.call
            .node
            .port(port)
            .is_some_and(|p| p.kind == WireKind::Then && p.direction == Direction::Output)
    }

    fn emit(&self, port: &str, value: Value) -> Result<(), ModuleError> {
        self.files.ctx.emit(&self.me, port, value)
    }

    /// The folder relative paths hang from: `dir` if it is absolute, else `dir` under the
    /// node's own directory.
    fn base(&self) -> Result<PathBuf, ModuleError> {
        let dir = self.text("dir");
        let dir = Path::new(dir.trim());
        if dir.is_absolute() {
            return Ok(dir.to_path_buf());
        }
        Ok(self.files.ctx.node_dir(&self.me)?.join(dir))
    }

    /// `path` as an absolute path: as is if it is one, else under [`Here::base`]. Not
    /// bounded: for paths the profile names.
    fn resolve(&self, path: &str) -> Result<PathBuf, ModuleError> {
        let path = Path::new(path.trim());
        if path.is_absolute() {
            return Ok(path.to_path_buf());
        }
        Ok(self.base()?.join(path))
    }

    /// `path` resolved, if it is under [`Here::base`] or the node has `allow_outside`.
    fn inside(&self, path: &Path) -> Result<PathBuf, ModuleError> {
        if self.text("allow_outside") == YES {
            return Ok(path.to_path_buf());
        }
        place::confine(&self.base()?, path)
    }

    /// The text of a file, refused if it is larger than `max_bytes`.
    fn read(&self, path: &Path) -> Result<String, ModuleError> {
        read_bounded(path, self.number("max_bytes"))
    }
}

/// The text of a regular file, at most `max` bytes of it: a larger one is refused, and so
/// is anything that is not a regular file (a FIFO or `/dev/zero` would hold the bus
/// thread). The cap holds even if the file grows while it is read. `max = 0` is refused:
/// there is no unbounded read.
pub(crate) fn read_bounded(path: &Path, max: u64) -> Result<String, ModuleError> {
    if max == 0 {
        return Err(ModuleError::InvalidParams(format!(
            "{}: a byte limit of 0 reads nothing; set `max_bytes` above 0",
            path.display()
        )));
    }
    let not_regular = || failed(path, "not a regular file");
    if !fs::metadata(path).map_err(|e| failed(path, e))?.is_file() {
        return Err(not_regular());
    }
    let file = fs::File::open(path).map_err(|e| failed(path, e))?;
    if !file.metadata().map_err(|e| failed(path, e))?.is_file() {
        return Err(not_regular());
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| failed(path, e))?;
    if bytes.len() as u64 > max {
        return Err(failed(path, format!("larger than {max} bytes")));
    }
    String::from_utf8(bytes).map_err(|e| failed(path, e))
}

/// A limit that must be above 0.
fn positive(here: &Here, field: &str) -> Result<u64, ModuleError> {
    match here.number(field) {
        0 => Err(ModuleError::InvalidParams(format!(
            "`{field}` of `{}` must be above 0",
            here.me.node_id
        ))),
        n => Ok(n),
    }
}

/// The files `pattern` names: itself, or, for a glob, every file it matches, sorted, and
/// refused if there are more than `max`.
fn matches(pattern: &Path, max: u64) -> Result<Vec<PathBuf>, ModuleError> {
    let text = pattern.to_string_lossy();
    if !text.contains(GLOB_CHARS) {
        return Ok(vec![pattern.to_path_buf()]);
    }
    let mut found: Vec<PathBuf> = glob::glob(&text)
        .map_err(|e| failed(pattern, e))?
        .filter_map(Result::ok)
        .filter(|p| p.is_file())
        .take(usize::try_from(max).unwrap_or(usize::MAX).saturating_add(1))
        .collect();
    if found.len() as u64 > max {
        return Err(failed(
            pattern,
            format!("matches more than {max} files (`max_files`)"),
        ));
    }
    found.sort();
    if found.is_empty() {
        return Err(failed(pattern, "no file matches"));
    }
    Ok(found)
}

/// Text as is; any other value as indented JSON.
fn as_text(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        other => serde_json::to_string_pretty(other).unwrap_or_default(),
    }
}

impl Files {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("files manifest is valid"),
            ctx: ModuleCtx::default(),
            watching: watch::Watchers::default(),
        }
    }

    fn here(&self, call: NodeCall) -> Here<'_> {
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        Here {
            files: self,
            call,
            me,
        }
    }

    fn event(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        let here = self.here(call);
        match here.call.node.node_type.as_str() {
            READ => read(&here, event),
            WRITE => write(&here, event),
            other => Err(ModuleError::InvalidParams(format!(
                "node type `{other}` takes no events"
            ))),
        }
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let here = self.here(call);
        match here.call.node.node_type.as_str() {
            DOCUMENT if here.has_output(OUT) => {
                here.emit(OUT, json!(document(&here)?))?;
                Ok(Value::Null)
            }
            WATCH => self.watching.start(&here),
            _ => Ok(Value::Null),
        }
    }

    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        self.watching.stop(&self.here(call).me);
        Ok(Value::Null)
    }
}

/// `read`: the file, or each file of a glob, on `out`.
fn read(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    // A path from the profile is the user's word; one that came on `in` stays inside.
    let mut path = here.text("path");
    let from_value = path.trim().is_empty();
    if from_value {
        path = event.value.as_str().unwrap_or_default().to_string();
    }
    if path.trim().is_empty() {
        return Err(ModuleError::InvalidParams(format!(
            "read `{}` has no `path` and got no path on `{}`",
            here.me.node_id, event.port
        )));
    }
    let max_bytes = positive(here, "max_bytes")?;
    let mut left = positive(here, "max_total_bytes")?;
    let mut files = matches(&here.resolve(&path)?, positive(here, "max_files")?)?;
    if from_value {
        files = files
            .iter()
            .map(|file| here.inside(file))
            .collect::<Result<_, _>>()?;
    }
    // One file at a time: with `each`, each goes out as soon as it is read.
    let each = here.text("each") == YES;
    let mut texts = Vec::new();
    for file in &files {
        let text = read_bounded(file, max_bytes.min(left)).map_err(|e| {
            ModuleError::Failed(format!(
                "{e} (`max_bytes` {max_bytes}, `max_total_bytes` left {left})"
            ))
        })?;
        left -= text.len() as u64;
        if each {
            here.emit(OUT, json!(text))?;
        } else {
            texts.push(text);
        }
    }
    if !each {
        here.emit(OUT, json!(texts.join(&here.text("separator"))))?;
    }
    let read: Vec<String> = files.iter().map(|f| f.display().to_string()).collect();
    Ok(json!({ "read": read }))
}

/// `write`: the value into the file, then its path on `written`.
fn write(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    // Writes stay under the base whatever names the path: a link in it cannot send them
    // out. The base itself is the profile's, so it may be created.
    let base = here.base()?;
    fs::create_dir_all(&base).map_err(|e| failed(&base, e))?;
    let path = here.inside(&here.resolve(&here.text("path"))?)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|e| failed(parent, e))?;
    }
    let text = as_text(&event.value);
    let mut content = Vec::new();
    if here.text("mode") == APPEND {
        match fs::metadata(&path) {
            Ok(meta) if !meta.is_file() => return Err(failed(&path, "not a regular file")),
            Ok(_) => content = fs::read(&path).map_err(|e| failed(&path, e))?,
            Err(_) => {}
        }
        if !content.is_empty() {
            content.extend(here.text("separator").into_bytes());
        }
    }
    content.extend(text.as_bytes());
    replace(&path, &content)?;
    let written = path.display().to_string();
    if here.has_output(WRITTEN) {
        here.emit(WRITTEN, json!(written))?;
    }
    Ok(json!({ "written": written, "bytes": text.len() }))
}

/// Puts `content` in `path` all at once: written to a hidden file beside it, then renamed
/// over it, so a reader (a `document` hook, a `watch`) never sees half of it.
fn replace(path: &Path, content: &[u8]) -> Result<(), ModuleError> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let temp = path.with_file_name(format!(".{name}.{}-{n}{TEMP_SUFFIX}", std::process::id()));
    let written = fs::write(&temp, content).and_then(|()| fs::rename(&temp, path));
    if let Err(error) = written {
        let _ = fs::remove_file(&temp);
        return Err(failed(path, error));
    }
    Ok(())
}

/// `document`: its `text`, or the file at `path`, read now.
fn document(here: &Here) -> Result<String, ModuleError> {
    let text = here.text("text");
    if !text.is_empty() {
        return Ok(text);
    }
    let path = here.text("path");
    if path.trim().is_empty() {
        return Err(ModuleError::InvalidParams(format!(
            "document `{}` has neither `text` nor `path`",
            here.me.node_id
        )));
    }
    here.read(&here.resolve(&path)?)
}

fn describe(here: &Here) -> Result<Value, ModuleError> {
    let fail: HookFailure = match here.text("fail").as_str() {
        "" => HookFailure::Open,
        fail => parse(json!(fail))?,
    };
    serde_json::to_value(Hook {
        event: here.text("event"),
        matcher: None,
        timeout_ms: None,
        returns: vec![HookEffect::Context],
        fail,
    })
    .map_err(|e| ModuleError::Failed(e.to_string()))
}

/// On its event, the document as context; on any other, nothing.
fn answer(here: &Here, request: HookRequest) -> Result<Value, ModuleError> {
    let answer = if request.event == here.text("event") {
        HookAnswer::Context {
            text: document(here)?,
        }
    } else {
        HookAnswer::Nothing
    };
    serde_json::to_value(answer).map_err(|e| ModuleError::Failed(e.to_string()))
}

impl Default for Files {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Files {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            method::NODE_EVENT => self.event(parse(params)?),
            method::NODE_START => self.start(parse(params)?),
            method::NODE_STOP => self.stop(parse(params)?),
            HOOK_DESCRIBE => describe(&self.here(parse(params)?)),
            method::NODE_HOOK => {
                let mut call: NodeCall = parse(params)?;
                let request = parse(std::mem::take(&mut call.args))?;
                answer(&self.here(call), request)
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.watching.stop_all();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("regulus-files-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn call(name: &str, node_type: &str, config: Value, args: Value) -> Result<Value, ModuleError> {
        let node = json!({ "id": "d", "type": node_type, "module_id": "regulus/files@0.1.0",
                           "config": config });
        Files::new().call(
            name,
            json!({ "execution_id": "e", "node": node, "args": args }),
        )
    }

    #[test]
    fn a_glob_matches_files_in_order_and_a_plain_path_is_itself() {
        let dir = temp("glob");
        for name in ["b.md", "a.md", "c.txt"] {
            fs::write(dir.join(name), name).unwrap();
        }
        let found = matches(&dir.join("*.md"), 10).unwrap();
        assert_eq!(found, [dir.join("a.md"), dir.join("b.md")]);
        assert!(matches(&dir.join("*.rs"), 10).is_err());
        assert!(matches(&dir.join("*.md"), 1).is_err(), "over max_files");
        assert_eq!(matches(&dir.join("x"), 1).unwrap(), [dir.join("x")]);
        assert!(
            read_bounded(&dir.join("a.md"), 2).is_err(),
            "over max_bytes"
        );
        assert_eq!(read_bounded(&dir.join("a.md"), 4).unwrap(), "a.md");
        assert!(
            read_bounded(&dir.join("a.md"), 0).is_err(),
            "no unbounded read"
        );
    }

    #[test]
    fn a_document_hooks_in_as_context_on_its_event_only() {
        let dir = temp("document");
        let path = dir.join("RULES.md");
        fs::write(&path, "Answer in Spanish.").unwrap();
        let config = json!({ "path": path });
        let described = call(HOOK_DESCRIBE, DOCUMENT, config.clone(), Value::Null).unwrap();
        assert_eq!(
            described,
            json!({ "event": "prompt", "returns": ["context"] })
        );
        let ask = |event: &str| {
            call(
                method::NODE_HOOK,
                DOCUMENT,
                config.clone(),
                json!({ "event": event }),
            )
            .unwrap()
        };
        assert_eq!(
            ask("prompt"),
            json!({ "decision": "context", "text": "Answer in Spanish." })
        );
        // Read every turn: an edit shows at once.
        fs::write(&path, "Answer in French.").unwrap();
        assert_eq!(ask("prompt")["text"], "Answer in French.");
        assert_eq!(ask("stop"), json!({ "decision": "none" }));
        let inline = json!({ "text": "inline", "event": "session_start", "fail": "closed" });
        let described = call(HOOK_DESCRIBE, DOCUMENT, inline, Value::Null).unwrap();
        assert_eq!(described["fail"], "closed");
        assert!(call(
            method::NODE_HOOK,
            DOCUMENT,
            json!({}),
            json!({ "event": "prompt" })
        )
        .is_err());
    }

    #[test]
    fn only_regular_files_are_read_and_never_past_the_cap() {
        let dir = temp("special");
        assert!(read_bounded(&dir, 10).is_err(), "a folder");
        #[cfg(unix)]
        {
            assert!(read_bounded(Path::new("/dev/zero"), 10).is_err());
            let fifo = dir.join("fifo");
            let made = std::process::Command::new("mkfifo").arg(&fifo).status();
            if made.is_ok_and(|s| s.success()) {
                // Opening a FIFO with no writer would block for ever.
                assert!(read_bounded(&fifo, 10).is_err());
            }
        }
        fs::write(dir.join("big"), "0123456789").unwrap();
        assert!(read_bounded(&dir.join("big"), 9).is_err());
        assert_eq!(read_bounded(&dir.join("big"), 10).unwrap(), "0123456789");
    }

    #[test]
    fn values_are_written_as_text_or_indented_json() {
        assert_eq!(as_text(&json!("plain")), "plain");
        assert_eq!(as_text(&json!({ "a": 1 })), "{\n  \"a\": 1\n}");
    }
}
