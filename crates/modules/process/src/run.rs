//! Child processes: each one in a process group of its own, tracked per node so that
//! stopping the node kills the whole group and waits until it is reaped.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Signal {
    /// Asks it to end.
    Term,
    /// Ends it.
    Kill,
}

/// Sends `signal` to the process group led by `pid`. Never to 0, 1 or a negative id:
/// those would reach our own group, init or every process we may signal.
#[cfg(unix)]
pub(crate) fn signal_group(pid: u32, signal: Signal) {
    let signal = match signal {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    let Ok(group) = libc::pid_t::try_from(pid) else {
        return;
    };
    if group <= 1 {
        return;
    }
    // SAFETY: `killpg` only sends a signal; it reads no memory of ours. `pid` leads a group
    // we created (`process_group(0)`, or `setsid` for a terminal) and is not reaped yet:
    // waiters drop it from `Runs` while the leader is still a zombie (`wait_exited`), so
    // its id cannot have been reused.
    unsafe {
        libc::killpg(group, signal);
    }
}

#[cfg(not(unix))]
pub(crate) fn signal_group(_pid: u32, _signal: Signal) {
    // Without process groups the waiter's own `kill` is all there is.
}

/// Blocks until the child `pid` has exited, without reaping it: until `wait` reaps it, its
/// id (and so its group's) stays ours and signalling the group is safe.
#[cfg(unix)]
pub(crate) fn wait_exited(pid: u32) {
    let pid = pid as libc::id_t;
    loop {
        // SAFETY: `siginfo_t` is plain data that `waitid` fills in; zeroed is a valid value.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid, exclusive pointer for the duration of the call.
        let done =
            unsafe { libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOWAIT) };
        if done == 0 || io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            return;
        }
    }
}

#[cfg(not(unix))]
pub(crate) fn wait_exited(_pid: u32) {}

#[derive(Debug, Default)]
struct NodeRuns {
    /// Leaders of the groups still running.
    pids: HashSet<u32>,
    /// `node/stop` came: nothing new starts for this node until `node/start` again.
    stopped: bool,
}

/// Why a run did not start.
pub(crate) enum Refused {
    /// The node is not running or already runs as many as it may.
    Node(String),
    /// The program could not be spawned; the node reports it as a failed run.
    Spawn(io::Error),
}

/// Every child of every started node of this module. A node has an entry from
/// `node/start` until `node/stop` has reaped all its children.
#[derive(Debug, Default)]
pub(crate) struct Runs {
    nodes: Mutex<HashMap<NodeRef, NodeRuns>>,
    ended: Condvar,
}

impl Runs {
    fn lock(&self) -> MutexGuard<'_, HashMap<NodeRef, NodeRuns>> {
        self.nodes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `node/start`: the node may run programs. Children of an earlier stop that are still
    /// being killed stay tracked.
    pub fn start(&self, node: &NodeRef) {
        self.lock().entry(node.clone()).or_default().stopped = false;
    }

    /// Whether the node is started and not stopped.
    pub fn is_running(&self, node: &NodeRef) -> bool {
        self.lock().get(node).is_some_and(|runs| !runs.stopped)
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
        let Some(runs) = nodes.get_mut(node).filter(|runs| !runs.stopped) else {
            return Err(Refused::Node(format!(
                "node `{}` is not running",
                node.node_id
            )));
        };
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

    /// The leader `pid` has exited and is about to be reaped: from here on nobody signals
    /// its group. A stopped node with nothing left loses its entry.
    pub fn ended(&self, node: &NodeRef, pid: u32) {
        let mut nodes = self.lock();
        if let Some(runs) = nodes.get_mut(node) {
            runs.pids.remove(&pid);
            if runs.stopped && runs.pids.is_empty() {
                nodes.remove(node);
            }
        }
        drop(nodes);
        self.ended.notify_all();
    }

    /// Stops the node at once: `SIGTERM` to each group (`SIGKILL` with no grace) and a
    /// reaper thread that sends `SIGKILL` to what is left after `grace`. Never waits: it
    /// runs inside `node/stop`. Nothing new starts for the node until `start`.
    pub fn stop(self: &Arc<Self>, node: &NodeRef, grace: Duration) {
        let pids: Vec<u32> = {
            let mut nodes = self.lock();
            let Some(runs) = nodes.get_mut(node) else {
                return;
            };
            if runs.pids.is_empty() {
                nodes.remove(node);
                return;
            }
            runs.stopped = true;
            runs.pids.iter().copied().collect()
        };
        let first = if grace.is_zero() {
            Signal::Kill
        } else {
            Signal::Term
        };
        for pid in &pids {
            signal_group(*pid, first);
        }
        if grace.is_zero() {
            return;
        }
        let (runs, target) = (self.clone(), node.clone());
        let reaper = move || {
            if runs.wait_gone(&target, &pids, grace) {
                return;
            }
            runs.kill_left(&target, &pids);
        };
        if let Err(error) = thread::Builder::new().name(THREAD.into()).spawn(reaper) {
            // No thread to wait out the grace: kill now rather than leave them running.
            eprintln!("regulus: process: no reaper thread ({error}); killing at once");
            for pid in self
                .lock()
                .get(node)
                .into_iter()
                .flat_map(|r| r.pids.iter())
            {
                signal_group(*pid, Signal::Kill);
            }
        }
    }

    /// `SIGKILL` to those of `pids` the node still has: the others were reaped.
    fn kill_left(&self, node: &NodeRef, pids: &[u32]) {
        let nodes = self.lock();
        if let Some(runs) = nodes.get(node) {
            for pid in pids.iter().filter(|pid| runs.pids.contains(pid)) {
                signal_group(*pid, Signal::Kill);
            }
        }
    }

    /// Every node, no grace, and waits a moment for the waiters: `shutdown` leaves nothing.
    pub fn stop_all(self: &Arc<Self>) {
        let nodes: Vec<(NodeRef, Vec<u32>)> = self
            .lock()
            .iter()
            .map(|(node, runs)| (node.clone(), runs.pids.iter().copied().collect()))
            .collect();
        for (node, pids) in nodes {
            self.stop(&node, Duration::ZERO);
            self.wait_gone(&node, &pids, REAP_WAIT);
        }
    }

    /// Waits until none of `pids` is left in `node`, at most `wait`. True if none is.
    fn wait_gone(&self, node: &NodeRef, pids: &[u32], wait: Duration) -> bool {
        let deadline = Instant::now() + wait;
        let mut nodes = self.lock();
        loop {
            let gone = nodes
                .get(node)
                .is_none_or(|runs| !pids.iter().any(|pid| runs.pids.contains(pid)));
            let now = Instant::now();
            if gone || now >= deadline {
                return gone;
            }
            nodes = self
                .ended
                .wait_timeout(nodes, deadline - now)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }
}

/// How long `shutdown` waits for killed groups to be reaped. Not a setting: `SIGKILL`
/// cannot be caught, so this only covers the waiters' own turn.
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

/// The leader of a run has exited (or not): what a timeout watcher checks before it
/// signals, under the same lock the waiter takes when the leader exits.
#[derive(Debug, Default)]
struct Watch {
    exited: bool,
    timed_out: bool,
}

/// Kills the group led by `pid` if its leader has not exited within `timeout`. Call
/// `exited` on the result as soon as the leader exits, before reaping it.
pub(crate) struct Watcher {
    state: Arc<Mutex<Watch>>,
    done: Option<mpsc::Sender<()>>,
}

impl Watcher {
    pub fn start(pid: u32, timeout: Option<Duration>) -> Self {
        let state = Arc::new(Mutex::new(Watch::default()));
        let (done, wait) = mpsc::channel::<()>();
        if let Some(timeout) = timeout {
            let state = state.clone();
            let watch = move || {
                if let Err(RecvTimeoutError::Timeout) = wait.recv_timeout(timeout) {
                    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
                    if !state.exited {
                        state.timed_out = true;
                        signal_group(pid, Signal::Kill);
                    }
                }
            };
            let _ = thread::Builder::new().name(THREAD.into()).spawn(watch);
        }
        Self {
            state,
            done: Some(done),
        }
    }

    /// The leader exited: the watcher will not signal. Says whether it timed out first.
    pub fn exited(&mut self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.exited = true;
        self.done.take();
        state.timed_out
    }
}

/// Reads a pipe on a thread of its own; the result arrives on the channel.
fn reader(pipe: Option<impl Read + Send + 'static>, max: usize) -> mpsc::Receiver<Vec<u8>> {
    let (send, receive) = mpsc::channel();
    match pipe {
        None => {
            let _ = send.send(Vec::new());
        }
        Some(pipe) => {
            let read = move || {
                let _ = send.send(read_capped(pipe, max));
            };
            // If no thread starts, the sender is dropped with the closure: the run reads
            // as having printed nothing.
            let _ = thread::Builder::new().name(THREAD.into()).spawn(read);
        }
    }
    receive
}

/// How a run is bounded.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub timeout: Option<Duration>,
    pub max_output: usize,
    /// After the leader exits, how long what it left behind may keep its pipes open.
    pub pipe_grace: Duration,
}

/// Feeds `stdin`, drains stdout and stderr, waits for the leader, then kills whatever it
/// left in its group (after `pipe_grace` for the pipes), and says how it ended. `ended`
/// runs once the leader has exited and before it is reaped. Blocks: run it on a thread
/// of its own.
pub(crate) fn collect(
    mut child: Child,
    stdin: Option<String>,
    limits: Limits,
    started: Instant,
    ended: impl FnOnce(),
) -> Exit {
    let pid = child.id();
    let mut watcher = Watcher::start(pid, limits.timeout);
    if let (Some(text), Some(mut pipe)) = (stdin, child.stdin.take()) {
        // A thread of its own: a program that prints before it reads must not deadlock.
        let _ = thread::Builder::new().name(THREAD.into()).spawn(move || {
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    let stdout = reader(child.stdout.take(), limits.max_output);
    let stderr = reader(child.stderr.take(), limits.max_output);
    wait_exited(pid);
    let timed_out = watcher.exited();
    let deadline = Instant::now() + limits.pipe_grace;
    let read = |pipe: &mpsc::Receiver<Vec<u8>>| {
        pipe.recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .ok()
    };
    let (mut out, mut err) = (read(&stdout), read(&stderr));
    // The leader is a zombie, so its group id is still ours: whatever it left behind
    // goes, and with it the last holders of the pipes.
    signal_group(pid, Signal::Kill);
    if out.is_none() {
        out = stdout.recv().ok();
    }
    if err.is_none() {
        err = stderr.recv().ok();
    }
    ended();
    let status = child.wait();
    let mut exit = finished(status, started);
    exit.stdout = String::from_utf8_lossy(&out.unwrap_or_default()).into_owned();
    exit.stderr = String::from_utf8_lossy(&err.unwrap_or_default()).into_owned();
    exit.timed_out = timed_out;
    exit
}

/// Waits for a child whose output goes elsewhere (a job's log), killing its group past
/// `timeout`. When the leader exits, `ended` runs (before it is reaped) and says whether
/// the node was stopped; then, or on a timeout, what the job left in its group is killed.
pub(crate) fn wait(
    mut child: Child,
    timeout: Option<Duration>,
    started: Instant,
    ended: impl FnOnce() -> bool,
) -> Exit {
    let pid = child.id();
    let mut watcher = Watcher::start(pid, timeout);
    wait_exited(pid);
    let timed_out = watcher.exited();
    let stopped = ended();
    if timed_out || stopped {
        signal_group(pid, Signal::Kill);
    }
    let status = child.wait();
    let mut exit = finished(status, started);
    exit.timed_out = timed_out;
    exit
}

/// A child whose waiting thread could not start (its `Child` went with the closure):
/// kill its group and reap it here.
#[cfg(unix)]
pub(crate) fn abandon(pid: u32) {
    signal_group(pid, Signal::Kill);
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return;
    };
    let mut status = 0;
    // SAFETY: `status` is a valid, exclusive pointer for the duration of the call; `pid`
    // is a child of ours nobody else waits on.
    unsafe {
        libc::waitpid(pid, &mut status, 0);
    }
}

#[cfg(not(unix))]
pub(crate) fn abandon(_pid: u32) {}

fn finished(status: io::Result<ExitStatus>, started: Instant) -> Exit {
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
    fn a_node_runs_again_after_a_stop_and_leaves_no_entry_behind() {
        let runs = Arc::new(Runs::default());
        let node = NodeRef {
            execution_id: "e".into(),
            node_id: "n".into(),
        };
        let spawn = |runs: &Runs| runs.spawn(&node, 0, || Ok((0, ()))).is_ok();
        assert!(!spawn(&runs), "not started yet");
        runs.start(&node);
        assert!(spawn(&runs));
        runs.ended(&node, 0);
        runs.stop(&node, Duration::from_secs(1));
        assert_eq!(runs.len(), 0);
        assert!(!spawn(&runs), "stopped");
        runs.start(&node);
        assert!(spawn(&runs));
    }

    #[test]
    fn read_capped_drains_everything_and_keeps_the_start() {
        let data = vec![b'x'; 20_000];
        assert_eq!(read_capped(&data[..], 10).len(), 10);
        assert_eq!(read_capped(&data[..], 1 << 20).len(), 20_000);
    }
}
