//! One thread that runs the deadlines of every pending decision. A copy of the timer of
//! `guards` (modules do not share code): it starts with the first timer, never on the bus thread.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

const THREAD: &str = "regulus-human";

type Job = Box<dyn FnOnce() + Send>;

#[derive(Default)]
pub(crate) struct Timers {
    queue: Mutex<Option<mpsc::Sender<(Instant, Job)>>>,
}

impl Timers {
    /// Runs `job` on the timer thread after `delay`. A job that is no longer wanted
    /// checks that itself when it runs (the decision may be gone).
    pub fn after(&self, delay: Duration, job: impl FnOnce() + Send + 'static) {
        // A wait too long to represent never comes due.
        let Some(at) = Instant::now().checked_add(delay) else {
            return;
        };
        let mut job: Job = Box::new(job);
        let mut queue = self.queue.lock().unwrap_or_else(|e| e.into_inner());
        // A thread that is gone hands the job back: start another one, once.
        for _ in 0..2 {
            if queue.is_none() {
                let (send, receive) = mpsc::channel();
                if thread::Builder::new()
                    .name(THREAD.into())
                    .spawn(move || run(receive))
                    .is_err()
                {
                    return;
                }
                *queue = Some(send);
            }
            let Some(send) = queue.as_ref() else {
                return;
            };
            match send.send((at, job)) {
                Ok(()) => return,
                Err(mpsc::SendError((_, back))) => {
                    job = back;
                    *queue = None;
                }
            }
        }
    }

    /// Ends the thread; timers still waiting never run.
    pub fn stop(&self) {
        self.queue.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

fn run(receive: mpsc::Receiver<(Instant, Job)>) {
    let mut due: BinaryHeap<Reverse<(Instant, u64)>> = BinaryHeap::new();
    let mut jobs: HashMap<u64, Job> = HashMap::new();
    let mut next = 0u64;
    loop {
        let now = Instant::now();
        while let Some(Reverse((at, id))) = due.peek().copied() {
            if at > now {
                break;
            }
            due.pop();
            if let Some(job) = jobs.remove(&id) {
                // One job that panics must not take every other deadline with it.
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
            }
        }
        let received = match due.peek() {
            None => receive.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(Reverse((at, _))) => {
                receive.recv_timeout(at.saturating_duration_since(Instant::now()))
            }
        };
        match received {
            Ok((at, job)) => {
                next += 1;
                due.push(Reverse((at, next)));
                jobs.insert(next, job);
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn jobs_run_in_the_order_they_are_due() {
        let timers = Timers::default();
        let ran = Arc::new(Mutex::new(Vec::new()));
        for (n, ms) in [(1, 60), (2, 10), (3, 30)] {
            let ran = ran.clone();
            timers.after(Duration::from_millis(ms), move || {
                ran.lock().unwrap().push(n)
            });
        }
        let deadline = Instant::now() + Duration::from_secs(5);
        while ran.lock().unwrap().len() < 3 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*ran.lock().unwrap(), [2, 3, 1]);
        timers.stop();
    }

    #[test]
    fn a_job_that_panics_leaves_the_others_running() {
        let timers = Timers::default();
        let ran = Arc::new(Mutex::new(false));
        timers.after(Duration::from_millis(5), || panic!("a broken deadline"));
        let flag = ran.clone();
        timers.after(Duration::from_millis(30), move || {
            *flag.lock().unwrap() = true
        });
        timers.after(Duration::MAX, || unreachable!("never due"));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !*ran.lock().unwrap() {
            assert!(Instant::now() < deadline, "the timer thread died");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
