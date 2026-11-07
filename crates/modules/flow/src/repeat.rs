//! `loop`: `in` starts a run, `body` goes out each turn, `back` brings the turn's result.

use regulus_core::ModuleError;
use regulus_proto::NodeEvent;
use serde_json::{json, Value};

use crate::rule::Rule;
use crate::{lock, Here};

const IN: &str = "in";
const BACK: &str = "back";
const BODY: &str = "body";
const DONE: &str = "done";
/// Where a run that hit `max` goes, if the node has it; otherwise `done`.
const EXHAUSTED: &str = "exhausted";

/// What a loop does with a value that came back from turn `turn`.
#[derive(Debug, PartialEq)]
enum Next {
    /// Another turn, numbered.
    Turn(u64),
    /// `until` held.
    Done,
    /// `max` turns ran.
    Exhausted,
}

fn next(turn: u64, max: u64, until: Option<&Rule>, value: &Value) -> Result<Next, ModuleError> {
    if let Some(rule) = until {
        if rule.test(value)? {
            return Ok(Next::Done);
        }
    }
    if max > 0 && turn >= max {
        return Ok(Next::Exhausted);
    }
    Ok(Next::Turn(turn + 1))
}

pub(crate) fn event(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    let loops = &here.flow.loops;
    match event.port.as_str() {
        // A new run starts over at turn 1, whatever ran before.
        IN => {
            lock(loops).insert(here.me.clone(), 1);
            here.emit(BODY, event.value)?;
            Ok(json!({ "turn": 1 }))
        }
        BACK => {
            let turn = lock(loops).get(&here.me).copied().ok_or_else(|| {
                ModuleError::Failed(format!(
                    "loop `{}` got `{BACK}` with no run going: start it on `{IN}`",
                    here.me.node_id
                ))
            })?;
            let until = Rule::optional(here.setting("until"))?;
            match next(turn, here.number("max"), until.as_ref(), &event.value)? {
                Next::Turn(turn) => {
                    lock(loops).insert(here.me.clone(), turn);
                    here.emit(BODY, event.value)?;
                    Ok(json!({ "turn": turn }))
                }
                end => {
                    lock(loops).remove(&here.me);
                    let port = match end {
                        Next::Exhausted if here.has_output(EXHAUSTED) => EXHAUSTED,
                        _ => DONE,
                    };
                    here.emit(port, event.value)?;
                    Ok(json!({ "turns": turn, "end": port }))
                }
            }
        }
        other => Err(ModuleError::InvalidParams(format!(
            "loop takes `{IN}` and `{BACK}`, not `{other}`"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_loop_turns_until_its_rule_or_its_cap() {
        let rule = Rule::optional(Some(&json!({ "op": "contains", "value": "DONE" })))
            .unwrap()
            .unwrap();
        assert_eq!(
            next(1, 3, Some(&rule), &json!("go")).unwrap(),
            Next::Turn(2)
        );
        assert_eq!(next(2, 3, Some(&rule), &json!("DONE")).unwrap(), Next::Done);
        assert_eq!(next(3, 3, None, &json!("go")).unwrap(), Next::Exhausted);
        // max 0: no cap of its own; the core's depth cut is what is left.
        assert_eq!(next(500, 0, None, &json!("go")).unwrap(), Next::Turn(501));
    }
}
