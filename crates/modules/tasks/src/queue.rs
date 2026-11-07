//! A queue's tasks and what can happen to them, without the store: [`Queue`] is loaded,
//! changed and saved back under the module's lock, so every change is atomic.

use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};

use regulus_core::{ModuleError, ModuleStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Todo,
    Claimed,
    Done,
    Failed,
    Blocked,
}

impl State {
    pub const ALL: [State; 5] = [
        State::Todo,
        State::Claimed,
        State::Done,
        State::Failed,
        State::Blocked,
    ];

    pub fn name(self) -> &'static str {
        match self {
            State::Todo => "todo",
            State::Claimed => "claimed",
            State::Done => "done",
            State::Failed => "failed",
            State::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    pub state: State,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lease_until: Option<u64>,
    #[serde(default)]
    pub attempts: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<Value>,
    pub seq: u64,
    pub created: u64,
    pub updated: u64,
}

/// A task as `add` takes it.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NewTask {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub deps: Vec<String>,
    #[serde(default)]
    pub data: Option<Value>,
}

/// The tasks a value on `add` describes: a task, an array of them (a text in it is a
/// title), or text with one per line (a line that is a JSON object is a task, any other
/// its title).
pub fn parse_new(value: &Value) -> Result<Vec<NewTask>, String> {
    let one = |value: &Value| -> Result<NewTask, String> {
        if let Value::String(title) = value {
            return Ok(NewTask {
                title: title.clone(),
                ..NewTask::default()
            });
        }
        let task: NewTask = serde_json::from_value(value.clone())
            .map_err(|e| format!("not a task ({e}): {value}"))?;
        if task.title.trim().is_empty() && task.id.is_none() {
            return Err(format!("a task needs a `title` or an `id`: {value}"));
        }
        Ok(task)
    };
    match value {
        Value::Array(items) => items.iter().map(one).collect(),
        Value::Object(_) => Ok(vec![one(value)?]),
        Value::String(text) => text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(|line| match serde_json::from_str::<Value>(line) {
                Ok(object @ Value::Object(_)) => one(&object),
                _ => Ok(NewTask {
                    title: line.to_string(),
                    ..NewTask::default()
                }),
            })
            .collect(),
        other => Err(format!("`add` takes a task, an array or text, not {other}")),
    }
}

/// Where tasks and ledgers live: the module's store, or memory in a core without one.
#[derive(Clone)]
pub enum Kv {
    Store(Arc<ModuleStore>),
    Memory(Arc<Mutex<BTreeMap<String, Value>>>),
}

impl Kv {
    fn memory(
        map: &Mutex<BTreeMap<String, Value>>,
    ) -> std::sync::MutexGuard<'_, BTreeMap<String, Value>> {
        map.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn get(&self, key: &str) -> Result<Option<Value>, ModuleError> {
        match self {
            Kv::Store(store) => store.get_json(key),
            Kv::Memory(map) => Ok(Self::memory(map).get(key).cloned()),
        }
    }

    pub fn set(&self, key: &str, value: &Value) -> Result<(), ModuleError> {
        match self {
            Kv::Store(store) => store.set_json(key, value),
            Kv::Memory(map) => {
                Self::memory(map).insert(key.to_string(), value.clone());
                Ok(())
            }
        }
    }

    /// Every key starting with `prefix`, in key order.
    pub fn list(&self, prefix: &str) -> Result<Vec<(String, Value)>, ModuleError> {
        match self {
            Kv::Store(store) => store
                .list(prefix)?
                .into_iter()
                .map(|(key, bytes)| {
                    serde_json::from_slice(&bytes)
                        .map(|value| (key.clone(), value))
                        .map_err(|e| ModuleError::Failed(format!("{key}: {e}")))
                })
                .collect(),
            Kv::Memory(map) => Ok(Self::memory(map)
                .range(prefix.to_string()..)
                .take_while(|(key, _)| key.starts_with(prefix))
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect()),
        }
    }
}

/// A queue name: one plain word, so its keys cannot reach another queue's.
pub fn check_name(queue: &str) -> Result<(), ModuleError> {
    if queue.is_empty() || queue.contains('/') || queue.trim() != queue {
        return Err(ModuleError::InvalidParams(format!(
            "`{queue}` is not a queue name: one word, no `/`"
        )));
    }
    Ok(())
}

/// One queue, loaded: its tasks in order and which of them changed.
pub struct Queue {
    kv: Kv,
    name: String,
    pub tasks: Vec<Task>,
    next_seq: u64,
    dirty: HashSet<usize>,
}

impl Queue {
    fn prefix(name: &str) -> String {
        format!("queue/{name}/task/")
    }

    pub fn load(kv: Kv, name: &str) -> Result<Self, ModuleError> {
        check_name(name)?;
        let mut tasks: Vec<Task> = Vec::new();
        for (key, value) in kv.list(&Self::prefix(name))? {
            match serde_json::from_value(value) {
                Ok(task) => tasks.push(task),
                Err(e) => eprintln!("regulus: tasks: skipping unreadable {key}: {e}"),
            }
        }
        tasks.sort_by_key(|t| t.seq);
        let next_seq = tasks.last().map_or(1, |t| t.seq + 1);
        Ok(Self {
            kv,
            name: name.to_string(),
            tasks,
            next_seq,
            dirty: HashSet::new(),
        })
    }

    pub fn save(&mut self) -> Result<(), ModuleError> {
        let prefix = Self::prefix(&self.name);
        let mut dirty: Vec<usize> = self.dirty.drain().collect();
        dirty.sort_unstable();
        for index in dirty {
            let task = &self.tasks[index];
            let value =
                serde_json::to_value(task).map_err(|e| ModuleError::Failed(e.to_string()))?;
            self.kv.set(&format!("{prefix}{}", task.id), &value)?;
        }
        Ok(())
    }

    fn touch(&mut self, index: usize, now: u64) {
        self.tasks[index].updated = now;
        self.dirty.insert(index);
    }

    fn index(&self, id: &str) -> Option<usize> {
        self.tasks.iter().position(|t| t.id == id)
    }

    /// Leases past `now` go back to `todo`, and every waiting task is `todo` or `blocked`
    /// as its dependencies say. Returns the tasks whose lease ran out, as they were.
    pub fn settle(&mut self, now: u64) -> Vec<Task> {
        let mut expired = Vec::new();
        for index in 0..self.tasks.len() {
            let task = &self.tasks[index];
            if task.state == State::Claimed && task.lease_until.is_some_and(|until| until <= now) {
                expired.push(task.clone());
                let task = &mut self.tasks[index];
                task.state = State::Todo;
                task.owner = None;
                task.lease_until = None;
                self.touch(index, now);
            }
        }
        let done: HashSet<String> = self
            .tasks
            .iter()
            .filter(|t| t.state == State::Done)
            .map(|t| t.id.clone())
            .collect();
        for index in 0..self.tasks.len() {
            let task = &self.tasks[index];
            if !matches!(task.state, State::Todo | State::Blocked) {
                continue;
            }
            let ready = task.deps.iter().all(|dep| done.contains(dep));
            let state = if ready { State::Todo } else { State::Blocked };
            if task.state != state {
                self.tasks[index].state = state;
                self.touch(index, now);
            }
        }
        expired
    }

    /// Adds every task or none: a repeated id, or more than `max` tasks in all, refuses
    /// the lot.
    pub fn add(&mut self, new: Vec<NewTask>, max: u64, now: u64) -> Result<Vec<Task>, String> {
        if (self.tasks.len() + new.len()) as u64 > max {
            return Err(format!(
                "queue `{}` holds {} tasks; {} more would pass `max_tasks` {max}",
                self.name,
                self.tasks.len(),
                new.len()
            ));
        }
        let mut seen: HashSet<String> = self.tasks.iter().map(|t| t.id.clone()).collect();
        let mut added = Vec::new();
        let mut seq = self.next_seq;
        for task in new {
            let id = task.id.clone().unwrap_or_else(|| format!("t{seq}"));
            if id.trim().is_empty() || id.contains('/') {
                return Err(format!("`{id}` is not a task id: not empty, no `/`"));
            }
            if !seen.insert(id.clone()) {
                return Err(format!("queue `{}` already has a task `{id}`", self.name));
            }
            let title = if task.title.trim().is_empty() {
                id.clone()
            } else {
                task.title
            };
            added.push(Task {
                id,
                title,
                state: State::Todo,
                deps: task.deps,
                owner: None,
                lease_until: None,
                attempts: 0,
                data: task.data,
                result: None,
                error: None,
                seq,
                created: now,
                updated: now,
            });
            seq += 1;
        }
        self.next_seq = seq;
        let first = self.tasks.len();
        self.tasks.extend(added);
        for index in first..self.tasks.len() {
            self.dirty.insert(index);
        }
        self.settle(now);
        Ok(self.tasks[first..].to_vec())
    }

    /// Tasks `owner` holds, oldest claim first.
    pub fn held(&self, owner: &str) -> Vec<&Task> {
        self.tasks
            .iter()
            .filter(|t| t.state == State::Claimed && t.owner.as_deref() == Some(owner))
            .collect()
    }

    /// The oldest free task, claimed for `owner` until `now + lease_ms` (0: no lease).
    pub fn claim(&mut self, owner: &str, lease_ms: u64, now: u64) -> Option<Task> {
        self.settle(now);
        let index = self.tasks.iter().position(|t| t.state == State::Todo)?;
        let task = &mut self.tasks[index];
        task.state = State::Claimed;
        task.owner = Some(owner.to_string());
        task.lease_until = (lease_ms > 0).then(|| now.saturating_add(lease_ms));
        task.attempts += 1;
        self.touch(index, now);
        Some(self.tasks[index].clone())
    }

    /// The task `owner` holds that `value` names: by its `id`, by a text that is the id,
    /// or the oldest it holds.
    fn which(&self, owner: &str, value: &Value) -> Result<usize, String> {
        let named = match value {
            Value::Object(map) => map.get("id").and_then(Value::as_str).map(str::to_string),
            Value::String(text) if self.index(text.trim()).is_some() => {
                Some(text.trim().to_string())
            }
            _ => None,
        };
        let index = match named {
            Some(id) => self
                .index(&id)
                .ok_or_else(|| format!("queue `{}` has no task `{id}`", self.name))?,
            None => {
                let oldest = self
                    .held(owner)
                    .into_iter()
                    .min_by_key(|t| (t.updated, t.seq))
                    .ok_or_else(|| format!("`{owner}` holds no task in queue `{}`", self.name))?;
                self.index(&oldest.id).expect("held tasks are in the queue")
            }
        };
        let task = &self.tasks[index];
        if task.state != State::Claimed || task.owner.as_deref() != Some(owner) {
            return Err(format!(
                "task `{}` is {} by {}, not claimed by `{owner}`",
                task.id,
                task.state.name(),
                task.owner.as_deref().unwrap_or("nobody")
            ));
        }
        Ok(index)
    }

    /// What `value` carries besides the task's id: its `result`/`error` field, or itself.
    fn payload(value: &Value, field: &str) -> Option<Value> {
        match value {
            Value::Object(map) if map.contains_key("id") => map.get(field).cloned(),
            Value::Null => None,
            other => Some(other.clone()),
        }
    }

    /// Marks the task `value` names `done`, keeping what came as its result.
    pub fn complete(&mut self, owner: &str, value: &Value, now: u64) -> Result<Task, String> {
        let index = self.which(owner, value)?;
        let result = Self::payload(value, "result");
        let task = &mut self.tasks[index];
        task.state = State::Done;
        task.lease_until = None;
        if result.is_some() {
            task.result = result;
        }
        self.touch(index, now);
        self.settle(now);
        Ok(self.tasks[index].clone())
    }

    /// Fails the task `value` names: back to `todo` while it has attempts left, `failed`
    /// after. The bool says whether it failed for good.
    pub fn fail(
        &mut self,
        owner: &str,
        value: &Value,
        max_attempts: u64,
        now: u64,
    ) -> Result<(Task, bool), String> {
        let index = self.which(owner, value)?;
        let error = Self::payload(value, "error");
        let task = &mut self.tasks[index];
        let for_good = task.attempts >= max_attempts.max(1);
        task.state = if for_good { State::Failed } else { State::Todo };
        task.owner = None;
        task.lease_until = None;
        if error.is_some() {
            task.error = error;
        }
        self.touch(index, now);
        self.settle(now);
        Ok((self.tasks[index].clone(), for_good))
    }

    /// Gives back every task `owner` holds.
    pub fn release(&mut self, owner: &str, now: u64) -> Vec<Task> {
        let mut released = Vec::new();
        for index in 0..self.tasks.len() {
            let task = &mut self.tasks[index];
            if task.state == State::Claimed && task.owner.as_deref() == Some(owner) {
                task.state = State::Todo;
                task.owner = None;
                task.lease_until = None;
                released.push(task.clone());
                self.touch(index, now);
            }
        }
        self.settle(now);
        released
    }

    /// The earliest lease still running.
    pub fn next_lease(&self) -> Option<u64> {
        self.tasks
            .iter()
            .filter(|t| t.state == State::Claimed)
            .filter_map(|t| t.lease_until)
            .min()
    }

    pub fn counts(&self) -> Value {
        let mut counts = serde_json::Map::new();
        for state in State::ALL {
            let n = self.tasks.iter().filter(|t| t.state == state).count();
            counts.insert(state.name().into(), json!(n));
        }
        Value::Object(counts)
    }

    /// Nothing to claim and nothing in hand.
    pub fn drained(&self) -> bool {
        !self
            .tasks
            .iter()
            .any(|t| matches!(t.state, State::Todo | State::Claimed))
    }

    pub fn snapshot(&self) -> Value {
        json!({ "queue": self.name, "tasks": self.tasks, "counts": self.counts() })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queue() -> Queue {
        Queue::load(Kv::Memory(Arc::default()), "q").unwrap()
    }

    fn add(q: &mut Queue, value: Value) -> Vec<Task> {
        q.add(parse_new(&value).unwrap(), 100, 1).unwrap()
    }

    #[test]
    fn dependencies_block_until_done_and_claims_go_oldest_first() {
        let mut q = queue();
        add(
            &mut q,
            json!([{ "id": "build", "title": "Build", "deps": ["setup"] }, { "id": "setup" }, "docs"]),
        );
        assert_eq!(q.tasks[0].state, State::Blocked);
        assert_eq!(q.tasks[2].id, "t3");
        let first = q.claim("me", 0, 2).unwrap();
        assert_eq!(first.id, "setup");
        assert_eq!(
            q.claim("me", 0, 2).unwrap().id,
            "t3",
            "build is still blocked"
        );
        assert!(q.claim("me", 0, 2).is_none());
        q.complete("me", &json!("setup"), 3).unwrap();
        assert_eq!(q.tasks[0].state, State::Todo, "unblocked");
        // Text that is not an id goes to the oldest held task as its result.
        let done = q.complete("me", &json!("written"), 4).unwrap();
        assert_eq!(
            (done.id.as_str(), done.result),
            ("t3", Some(json!("written")))
        );
        assert_eq!(q.claim("me", 0, 5).unwrap().id, "build");
        assert!(!q.drained());
        q.complete("me", &json!({ "id": "build", "result": 7 }), 6)
            .unwrap();
        assert!(q.drained());
        assert_eq!(q.counts()["done"], 3);
    }

    #[test]
    fn only_the_owner_finishes_and_a_lease_runs_out() {
        let mut q = queue();
        add(&mut q, json!("a\nb"));
        let a = q.claim("one", 10, 100).unwrap();
        assert_eq!(a.lease_until, Some(110));
        assert!(q.complete("two", &json!({ "id": a.id }), 101).is_err());
        assert_eq!(q.next_lease(), Some(110));
        let expired = q.settle(110);
        assert_eq!(expired.len(), 1);
        assert!(
            q.complete("one", &json!({ "id": "t1" }), 111).is_err(),
            "lease gone"
        );
        assert_eq!(q.claim("two", 0, 112).unwrap().id, "t1");
        assert_eq!(q.tasks[0].attempts, 2);
    }

    #[test]
    fn a_fail_retries_while_attempts_last_and_repeated_ids_are_refused() {
        let mut q = queue();
        add(&mut q, json!({ "id": "x", "title": "X" }));
        q.claim("me", 0, 1).unwrap();
        let (task, for_good) = q.fail("me", &json!("boom"), 2, 2).unwrap();
        assert!(!for_good);
        assert_eq!((task.state, task.error), (State::Todo, Some(json!("boom"))));
        q.claim("me", 0, 3).unwrap();
        let (task, for_good) = q.fail("me", &Value::Null, 2, 4).unwrap();
        assert!(for_good);
        assert_eq!(task.state, State::Failed);
        assert!(q
            .add(parse_new(&json!({ "id": "x" })).unwrap(), 100, 5)
            .is_err());
        assert!(
            q.add(parse_new(&json!(["a", "b"])).unwrap(), 2, 5).is_err(),
            "max"
        );
        assert!(parse_new(&json!(3)).is_err());
        assert!(parse_new(&json!({ "nope": 1 })).is_err());
        assert!(check_name("a/b").is_err());
    }
}
