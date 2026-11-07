//! Each value a node gets is handled on a thread of its own, at most `max_in_flight` at a
//! time per node; past that up to `max_queue` wait in order, and past that a value is
//! refused. Stopping a node drops what waits and discards what is in flight: no request
//! starts after it, and one in flight ends by its own `timeout_ms`.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::lock;

pub(crate) type Key = (String, String);

struct NodeState {
    running: usize,
    queue: VecDeque<Value>,
    stop: Arc<AtomicBool>,
}

#[derive(Default)]
pub(crate) struct Pool {
    nodes: Mutex<HashMap<Key, NodeState>>,
}

/// What happened to a value handed to the pool.
pub(crate) enum Taken {
    Running,
    Queued(usize),
    Refused(String),
}

impl Pool {
    /// Runs `work` on `value` now on a new thread, or queues it. `work` gets each value of
    /// the node in turn on that thread, and the node's stop flag.
    pub fn take(
        self: &Arc<Self>,
        key: Key,
        value: Value,
        max_in_flight: usize,
        max_queue: usize,
        work: impl Fn(Value, &AtomicBool) + Send + Sync + 'static,
    ) -> Taken {
        let stop = {
            let mut nodes = lock(&self.nodes);
            let state = nodes.entry(key.clone()).or_insert_with(|| NodeState {
                running: 0,
                queue: VecDeque::new(),
                stop: Arc::default(),
            });
            if state.running >= max_in_flight.max(1) {
                if state.queue.len() < max_queue {
                    state.queue.push_back(value);
                    return Taken::Queued(state.queue.len());
                }
                return Taken::Refused(format!(
                    "{} values in flight and {max_queue} waiting (`max_in_flight`, \
                     `max_queue`): this one is dropped",
                    state.running
                ));
            }
            state.running += 1;
            state.stop.clone()
        };
        let pool = self.clone();
        let spawned = std::thread::Builder::new()
            .name("regulus-web".into())
            .spawn({
                let key = key.clone();
                move || {
                    let mut value = value;
                    loop {
                        if stop.load(Ordering::SeqCst) {
                            return;
                        }
                        // A panic in a parser stays in this value: the node goes on.
                        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            work(value, &stop)
                        }));
                        let mut nodes = lock(&pool.nodes);
                        // A node stopped and started again under the same key has a new
                        // state: this thread belongs to the old one and leaves it alone.
                        let Some(state) = nodes
                            .get_mut(&key)
                            .filter(|state| Arc::ptr_eq(&state.stop, &stop))
                        else {
                            return;
                        };
                        match state.queue.pop_front() {
                            Some(next) => value = next,
                            None => {
                                state.running = state.running.saturating_sub(1);
                                return;
                            }
                        }
                    }
                }
            });
        match spawned {
            Ok(_) => Taken::Running,
            Err(e) => {
                if let Some(state) = lock(&self.nodes).get_mut(&key) {
                    state.running = state.running.saturating_sub(1);
                }
                Taken::Refused(format!("cannot start a thread: {e}"))
            }
        }
    }

    pub fn stop(&self, key: &Key) {
        if let Some(state) = lock(&self.nodes).remove(key) {
            state.stop.store(true, Ordering::SeqCst);
        }
    }

    pub fn stop_all(&self) {
        for (_, state) in lock(&self.nodes).drain() {
            state.stop.store(true, Ordering::SeqCst);
        }
    }
}
