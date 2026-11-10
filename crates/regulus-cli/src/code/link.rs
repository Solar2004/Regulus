//! The coding screen's line to the core. Like the terminal UI's (`tui/core.rs`), the screen
//! never waits on it: requests go in, and every answer comes back as an [`Event`] the
//! screen applies when it next draws. What Operate already asks (the conversation, the
//! decisions, what runs) goes through its own requests; what only this screen asks (a
//! launch, the workspace, the tree of executions) is added here.

use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use jsonrpsee::ws_client::WsClient;
use regulus_core::Config;
use regulus_proto::{method, ExecutionTree};
use serde_json::{json, Value};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};

use super::inspect::{self, Ask};
use super::launch::{self, Failure, Survey};
use super::session::{Session, Step, Wanted};
use crate::rpc;
use crate::tui::core::{self, Connection};

#[derive(Debug, Clone, PartialEq)]
pub enum Request {
    /// What Operate asks too: status, the agent's history, sends, cancels, decisions,
    /// stopping an execution.
    Core(core::Request),
    /// Look at what the core has, to decide what a launch does.
    Survey(Box<Wanted>),
    /// Carry a step out: attach, start, or write a profile and start it.
    Launch { step: Step, wanted: Box<Wanted> },
    /// One read-only question to a workspace node.
    Inspect {
        execution: String,
        node: String,
        ask: Ask,
    },
    /// An execution and the children its nodes started.
    Tree { execution: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Core(core::Event),
    Surveyed(Result<Box<Survey>, Failure>),
    Launched(Result<Box<Session>, Failure>),
    Inspected {
        execution: String,
        node: String,
        ask: Ask,
        result: Result<Value, Failure>,
    },
    Tree {
        execution: String,
        result: Result<ExecutionTree, Failure>,
    },
}

/// Where requests go. Dropping it ends the line.
pub struct Link {
    requests: UnboundedSender<Request>,
}

impl Link {
    pub fn send(&self, request: Request) {
        let _ = self.requests.send(request);
    }
}

const CONNECTION_LOST: &str = "the core does not answer";

/// Finds the core (or starts one, docs/04-client.md §4) and serves requests on the
/// current tokio runtime. A request made while no core answers tries to reach one again.
pub fn spawn(config: Config, timeout: Duration, events: mpsc::Sender<Event>) -> Link {
    let (requests, mut incoming) = unbounded_channel::<Request>();
    tokio::spawn(async move {
        let mut line: Option<Arc<WsClient>> = None;
        let mut first = true;
        let say = |connection: Connection| Event::Core(core::Event::Connection(connection));
        while let Some(request) = incoming.recv().await {
            if !line.as_ref().is_some_and(|ws| ws.is_connected()) {
                let _ = events.send(say(Connection::Connecting));
                let starting = || {
                    let _ = events.send(say(Connection::Starting));
                };
                match core::open(&config, timeout, first, &starting).await {
                    Ok((ws, connection)) => {
                        line = Some(Arc::new(ws));
                        let _ = events.send(say(connection));
                    }
                    Err(error) => {
                        line = None;
                        let _ = events.send(say(Connection::Failed(format!("{error:#}"))));
                    }
                }
                first = false;
            }
            let Some(ws) = line.clone() else {
                // What was asked cannot be answered: say so where the screen waits for it.
                if let Some(event) = unanswered(&request) {
                    let _ = events.send(event);
                }
                continue;
            };
            let events = events.clone();
            let config = config.clone();
            // A launch starts an engine and a turn takes minutes: each request on its own.
            tokio::spawn(async move {
                let _ = events.send(answer(&ws, request, &config).await);
            });
        }
    });
    Link { requests }
}

/// What a request that could not be sent answers, when the screen is waiting for it.
fn unanswered(request: &Request) -> Option<Event> {
    let lost = || Failure {
        code: None,
        message: CONNECTION_LOST.into(),
    };
    match request {
        Request::Survey(_) => Some(Event::Surveyed(Err(lost()))),
        Request::Launch { .. } => Some(Event::Launched(Err(lost()))),
        Request::Inspect {
            execution,
            node,
            ask,
        } => Some(Event::Inspected {
            execution: execution.clone(),
            node: node.clone(),
            ask: ask.clone(),
            result: Err(lost()),
        }),
        Request::Tree { execution } => Some(Event::Tree {
            execution: execution.clone(),
            result: Err(lost()),
        }),
        Request::Core(_) => None,
    }
}

async fn answer(ws: &WsClient, request: Request, config: &Config) -> Event {
    match request {
        Request::Core(request) => Event::Core(core::answer(ws, request).await),
        Request::Survey(wanted) => {
            Event::Surveyed(launch::survey(ws, &wanted, config).await.map(Box::new))
        }
        Request::Launch { step, wanted } => {
            Event::Launched(launch::carry_out(ws, step, &wanted).await.map(Box::new))
        }
        Request::Inspect {
            execution,
            node,
            ask,
        } => {
            let params = ask.params(&execution, &node);
            let result = rpc::call_coded(ws, inspect::METHOD, params).await;
            Event::Inspected {
                execution,
                node,
                ask,
                result,
            }
        }
        Request::Tree { execution } => {
            let params = json!({ "execution_id": execution });
            let result = rpc::call_coded(ws, method::EXECUTION_TREE, params).await;
            Event::Tree { execution, result }
        }
    }
}
