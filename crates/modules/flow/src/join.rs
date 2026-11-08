//! `join`: fan-in. `any` passes each value on; `barrier` waits for every branch.
//!
//! A barrier with `timeout_ms` does not wait forever: a round (from the first value it
//! holds) that has not opened when the time is up sends what came on `timeout`, from a
//! thread of the node's own, and starts over. That emission is a new chain at depth 0.

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_core::{ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{Direction, NodeEvent};
use serde_json::{json, Map, Value};

use crate::wait::{self, Held};
use crate::{lock, Here};

const OUT: &str = "out";
/// Where a barrier whose time ran out sends what it had.
const TIMEOUT: &str = "timeout";
/// The name of the threads that time barriers, as seen by debuggers and `top`.
const THREAD_NAME: &str = "regulus-flow-join";
const ANY: &str = "any";
const BARRIER: &str = "barrier";
const OBJECT: &str = "object";

/// What a barrier holds: values queued per input until each has one, or, with `count`,
/// values in the order they came.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    queues: BTreeMap<String, VecDeque<Value>>,
    gathered: Vec<Value>,
    /// With `timeout_ms`: when the round that holds values runs out.
    deadline: Option<Instant>,
    /// A thread is timing this barrier.
    timer: bool,
}

impl Gate {
    /// Takes a value on `port`. When the barrier opens, what goes out: one value per
    /// input in `inputs` order (or the `count` gathered), as an array or by input.
    /// An input already holding `max_queue` values refuses another.
    fn take(
        &mut self,
        port: &str,
        value: Value,
        inputs: &[String],
        limits: Limits,
    ) -> Result<Option<Value>, String> {
        let Limits {
            count,
            object,
            max_queue,
        } = limits;
        if count > 0 {
            self.gathered.push(value);
            if (self.gathered.len() as u64) < count {
                return Ok(None);
            }
            return Ok(Some(Value::Array(std::mem::take(&mut self.gathered))));
        }
        let queue = self.queues.entry(port.into()).or_default();
        if queue.len() as u64 >= max_queue {
            return Err(format!(
                "input `{port}` already holds {max_queue} values (`max_queue`): this one is dropped"
            ));
        }
        queue.push_back(value);
        if !inputs
            .iter()
            .all(|i| self.queues.get(i).is_some_and(|q| !q.is_empty()))
        {
            return Ok(None);
        }
        let taken = inputs.iter().map(|input| {
            let value = self.queues.get_mut(input).and_then(VecDeque::pop_front);
            (input.clone(), value.unwrap_or_default())
        });
        Ok(Some(if object {
            Value::Object(taken.collect::<Map<_, _>>())
        } else {
            Value::Array(taken.map(|(_, v)| v).collect())
        }))
    }

    /// Whether some value waits for the barrier to open.
    fn holds(&self) -> bool {
        !self.gathered.is_empty() || self.queues.values().any(|q| !q.is_empty())
    }

    /// What the round has when its time runs out, shaped like `out`: with `count`, the
    /// values gathered so far (fewer than `count`); by input, the first value of each
    /// one, `null` where an input has none. Values queued behind those stay.
    fn partial(&mut self, inputs: &[String], limits: Limits) -> Value {
        if limits.count > 0 {
            return Value::Array(std::mem::take(&mut self.gathered));
        }
        let taken = inputs.iter().map(|input| {
            let value = self.queues.get_mut(input).and_then(VecDeque::pop_front);
            (input.clone(), value.unwrap_or_default())
        });
        if limits.object {
            Value::Object(taken.collect::<Map<_, _>>())
        } else {
            Value::Array(taken.map(|(_, v)| v).collect())
        }
    }
}

/// What a barrier's timeout thread needs: the node, its settings and how to emit.
struct Clock {
    ctx: ModuleCtx,
    me: NodeRef,
    gate: Arc<Mutex<Gate>>,
    held: Arc<Held>,
    inputs: Vec<String>,
    limits: Limits,
    timeout: Duration,
    /// Whether the node has a `timeout` output; without one, what ran out is dropped.
    has_output: bool,
}

impl Clock {
    /// Sleeps until the round's deadline and, if it has not opened by then, sends what
    /// it holds on `timeout`. One thread per node: it follows each new round and ends
    /// when the barrier holds nothing or the node stops.
    fn run(self) {
        loop {
            let left = {
                let mut gate = lock(&self.gate);
                match gate.deadline {
                    None => {
                        gate.timer = false;
                        return;
                    }
                    Some(at) => at.saturating_duration_since(Instant::now()),
                }
            };
            if !left.is_zero() {
                if !wait::sleep(&self.held, left) {
                    return;
                }
                continue;
            }
            let ran_out = {
                let mut gate = lock(&self.gate);
                let now = Instant::now();
                match gate.deadline {
                    Some(at) if at <= now => {
                        let value = gate.partial(&self.inputs, self.limits);
                        gate.deadline = gate.holds().then(|| now + self.timeout);
                        Some(value)
                    }
                    _ => None,
                }
            };
            let Some(value) = ran_out else { continue };
            // A stop that came while the round was taken: nothing leaves anymore.
            if wait::stopped(&self.held) {
                return;
            }
            let ms = self.timeout.as_millis();
            if !self.has_output {
                eprintln!(
                    "regulus: join `{}`: timed out after {ms} ms with no `{TIMEOUT}` output: what came is dropped",
                    self.me.node_id
                );
            } else if let Err(error) = self.ctx.emit(&self.me, TIMEOUT, value) {
                eprintln!("regulus: join `{}`: {error}", self.me.node_id);
            }
        }
    }
}

/// How a barrier opens and how much it holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Limits {
    count: u64,
    object: bool,
    max_queue: u64,
}

/// A barrier needs something to wait for and a bound on what it holds; both are checked
/// when the node starts, not on its first value.
pub(crate) fn start(here: &Here) -> Result<Value, ModuleError> {
    let barrier = matches!(here.text("mode").as_str(), BARRIER | "");
    let refuse = |why: &str| {
        Err(ModuleError::InvalidParams(format!(
            "join `{}`: {why}",
            here.me.node_id
        )))
    };
    if barrier && here.number("count") == 0 && here.then_ports(Direction::Input).is_empty() {
        return refuse("a barrier needs \"then\" inputs to wait for, or a `count`");
    }
    if here.number("max_queue") == 0 {
        return refuse("`max_queue` must be above 0");
    }
    if barrier && here.number("timeout_ms") > 0 {
        here.flow.timers.start(&here.me);
    }
    Ok(Value::Null)
}

pub(crate) fn event(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    match here.text("mode").as_str() {
        ANY => {
            here.emit(OUT, event.value)?;
            Ok(json!({ "sent": [OUT] }))
        }
        BARRIER | "" => {
            let inputs = here.then_ports(Direction::Input);
            let limits = Limits {
                count: here.number("count"),
                object: here.text("shape") == OBJECT,
                max_queue: here.number("max_queue"),
            };
            let gate = lock(&here.flow.joins)
                .entry(here.me.clone())
                .or_default()
                .clone();
            let timeout = Duration::from_millis(here.number("timeout_ms"));
            let (opened, start_clock) = {
                let mut held = lock(&gate);
                let opened = held
                    .take(&event.port, event.value, &inputs, limits)
                    .map_err(|e| ModuleError::Failed(format!("join `{}`: {e}", here.me.node_id)))?;
                let mut start_clock = false;
                if !timeout.is_zero() {
                    // A round starts with the first value held, or with what is left
                    // queued when one opens.
                    if opened.is_some() || !held.holds() {
                        held.deadline = None;
                    }
                    if held.holds() && held.deadline.is_none() {
                        held.deadline = Some(Instant::now() + timeout);
                    }
                    if held.deadline.is_some() && !held.timer {
                        held.timer = true;
                        start_clock = true;
                    }
                }
                (opened, start_clock)
            };
            if start_clock {
                clock(here, gate, inputs, limits, timeout)?;
            }
            match opened {
                Some(value) => {
                    here.emit(OUT, value)?;
                    Ok(json!({ "sent": [OUT] }))
                }
                None => Ok(json!({ "waiting": true })),
            }
        }
        other => Err(ModuleError::InvalidParams(format!(
            "join mode is `{BARRIER}` or `{ANY}`, not `{other}`"
        ))),
    }
}

/// Starts the thread that times the node's rounds. A node stopped meanwhile has no
/// clock to take: nothing is timed, and it holds nothing anymore either.
fn clock(
    here: &Here,
    gate: Arc<Mutex<Gate>>,
    inputs: Vec<String>,
    limits: Limits,
    timeout: Duration,
) -> Result<(), ModuleError> {
    let Some(held) = here.flow.timers.held(&here.me) else {
        lock(&gate).timer = false;
        return Ok(());
    };
    let clock = Clock {
        ctx: here.ctx().clone(),
        me: here.me.clone(),
        gate: gate.clone(),
        held,
        inputs,
        limits,
        timeout,
        has_output: here.has_output(TIMEOUT),
    };
    std::thread::Builder::new()
        .name(THREAD_NAME.into())
        .spawn(move || clock.run())
        .map(|_| ())
        .map_err(|e| {
            lock(&gate).timer = false;
            ModuleError::Failed(format!("join cannot start its timer: {e}"))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn what_runs_out_keeps_the_shape_of_out() {
        let mut gate = Gate::default();
        gate.take("b", json!("x"), &inputs(), limits(0, false))
            .unwrap();
        gate.take("b", json!("y"), &inputs(), limits(0, false))
            .unwrap();
        assert!(gate.holds());
        assert_eq!(
            gate.partial(&inputs(), limits(0, false)),
            json!([null, "x"])
        );
        // The second `b` stays for the next round.
        assert!(gate.holds());
        assert_eq!(
            gate.partial(&inputs(), limits(0, true)),
            json!({ "a": null, "b": "y" })
        );
        assert!(!gate.holds());
        gate.take("a", json!(1), &inputs(), limits(3, false))
            .unwrap();
        assert_eq!(gate.partial(&inputs(), limits(3, false)), json!([1]));
        assert!(!gate.holds());
    }

    fn inputs() -> Vec<String> {
        vec!["a".into(), "b".into()]
    }

    fn limits(count: u64, object: bool) -> Limits {
        Limits {
            count,
            object,
            max_queue: 64,
        }
    }

    #[test]
    fn an_input_past_max_queue_refuses_more() {
        let mut gate = Gate::default();
        let tight = Limits {
            count: 0,
            object: false,
            max_queue: 2,
        };
        assert_eq!(gate.take("a", json!(1), &inputs(), tight), Ok(None));
        assert_eq!(gate.take("a", json!(2), &inputs(), tight), Ok(None));
        assert!(gate.take("a", json!(3), &inputs(), tight).is_err());
        assert_eq!(gate.queues["a"].len(), 2, "the refused one was not kept");
    }

    #[test]
    fn a_barrier_waits_for_every_input_and_keeps_extras_queued() {
        let mut gate = Gate::default();
        assert_eq!(
            gate.take("a", json!(1), &inputs(), limits(0, false))
                .unwrap(),
            None
        );
        assert_eq!(
            gate.take("a", json!(2), &inputs(), limits(0, false))
                .unwrap(),
            None
        );
        assert_eq!(
            gate.take("b", json!("x"), &inputs(), limits(0, false))
                .unwrap(),
            Some(json!([1, "x"]))
        );
        // The second `a` waits for the next `b`.
        assert_eq!(
            gate.take("b", json!("y"), &inputs(), limits(0, true))
                .unwrap(),
            Some(json!({ "a": 2, "b": "y" }))
        );
    }

    #[test]
    fn a_count_gathers_that_many_from_any_input() {
        let mut gate = Gate::default();
        assert_eq!(
            gate.take("a", json!(1), &inputs(), limits(3, false))
                .unwrap(),
            None
        );
        assert_eq!(
            gate.take("a", json!(2), &inputs(), limits(3, false))
                .unwrap(),
            None
        );
        assert_eq!(
            gate.take("b", json!(3), &inputs(), limits(3, false))
                .unwrap(),
            Some(json!([1, 2, 3]))
        );
        assert!(gate.gathered.is_empty());
    }
}
