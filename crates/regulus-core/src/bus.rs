//! The event bus that carries "then" wires (docs/02-architecture.md §6).
//!
//! Each execution has its own queue of deliveries, drained in order by one thread at a
//! time, so `emit` never blocks the emitter nor re-enters its module. The journal writes
//! every event to the store and keeps the latest in memory, so following clients
//! (`event.list`) rarely reach the database.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::Arc;

use regulus_proto::{BusEvent, EventListParams, PortRef};
use serde_json::Value;

use crate::registry::now_ms;
use crate::store::Store;

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

/// Where the answer to an `event.list` comes from.
pub(crate) enum Listing {
    /// Memory holds everything after the cursor: the events and the next cursor.
    Ready(Vec<BusEvent>, u64),
    /// The cursor is older than memory: read the store, up to `upto`, without holding
    /// the journal.
    Store { store: Arc<Store>, upto: u64 },
}

/// The cursor after a page: its last event when the page is full (there may be more),
/// otherwise everything up to `upto` was looked at, matched or not.
pub(crate) fn next_cursor(events: &[BusEvent], limit: Option<usize>, since: u64, upto: u64) -> u64 {
    match (events.last(), limit) {
        (Some(last), Some(limit)) if events.len() >= limit => last.seq,
        _ => upto.max(since),
    }
}

/// Every bus event, in the store when there is one, and the latest `capacity` of them in
/// memory, oldest first.
pub(crate) struct Journal {
    events: VecDeque<BusEvent>,
    next_seq: u64,
    capacity: usize,
    store: Option<Arc<Store>>,
}

impl Journal {
    /// `seq` goes on from the highest the store keeps, so cursors stay valid across
    /// restarts.
    pub fn new(capacity: usize, store: Option<Arc<Store>>) -> Self {
        let next_seq = store.as_ref().map_or(1, |store| store.first_seq());
        Self {
            events: VecDeque::new(),
            next_seq,
            capacity,
            store,
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
        if let Some(store) = &self.store {
            if let Err(error) = store.append_event(&event) {
                eprintln!("regulus: store: {error}");
            }
        }
        if self.capacity > 0 {
            if self.events.len() == self.capacity {
                self.events.pop_front();
            }
            self.events.push_back(event);
        }
        seq
    }

    /// At most `limit` of the events `params` asks for. Memory answers when it holds
    /// everything after `since`; otherwise the caller reads the store.
    pub fn list(&self, params: &EventListParams, limit: Option<usize>) -> Listing {
        let upto = self.next_seq - 1;
        let in_memory = self
            .events
            .front()
            .map_or(params.since >= upto, |first| params.since + 1 >= first.seq);
        if let (false, Some(store)) = (in_memory, &self.store) {
            return Listing::Store {
                store: store.clone(),
                upto,
            };
        }
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
            .take(limit.unwrap_or(usize::MAX))
            .cloned()
            .collect();
        let next = next_cursor(&events, limit, params.since, upto);
        Listing::Ready(events, next)
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
