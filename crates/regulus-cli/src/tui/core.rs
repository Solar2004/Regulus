//! The terminal UI's line to the core. The screen never waits on it: requests go in, and
//! every answer comes back as an [`Event`] the screen applies when it next draws.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use jsonrpsee::ws_client::WsClient;
use regulus_core::{local, Config};
use regulus_proto::{
    method, Diagnostic, EventPage, Execution, Manifest, ModuleSummary, Profile, ProfileSaved,
    ProfileSummary, TemplateApplied, TemplateSummary, WorkflowSent,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use crate::rpc::{self, CallError};

/// A failed call with the core's code (`rpc::CallError`).
pub type Failure = CallError;

const CLIENT_NAME: &str = concat!("regulus-tui/", env!("CARGO_PKG_VERSION"));
const AGENT_PROMPT: &str = "agent.prompt";
const COMM_SEND: &str = "comm.send";
const COMM_HISTORY: &str = "comm.history";
const COMM_PARTICIPANTS: &str = "comm.participants";
/// Methods of the module that holds decisions for a person; absent, they answer -32601.
const HUMAN_LIST: &str = "human.list";
const HUMAN_ANSWER: &str = "human.answer";
const APPROVE: &str = "approve";
const REJECT: &str = "reject";
/// Who messages typed here are from.
pub const SENDER: &str = "user";

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// Executions and modules.
    Status,
    Profiles,
    Channel {
        execution: String,
        node: String,
    },
    /// `workflow: None` runs the profile's main workflow.
    Start {
        profile: String,
        workflow: Option<String>,
    },
    /// Every module's manifest.
    Manifests,
    Save {
        profile: Box<Profile>,
        force: bool,
    },
    Check {
        profile: String,
    },
    Templates,
    Apply {
        template: String,
        vars: serde_json::Map<String, Value>,
        id: Option<String>,
        force: bool,
    },
    /// Bus events of an execution after `since`.
    Events {
        execution: String,
        since: u64,
        limit: usize,
    },
    /// Decisions waiting for a person in an execution.
    Decisions {
        execution: String,
    },
    Decide {
        id: String,
        approve: bool,
    },
    /// A value for an input of the workflow an execution runs.
    WorkflowSend {
        execution: String,
        port: String,
        value: Value,
    },
    Stop {
        execution: String,
    },
    Prompt {
        execution: String,
        node: String,
        text: String,
    },
    Send {
        execution: String,
        node: String,
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connection {
    Connecting,
    /// No core answered, so one is being started.
    Starting,
    Connected {
        core: String,
        proto: String,
    },
    Failed(String),
}

/// One message of a channel, as `comm.history` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Message {
    pub id: u64,
    pub from: String,
    #[serde(default)]
    pub to: Vec<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub reply_to: Option<u64>,
    #[serde(default)]
    pub error: Option<String>,
}

/// One participant of a channel, as `comm.participants` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Participant {
    pub name: String,
    #[serde(default)]
    pub node: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub queued: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelState {
    pub messages: Vec<Message>,
    pub participants: Vec<Participant>,
}

/// A decision that waits for a person, as `human.list` gives it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Pending {
    pub id: String,
    #[serde(default)]
    pub kind: String,
    #[serde(default)]
    pub execution_id: String,
    #[serde(default)]
    pub node_id: String,
    #[serde(default)]
    pub value: Value,
    #[serde(default)]
    pub prompt: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Connection(Connection),
    Profiles(Vec<Profile>),
    Status {
        executions: Vec<Execution>,
        modules: Vec<ModuleSummary>,
    },
    Channel {
        execution: String,
        node: String,
        state: Result<ChannelState, String>,
    },
    Started(Result<Execution, String>),
    Stopped(Result<Execution, String>),
    Answer {
        execution: String,
        node: String,
        answer: Result<String, String>,
    },
    Sent {
        execution: String,
        node: String,
        result: Result<(), String>,
    },
    Manifests(Result<Vec<Manifest>, Failure>),
    Saved(Result<ProfileSaved, Failure>),
    Checked {
        profile: String,
        result: Result<Vec<Diagnostic>, Failure>,
    },
    Templates(Result<Vec<TemplateSummary>, Failure>),
    Applied(Result<TemplateApplied, Failure>),
    Events {
        execution: String,
        since: u64,
        result: Result<EventPage, Failure>,
    },
    Decisions {
        execution: String,
        result: Result<Vec<Pending>, Failure>,
    },
    Decided(Result<(), Failure>),
    WorkflowSent(Result<u64, Failure>),
}

/// Where requests go. Dropping it ends the line.
pub struct Core {
    requests: UnboundedSender<Request>,
}

impl Core {
    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }
}

/// Finds the core (or starts one, docs/04-client.md §4) and serves requests on the
/// current tokio runtime.
pub fn spawn(config: Config, timeout: Duration, events: mpsc::Sender<Event>) -> Core {
    let (requests, mut incoming) = unbounded_channel::<Request>();
    tokio::spawn(async move {
        let mut line: Option<Arc<WsClient>> = None;
        let mut first = true;
        while let Some(request) = incoming.recv().await {
            if !line.as_ref().is_some_and(|ws| ws.is_connected()) {
                let _ = events.send(Event::Connection(Connection::Connecting));
                match open(&config, timeout, first, &events).await {
                    Ok((ws, connection)) => {
                        line = Some(Arc::new(ws));
                        let _ = events.send(Event::Connection(connection));
                    }
                    Err(error) => {
                        line = None;
                        let _ = events
                            .send(Event::Connection(Connection::Failed(format!("{error:#}"))));
                    }
                }
                first = false;
            }
            let Some(ws) = line.clone() else { continue };
            let events = events.clone();
            // Agent turns take minutes: every request runs on its own.
            tokio::spawn(async move {
                let _ = events.send(answer(&ws, request).await);
            });
        }
    });
    Core { requests }
}

/// Connects to the core the home names. Only on the first try, if none answers, starts one.
async fn open(
    config: &Config,
    timeout: Duration,
    may_start: bool,
    events: &mpsc::Sender<Event>,
) -> Result<(WsClient, Connection)> {
    let found = match local::endpoint(config) {
        Some(endpoint) => rpc::connect(&endpoint, CLIENT_NAME, timeout).await,
        None => Err(anyhow::anyhow!(
            "no core has run in {}",
            config.home.display()
        )),
    };
    let (ws, hello) = match found {
        Ok(found) => found,
        Err(error) if !may_start || !config.client.autostart => {
            let program = local::core_program(config, std::env::current_exe().ok());
            anyhow::bail!(
                "{error:#}; start one with: {}",
                local::start_command(config, &program)
            )
        }
        Err(_) => {
            let _ = events.send(Event::Connection(Connection::Starting));
            let config = config.clone();
            let endpoint = tokio::task::spawn_blocking(move || {
                let program = local::core_program(&config, std::env::current_exe().ok());
                local::start(&config, &program)
            })
            .await?
            .map_err(anyhow::Error::msg)?;
            rpc::connect(&endpoint, CLIENT_NAME, timeout).await?
        }
    };
    Ok((
        ws,
        Connection::Connected {
            core: hello.core,
            proto: hello.proto,
        },
    ))
}

fn text(error: anyhow::Error) -> String {
    format!("{error:#}")
}

async fn answer(ws: &WsClient, request: Request) -> Event {
    match request {
        Request::Status => {
            let executions = rpc::call(ws, method::EXECUTION_LIST, Value::Null).await;
            let modules = rpc::call(ws, method::MODULE_LIST, Value::Null).await;
            match (executions, modules) {
                (Ok(executions), Ok(modules)) => Event::Status {
                    executions,
                    modules,
                },
                (Err(error), _) | (_, Err(error)) => {
                    Event::Connection(Connection::Failed(text(error)))
                }
            }
        }
        Request::Profiles => Event::Profiles(profiles(ws).await.unwrap_or_default()),
        Request::Channel { execution, node } => {
            let params = json!({ "execution_id": execution, "node_id": node });
            let messages = rpc::call(ws, COMM_HISTORY, params.clone()).await;
            let participants = rpc::call(ws, COMM_PARTICIPANTS, params).await;
            let state = match (messages, participants) {
                (Ok(messages), Ok(participants)) => Ok(ChannelState {
                    messages,
                    participants,
                }),
                (Err(error), _) | (_, Err(error)) => Err(text(error)),
            };
            Event::Channel {
                execution,
                node,
                state,
            }
        }
        Request::Start { profile, workflow } => {
            let params = json!({ "profile_id": profile, "workflow_id": workflow });
            Event::Started(
                rpc::call(ws, method::EXECUTION_START, params)
                    .await
                    .map_err(text),
            )
        }
        Request::Stop { execution } => {
            let params = json!({ "id": execution });
            Event::Stopped(
                rpc::call(ws, method::EXECUTION_STOP, params)
                    .await
                    .map_err(text),
            )
        }
        Request::Manifests => {
            Event::Manifests(rpc::call_coded(ws, method::MODULE_MANIFEST, Value::Null).await)
        }
        Request::Save { profile, force } => {
            let params = json!({ "profile": profile, "force": force });
            Event::Saved(rpc::call_coded(ws, method::PROFILE_SAVE, params).await)
        }
        Request::Check { profile } => {
            let params = json!({ "id": profile });
            let result = rpc::call_coded(ws, method::PROFILE_CHECK, params).await;
            Event::Checked { profile, result }
        }
        Request::Templates => {
            Event::Templates(rpc::call_coded(ws, method::TEMPLATE_LIST, Value::Null).await)
        }
        Request::Apply {
            template,
            vars,
            id,
            force,
        } => {
            let params = json!({ "template": template, "vars": vars, "id": id, "force": force });
            Event::Applied(rpc::call_coded(ws, method::TEMPLATE_APPLY, params).await)
        }
        Request::Events {
            execution,
            since,
            limit,
        } => {
            let params = json!({ "execution_id": execution, "since": since, "limit": limit });
            let result = rpc::call_coded(ws, method::EVENT_LIST, params).await;
            Event::Events {
                execution,
                since,
                result,
            }
        }
        Request::Decisions { execution } => {
            let params = json!({ "execution_id": execution });
            let result = rpc::call_coded(ws, HUMAN_LIST, params).await;
            Event::Decisions { execution, result }
        }
        Request::Decide { id, approve } => {
            let decision = if approve { APPROVE } else { REJECT };
            let params = json!({ "id": id, "decision": decision });
            let done: Result<Value, Failure> = rpc::call_coded(ws, HUMAN_ANSWER, params).await;
            Event::Decided(done.map(|_| ()))
        }
        Request::WorkflowSend {
            execution,
            port,
            value,
        } => {
            let params = json!({ "execution_id": execution, "port": port, "value": value });
            let sent: Result<WorkflowSent, Failure> =
                rpc::call_coded(ws, method::WORKFLOW_SEND, params).await;
            Event::WorkflowSent(sent.map(|sent| sent.event))
        }
        Request::Prompt {
            execution,
            node,
            text: prompt,
        } => {
            let params = json!({ "execution_id": execution, "node_id": node, "text": prompt });
            let turn: Result<Value> = rpc::call(ws, AGENT_PROMPT, params).await;
            let answer = turn.map_err(text).map(|turn| {
                let mut said = turn["text"].as_str().unwrap_or_default().to_string();
                if let Some(reason) = turn["stop_reason"].as_str().filter(|r| *r != "end_turn") {
                    said.push_str(&format!("\n(the turn ended with `{reason}`)"));
                }
                said
            });
            Event::Answer {
                execution,
                node,
                answer,
            }
        }
        Request::Send {
            execution,
            node,
            text: message,
        } => {
            let params = json!({
                "execution_id": execution, "node_id": node, "from": SENDER, "text": message,
            });
            let sent: Result<Value> = rpc::call(ws, COMM_SEND, params).await;
            Event::Sent {
                execution,
                node,
                result: sent.map(|_| ()).map_err(text),
            }
        }
    }
}

async fn profiles(ws: &WsClient) -> Result<Vec<Profile>> {
    let listed: Vec<ProfileSummary> = rpc::call(ws, method::PROFILE_LIST, Value::Null).await?;
    let mut profiles = Vec::with_capacity(listed.len());
    for summary in listed {
        let params = json!({ "id": summary.id });
        profiles.push(rpc::call(ws, method::PROFILE_GET, params).await?);
    }
    Ok(profiles)
}

/// JSON-RPC's "method not found": the module that serves it is not installed.
pub const METHOD_NOT_FOUND: i32 = -32601;

/// How the TUI words a failed call: by its code, never as a generic error. A module
/// method that is not there is "not installed"; one whose module cannot serve, "unavailable"
/// with the core's reason (docs/04-client.md §3, §8).
pub fn describe(failure: &Failure) -> String {
    use regulus_proto::code;
    let message = &failure.message;
    match failure.code {
        Some(METHOD_NOT_FOUND) => "□ not installed".to_string(),
        Some(code::MODULE_UNAVAILABLE) => format!("✗ unavailable: {message}"),
        Some(code::TEMPLATE_NOT_FOUND) => format!("✗ no such template: {message}"),
        Some(code::TEMPLATE_INVALID) => format!("✗ invalid template: {message}"),
        Some(code::TEMPLATE_UNMET) => format!("✗ a required module is missing: {message}"),
        Some(code::PROFILE_EXISTS) => format!("✗ already there: {message}"),
        Some(code::PROFILE_INVALID) => format!("✗ the profile has errors: {message}"),
        Some(code::PROFILE_IN_USE) => format!("✗ in use: {message}"),
        Some(code::INVALID_PARAMS) => format!("✗ invalid values: {message}"),
        _ => format!("✗ {message}"),
    }
}
