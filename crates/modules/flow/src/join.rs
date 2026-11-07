//! `join`: fan-in. `any` passes each value on; `barrier` waits for every branch.

use std::collections::{BTreeMap, VecDeque};

use regulus_core::ModuleError;
use regulus_proto::{Direction, NodeEvent};
use serde_json::{json, Map, Value};

use crate::{lock, Here};

const OUT: &str = "out";
const ANY: &str = "any";
const BARRIER: &str = "barrier";
const OBJECT: &str = "object";

/// What a barrier holds: values queued per input until each has one, or, with `count`,
/// values in the order they came.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    queues: BTreeMap<String, VecDeque<Value>>,
    gathered: Vec<Value>,
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
}

/// How a barrier opens and how much it holds.
#[derive(Debug, Clone, Copy)]
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
            let opened = lock(&here.flow.joins)
                .entry(here.me.clone())
                .or_default()
                .take(
                    &event.port,
                    event.value,
                    &inputs,
                    Limits {
                        count: here.number("count"),
                        object: here.text("shape") == OBJECT,
                        max_queue: here.number("max_queue"),
                    },
                )
                .map_err(|e| ModuleError::Failed(format!("join `{}`: {e}", here.me.node_id)))?;
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

#[cfg(test)]
mod tests {
    use super::*;

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
