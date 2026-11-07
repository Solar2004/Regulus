//! `wait`: holds each value `ms` milliseconds on a thread of its own, so the bus goes on
//! delivering meanwhile, then sends it on. Stopping the node drops what it holds.
//!
//! What it emits starts a new chain at depth 0 (docs/02-architecture.md §6): a loop that
//! goes through a `wait` is out of reach of `[bus] max_depth`, so give it a `max`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use regulus_core::{ModuleError, NodeRef};
use regulus_proto::NodeEvent;
use serde_json::{json, Value};

use crate::{lock, Here};

const OUT: &str = "out";
/// The name of the threads that hold values, as seen by debuggers and `top`.
const THREAD_NAME: &str = "regulus-flow-wait";

/// What one `wait` node holds.
#[derive(Default)]
struct Held {
    stopped: Mutex<bool>,
    wake: Condvar,
    pending: AtomicUsize,
}

/// Every `wait` node holding something, so a stop reaches its threads.
#[derive(Default)]
pub(crate) struct Timers {
    nodes: Mutex<HashMap<NodeRef, Arc<Held>>>,
}

impl Timers {
    fn held(&self, node: &NodeRef) -> Arc<Held> {
        lock(&self.nodes).entry(node.clone()).or_default().clone()
    }

    /// Wakes the node's threads and has them drop their values.
    pub fn cancel(&self, node: &NodeRef) {
        if let Some(held) = lock(&self.nodes).remove(node) {
            stop(&held);
        }
    }

    pub fn cancel_all(&self) {
        for (_, held) in lock(&self.nodes).drain() {
            stop(&held);
        }
    }
}

fn stop(held: &Held) {
    *lock(&held.stopped) = true;
    held.wake.notify_all();
}

/// Sleeps until `delay` passes or the node stops; true if it is still running.
fn sleep(held: &Held, delay: Duration) -> bool {
    let deadline = Instant::now() + delay;
    let mut stopped = lock(&held.stopped);
    while !*stopped {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        stopped = held
            .wake
            .wait_timeout(stopped, left)
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .0;
    }
    false
}

pub(crate) fn event(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    let delay = Duration::from_millis(here.number("ms"));
    let max = here.number("max_pending") as usize;
    let held = here.flow.timers.held(&here.me);
    let pending = held.pending.fetch_add(1, Ordering::SeqCst) + 1;
    if max > 0 && pending > max {
        held.pending.fetch_sub(1, Ordering::SeqCst);
        return Err(ModuleError::Failed(format!(
            "wait `{}` already holds {max} values (`max_pending`)",
            here.me.node_id
        )));
    }
    let ctx = here.ctx().clone();
    let me = here.me.clone();
    let value = event.value;
    let thread = held.clone();
    std::thread::Builder::new()
        .name(THREAD_NAME.into())
        .spawn(move || {
            if sleep(&thread, delay) {
                if let Err(error) = ctx.emit(&me, OUT, value) {
                    eprintln!("regulus: wait `{}`: {error}", me.node_id);
                }
            }
            thread.pending.fetch_sub(1, Ordering::SeqCst);
        })
        .map_err(|e| {
            held.pending.fetch_sub(1, Ordering::SeqCst);
            ModuleError::Failed(format!("wait cannot start its timer: {e}"))
        })?;
    Ok(json!({ "held_ms": delay.as_millis() as u64, "pending": pending }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_wakes_a_sleeper_at_once_and_it_drops_its_value() {
        let held = Arc::new(Held::default());
        let sleeper = held.clone();
        let started = Instant::now();
        let thread = std::thread::spawn(move || sleep(&sleeper, Duration::from_secs(30)));
        std::thread::sleep(Duration::from_millis(20));
        stop(&held);
        assert!(!thread.join().unwrap(), "a stopped node sends nothing");
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(sleep(&Held::default(), Duration::from_millis(1)));
    }
}
