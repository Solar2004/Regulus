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
    fn take(
        &mut self,
        port: &str,
        value: Value,
        inputs: &[String],
        count: u64,
        object: bool,
    ) -> Option<Value> {
        if count > 0 {
            self.gathered.push(value);
            if (self.gathered.len() as u64) < count {
                return None;
            }
            return Some(Value::Array(std::mem::take(&mut self.gathered)));
        }
        self.queues.entry(port.into()).or_default().push_back(value);
        if !inputs
            .iter()
            .all(|i| self.queues.get(i).is_some_and(|q| !q.is_empty()))
        {
            return None;
        }
        let taken = inputs.iter().map(|input| {
            let value = self.queues.get_mut(input).and_then(VecDeque::pop_front);
            (input.clone(), value.unwrap_or_default())
        });
        Some(if object {
            Value::Object(taken.collect::<Map<_, _>>())
        } else {
            Value::Array(taken.map(|(_, v)| v).collect())
        })
    }
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
                    here.number("count"),
                    here.text("shape") == OBJECT,
                );
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

    #[test]
    fn a_barrier_waits_for_every_input_and_keeps_extras_queued() {
        let mut gate = Gate::default();
        assert_eq!(gate.take("a", json!(1), &inputs(), 0, false), None);
        assert_eq!(gate.take("a", json!(2), &inputs(), 0, false), None);
        assert_eq!(
            gate.take("b", json!("x"), &inputs(), 0, false),
            Some(json!([1, "x"]))
        );
        // The second `a` waits for the next `b`.
        assert_eq!(
            gate.take("b", json!("y"), &inputs(), 0, true),
            Some(json!({ "a": 2, "b": "y" }))
        );
    }

    #[test]
    fn a_count_gathers_that_many_from_any_input() {
        let mut gate = Gate::default();
        assert_eq!(gate.take("a", json!(1), &inputs(), 3, false), None);
        assert_eq!(gate.take("a", json!(2), &inputs(), 3, false), None);
        assert_eq!(
            gate.take("b", json!(3), &inputs(), 3, false),
            Some(json!([1, 2, 3]))
        );
        assert!(gate.gathered.is_empty());
    }
}
