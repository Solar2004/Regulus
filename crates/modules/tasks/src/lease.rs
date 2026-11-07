//! The end of each lease, on one thread of the module: started with the first lease,
//! sleeping until the earliest one, stopped at shutdown. Never on the bus thread.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::time::Duration;

use regulus_core::NodeRef;

use crate::{now_ms, Shared};

const THREAD: &str = "regulus-tasks-leases";

#[derive(Default)]
struct Due {
    /// When, and a claim node of the queue whose lease ends then.
    heap: BinaryHeap<Reverse<(u64, String, String)>>,
    running: bool,
    stopping: bool,
}

#[derive(Default)]
pub(crate) struct Leases {
    due: Mutex<Due>,
    wake: Condvar,
}

impl Leases {
    fn due(&self) -> std::sync::MutexGuard<'_, Due> {
        self.due.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Looks at `node`'s queue again at `until` (ms since the epoch). A look that finds
    /// nothing expired does nothing, so a deadline that no longer holds is harmless.
    pub fn at(&self, shared: &Arc<Shared>, until: u64, node: NodeRef) {
        let mut due = self.due();
        if due.stopping {
            return;
        }
        let entry = Reverse((until, node.execution_id, node.node_id));
        if due.heap.iter().any(|e| *e == entry) {
            return;
        }
        due.heap.push(entry);
        if !due.running {
            let weak = Arc::downgrade(shared);
            let started = std::thread::Builder::new()
                .name(THREAD.into())
                .spawn(move || run(weak));
            due.running = started.is_ok();
        }
        self.wake.notify_all();
    }

    pub fn shutdown(&self) {
        let mut due = self.due();
        due.stopping = true;
        due.heap.clear();
        self.wake.notify_all();
    }
}

fn run(shared: Weak<Shared>) {
    loop {
        let Some(strong) = shared.upgrade() else {
            return;
        };
        let leases = &strong.leases;
        let mut due = leases.due();
        let node = loop {
            if due.stopping {
                due.running = false;
                return;
            }
            let now = now_ms();
            match due.heap.peek() {
                Some(Reverse((until, _, _))) if *until <= now => {
                    let Reverse((_, execution_id, node_id)) = due.heap.pop().expect("peeked");
                    break NodeRef {
                        execution_id,
                        node_id,
                    };
                }
                Some(Reverse((until, _, _))) => {
                    let wait = Duration::from_millis(until - now);
                    due = leases
                        .wake
                        .wait_timeout(due, wait)
                        .unwrap_or_else(|e| e.into_inner())
                        .0;
                }
                None => {
                    due = leases.wake.wait(due).unwrap_or_else(|e| e.into_inner());
                }
            }
        };
        drop(due);
        strong.expire(&node);
    }
}
