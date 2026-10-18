//! Communication (docs/11-composition.md §3), after hcom by aannoo. A channel node takes
//! a message, works out who it is for, queues it per participant and delivers it over
//! `participant@1`; for an agent that is a prompt in its session, the way hcom typed into
//! the agent's terminal (docs/old/07-ecosystem.md §1.4).
//!
//! What a participant answers is recorded, not delivered: an agent writes to another one
//! on purpose, never as a side effect of answering. Every message is kept in the channel
//! node's own directory, so the history outlives executions.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use regulus_contract_participant::{Delivery, Reply, DELIVER};
use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, NodeCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MANIFEST: &str = include_str!("../regulus-module.toml");
const SEND: &str = "comm.send";
const HISTORY: &str = "comm.history";
const PARTICIPANTS_PORT: &str = "participants";
const LOG_FILE: &str = "messages.jsonl";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub id: u64,
    /// Seconds since the Unix epoch.
    pub at: u64,
    pub from: String,
    pub to: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
    /// The message this one answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<u64>,
    /// Why the message it answers could not be delivered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct SendParams {
    execution_id: String,
    node_id: String,
    /// The sender's name: a participant, or whoever writes from outside (the CLI says
    /// `user` unless told otherwise).
    from: String,
    text: String,
}

#[derive(Deserialize)]
struct HistoryParams {
    execution_id: String,
    node_id: String,
    /// Only the last `limit` messages.
    #[serde(default)]
    limit: Option<usize>,
}

#[derive(Serialize)]
struct Sent {
    id: u64,
    to: Vec<String>,
}

/// The channel's message file. One lock for every channel: writes are a line each.
#[derive(Default)]
struct Log {
    lock: Mutex<()>,
}

impl Log {
    fn read(path: &Path) -> Result<Vec<Message>, String> {
        let file = match std::fs::File::open(path) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(format!("cannot read {}: {e}", path.display())),
        };
        BufReader::new(file)
            .lines()
            .map(|line| {
                let line = line.map_err(|e| e.to_string())?;
                serde_json::from_str(&line)
                    .map_err(|e| format!("{} is damaged: {e}", path.display()))
            })
            .collect()
    }

    /// Writes the message with the next id and returns it.
    fn append(&self, path: &Path, mut message: Message) -> Result<Message, String> {
        let _guard = self.lock.lock().unwrap_or_else(PoisonError::into_inner);
        message.id = Self::read(path)?.last().map_or(1, |m| m.id + 1);
        message.at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let line = serde_json::to_string(&message).map_err(|e| e.to_string())?;
        OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut file| writeln!(file, "{line}"))
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        Ok(message)
    }
}

/// One message on its way to one participant.
struct Job {
    to: Endpoint,
    delivery: Delivery,
    message_id: u64,
    log: PathBuf,
}

pub struct Communication {
    manifest: Manifest,
    ctx: ModuleCtx,
    log: Arc<Log>,
    /// One queue per participant of a running execution: `<execution>/<node>`.
    queues: Mutex<HashMap<String, Sender<Job>>>,
}

fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, ModuleError> {
    serde_json::from_value(value).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn json(value: impl Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(|e| ModuleError::Failed(e.to_string()))
}

/// The names a text mentions with `@name`, in order, each once.
pub fn mentions(text: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for word in text.split(|c: char| !(c.is_alphanumeric() || "@-_".contains(c))) {
        let Some(name) = word.strip_prefix('@') else {
            continue;
        };
        let name = name.trim_end_matches(['-', '_']);
        if !name.is_empty() && !found.iter().any(|f| f == name) {
            found.push(name.to_string());
        }
    }
    found
}

/// Who a message from `from` is for: the participants it mentions, or all but the sender.
pub fn recipients(from: &str, text: &str, participants: &[String]) -> Result<Vec<String>, String> {
    let named = mentions(text);
    let to: Vec<String> = if named.is_empty() {
        participants
            .iter()
            .filter(|p| *p != from)
            .cloned()
            .collect()
    } else {
        let unknown: Vec<&String> = named
            .iter()
            .filter(|n| *n != from && !participants.contains(n))
            .collect();
        if !unknown.is_empty() {
            return Err(format!(
                "nobody here is called {}. Participants: {}",
                unknown
                    .iter()
                    .map(|n| format!("@{n}"))
                    .collect::<Vec<_>>()
                    .join(", "),
                participants.join(", ")
            ));
        }
        named.into_iter().filter(|n| n != from).collect()
    };
    if to.is_empty() {
        return Err("the message has nobody to go to: wire a participant into the channel".into());
    }
    Ok(to)
}

impl Communication {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("communication manifest is valid"),
            ctx: ModuleCtx::default(),
            log: Arc::default(),
            queues: Mutex::default(),
        }
    }

    fn log_path(&self, channel: &NodeRef) -> Result<PathBuf, ModuleError> {
        Ok(self.ctx.node_dir(channel)?.join(LOG_FILE))
    }

    fn send(&self, params: SendParams) -> Result<Value, ModuleError> {
        let channel = NodeRef {
            execution_id: params.execution_id,
            node_id: params.node_id,
        };
        let text = params.text.trim();
        if text.is_empty() {
            return Err(ModuleError::InvalidParams("the message is empty".into()));
        }
        let wired = self.ctx.resolve(&channel, PARTICIPANTS_PORT);
        if wired.is_empty() {
            return Err(ModuleError::Failed(format!(
                "no channel `{}` with participants in execution `{}`",
                channel.node_id, channel.execution_id
            )));
        }
        let names: Vec<String> = wired.iter().map(|e| e.node().node_id.clone()).collect();
        let from = params.from.trim().to_string();
        if from.is_empty() || from.contains(char::is_whitespace) {
            return Err(ModuleError::InvalidParams(format!(
                "`{from}` is not a sender name"
            )));
        }
        let to = recipients(&from, text, &names).map_err(ModuleError::Failed)?;
        let log = self.log_path(&channel)?;
        let message = self
            .log
            .append(
                &log,
                Message {
                    id: 0,
                    at: 0,
                    from: from.clone(),
                    to: to.clone(),
                    text: text.to_string(),
                    reply_to: None,
                    error: None,
                },
            )
            .map_err(ModuleError::Failed)?;
        for endpoint in wired.into_iter().filter(|e| to.contains(&e.node().node_id)) {
            self.enqueue(Job {
                to: endpoint,
                delivery: Delivery {
                    channel: channel.node_id.clone(),
                    from: from.clone(),
                    text: text.to_string(),
                },
                message_id: message.id,
                log: log.clone(),
            });
        }
        json(Sent { id: message.id, to })
    }

    fn enqueue(&self, job: Job) {
        let key = format!("{}/{}", job.to.node().execution_id, job.to.node().node_id);
        let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        let job = match queues.get(&key) {
            Some(queue) => match queue.send(job) {
                Ok(()) => return,
                // Its worker is gone: start a new one with the same job.
                Err(mpsc::SendError(job)) => job,
            },
            None => job,
        };
        let (queue, jobs) = mpsc::channel::<Job>();
        let (ctx, log) = (self.ctx.clone(), self.log.clone());
        std::thread::spawn(move || {
            for job in jobs {
                deliver(&ctx, &log, job);
            }
        });
        let _ = queue.send(job);
        queues.insert(key, queue);
    }

    /// Drops the queues of an execution's participants once its channel stops; messages
    /// already queued are still delivered or fail on their own.
    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let prefix = format!("{}/", call.execution_id);
        self.queues
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|key, _| !key.starts_with(&prefix));
        Ok(Value::Null)
    }

    fn history(&self, params: HistoryParams) -> Result<Value, ModuleError> {
        let channel = NodeRef {
            execution_id: params.execution_id,
            node_id: params.node_id,
        };
        let mut messages = Log::read(&self.log_path(&channel)?).map_err(ModuleError::Failed)?;
        if let Some(limit) = params.limit {
            let skip = messages.len().saturating_sub(limit);
            messages.drain(..skip);
        }
        json(messages)
    }
}

/// Hands one message to one participant and records what it answered, or why it failed.
fn deliver(ctx: &ModuleCtx, log: &Log, job: Job) {
    let participant = job.to.node().node_id.clone();
    let sender = job.delivery.from.clone();
    let outcome = serde_json::to_value(&job.delivery)
        .map_err(|e| ModuleError::Failed(e.to_string()))
        .and_then(|args| ctx.call(&job.to, DELIVER, args))
        .and_then(parse::<Reply>);
    let (text, error) = match outcome {
        Ok(reply) if reply.text.trim().is_empty() => return,
        Ok(reply) => (reply.text, None),
        Err(e) => (String::new(), Some(e.to_string())),
    };
    let answer = Message {
        id: 0,
        at: 0,
        from: participant,
        to: vec![sender],
        text,
        reply_to: Some(job.message_id),
        error,
    };
    if let Err(e) = log.append(&job.log, answer) {
        eprintln!("regulus: communication: {e}");
    }
}

impl Default for Communication {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Communication {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            SEND => self.send(parse(params)?),
            HISTORY => self.history(parse(params)?),
            method::NODE_STOP => self.stop(parse(params)?),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.queues
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn mentions_are_found_once_and_in_order() {
        assert_eq!(
            mentions("@dev and @reviewer, ping @dev."),
            names(&["dev", "reviewer"])
        );
        assert_eq!(mentions("mail me at a@b and @x-1_"), names(&["x-1"]));
        assert!(mentions("no names here").is_empty());
    }

    #[test]
    fn a_message_goes_to_who_it_names_or_to_everyone_else() {
        let here = names(&["dev", "reviewer", "tester"]);
        assert_eq!(
            recipients("dev", "all good?", &here).unwrap(),
            names(&["reviewer", "tester"])
        );
        assert_eq!(
            recipients("user", "@tester run it", &here).unwrap(),
            names(&["tester"])
        );
        assert_eq!(
            recipients("dev", "@dev @reviewer look", &here).unwrap(),
            names(&["reviewer"])
        );
        let error = recipients("user", "@nobody hi", &here).unwrap_err();
        assert!(
            error.contains("@nobody") && error.contains("reviewer"),
            "{error}"
        );
        assert!(recipients("dev", "hi", &names(&["dev"])).is_err());
    }

    #[test]
    fn the_log_numbers_messages_and_keeps_them() {
        let dir = std::env::temp_dir().join(format!("regulus-comm-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(LOG_FILE);
        let log = Log::default();
        let message = |text: &str| Message {
            id: 0,
            at: 0,
            from: "user".into(),
            to: names(&["dev"]),
            text: text.into(),
            reply_to: None,
            error: None,
        };
        assert_eq!(log.append(&path, message("one")).unwrap().id, 1);
        assert_eq!(log.append(&path, message("two")).unwrap().id, 2);
        let read = Log::read(&path).unwrap();
        assert_eq!(read.len(), 2);
        assert_eq!(read[1].text, "two");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
