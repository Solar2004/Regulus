//! Child processes: each one in a process group of its own, tracked per node so that
//! stopping the node kills the whole group and waits until it is reaped.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

use regulus_core::NodeRef;

use crate::exit::Exit;

/// The threads that wait on children.
pub(crate) const THREAD: &str = "regulus-process";

/// What to run, already read from the node's fields.
#[derive(Debug, Clone)]
pub(crate) struct Spec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
}

impl Spec {
    /// The command, in a process group of its own (Unix): killing the group reaches
    /// whatever it started.
    pub fn command(&self) -> Command {
        let mut command = Command::new(&self.program);
        command
            .args(&self.args)
            .current_dir(&self.cwd)
            .envs(self.env.iter().map(|(k, v)| (k, v)));
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command
    }
}

/// How a run is bounded.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub timeout: Option<Duration>,
    pub max_output: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// Asks it to end.
    Term,
    /// Ends it.
    Kill,
}

/// Sends `signal` to the process group led by `pid`.
#[cfg(unix)]
pub(crate) fn signal_group(pid: u32, signal: Signal) {
    let signal = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    let Ok(group) = libc::pid_t::try_from(pid) else {
        return;
    };
    // SAFETY: `killpg` only sends a signal; it reads no memory of ours. `pid` leads a group
    // we created (`process_group(0)`, or `setsid` for a terminal) and is still unreaped:
    // `Runs` drops it only after its waiter reaped it.
    unsafe {
        libc::killpg(group, signal);
    }
}

#[cfg(not(unix))]
pub(crate) fn signal_group(_pid: u32, _signal: Signal) {
    // Without process groups the waiter's own `kill` is all there is (see `Runs::stop`).
}

#[derive(Debug, Default)]
struct NodeRuns {
    /// Leaders of the groups still running.
    pids: HashSet<u32>,
    /// `node/stop` came: nothing new starts for this node.
    stopped: bool,
}

/// Why a run did not start.
pub(crate) enum Refused {
    /// The node is stopped or already runs as many as it may.
    Node(String),
    /// The program could not be spawned; the node reports it as a failed run.
    Spawn(io::Error),
}

/// Every child of every node of this module.
#[derive(Debug, Default)]
pub(crate) struct Runs {
    nodes: Mutex<HashMap<NodeRef, NodeRuns>>,
    ended: Condvar,
}

impl Runs {
    fn lock(&self) -> MutexGuard<'_, HashMap<NodeRef, NodeRuns>> {
        self.nodes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Spawns under the lock, so a `stop` either sees the new pid or refuses the spawn.
    /// `spawn` returns the leader's pid and whatever the caller keeps. `max` 0: no limit.
    pub fn spawn<T>(
        &self,
        node: &NodeRef,
        max: usize,
        spawn: impl FnOnce() -> io::Result<(u32, T)>,
    ) -> Result<T, Refused> {
        let mut nodes = self.lock();
        let runs = nodes.entry(node.clone()).or_default();
        if runs.stopped {
            return Err(Refused::Node(format!("node `{}` is stopped", node.node_id)));
        }
        if max > 0 && runs.pids.len() >= max {
            return Err(Refused::Node(format!(
                "node `{}` already runs {max} at once (`max_running`)",
                node.node_id
            )));
        }
        let (pid, kept) = spawn().map_err(Refused::Spawn)?;
        runs.pids.insert(pid);
        Ok(kept)
    }

    /// A waiter reaped `pid`.
    pub fn ended(&self, node: &NodeRef, pid: u32) {
        let mut nodes = self.lock();
        if let Some(runs) = nodes.get_mut(node) {
            runs.pids.remove(&pid);
        }
        drop(nodes);
        self.ended.notify_all();
    }

    /// Stops the node for good: `SIGTERM` to each group, `SIGKILL` to what is left after
    /// `grace`, then waits (as long again, at least a moment) for the waiters to reap
    /// them. The node's entry stays, marked stopped, so a value still on its way cannot
    /// start a run after this.
    pub fn stop(&self, node: &NodeRef, grace: Duration) {
        let pids: Vec<u32> = {
            let mut nodes = self.lock();
            let runs = nodes.entry(node.clone()).or_default();
            runs.stopped = true;
            runs.pids.iter().copied().collect()
        };
        if pids.is_empty() {
            return;
        }
        for pid in &pids {
            signal_group(
                *pid,
                if grace.is_zero() {
                    Signal::Kill
                } else {
                    Signal::Term
                },
            );
        }
        if self.wait_empty(node, grace) {
            return;
        }
        let left: Vec<u32> = self
            .lock()
            .get(node)
            .map(|runs| runs.pids.iter().copied().collect())
            .unwrap_or_default();
        for pid in left {
            signal_group(pid, Signal::Kill);
        }
        self.wait_empty(node, grace.max(REAP_WAIT));
    }

    /// Every node, no grace: what `shutdown` leaves behind is killed.
    pub fn stop_all(&self) {
        let nodes: Vec<NodeRef> = self.lock().keys().cloned().collect();
        for node in nodes {
            self.stop(&node, Duration::ZERO);
        }
    }

    /// Waits until `node` has no live child, at most `wait`. True if it has none.
    fn wait_empty(&self, node: &NodeRef, wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        let mut nodes = self.lock();
        loop {
            let empty = nodes.get(node).is_none_or(|runs| runs.pids.is_empty());
            let now = Instant::now();
            if empty || now >= deadline {
                return empty;
            }
            nodes = self
                .ended
                .wait_timeout(nodes, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
}

/// How long `stop` waits for a killed group to be reaped when the node gives no grace.
/// Not a setting: `SIGKILL` cannot be caught, so this only covers the waiter's own turn.
const REAP_WAIT: Duration = Duration::from_secs(1);

/// Reads all of `from`, keeping the first `max` bytes: the program never blocks on a
/// full pipe.
pub(crate) fn read_capped(mut from: impl Read, max: usize) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match from.read(&mut buf) {
            Ok(0) => return kept,
            Ok(n) => {
                let room = max.saturating_sub(kept.len());
                kept.extend_from_slice(&buf[..n.min(room)]);
            }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(_) => return kept,
        }
    }
}

/// Kills the group led by `pid` if `done` does not arrive within `timeout`; says whether
/// it did. Returns the sender to drop when the run is over.
pub(crate) fn watch(pid: u32, timeout: Option<Duration>) -> (mpsc::Sender<()>, Arc<AtomicBool>) {
    let (done, wait) = mpsc::channel::<()>();
    let timed_out = Arc::new(AtomicBool::new(false));
    if let Some(timeout) = timeout {
        let flag = timed_out.clone();
        let _ = thread::Builder::new().name(THREAD.into()).spawn(move || {
            if let Err(RecvTimeoutError::Timeout) = wait.recv_timeout(timeout) {
                flag.store(true, Ordering::SeqCst);
                signal_group(pid, Signal::Kill);
            }
        });
    }
    (done, timed_out)
}

/// Feeds `stdin`, drains stdout and stderr, waits for the child and says how it ended.
/// Blocks: run it on a thread of its own.
pub(crate) fn collect(
    mut child: Child,
    stdin: Option<String>,
    limits: Limits,
    started: Instant,
) -> Exit {
    let pid = child.id();
    let (done, timed_out) = watch(pid, limits.timeout);
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A thread of its own: a program that prints before it reads must not deadlock.
        let _ = thread::Builder::new().name(THREAD.into()).spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let max = limits.max_output;
    let stderr = child.stderr.take().map(|pipe| {
        thread::Builder::new()
            .name(THREAD.into())
            .spawn(move || read_capped(pipe, max))
    });
    let stdout = child
        .stdout
        .take()
        .map(|pipe| read_capped(pipe, max))
        .unwrap_or_default();
    let stderr = match stderr {
        Some(Ok(handle)) => handle.join().unwrap_or_default(),
        _ => Vec::new(),
    };
    let status = child.wait();
    drop(done);
    let mut exit = ended(status, started);
    exit.stdout = String::from_utf8_lossy(&stdout).into_owned();
    exit.stderr = String::from_utf8_lossy(&stderr).into_owned();
    exit.timed_out = timed_out.load(Ordering::SeqCst);
    exit
}

/// Waits for a child whose output goes elsewhere (a job's log), killing it past `timeout`.
pub(crate) fn wait(mut child: Child, timeout: Option<Duration>, started: Instant) -> Exit {
    let (done, timed_out) = watch(child.id(), timeout);
    let status = child.wait();
    drop(done);
    let mut exit = ended(status, started);
    exit.timed_out = timed_out.load(Ordering::SeqCst);
    exit
}

fn ended(status: io::Result<ExitStatus>, started: Instant) -> Exit {
    let ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    match status {
        Err(e) => Exit {
            ms,
            ..Exit::not_started(e)
        },
        Ok(status) => Exit {
            code: status.code(),
            signal: signal_of(&status),
            ms,
            ..Exit::default()
        },
    }
}

#[cfg(unix)]
fn signal_of(status: &ExitStatus) -> Option<i32> {
    use std::os::unix::process::ExitStatusExt;
    status.signal()
}

#[cfg(not(unix))]
fn signal_of(_status: &ExitStatus) -> Option<i32> {
    None
}

/// The next free log number in `dir`: one more than the highest `<n>.log`.
pub(crate) fn next_log(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 1;
    };
    entries
        .filter_map(|e| e.ok())
        .filter_map(|e| {
            let name = e.file_name();
            name.to_str()?.strip_suffix(".log")?.parse::<u64>().ok()
        })
        .max()
        .map_or(1, |n| n + 1)
}

/// The last `lines` lines of the file at `path`, read from its end.
pub(crate) fn tail(path: &Path, lines: usize) -> String {
    const BLOCK: u64 = 8192;
    let Ok(mut file) = File::open(path) else {
        return String::new();
    };
    let Ok(len) = file.seek(SeekFrom::End(0)) else {
        return String::new();
    };
    if lines == 0 {
        return String::new();
    }
    let mut start = len;
    let mut bytes = Vec::new();
    // One newline more than lines wanted, not counting a final one.
    while start > 0 {
        let from = start.saturating_sub(BLOCK);
        let mut block = vec![0u8; usize::try_from(start - from).unwrap_or(0)];
        if file.seek(SeekFrom::Start(from)).is_err() || file.read_exact(&mut block).is_err() {
            break;
        }
        block.extend_from_slice(&bytes);
        bytes = block;
        start = from;
        let body = bytes.strip_suffix(b"\n").unwrap_or(&bytes);
        if body.iter().filter(|b| **b == b'\n').count() >= lines {
            break;
        }
    }
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let kept: Vec<&str> = text.rsplit('\n').take(lines).collect();
    kept.into_iter().rev().collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tail_keeps_the_last_lines_across_blocks() {
        let dir = std::env::temp_dir().join(format!("regulus-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("1.log");
        let text: String = (1..=5000).map(|n| format!("line {n}\n")).collect();
        std::fs::write(&path, text).unwrap();
        assert_eq!(tail(&path, 3), "line 4998\nline 4999\nline 5000");
        assert_eq!(tail(&path, 0), "");
        std::fs::write(&path, "only").unwrap();
        assert_eq!(tail(&path, 5), "only");
        assert_eq!(next_log(&dir), 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn read_capped_drains_everything_and_keeps_the_start() {
        let data = vec![b'x'; 20_000];
        assert_eq!(read_capped(&data[..], 10).len(), 10);
        assert_eq!(read_capped(&data[..], 1 << 20).len(), 20_000);
    }
}
