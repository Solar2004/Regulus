//! Communication (docs/11-composition.md §3), after hcom by aannoo. A channel node takes
//! a message, works out who it is for, queues it per participant and delivers it over
//! `participant@1`; for an agent that is a prompt in its session, the way hcom typed into
//! the agent's terminal (docs/old/07-ecosystem.md §1.4).
//!
//! A participant says who it is and whether it is free (`participant.describe`); the
//! channel waits while it is busy, so a message never lands in the middle of a turn. How
//! the message reaches it (an ACP prompt for an agent) is the participant's business.
//!
//! What a participant answers is recorded, not delivered: an agent writes to another one
//! on purpose, never as a side effect of answering. Every message is kept in the channel
//! node's own directory, so the history outlives executions.

use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Sender};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use regulus_contract_participant::{Delivery, Identity, Reply, State, DELIVER, DESCRIBE};
use regulus_contract_tool as tool;
use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, NodeCall};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const MANIFEST: &str = include_str!("../regulus-module.toml");
const SEND: &str = "comm.send";
const HISTORY: &str = "comm.history";
const PARTICIPANTS: &str = "comm.participants";
const POLL_FIELD: &str = "poll_ms";
const MAX_WAIT_FIELD: &str = "max_wait_seconds";
const INBOX_FIELD: &str = "inbox_limit";
// The operations the channel offers over `tool@1`.
const TOOL_SEND: &str = "send";
const TOOL_WHO: &str = "participants";
const TOOL_INBOX: &str = "inbox";
const CHANNEL: &str = "channel";
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
struct ChannelParams {
    execution_id: String,
    node_id: String,
}

/// A participant as `comm.participants` shows it.
#[derive(Serialize)]
struct Seen {
    #[serde(flatten)]
    identity: Identity,
    node: String,
    /// Messages waiting for it, the one being delivered included.
    queued: usize,
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

/// How a channel waits for a busy participant, from its fields.
#[derive(Debug, Clone, Copy)]
struct Pace {
    poll: Duration,
    max_wait: Duration,
}

impl Pace {
    /// From a field reader; `setting` gives the node's value or the manifest default.
    fn read<'a>(setting: impl Fn(&str) -> Option<&'a Value>) -> Self {
        let number = |field| {
            setting(field)
                .and_then(Value::as_f64)
                .unwrap_or_default()
                .max(0.0)
        };
        Self {
            poll: Duration::from_secs_f64(number(POLL_FIELD) / 1000.0)
                .max(Duration::from_millis(1)),
            max_wait: Duration::from_secs_f64(number(MAX_WAIT_FIELD)),
        }
    }
}

/// One message on its way to one participant.
struct Job {
    to: Endpoint,
    /// Its name in the channel.
    name: String,
    delivery: Delivery,
    message_id: u64,
    log: PathBuf,
    pace: Pace,
}

/// A participant's queue and how many messages wait in it.
struct Queue {
    jobs: Sender<Job>,
    waiting: Arc<AtomicUsize>,
}

pub struct Communication {
    manifest: Manifest,
    ctx: ModuleCtx,
    log: Arc<Log>,
    /// One queue per participant of a running execution: `<execution>/<node>`.
    queues: Mutex<HashMap<String, Queue>>,
    /// The pace of each started channel; one not started (yet) uses the defaults.
    paces: Mutex<HashMap<NodeRef, Pace>>,
}

/// Who a participant is. One that does not say is named after its node.
fn identify(ctx: &ModuleCtx, endpoint: &Endpoint) -> Identity {
    let unknown = || Identity {
        name: endpoint.node().node_id.clone(),
        kind: String::new(),
        state: State::Unknown,
    };
    ctx.call(endpoint, DESCRIBE, Value::Null)
        .ok()
        .and_then(|answer| serde_json::from_value::<Identity>(answer).ok())
        .filter(|identity| !identity.name.trim().is_empty())
        .unwrap_or_else(unknown)
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
            paces: Mutex::default(),
        }
    }

    fn pace(&self, channel: &NodeRef) -> Pace {
        let started = self
            .paces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(channel)
            .copied();
        started.unwrap_or_else(|| {
            let channel = self.manifest.node_type(CHANNEL);
            Pace::read(|field| channel?.field(field)?.default.as_ref())
        })
    }

    /// The participants wired into a channel, with who they say they are.
    fn participants(&self, channel: &NodeRef) -> Result<Vec<(Endpoint, Identity)>, ModuleError> {
        let wired = self.ctx.resolve(channel, PARTICIPANTS_PORT);
        if wired.is_empty() {
            return Err(ModuleError::Failed(format!(
                "no channel `{}` with participants in execution `{}`",
                channel.node_id, channel.execution_id
            )));
        }
        let found: Vec<(Endpoint, Identity)> = wired
            .into_iter()
            .map(|endpoint| {
                let identity = identify(&self.ctx, &endpoint);
                (endpoint, identity)
            })
            .collect();
        for (i, (endpoint, identity)) in found.iter().enumerate() {
            if let Some((other, _)) = found[..i].iter().find(|(_, o)| o.name == identity.name) {
                return Err(ModuleError::Failed(format!(
                    "`{}` and `{}` are both called @{}: rename one",
                    other.node().node_id,
                    endpoint.node().node_id,
                    identity.name
                )));
            }
        }
        Ok(found)
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
        let wired = self.participants(&channel)?;
        let names: Vec<String> = wired.iter().map(|(_, i)| i.name.clone()).collect();
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
        let pace = self.pace(&channel);
        for (endpoint, identity) in wired.into_iter().filter(|(_, i)| to.contains(&i.name)) {
            self.enqueue(Job {
                to: endpoint,
                name: identity.name,
                delivery: Delivery {
                    channel: channel.node_id.clone(),
                    from: from.clone(),
                    text: text.to_string(),
                },
                message_id: message.id,
                log: log.clone(),
                pace,
            });
        }
        json(Sent { id: message.id, to })
    }

    fn enqueue(&self, job: Job) {
        let key = format!("{}/{}", job.to.node().execution_id, job.to.node().node_id);
        let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        let job = match queues.get(&key) {
            Some(queue) => {
                queue.waiting.fetch_add(1, Ordering::SeqCst);
                match queue.jobs.send(job) {
                    Ok(()) => return,
                    // Its worker is gone: start a new one with the same job.
                    Err(mpsc::SendError(job)) => job,
                }
            }
            None => job,
        };
        let (sender, jobs) = mpsc::channel::<Job>();
        let waiting = Arc::new(AtomicUsize::new(1));
        let (ctx, log, left) = (self.ctx.clone(), self.log.clone(), waiting.clone());
        std::thread::spawn(move || {
            for job in jobs {
                deliver(&ctx, &log, job);
                left.fetch_sub(1, Ordering::SeqCst);
            }
        });
        let _ = sender.send(job);
        queues.insert(
            key,
            Queue {
                jobs: sender,
                waiting,
            },
        );
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let pace = Pace::read(|field| self.manifest.setting(&call.node, field));
        let me = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        self.paces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(me, pace);
        Ok(Value::Null)
    }

    fn list(&self, params: ChannelParams) -> Result<Value, ModuleError> {
        let channel = NodeRef {
            execution_id: params.execution_id,
            node_id: params.node_id,
        };
        let queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        let seen: Vec<Seen> = self
            .participants(&channel)?
            .into_iter()
            .map(|(endpoint, identity)| {
                let node = endpoint.node();
                let key = format!("{}/{}", node.execution_id, node.node_id);
                Seen {
                    identity,
                    node: node.node_id.clone(),
                    queued: queues
                        .get(&key)
                        .map_or(0, |q| q.waiting.load(Ordering::SeqCst)),
                }
            })
            .collect();
        json(seen)
    }

    /// Drops the queues of an execution's participants once its channel stops; messages
    /// already queued are still delivered or fail on their own.
    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let prefix = format!("{}/", call.execution_id);
        self.paces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|node, _| node.execution_id != call.execution_id);
        self.queues
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|key, _| !key.starts_with(&prefix));
        Ok(Value::Null)
    }

    fn tool_call(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let request: tool::Call = parse(call.args.clone())?;
        let channel = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        let data = match request.name.as_str() {
            TOOL_SEND => self.send(SendParams {
                execution_id: channel.execution_id,
                node_id: channel.node_id,
                from: request.caller.name,
                text: request
                    .args
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            TOOL_WHO => self.list(ChannelParams {
                execution_id: channel.execution_id,
                node_id: channel.node_id,
            }),
            TOOL_INBOX => {
                let since = request
                    .args
                    .get("since")
                    .and_then(Value::as_u64)
                    .unwrap_or(0);
                let limit = self
                    .manifest
                    .setting(&call.node, INBOX_FIELD)
                    .and_then(Value::as_f64)
                    .map_or(usize::MAX, |n| n.max(0.0) as usize);
                let mut mine: Vec<Message> = Log::read(&self.log_path(&channel)?)
                    .map_err(ModuleError::Failed)?
                    .into_iter()
                    .filter(|m| m.id > since && m.to.contains(&request.caller.name))
                    .collect();
                let skip = mine.len().saturating_sub(limit);
                mine.drain(..skip);
                json(mine)
            }
            other => return Err(ModuleError::InvalidParams(format!("no tool `{other}`"))),
        };
        // What went wrong is an answer for the agent to read, not a broken call.
        Ok(match data {
            Ok(data) => json(tool::Outcome { data, error: false })?,
            Err(e) => json(tool::Outcome {
                data: Value::String(e.to_string()),
                error: true,
            })?,
        })
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

/// Waits until the participant can take a message: busy waits up to `max_wait`, gone
/// fails at once, unknown does not wait.
fn wait_free(ctx: &ModuleCtx, job: &Job) -> Result<(), ModuleError> {
    let since = Instant::now();
    loop {
        match identify(ctx, &job.to).state {
            State::Idle | State::Unknown => return Ok(()),
            State::Gone => {
                return Err(ModuleError::Failed(format!("@{} is not running", job.name)))
            }
            State::Busy if since.elapsed() >= job.pace.max_wait => {
                return Err(ModuleError::Failed(format!(
                    "@{} stayed busy for more than {} s",
                    job.name,
                    job.pace.max_wait.as_secs()
                )))
            }
            State::Busy => std::thread::sleep(job.pace.poll),
        }
    }
}

/// The operations a channel offers to agents. What they do is here; how they reach an
/// agent (MCP or anything else) is the tools box's business.
fn tool_specs() -> Vec<tool::Spec> {
    vec![
        tool::Spec {
            name: TOOL_SEND.into(),
            description: "Send a message to the channel. Mention participants with @name to \
                          send it only to them; without mentions it goes to everyone else. \
                          You send in your own name."
                .into(),
            input: serde_json::json!({
                "type": "object",
                "properties": { "text": { "type": "string", "description": "The message." } },
                "required": ["text"],
            }),
        },
        tool::Spec {
            name: TOOL_WHO.into(),
            description: "Who is in the channel: each participant's @name and whether it is \
                          idle, busy or gone."
                .into(),
            input: serde_json::json!({ "type": "object", "properties": {} }),
        },
        tool::Spec {
            name: TOOL_INBOX.into(),
            description: "Messages sent to you in the channel, oldest first. Pass `since` with \
                          the last id you saw to get only newer ones."
                .into(),
            input: serde_json::json!({
                "type": "object",
                "properties": { "since": { "type": "integer", "description": "Only ids above this." } },
            }),
        },
    ]
}

/// Hands one message to one participant and records what it answered, or why it failed.
fn deliver(ctx: &ModuleCtx, log: &Log, job: Job) {
    let participant = job.name.clone();
    let sender = job.delivery.from.clone();
    let outcome = wait_free(ctx, &job)
        .and_then(|()| {
            serde_json::to_value(&job.delivery).map_err(|e| ModuleError::Failed(e.to_string()))
        })
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
            PARTICIPANTS => self.list(parse(params)?),
            tool::LIST => json(tool_specs()),
            tool::CALL => self.tool_call(parse(params)?),
            method::NODE_START => self.start(parse(params)?),
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
