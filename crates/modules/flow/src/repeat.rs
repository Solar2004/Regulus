//! `loop`: `in` starts a run, `body` goes out each turn, `back` brings the turn's result.

use regulus_core::ModuleError;
use regulus_proto::{NodeEvent, NodeStart};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::rule::{Matcher, Rule};
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

fn next(
    turn: u64,
    max: u64,
    until: Option<&Rule>,
    value: &Value,
    matcher: &Matcher,
) -> Result<Next, ModuleError> {
    if let Some(rule) = until {
        if rule.test(value, matcher)? {
            return Ok(Next::Done);
        }
    }
    if max > 0 && turn >= max {
        return Ok(Next::Exhausted);
    }
    Ok(Next::Turn(turn + 1))
}

/// The one run a loop node has going: the turn it is on and the last value it sent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Run {
    pub turn: u64,
    pub last: Value,
}

/// Saves the run, or that there is none, so `back` still finds it after a restart. A core
/// without a store has nothing to resume, and says so in the error that is dropped here.
fn save(here: &Here, run: Option<&Run>) {
    let state = run.map_or(Value::Null, |run| json!(run));
    let _ = here.ctx().checkpoint(&here.me, state);
}

/// A resumed node gets its run back.
pub(crate) fn start(here: &Here) -> Result<Value, ModuleError> {
    // A fresh start sends null; a resumed one, `NodeStart`.
    let start: NodeStart = match &here.call.args {
        Value::Null => NodeStart::default(),
        args => crate::parse(args.clone())?,
    };
    if let Some(run) = start
        .checkpoint
        .and_then(|c| serde_json::from_value::<Run>(c).ok())
    {
        lock(&here.flow.loops).insert(here.me.clone(), run);
    }
    Ok(Value::Null)
}

fn turn(here: &Here, run: Run) -> Result<Value, ModuleError> {
    let turn = run.turn;
    save(here, Some(&run));
    lock(&here.flow.loops).insert(here.me.clone(), run.clone());
    here.emit(BODY, run.last)?;
    Ok(json!({ "turn": turn }))
}

pub(crate) fn event(here: &Here, event: NodeEvent) -> Result<Value, ModuleError> {
    let loops = &here.flow.loops;
    match event.port.as_str() {
        // One run per node: a new `in` ends the one going, on `exhausted` if the node has
        // it, and starts over at turn 1.
        IN => {
            let cut = lock(loops).remove(&here.me);
            if let Some(cut) = cut {
                if here.has_output(EXHAUSTED) {
                    here.emit(EXHAUSTED, cut.last)?;
                }
            }
            turn(
                here,
                Run {
                    turn: 1,
                    last: event.value,
                },
            )
        }
        BACK => {
            let current = lock(loops)
                .get(&here.me)
                .map(|run| run.turn)
                .ok_or_else(|| {
                    ModuleError::Failed(format!(
                        "loop `{}` got `{BACK}` with no run going: start it on `{IN}`",
                        here.me.node_id
                    ))
                })?;
            let until = Rule::optional(here.setting("until"))?;
            let matcher = here.matcher()?;
            match next(
                current,
                here.number("max"),
                until.as_ref(),
                &event.value,
                &matcher,
            )? {
                Next::Turn(number) => turn(
                    here,
                    Run {
                        turn: number,
                        last: event.value,
                    },
                ),
                end => {
                    lock(loops).remove(&here.me);
                    save(here, None);
                    let port = match end {
                        Next::Exhausted if here.has_output(EXHAUSTED) => EXHAUSTED,
                        _ => DONE,
                    };
                    here.emit(port, event.value)?;
                    Ok(json!({ "turns": current, "end": port }))
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
        let regexes = crate::rule::Regexes::default();
        let m = crate::rule::tests::matcher(&regexes);
        let rule = Rule::optional(Some(&json!({ "op": "contains", "value": "DONE" })))
            .unwrap()
            .unwrap();
        assert_eq!(
            next(1, 3, Some(&rule), &json!("go"), &m).unwrap(),
            Next::Turn(2)
        );
        assert_eq!(
            next(2, 3, Some(&rule), &json!("DONE"), &m).unwrap(),
            Next::Done
        );
        assert_eq!(next(3, 3, None, &json!("go"), &m).unwrap(), Next::Exhausted);
        // max 0: no cap of its own; the core's depth cut is what is left.
        assert_eq!(
            next(500, 0, None, &json!("go"), &m).unwrap(),
            Next::Turn(501)
        );
    }
}
