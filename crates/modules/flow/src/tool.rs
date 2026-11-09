//! `tool`: a workflow offered as a `tool@1` operation (docs/13-blocks.md §3.2). An agent
//! calls it through the Tools box like any other tool; each call runs the workflow of
//! `workflow` as a called child with the arguments on its "then" inputs.
//!
//! The arguments' schema comes from the node's own "then" inputs, which mirror the
//! workflow's like `call`'s do (`profile.check` warns when they do not line up): one
//! property per input, typed by capability; or, with `input` set, a single `text` string
//! for that input. What the child returns also leaves on the node's output of the same id,
//! if it has one. Whatever is wired into the node's "uses" inputs goes to the child's of
//! the same id, so the implementation behind the tool is swapped without touching it.
//!
//! `call` mode waits for the answer up to `timeout_ms`. It blocks the thread the call came
//! on: a call that arrives on the bus thread of the node's own execution (a module calling
//! `tool.call` while handling an event) would wait for news that thread has to deliver,
//! until the timeout. Calls through the Tools box come over RPC, never on a bus thread.
//! `job` mode answers `{job}` at once and offers `<name>_status`, `<name>_result` and
//! `<name>_cancel`; its table is checkpointed, and after a restart of the core a job that
//! was running is `interrupted` (children are not resumed).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use regulus_contract_tool as tool;
use regulus_core::{ChildOptions, ModuleError, NodeRef};
use regulus_proto::{ChildEvent, ChildEventKind, Direction, NodeStart, WireKind};
use serde_json::{json, Map, Value};

use crate::{batch, bound, lock, Here};

const STATUS: &str = "_status";
const RESULT: &str = "_result";
const CANCEL: &str = "_cancel";
/// The one property of the schema when `input` names the input that takes the text.
const TEXT_ARG: &str = "text";
const JOB_ARG: &str = "job";

/// A call waiting for its child: the port and value it returned, or why it did not.
#[derive(Default)]
struct Waiter {
    answer: Mutex<Option<Result<(String, Value), String>>>,
    wake: Condvar,
}

struct Job {
    tag: String,
    child: Option<String>,
    state: &'static str,
    result: Option<Value>,
    error: Option<String>,
}

impl Job {
    fn view(&self) -> Value {
        let mut view = json!({ "job": self.child, "state": self.state });
        if let Some(result) = &self.result {
            view["result"] = result.clone();
        }
        if let Some(error) = &self.error {
            view["error"] = json!(error);
        }
        view
    }

    fn running(&self) -> bool {
        self.state == "running"
    }
}

/// The calls waiting and the jobs of every `tool` node.
#[derive(Default)]
pub(crate) struct Tools {
    next: AtomicU64,
    calls: Mutex<HashMap<String, (NodeRef, Arc<Waiter>)>>,
    jobs: Mutex<HashMap<NodeRef, Vec<Job>>>,
}

impl Tools {
    /// A node that stops answers its waiting calls and forgets its jobs.
    pub(crate) fn stop(&self, me: &NodeRef) {
        let waiting: Vec<Arc<Waiter>> = lock(&self.calls)
            .values()
            .filter(|(node, _)| node == me)
            .map(|(_, w)| w.clone())
            .collect();
        for waiter in waiting {
            answer(&waiter, Err("the tool stopped".into()));
        }
        lock(&self.jobs).remove(me);
    }
}

fn answer(waiter: &Waiter, outcome: Result<(String, Value), String>) {
    let mut slot = lock(&waiter.answer);
    if slot.is_none() {
        *slot = Some(outcome);
        waiter.wake.notify_all();
    }
}

/// The node's name for the tool: `name`, or its id.
fn name(here: &Here) -> Result<String, ModuleError> {
    let name = match here.text("name").trim() {
        "" => here.me.node_id.clone(),
        name => name.to_string(),
    };
    let valid = name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if name.is_empty() || !valid {
        return Err(ModuleError::InvalidParams(format!(
            "tool `{}`: name `{name}` may only hold letters, digits, `_` and `-`",
            here.me.node_id
        )));
    }
    Ok(name)
}

fn workflow(here: &Here) -> Result<String, ModuleError> {
    match here.text("workflow").trim() {
        "" => Err(ModuleError::InvalidParams(format!(
            "tool `{}` names no workflow in `workflow`",
            here.me.node_id
        ))),
        id => Ok(id.to_string()),
    }
}

fn job_mode(here: &Here) -> Result<bool, ModuleError> {
    match here.text("mode").as_str() {
        "call" => Ok(false),
        "job" => Ok(true),
        other => Err(ModuleError::InvalidParams(format!(
            "tool `{}`: `mode` is `call` or `job`, not `{other}`",
            here.me.node_id
        ))),
    }
}

/// The node's "then" inputs with their capabilities: what the workflow takes.
fn inputs_of(here: &Here) -> Vec<(String, String)> {
    here.call
        .node
        .ports
        .iter()
        .filter(|p| p.kind == WireKind::Then && p.direction == Direction::Input)
        .map(|p| (p.id.clone(), p.capability.clone()))
        .collect()
}

/// The JSON Schema type of a value of `capability`; anything not known is any value.
fn json_type(capability: &str) -> Value {
    match capability {
        "text@1" | "path@1" => json!({ "type": "string" }),
        _ => json!({}),
    }
}

fn schema(here: &Here) -> Value {
    let single = here.text("input");
    if !single.trim().is_empty() {
        return json!({ "type": "object",
                       "properties": { TEXT_ARG: { "type": "string" } },
                       "required": [TEXT_ARG] });
    }
    let inputs = inputs_of(here);
    let mut properties = Map::new();
    for (id, capability) in &inputs {
        properties.insert(id.clone(), json_type(capability));
    }
    let required: Vec<&String> = if inputs.len() == 1 {
        Vec::new()
    } else {
        inputs.iter().map(|(id, _)| id).collect()
    };
    json!({ "type": "object", "properties": properties, "required": required })
}

/// The child's "then" inputs from the arguments. With one input, a missing property takes
/// the arguments whole, so a caller that names it otherwise still gets through.
fn child_inputs(here: &Here, args: &Value) -> Result<Vec<(String, Value)>, String> {
    let single = here.text("input");
    let single = single.trim();
    if !single.is_empty() {
        let text = match args {
            Value::String(text) => text.clone(),
            other => match other.get(TEXT_ARG) {
                Some(Value::String(text)) => text.clone(),
                Some(other) => other.to_string(),
                None => return Err(format!("missing argument `{TEXT_ARG}`")),
            },
        };
        return Ok(vec![(single.to_string(), Value::String(text))]);
    }
    let inputs = inputs_of(here);
    if let [(id, _)] = inputs.as_slice() {
        let value = args.get(id).cloned().unwrap_or_else(|| args.clone());
        return Ok(vec![(id.clone(), value)]);
    }
    inputs
        .into_iter()
        .map(|(id, _)| match args.get(&id) {
            Some(value) => Ok((id, value.clone())),
            None => Err(format!("missing argument `{id}`")),
        })
        .collect()
}

pub(crate) fn list(here: &Here) -> Result<Value, ModuleError> {
    let name = name(here)?;
    let workflow = workflow(here)?;
    let description = match here.text("description").trim() {
        "" => format!("Runs the workflow `{workflow}` and answers what it returns."),
        text => text.to_string(),
    };
    let job = json!({ "type": "object", "properties": { JOB_ARG: { "type": "string" } },
                      "required": [JOB_ARG] });
    let mut specs = vec![tool::Spec {
        name: name.clone(),
        description: if job_mode(here)? {
            format!("{description} Starts it as a job and answers `{{job}}` at once.")
        } else {
            description
        },
        input: schema(here),
    }];
    if job_mode(here)? {
        for (suffix, what) in [
            (STATUS, "Whether a job of this tool still runs."),
            (
                RESULT,
                "What a job of this tool returned, or that it is still pending.",
            ),
            (CANCEL, "Stops a job of this tool."),
        ] {
            specs.push(tool::Spec {
                name: format!("{name}{suffix}"),
                description: what.into(),
                input: job.clone(),
            });
        }
    }
    serde_json::to_value(specs).map_err(|e| ModuleError::Failed(e.to_string()))
}

pub(crate) fn call(here: &Here, request: tool::Call) -> Result<Value, ModuleError> {
    let outcome = match run(here, &request) {
        Ok(data) => tool::Outcome { data, error: false },
        // What went wrong is an answer for the agent to read, not a broken call.
        Err(error) => tool::Outcome {
            data: Value::String(error),
            error: true,
        },
    };
    serde_json::to_value(outcome).map_err(|e| ModuleError::Failed(e.to_string()))
}

fn run(here: &Here, request: &tool::Call) -> Result<Value, String> {
    let name = name(here).map_err(|e| e.to_string())?;
    let job = job_mode(here).map_err(|e| e.to_string())?;
    if request.name == name {
        return if job {
            start_job(here, &request.args)
        } else {
            call_and_wait(here, &request.args)
        };
    }
    let suffix = request
        .name
        .strip_prefix(name.as_str())
        .filter(|_| job)
        .ok_or_else(|| format!("no tool `{}`", request.name))?;
    let id = request
        .args
        .get(JOB_ARG)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("missing argument `{JOB_ARG}`"))?;
    match suffix {
        STATUS => with_job(here, id, |job| job.view()),
        RESULT => {
            let view = with_job(here, id, |job| job.view())?;
            match view["state"].as_str() {
                Some("running") => Ok(json!({ "job": id, "pending": true })),
                Some("returned") => Ok(view["result"].clone()),
                _ => Err(view["error"].as_str().unwrap_or("no result").to_string()),
            }
        }
        CANCEL => {
            let running = with_job(here, id, Job::running)?;
            if running {
                here.ctx()
                    .stop_child(&here.me, id)
                    .map_err(|e| e.to_string())?;
                set_job(here, id, |job| {
                    if job.running() {
                        job.state = "cancelled";
                        job.error = Some("cancelled".into());
                    }
                });
            }
            with_job(here, id, |job| job.view())
        }
        _ => Err(format!("no tool `{}`", request.name)),
    }
}

/// Calls and running jobs of the node, against `max_in_flight` (0: no cap here).
fn check_room(here: &Here, tools: &Tools) -> Result<(), String> {
    let max = here.number("max_in_flight");
    if max == 0 {
        return Ok(());
    }
    let calls = lock(&tools.calls)
        .values()
        .filter(|(node, _)| *node == here.me)
        .count();
    let jobs = lock(&tools.jobs)
        .get(&here.me)
        .map_or(0, |jobs| jobs.iter().filter(|j| j.running()).count());
    if (calls + jobs) as u64 >= max {
        return Err(format!(
            "`{}` already runs {max} calls (`max_in_flight`): try again later",
            here.me.node_id
        ));
    }
    Ok(())
}

fn options(here: &Here, tag: &str) -> ChildOptions {
    ChildOptions {
        tag: Some(tag.to_string()),
        bindings: bound::of(here.ctx(), &here.me, &here.call.node),
        ..ChildOptions::default()
    }
}

fn call_and_wait(here: &Here, args: &Value) -> Result<Value, String> {
    let tools = &here.flow.tools;
    check_room(here, tools)?;
    let inputs = child_inputs(here, args)?;
    let workflow = workflow(here).map_err(|e| e.to_string())?;
    let tag = format!("tool/{}", tools.next.fetch_add(1, Ordering::Relaxed) + 1);
    let waiter = Arc::new(Waiter::default());
    lock(&tools.calls).insert(tag.clone(), (here.me.clone(), waiter.clone()));
    let mut opts = options(here, &tag);
    let started = bound::start(here.ctx(), &here.me, &workflow, inputs, &mut opts);
    let child = match started {
        Ok(child) => child,
        Err(error) => {
            lock(&tools.calls).remove(&tag);
            return Err(error.to_string());
        }
    };
    let timeout = match here.number("timeout_ms") {
        0 => None,
        ms => Some(Duration::from_millis(ms)),
    };
    let deadline = timeout.map(|t| Instant::now() + t);
    let mut slot = lock(&waiter.answer);
    let outcome = loop {
        if let Some(outcome) = slot.take() {
            break Some(outcome);
        }
        match deadline {
            None => {
                slot = waiter
                    .wake
                    .wait(slot)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
            }
            Some(deadline) => {
                let left = deadline.saturating_duration_since(Instant::now());
                if left.is_zero() {
                    break None;
                }
                slot = waiter
                    .wake
                    .wait_timeout(slot, left)
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .0;
            }
        }
    };
    drop(slot);
    lock(&tools.calls).remove(&tag);
    match outcome {
        Some(Ok((port, value))) => Ok(json!({ "port": port, "value": value })),
        Some(Err(error)) => Err(error),
        None => {
            let _ = here.ctx().stop_child(&here.me, &child);
            let ms = timeout.map_or(0, |t| t.as_millis());
            Err(format!("`{workflow}` did not answer within {ms} ms"))
        }
    }
}

fn start_job(here: &Here, args: &Value) -> Result<Value, String> {
    let tools = &here.flow.tools;
    check_room(here, tools)?;
    let inputs = child_inputs(here, args)?;
    let workflow = workflow(here).map_err(|e| e.to_string())?;
    let tag = format!("tool/{}", tools.next.fetch_add(1, Ordering::Relaxed) + 1);
    lock(&tools.jobs)
        .entry(here.me.clone())
        .or_default()
        .push(Job {
            tag: tag.clone(),
            child: None,
            state: "running",
            result: None,
            error: None,
        });
    let mut opts = options(here, &tag);
    let started = bound::start(here.ctx(), &here.me, &workflow, inputs, &mut opts);
    let mut jobs = lock(&tools.jobs);
    let list = jobs.entry(here.me.clone()).or_default();
    let Some(index) = list.iter().position(|j| j.tag == tag) else {
        return Err("the tool stopped".into());
    };
    match started {
        Ok(child) => {
            list[index].child = Some(child.clone());
            forget_oldest(list, here.number("jobs_keep"));
            drop(jobs);
            save(here);
            Ok(json!({ "job": child }))
        }
        Err(error) => {
            list.remove(index);
            Err(error.to_string())
        }
    }
}

/// Keeps at most `keep` finished jobs (0: all), dropping the oldest.
fn forget_oldest(jobs: &mut Vec<Job>, keep: u64) {
    if keep == 0 {
        return;
    }
    let keep = usize::try_from(keep).unwrap_or(usize::MAX);
    let finished = jobs.iter().filter(|j| !j.running()).count();
    let mut extra = finished.saturating_sub(keep);
    jobs.retain(|job| {
        if extra > 0 && !job.running() {
            extra -= 1;
            return false;
        }
        true
    });
}

fn with_job<T>(here: &Here, id: &str, f: impl FnOnce(&Job) -> T) -> Result<T, String> {
    lock(&here.flow.tools.jobs)
        .get(&here.me)
        .and_then(|jobs| jobs.iter().find(|j| j.child.as_deref() == Some(id)))
        .map(f)
        .ok_or_else(|| format!("no job `{id}`"))
}

fn set_job(here: &Here, id: &str, f: impl FnOnce(&mut Job)) {
    if let Some(job) = lock(&here.flow.tools.jobs)
        .get_mut(&here.me)
        .and_then(|jobs| jobs.iter_mut().find(|j| j.child.as_deref() == Some(id)))
    {
        f(job);
    }
    save(here);
}

/// The job table as a checkpoint: `{ jobs: { <job>: { state, result?, error? } } }`. A
/// core without a store keeps it in memory only.
fn save(here: &Here) {
    let jobs: Map<String, Value> = lock(&here.flow.tools.jobs)
        .get(&here.me)
        .map(|jobs| {
            jobs.iter()
                .filter_map(|job| {
                    let mut view = job.view();
                    let id = view.as_object_mut()?.remove(JOB_ARG)?;
                    Some((id.as_str()?.to_string(), view))
                })
                .collect()
        })
        .unwrap_or_default();
    let _ = here.ctx().checkpoint(&here.me, json!({ "jobs": jobs }));
}

/// Back from a checkpoint: the jobs it lists, a running one as `interrupted`.
pub(crate) fn start(here: &Here) -> Result<Value, ModuleError> {
    let start: NodeStart = serde_json::from_value(here.call.args.clone()).unwrap_or_default();
    let Some(Value::Object(saved)) = start.checkpoint.and_then(|c| c.get("jobs").cloned()) else {
        return Ok(Value::Null);
    };
    let jobs: Vec<Job> = saved
        .into_iter()
        .map(|(id, view)| {
            let (state, error) = match view["state"].as_str() {
                Some("returned") => ("returned", None),
                Some("failed") => ("failed", view["error"].as_str().map(String::from)),
                Some("cancelled") => ("cancelled", Some("cancelled".to_string())),
                _ => (
                    "interrupted",
                    Some("the core restarted while it ran".to_string()),
                ),
            };
            Job {
                tag: id.clone(),
                child: Some(id),
                state,
                result: view.get("result").cloned(),
                error,
            }
        })
        .collect();
    lock(&here.flow.tools.jobs).insert(here.me.clone(), jobs);
    save(here);
    Ok(Value::Null)
}

/// News from a child: the answer of a waiting call or of a job. What it returned also
/// leaves on the node's output of the same id.
pub(crate) fn child(here: &Here, child: ChildEvent) -> Result<Value, ModuleError> {
    let Some(tag) = child.tag else {
        return Ok(Value::Null);
    };
    let tools = &here.flow.tools;
    let returned = match &child.kind {
        ChildEventKind::Returned { port, value, .. }
        | ChildEventKind::Output { port, value, .. } => Some((port.clone(), value.clone())),
        ChildEventKind::Ended { .. } => None,
    };
    let waiter = lock(&tools.calls).get(&tag).map(|(_, w)| w.clone());
    if let Some(waiter) = waiter {
        let outcome = match (&returned, &child.kind) {
            (Some(answer), _) => Ok(answer.clone()),
            (None, ChildEventKind::Ended { state }) => Err(batch::reason(state)),
            (None, _) => return Ok(Value::Null),
        };
        answer(&waiter, outcome);
    } else {
        let mut changed = false;
        if let Some(job) = lock(&tools.jobs)
            .get_mut(&here.me)
            .and_then(|jobs| jobs.iter_mut().find(|j| j.tag == tag))
            .filter(|job| job.running())
        {
            match (&returned, &child.kind) {
                (Some((port, value)), _) => {
                    job.state = "returned";
                    job.result = Some(json!({ "port": port, "value": value }));
                }
                (None, ChildEventKind::Ended { state }) => {
                    job.state = "failed";
                    job.error = Some(batch::reason(state));
                }
                _ => {}
            }
            changed = true;
        }
        if changed {
            save(here);
        }
    }
    if let Some((port, value)) = returned {
        if here.has_output(&port) {
            here.emit(&port, value)?;
        }
    }
    Ok(Value::Null)
}
