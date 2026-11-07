//! `watch`: a file or folder watched from the node's start to its stop, with `notify`.
//! Changes go to a thread of the node's own, which waits for `debounce_ms` of quiet and
//! then sends each changed file once.
//!
//! A single file is watched through its folder: editors save by writing a new file and
//! renaming it over the old one, which a watch on the file itself would lose.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::Mutex;
use std::time::Duration;

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use regulus_core::{ModuleCtx, ModuleError, NodeRef};
use serde_json::{json, Value};

use crate::{failed, read_bounded, Here, YES};

const CHANGED: &str = "changed";
const TEXT: &str = "text";
/// The name of the threads that wait for quiet, as seen by debuggers and `top`.
const THREAD_NAME: &str = "regulus-files-watch";

enum Message {
    Changed(notify::Result<notify::Event>),
    Stop,
}

/// One watching node: dropping the watcher ends the watch; `Stop` ends the thread.
struct Watching {
    _watcher: RecommendedWatcher,
    stop: Sender<Message>,
}

#[derive(Default)]
pub(crate) struct Watchers {
    nodes: Mutex<HashMap<NodeRef, Watching>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// What a node sends, and where from.
struct Target {
    /// The file or folder the user named.
    path: PathBuf,
    /// A folder: every file in it counts. A file: only it does.
    folder: bool,
    ctx: ModuleCtx,
    me: NodeRef,
    debounce: Duration,
    max_bytes: u64,
    changed: bool,
    text: bool,
}

impl Target {
    fn counts(&self, path: &Path) -> bool {
        if self.folder {
            path.starts_with(&self.path) && !path.is_dir()
        } else {
            path == self.path
        }
    }

    fn send(&self, path: &Path) {
        let mut out: Vec<(&str, Value)> = Vec::new();
        if self.changed {
            out.push((CHANGED, json!(path.display().to_string())));
        }
        // A file that is gone has a path but no text.
        if self.text && path.is_file() {
            match read_bounded(path, self.max_bytes) {
                Ok(text) => out.push((TEXT, json!(text))),
                Err(error) => eprintln!("regulus: watch `{}`: {error}", self.me.node_id),
            }
        }
        for (port, value) in out {
            if let Err(error) = self.ctx.emit(&self.me, port, value) {
                eprintln!("regulus: watch `{}`: {error}", self.me.node_id);
            }
        }
    }

    /// Gathers changes until `debounce` passes without any, then sends them.
    fn run(self, messages: mpsc::Receiver<Message>) {
        let mut pending = BTreeSet::new();
        loop {
            let message = if pending.is_empty() {
                messages.recv().map_err(|_| RecvTimeoutError::Disconnected)
            } else {
                messages.recv_timeout(self.debounce)
            };
            match message {
                Ok(Message::Changed(Ok(event))) => {
                    if !matches!(event.kind, EventKind::Access(_)) {
                        pending.extend(event.paths.into_iter().filter(|p| self.counts(p)));
                    }
                }
                Ok(Message::Changed(Err(error))) => {
                    eprintln!("regulus: watch `{}`: {error}", self.me.node_id);
                }
                Err(RecvTimeoutError::Timeout) => {
                    for path in std::mem::take(&mut pending) {
                        self.send(&path);
                    }
                }
                Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

impl Watchers {
    /// Starts watching for `here`'s node; a node already watching starts over.
    pub fn start(&self, here: &Here) -> Result<Value, ModuleError> {
        let path = here.text("path");
        if path.trim().is_empty() {
            return Err(ModuleError::InvalidParams(format!(
                "watch `{}` has no `path`",
                here.me.node_id
            )));
        }
        let path = here.resolve(&path)?;
        let folder = path.is_dir();
        let (watched, mode) = match (folder, here.text("recursive") == YES) {
            (true, true) => (path.clone(), RecursiveMode::Recursive),
            (true, false) => (path.clone(), RecursiveMode::NonRecursive),
            (false, _) => {
                let parent = path.parent().unwrap_or(Path::new("/")).to_path_buf();
                std::fs::create_dir_all(&parent).map_err(|e| failed(&parent, e))?;
                (parent, RecursiveMode::NonRecursive)
            }
        };
        let target = Target {
            path,
            folder,
            ctx: here.files.ctx.clone(),
            me: here.me.clone(),
            debounce: Duration::from_millis(here.number("debounce_ms")),
            max_bytes: crate::positive(here, "max_bytes")?,
            changed: here.has_output(CHANGED),
            text: here.has_output(TEXT),
        };
        let (sender, messages) = mpsc::channel();
        let events = sender.clone();
        let mut watcher = notify::recommended_watcher(move |event| {
            let _ = events.send(Message::Changed(event));
        })
        .map_err(|e| failed(&watched, e))?;
        watcher
            .watch(&watched, mode)
            .map_err(|e| failed(&watched, e))?;
        std::thread::Builder::new()
            .name(THREAD_NAME.into())
            .spawn(move || target.run(messages))
            .map_err(|e| ModuleError::Failed(format!("watch cannot start its thread: {e}")))?;
        let watching = Watching {
            _watcher: watcher,
            stop: sender,
        };
        if let Some(old) = lock(&self.nodes).insert(here.me.clone(), watching) {
            let _ = old.stop.send(Message::Stop);
        }
        Ok(json!({ "watching": watched.display().to_string() }))
    }

    pub fn stop(&self, node: &NodeRef) {
        if let Some(watching) = lock(&self.nodes).remove(node) {
            let _ = watching.stop.send(Message::Stop);
        }
    }

    pub fn stop_all(&self) {
        for (_, watching) in lock(&self.nodes).drain() {
            let _ = watching.stop.send(Message::Stop);
        }
    }
}
