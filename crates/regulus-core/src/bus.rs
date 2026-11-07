//! The event bus that carries "then" wires (docs/02-architecture.md §6).
//!
//! Each execution has its own queue of deliveries, drained in order by one thread at a
//! time, so `emit` never blocks the emitter nor re-enters its module. The journal keeps
//! the latest events in memory for clients (`event.list`) until the store exists.

use std::cell::RefCell;
use std::collections::VecDeque;

use regulus_proto::{BusEvent, EventListParams, PortRef};
use serde_json::Value;

use crate::registry::now_ms;

/// The name of the threads that drain a queue, as seen by debuggers and `top`.
pub(crate) const THREAD_NAME: &str = "regulus-bus";

/// A value on its way to one input.
pub(crate) struct Pending {
    pub event: u64,
    pub depth: u32,
    pub from: PortRef,
    pub capability: String,
    pub value: Value,
    pub to: PortRef,
}

/// The latest bus events, oldest first, at most `capacity` of them.
pub(crate) struct Journal {
    events: VecDeque<BusEvent>,
    next_seq: u64,
    capacity: usize,
}

impl Journal {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: VecDeque::new(),
            next_seq: 1,
            capacity,
        }
    }

    /// Stamps `event` with its `seq` and time, keeps it, and returns the `seq`. An event
    /// with `event = 0` is about itself.
    pub fn record(&mut self, mut event: BusEvent) -> u64 {
        let seq = self.next_seq;
        self.next_seq += 1;
        event.seq = seq;
        event.at = now_ms();
        if event.event == 0 {
            event.event = seq;
        }
        if self.capacity > 0 {
            if self.events.len() == self.capacity {
                self.events.pop_front();
            }
            self.events.push_back(event);
        }
        seq
    }

    /// The events `params` asks for and the cursor to follow from.
    pub fn list(&self, params: &EventListParams) -> (Vec<BusEvent>, u64) {
        let events: Vec<BusEvent> = self
            .events
            .iter()
            .filter(|e| e.seq > params.since)
            .filter(|e| {
                params
                    .execution_id
                    .as_deref()
                    .is_none_or(|id| e.execution_id == id)
            })
            .filter(|e| {
                params
                    .project_id
                    .as_deref()
                    .is_none_or(|id| e.project_id.as_deref() == Some(id))
            })
            .take(params.limit.unwrap_or(usize::MAX))
            .cloned()
            .collect();
        let next = events.last().map_or(params.since, |e| e.seq);
        (events, next)
    }
}

thread_local! {
    /// The execution and depth of the event this thread is delivering, if any.
    static HANDLING: RefCell<Option<(String, u32)>> = const { RefCell::new(None) };
}

/// The depth of the event being handled on this thread for `execution`: an emission made
/// now continues that chain.
pub(crate) fn handling_depth(execution: &str) -> Option<u32> {
    HANDLING.with(|current| {
        current
            .borrow()
            .as_ref()
            .filter(|(id, _)| id == execution)
            .map(|(_, depth)| *depth)
    })
}

/// Runs `f` as the handling of an event of `depth` in `execution`.
pub(crate) fn while_handling<T>(execution: &str, depth: u32, f: impl FnOnce() -> T) -> T {
    let previous = HANDLING.with(|current| current.replace(Some((execution.into(), depth))));
    let result = f();
    HANDLING.with(|current| *current.borrow_mut() = previous);
    result
}
