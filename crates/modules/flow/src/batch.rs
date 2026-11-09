//! `batch`: map-collect (docs/13-blocks.md §3.2). An array on `in` runs the workflow of
//! `workflow` once per element (a value that is not an array is one element), each on the
//! child's "then" input `input`, at most `max_in_flight` children live at once. Each
//! answer leaves on `item` as `{batch, index, port, value}`; when every child is over,
//! `done` says `{batch, total, returned, failed, cancelled, results}`, the results in
//! the order of the elements, and just before it `values` (if the node declares it) the
//! answers alone, in that order. `timeout_ms` after the first start, what is still running is
//! stopped and the same summary leaves on `partial`. A value on `cancel` stops every
//! batch the node has going, and `on_failure = cancel` does so after the first child that
//! ends without an answer.
//!
//! Children are called (`ChildMode::Call`) with the tag `<batch>/<index>`: that is how an
//! answer finds its element. Refills run on a thread of their own, so a long batch does
//! not deepen one chain past `[bus] max_depth`; the batch is bounded by its elements.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use regulus_core::{ChildOptions, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{ChildEvent, ChildEventKind, ExecutionState, NodeEvent};
use serde_json::{json, Value};

use crate::{bound, lock, Here};

const CANCEL: &str = "cancel";
const ITEM: &str = "item";
const DONE: &str = "done";
const PARTIAL: &str = "partial";
const VALUES: &str = "values";
const THREAD_NAME: &str = "regulus-flow-batch";

/// One batch of one node.
struct Run {
    id: String,
    workflow: String,
    input: String,
    /// 0: as many as `[call] max_children` lets live.
    max_in_flight: usize,
    cancel_on_failure: bool,
    /// The node declares `values`: the answers alone, in element order, at the end.
    values: bool,
    /// The bindings each child gets, narrowed to what the workflow takes after the first.
    opts: ChildOptions,
    waiting: VecDeque<(usize, Value)>,
    /// Elements whose child is live, with its id once `start_child` gave it.
    live: HashMap<usize, Option<String>>,
    results: Vec<Option<Value>>,
    returned: usize,
    failed: usize,
    cancelled: usize,
    over: bool,
}

impl Run {
    fn summary(&self) -> Value {
        let results: Vec<&Value> = self.results.iter().flatten().collect();
        json!({ "batch": self.id, "total": self.results.len(), "returned": self.returned,
                "failed": self.failed, "cancelled": self.cancelled, "results": results })
    }

    /// What came back, in element order, without the failed or cancelled elements.
    fn values(&self) -> Value {
        let values: Vec<&Value> = self
            .results
            .iter()
            .flatten()
            .filter_map(|r| r.get("value"))
            .collect();
        json!(values)
    }

    fn room(&self) -> bool {
        !self.over
            && !self.waiting.is_empty()
            && (self.max_in_flight == 0 || self.live.len() < self.max_in_flight)
    }
}

struct Shared {
    run: Mutex<Run>,
    /// Wakes the timeout thread when the batch is over.
    wake: Condvar,
}

#[derive(Default)]
struct NodeRuns {
    next: u64,
    runs: HashMap<String, Arc<Shared>>,
}

/// Every batch going, by node. Cheap to clone: threads take a copy.
#[derive(Clone, Default)]
pub(crate) struct Batches(Arc<Mutex<HashMap<NodeRef, NodeRuns>>>);

impl Batches {
    fn get(&self, me: &NodeRef, id: &str) -> Option<Arc<Shared>> {
        lock(&self.0).get(me)?.runs.get(id).cloned()
    }

    fn remove(&self, me: &NodeRef, id: &str) {
        if let Some(node) = lock(&self.0).get_mut(me) {
            node.runs.remove(id);
        }
    }

    /// A node that stops drops its batches; the core stops their children with it.
    pub(crate) fn stop(&self, me: &NodeRef) {
        let Some(node) = lock(&self.0).remove(me) else {
            return;
        };
        for shared in node.runs.into_values() {
            lock(&shared.run).over = true;
            shared.wake.notify_all();
        }
    }
}

/// What one batch needs from outside: how to act as its node.
#[derive(Clone)]
struct Owner {
    ctx: ModuleCtx,
    me: NodeRef,
    batches: Batches,
}

impl Owner {
    fn emit(&self, port: &str, value: Value) {
        if let Err(error) = self.ctx.emit(&self.me, port, value) {
            eprintln!("regulus: batch `{}`: {error}", self.me.node_id);
        }
    }

    /// Starts children while there is room. A start that fails while others run waits
    /// for one of them to end (`[call] max_children` may be what refused it); with none
    /// running, that element fails.
    fn launch(&self, shared: &Arc<Shared>) {
        loop {
            let (index, item, mut opts, workflow, input) = {
                let mut run = lock(&shared.run);
                if !run.room() {
                    break;
                }
                let Some((index, item)) = run.waiting.pop_front() else {
                    break;
                };
                run.live.insert(index, None);
                let mut opts = run.opts.clone();
                opts.tag = Some(format!("{}/{index}", run.id));
                (index, item, opts, run.workflow.clone(), run.input.clone())
            };
            let started = bound::start(
                &self.ctx,
                &self.me,
                &workflow,
                vec![(input, item.clone())],
                &mut opts,
            );
            let mut stray = None;
            let mut failure = false;
            {
                let mut run = lock(&shared.run);
                run.opts.bindings = opts.bindings;
                match started {
                    Ok(child) if run.over => stray = Some(child),
                    Ok(child) => {
                        // Absent: it already ended, its news came first.
                        if let Some(slot) = run.live.get_mut(&index) {
                            *slot = Some(child);
                        }
                    }
                    Err(error) => {
                        run.live.remove(&index);
                        if !run.live.is_empty() && !run.over {
                            run.waiting.push_front((index, item));
                            break;
                        }
                        run.results[index] =
                            Some(json!({ "index": index, "error": error.to_string() }));
                        run.failed += 1;
                        failure = run.cancel_on_failure;
                    }
                }
            }
            if let Some(child) = stray {
                let _ = self.ctx.stop_child(&self.me, &child);
            }
            if failure {
                return self.close(shared, DONE, "cancelled after a failure");
            }
        }
        self.finish(shared);
    }

    /// Every element answered or failed: the summary leaves on `done`.
    fn finish(&self, shared: &Arc<Shared>) {
        let summary = {
            let mut run = lock(&shared.run);
            if run.over || !run.waiting.is_empty() || !run.live.is_empty() {
                return;
            }
            run.over = true;
            (run.summary(), run.values.then(|| run.values()))
        };
        self.end(shared, DONE, summary);
    }

    /// Ends the batch before its time: what waits is not started, what runs is stopped,
    /// each counted as cancelled with `reason`, and the summary leaves on `port`.
    fn close(&self, shared: &Arc<Shared>, port: &str, reason: &str) {
        let (children, summary) = {
            let mut run = lock(&shared.run);
            if run.over {
                return;
            }
            run.over = true;
            let mut left: Vec<usize> = run.waiting.drain(..).map(|(index, _)| index).collect();
            let mut children = Vec::new();
            for (index, child) in std::mem::take(&mut run.live) {
                left.push(index);
                children.extend(child);
            }
            for index in left {
                if run.results[index].is_none() {
                    run.results[index] = Some(json!({ "index": index, "error": reason }));
                    run.cancelled += 1;
                }
            }
            (children, (run.summary(), run.values.then(|| run.values())))
        };
        for child in children {
            let _ = self.ctx.stop_child(&self.me, &child);
        }
        self.end(shared, port, summary);
    }

    fn end(&self, shared: &Arc<Shared>, port: &str, (summary, values): (Value, Option<Value>)) {
        shared.wake.notify_all();
        if let Some(id) = summary["batch"].as_str() {
            self.batches.remove(&self.me, id);
        }
        if let Some(values) = values {
            self.emit(VALUES, values);
        }
        self.emit(port, summary);
    }

    /// Refills on a thread of its own: started from the bus thread that delivers a
    /// child's news, each wave of children would be one delivery deeper than the last.
    fn refill(&self, shared: &Arc<Shared>) {
        let owner = self.clone();
        let thread = shared.clone();
        let spawned = std::thread::Builder::new()
            .name(THREAD_NAME.into())
            .spawn(move || owner.launch(&thread));
        if spawned.is_err() {
            self.launch(shared);
        }
    }

    /// Past `timeout`, what is still running is stopped and `partial` says what came.
    fn time(&self, shared: &Arc<Shared>, timeout: Duration) {
        let owner = self.clone();
        let thread = shared.clone();
        let deadline = Instant::now() + timeout;
        let spawned = std::thread::Builder::new()
            .name(THREAD_NAME.into())
            .spawn(move || {
                let mut run = lock(&thread.run);
                while !run.over {
                    let left = deadline.saturating_duration_since(Instant::now());
                    if left.is_zero() {
                        drop(run);
                        return owner.close(&thread, PARTIAL, "timed out");
                    }
                    run = thread
                        .wake
                        .wait_timeout(run, left)
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .0;
                }
            });
        if let Err(error) = spawned {
            eprintln!(
                "regulus: batch `{}` cannot time itself: {error}",
                self.me.node_id
            );
        }
    }
}

fn owner(here: &Here) -> Owner {
    Owner {
        ctx: here.ctx().clone(),
        me: here.me.clone(),
        batches: here.flow.batches.clone(),
    }
}

pub(crate) fn event(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    if event.port == CANCEL {
        return Ok(cancel(here));
    }
    let workflow = here.text("workflow");
    if workflow.is_empty() {
        return Err(ModuleError::InvalidParams(format!(
            "batch `{}` names no workflow in `workflow`",
            here.me.node_id
        )));
    }
    let input = match here.text("input") {
        input if input.trim().is_empty() => "in".to_string(),
        input => input.trim().to_string(),
    };
    let on_failure = here.text("on_failure");
    if !matches!(on_failure.as_str(), "continue" | "cancel") {
        return Err(ModuleError::InvalidParams(format!(
            "batch `{}`: `on_failure` is `continue` or `cancel`, not `{on_failure}`",
            here.me.node_id
        )));
    }
    let items = match event.value {
        Value::Array(items) => items,
        one => vec![one],
    };
    let owner = owner(here);
    let id = {
        let mut nodes = lock(&here.flow.batches.0);
        let node = nodes.entry(here.me.clone()).or_default();
        node.next += 1;
        format!("b{}", node.next)
    };
    let total = items.len();
    let shared = Arc::new(Shared {
        run: Mutex::new(Run {
            id: id.clone(),
            workflow,
            input,
            max_in_flight: usize::try_from(here.number("max_in_flight")).unwrap_or(usize::MAX),
            cancel_on_failure: on_failure == "cancel",
            values: here.has_output(VALUES),
            opts: ChildOptions {
                bindings: bound::of(here.ctx(), &here.me, &here.call.node),
                ..ChildOptions::default()
            },
            waiting: items.into_iter().enumerate().collect(),
            live: HashMap::new(),
            results: vec![None; total],
            returned: 0,
            failed: 0,
            cancelled: 0,
            over: false,
        }),
        wake: Condvar::new(),
    });
    lock(&here.flow.batches.0)
        .entry(here.me.clone())
        .or_default()
        .runs
        .insert(id.clone(), shared.clone());
    match here.number("timeout_ms") {
        0 => {}
        ms => owner.time(&shared, Duration::from_millis(ms)),
    }
    owner.launch(&shared);
    Ok(json!({ "batch": id, "total": total }))
}

/// Stops every batch of the node; each says so on `done`.
fn cancel(here: &Here) -> Value {
    let owner = owner(here);
    let runs: Vec<(String, Arc<Shared>)> = lock(&here.flow.batches.0)
        .get(&here.me)
        .map(|node| {
            node.runs
                .iter()
                .map(|(id, s)| (id.clone(), s.clone()))
                .collect()
        })
        .unwrap_or_default();
    let mut ids: Vec<String> = Vec::with_capacity(runs.len());
    for (id, shared) in runs {
        owner.close(&shared, DONE, "cancelled");
        ids.push(id);
    }
    ids.sort();
    json!({ "cancelled": ids })
}

/// Why a child that ended without an answer counts as failed.
pub(crate) fn reason(state: &ExecutionState) -> String {
    match state {
        ExecutionState::Failed { reason } => reason.clone(),
        other => {
            let status = serde_json::to_value(other)
                .ok()
                .and_then(|v| v["status"].as_str().map(String::from))
                .unwrap_or_default();
            format!("ended {status} without an answer")
        }
    }
}

pub(crate) fn child(here: &Here, child: ChildEvent) -> Result<Value, ModuleError> {
    let Some((id, index)) = child.tag.as_deref().and_then(|tag| {
        let (id, index) = tag.rsplit_once('/')?;
        Some((id.to_string(), index.parse::<usize>().ok()?))
    }) else {
        return Ok(Value::Null);
    };
    let owner = owner(here);
    // A batch that is over no longer listens: stopped children still say they ended.
    let Some(shared) = owner.batches.get(&here.me, &id) else {
        return Ok(Value::Null);
    };
    match child.kind {
        ChildEventKind::Returned { port, value, .. }
        | ChildEventKind::Output { port, value, .. } => {
            {
                let mut run = lock(&shared.run);
                if run.over || index >= run.results.len() || run.results[index].is_some() {
                    return Ok(Value::Null);
                }
                run.results[index] = Some(json!({ "index": index, "port": port, "value": value }));
                run.returned += 1;
            }
            owner.emit(
                ITEM,
                json!({ "batch": id, "index": index, "port": port, "value": value }),
            );
        }
        ChildEventKind::Ended { state } => {
            let (failure, room) = {
                let mut run = lock(&shared.run);
                if run.over || index >= run.results.len() {
                    return Ok(Value::Null);
                }
                run.live.remove(&index);
                let failure = run.results[index].is_none();
                if failure {
                    run.results[index] = Some(json!({ "index": index, "error": reason(&state) }));
                    run.failed += 1;
                }
                (failure && run.cancel_on_failure, run.room())
            };
            if failure {
                owner.close(&shared, DONE, "cancelled after a failure");
            } else if room {
                owner.refill(&shared);
            } else {
                owner.finish(&shared);
            }
        }
    }
    Ok(Value::Null)
}
