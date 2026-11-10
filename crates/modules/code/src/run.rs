//! The programs a workspace runs: an agent's command line, and git. Each one in a
//! process group of its own, with a time limit and its output kept within a size, and
//! nothing of it left running once it ends: what a command started in the background
//! goes with it. Stopping the node kills what it has running.
//!
//! The process-group handling is adapted from the process module
//! (crates/modules/process/src/run.rs); modules share no code.

use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::path::PathBuf;
use std::process::Child;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

/// A node of an execution: `(execution id, node id)`.
pub(crate) type Key = (String, String);

/// The threads that read and wait on a program.
#[cfg(unix)]
const THREAD: &str = "regulus-code-run";

/// How much of a stream is kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Keep {
    /// Its first bytes.
    Head(usize),
    /// Its first and its last bytes, half each: a build says what broke at the end.
    Ends(usize),
}

/// What to run.
#[derive(Debug, Clone)]
pub(crate) struct Spec {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// The whole environment: nothing is inherited.
    pub env: Vec<(String, String)>,
    pub timeout: Duration,
    /// What the group has to end after SIGTERM before SIGKILL.
    pub grace: Duration,
    /// What is left running has this long to close the output once the program ended.
    pub pipe_grace: Duration,
    pub stdout: Keep,
    pub stderr: Keep,
}

/// How a program ended.
#[derive(Debug, Default)]
pub(crate) struct Done {
    /// Its exit code; none when a signal ended it.
    pub code: Option<i32>,
    pub signal: Option<i32>,
    pub timed_out: bool,
    pub ms: u64,
    pub stdout: Vec<u8>,
    /// There was more output than was kept.
    pub stdout_cut: bool,
    pub stderr: Vec<u8>,
    pub stderr_cut: bool,
}

impl Done {
    pub fn succeeded(&self) -> bool {
        self.code == Some(0) && !self.timed_out
    }
}

/// Why a program did not run.
#[derive(Debug)]
pub(crate) enum Refused {
    /// The node is not running, or runs as many as it may.
    Node(String),
    /// The program could not be started.
    Spawn(io::Error),
}

/// A stream as it is read: bounded, whatever the program prints.
#[derive(Debug)]
struct Capture {
    keep: Keep,
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
}

impl Capture {
    fn new(keep: Keep) -> Self {
        Self {
            keep,
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
        }
    }

    fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let (head_room, tail_room) = match self.keep {
            Keep::Head(max) => (max, 0),
            Keep::Ends(max) => (max - max / 2, max / 2),
        };
        let taken = head_room.saturating_sub(self.head.len()).min(bytes.len());
        self.head.extend_from_slice(&bytes[..taken]);
        let rest = &bytes[taken..];
        if tail_room == 0 || rest.is_empty() {
            return;
        }
        if rest.len() >= tail_room {
            self.tail.clear();
            self.tail.extend(&rest[rest.len() - tail_room..]);
            return;
        }
        let over = (self.tail.len() + rest.len()).saturating_sub(tail_room);
        self.tail.drain(..over);
        self.tail.extend(rest);
    }

    /// What was kept, with a line where the middle went, and whether anything did.
    fn finish(&self) -> (Vec<u8>, bool) {
        let kept = (self.head.len() + self.tail.len()) as u64;
        let cut = self.total > kept;
        let mut out = self.head.clone();
        if cut && !self.tail.is_empty() {
            out.extend_from_slice(format!("\n[… {} bytes cut …]\n", self.total - kept).as_bytes());
        }
        out.extend(self.tail.iter());
        (out, cut)
    }
}

#[derive(Debug, Default)]
struct NodeRuns {
    /// Leaders of the groups still running.
    pids: HashSet<u32>,
    /// `node/stop` came: nothing new starts for this node until `node/start` again.
    stopped: bool,
}

/// The commands of every started workspace node. A node has an entry from `node/start`
/// until `node/stop` and the end of what it was running.
#[derive(Debug, Default)]
pub(crate) struct Runs {
    nodes: Mutex<HashMap<Key, NodeRuns>>,
}

impl Runs {
    fn lock(&self) -> MutexGuard<'_, HashMap<Key, NodeRuns>> {
        self.nodes.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// `node/start`: the node may run commands.
    pub fn start(&self, key: &Key) {
        self.lock().entry(key.clone()).or_default().stopped = false;
    }

    /// Whether the node is started and not stopped.
    pub fn is_running(&self, key: &Key) -> bool {
        self.lock().get(key).is_some_and(|runs| !runs.stopped)
    }

    /// Spawns under the lock, so a `stop` either sees the new group or refuses the spawn.
    /// `max` 0: no limit.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn spawn(
        &self,
        key: &Key,
        max: usize,
        spawn: impl FnOnce() -> io::Result<Child>,
    ) -> Result<Child, Refused> {
        let mut nodes = self.lock();
        let Some(runs) = nodes.get_mut(key).filter(|runs| !runs.stopped) else {
            return Err(Refused::Node(format!(
                "the workspace `{}` is not running",
                key.1
            )));
        };
        if max > 0 && runs.pids.len() >= max {
            return Err(Refused::Node(format!(
                "the workspace `{}` already runs {max} commands at once (`max_running`)",
                key.1
            )));
        }
        let child = spawn().map_err(Refused::Spawn)?;
        runs.pids.insert(child.id());
        Ok(child)
    }

    /// The leader `pid` has exited and is about to be reaped: from here on nobody signals
    /// its group. A stopped node with nothing left loses its entry.
    #[cfg_attr(not(unix), allow(dead_code))]
    fn ended(&self, key: &Key, pid: u32) {
        let mut nodes = self.lock();
        if let Some(runs) = nodes.get_mut(key) {
            runs.pids.remove(&pid);
            if runs.stopped && runs.pids.is_empty() {
                nodes.remove(key);
            }
        }
    }

    /// `node/stop`: kills what the node has running, at once, and lets nothing new start.
    /// The signals go out under the lock: a leader leaves `pids` before it is reaped, so
    /// every id here is still its group's.
    pub fn stop(&self, key: &Key) {
        let mut nodes = self.lock();
        let Some(runs) = nodes.get_mut(key) else {
            return;
        };
        if runs.pids.is_empty() {
            nodes.remove(key);
            return;
        }
        runs.stopped = true;
        for pid in &runs.pids {
            signal_group(*pid, Signal::Kill);
        }
    }

    /// Every node: the module is shutting down.
    pub fn stop_all(&self) {
        let keys: Vec<Key> = self.lock().keys().cloned().collect();
        for key in keys {
            self.stop(&key);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Signal {
    /// Asks it to end.
    #[cfg_attr(not(unix), allow(dead_code))]
    Term,
    /// Ends it.
    Kill,
}

/// Sends `signal` to the process group led by `pid`. Never to 0, 1 or a negative id:
/// those would reach our own group, init or every process we may signal.
#[cfg(unix)]
fn signal_group(pid: u32, signal: Signal) {
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
    // SAFETY: `killpg` only sends a signal; it reads no memory of ours. `pid` leads a
    // group we created (`process_group(0)`) and is not reaped yet: `exec` signals only
    // before it reaps, and `Runs::stop` only ids still in `pids`, which a leader leaves
    // before it is reaped. So the id cannot have been reused.
    unsafe {
        libc::killpg(group, signal);
    }
}

#[cfg(not(unix))]
fn signal_group(_pid: u32, _signal: Signal) {}

/// Blocks until the child `pid` has exited, without reaping it: until `wait` reaps it, its
/// id (and so its group's) stays ours and signalling the group is safe.
#[cfg(unix)]
fn wait_exited(pid: u32) {
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

/// Runs `spec` to its end and returns how it went. With `track`, the program counts as
/// one of that node's commands: refused past its limit, killed when the node stops.
#[cfg(unix)]
pub(crate) fn exec(spec: &Spec, track: Option<(&Runs, &Key, usize)>) -> Result<Done, Refused> {
    use std::io::Read;
    use std::os::unix::process::{CommandExt, ExitStatusExt};
    use std::process::{Command, Stdio};
    use std::sync::{mpsc, Arc};
    use std::thread;
    use std::time::Instant;

    fn lock(capture: &Mutex<Capture>) -> MutexGuard<'_, Capture> {
        capture.lock().unwrap_or_else(|e| e.into_inner())
    }

    let mut command = Command::new(&spec.program);
    command
        .args(&spec.args)
        .current_dir(&spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // A group of its own: a signal to it reaches whatever the program started.
        .process_group(0);
    let started = Instant::now();
    let mut child = match track {
        Some((runs, key, max)) => runs.spawn(key, max, || command.spawn())?,
        None => command.spawn().map_err(Refused::Spawn)?,
    };
    let pid = child.id();

    let out = Arc::new(Mutex::new(Capture::new(spec.stdout)));
    let err = Arc::new(Mutex::new(Capture::new(spec.stderr)));
    let (closed, pipes) = mpsc::channel::<()>();
    let mut open = 0usize;
    let mut read = |from: Option<Box<dyn Read + Send>>, into: &Arc<Mutex<Capture>>| {
        let (Some(mut from), into, closed) = (from, into.clone(), closed.clone()) else {
            return;
        };
        let reader = move || {
            let mut buffer = [0u8; 8192];
            loop {
                match from.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => lock(&into).push(&buffer[..n]),
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = closed.send(());
        };
        if thread::Builder::new()
            .name(THREAD.into())
            .spawn(reader)
            .is_ok()
        {
            open += 1;
        }
    };
    read(
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        &out,
    );
    read(
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        &err,
    );
    drop(closed);

    let (exited, exit) = mpsc::channel::<()>();
    let waiting = thread::Builder::new()
        .name(THREAD.into())
        .spawn(move || {
            wait_exited(pid);
            let _ = exited.send(());
        })
        .is_ok();
    let mut timed_out = false;
    if !waiting {
        // Nobody to watch the clock: end it now rather than let it run unbounded.
        signal_group(pid, Signal::Kill);
        wait_exited(pid);
    } else if exit.recv_timeout(spec.timeout).is_err() {
        timed_out = true;
        signal_group(pid, Signal::Term);
        if exit.recv_timeout(spec.grace).is_err() {
            signal_group(pid, Signal::Kill);
            let _ = exit.recv();
        }
    }

    // The leader has exited and is not reaped yet, so its group id is still ours. What it
    // left running has a moment to let go of the output; then the group goes with it.
    let drain = |open: &mut usize| {
        let deadline = Instant::now() + spec.pipe_grace;
        while *open > 0 {
            match pipes.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                Ok(()) => *open -= 1,
                Err(_) => break,
            }
        }
    };
    drain(&mut open);
    signal_group(pid, Signal::Kill);
    if let Some((runs, key, _)) = track {
        runs.ended(key, pid);
    }
    let status = child.wait();
    // Killed, the rest of the group closes the pipes. Something that left the group may
    // keep them open: its output is given up on, and its reader ends when it does.
    drain(&mut open);

    let (stdout, stdout_cut) = lock(&out).finish();
    let (stderr, stderr_cut) = lock(&err).finish();
    let (code, signal) = match status {
        Ok(status) => (status.code(), status.signal()),
        Err(_) => (None, None),
    };
    Ok(Done {
        code,
        signal,
        timed_out,
        ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        stdout,
        stdout_cut,
        stderr,
        stderr_cut,
    })
}

/// Process groups, and so a command that leaves nothing behind, need Unix.
#[cfg(not(unix))]
pub(crate) fn exec(_spec: &Spec, _track: Option<(&Runs, &Key, usize)>) -> Result<Done, Refused> {
    Err(Refused::Node(
        "running programs from a code workspace needs a Unix host".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_head_keeps_the_first_bytes() {
        let mut capture = Capture::new(Keep::Head(4));
        capture.push(b"ab");
        capture.push(b"cdef");
        capture.push(b"gh");
        assert_eq!(capture.finish(), (b"abcd".to_vec(), true));
        let mut whole = Capture::new(Keep::Head(4));
        whole.push(b"abcd");
        assert_eq!(whole.finish(), (b"abcd".to_vec(), false));
    }

    #[test]
    fn ends_keep_the_start_and_the_end_and_say_what_went() {
        let mut capture = Capture::new(Keep::Ends(6));
        for chunk in [&b"abc"[..], b"defgh", b"i", b"jklmnopq", b"rs"] {
            capture.push(chunk);
        }
        let (kept, cut) = capture.finish();
        assert!(cut);
        assert_eq!(
            String::from_utf8(kept).unwrap(),
            "abc\n[… 13 bytes cut …]\nqrs"
        );
        let mut whole = Capture::new(Keep::Ends(6));
        whole.push(b"abcdef");
        assert_eq!(whole.finish(), (b"abcdef".to_vec(), false));
    }

    #[cfg(unix)]
    fn spec(line: &str, timeout_ms: u64) -> Spec {
        Spec {
            program: "sh".into(),
            args: vec!["-c".into(), line.into()],
            cwd: std::env::temp_dir(),
            env: vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())],
            timeout: Duration::from_millis(timeout_ms),
            grace: Duration::from_millis(200),
            pipe_grace: Duration::from_millis(200),
            stdout: Keep::Ends(1000),
            stderr: Keep::Ends(1000),
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_program_gives_its_code_and_both_streams() {
        let done = exec(&spec("echo out; echo err >&2; exit 3", 5000), None).unwrap();
        assert_eq!(
            (done.code, done.signal, done.timed_out),
            (Some(3), None, false)
        );
        assert_eq!(done.stdout, b"out\n");
        assert_eq!(done.stderr, b"err\n");
        assert!(!done.succeeded());
        assert!(exec(&spec("true", 5000), None).unwrap().succeeded());
    }

    #[cfg(unix)]
    #[test]
    fn the_environment_is_only_what_the_spec_gives() {
        std::env::set_var("REGULUS_CODE_RUN_LEAK", "secret");
        let done = exec(&spec("echo \"[$REGULUS_CODE_RUN_LEAK]\"", 5000), None).unwrap();
        assert_eq!(done.stdout, b"[]\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_program_is_refused_as_a_spawn() {
        let mut missing = spec("true", 1000);
        missing.program = "regulus-code-no-such-program".into();
        assert!(matches!(exec(&missing, None), Err(Refused::Spawn(_))));
    }

    #[cfg(unix)]
    #[test]
    fn untracked_nodes_and_full_nodes_are_refused() {
        let runs = Runs::default();
        let key: Key = ("exec-1".into(), "ws".into());
        assert!(matches!(
            exec(&spec("true", 1000), Some((&runs, &key, 1))),
            Err(Refused::Node(_))
        ));
        runs.start(&key);
        assert!(runs.is_running(&key));
        assert!(exec(&spec("true", 1000), Some((&runs, &key, 1))).is_ok());
        runs.stop(&key);
        assert!(
            !runs.is_running(&key),
            "nothing was running: the entry goes"
        );
    }
}
