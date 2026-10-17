//! The ACP channel engine modules share (docs/12-agents.md §4). It is a library, not a
//! module: an engine decides how to launch its CLI and hands the result here.
//!
//! One [`Session`] is one agent process with one ACP session. The protocol is async; a
//! session runs it on a thread of its own, so modules, whose calls are synchronous, can
//! use it without a runtime. Dropping a session kills the agent's whole process group.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

pub mod engine;

use agent_client_protocol::schema::v1::{
    ContentBlock, ContentChunk, InitializeRequest, NewSessionRequest, PermissionOptionKind,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{
    on_receive_request, AcpAgent, AcpAgentConfig, ActiveSession, Agent, Client, ConnectionTo,
    SessionMessage,
};
use futures::channel::{mpsc as async_mpsc, oneshot};
use futures::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// How to start the agent process.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub program: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    /// Added to the environment the agent inherits.
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    /// Removed from the environment the agent inherits.
    #[serde(default)]
    pub env_remove: Vec<String>,
}

/// What the session answers when the agent asks for permission. Asking a person needs
/// the bus to reach a client; until then a node chooses one of these.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permissions {
    Allow,
    #[default]
    Reject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionOptions {
    pub cwd: PathBuf,
    #[serde(default)]
    pub permissions: Permissions,
    /// Sent as `_meta` of `session/new`: agent-specific options (docs/12-agents.md §3).
    #[serde(default)]
    pub meta: Option<Map<String, Value>>,
    /// How long the agent may take to start and open the session.
    pub startup: Duration,
}

/// Who answered `initialize`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
}

/// The result of one prompt: the agent's text and why it stopped (`end_turn`,
/// `max_tokens`, `refusal`, `cancelled`…).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub stop_reason: String,
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the agent did not open a session within {0:?}")]
    Timeout(Duration),
    #[error("the agent session is closed")]
    Closed,
    #[error("{0}")]
    Protocol(String),
}

type Reply = mpsc::Sender<Result<Turn, Error>>;

pub struct Session {
    id: String,
    agent: AgentInfo,
    prompts: async_mpsc::UnboundedSender<(String, Reply)>,
    cancel: Mutex<Option<oneshot::Sender<()>>>,
    thread: Option<JoinHandle<()>>,
}

impl Session {
    /// Starts the agent, says `initialize` and opens a session in `options.cwd`.
    pub fn start(launch: Launch, options: SessionOptions) -> Result<Self, Error> {
        let startup = options.startup;
        let (ready_tx, ready_rx) = mpsc::channel();
        let (prompts, prompt_rx) = async_mpsc::unbounded();
        let (cancel, cancel_rx) = oneshot::channel();
        let agent = AcpAgent::new(config(&launch));
        let thread = std::thread::Builder::new()
            .name("regulus-acp".into())
            .spawn(move || {
                let failed = ready_tx.clone();
                let run = Box::pin(run(agent, options, ready_tx, prompt_rx));
                // Losing the race to `cancel` drops the connection, which kills the agent.
                let outcome = futures::executor::block_on(futures::future::select(run, cancel_rx));
                if let futures::future::Either::Left((Err(error), _)) = outcome {
                    let _ = failed.send(Err(Error::Protocol(error.to_string())));
                }
            })
            .map_err(|e| Error::Protocol(format!("cannot start the session thread: {e}")))?;
        let mut session = Session {
            id: String::new(),
            agent: AgentInfo::default(),
            prompts,
            cancel: Mutex::new(Some(cancel)),
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(startup) {
            Ok(Ok((id, agent))) => {
                session.id = id;
                session.agent = agent;
                Ok(session)
            }
            Ok(Err(error)) => Err(error),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(Error::Timeout(startup)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(Error::Closed),
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn agent(&self) -> &AgentInfo {
        &self.agent
    }

    /// Sends a prompt and waits for the end of the turn. Prompts to one session queue up.
    pub fn prompt(&self, text: &str) -> Result<Turn, Error> {
        let (reply, answer) = mpsc::channel();
        self.prompts
            .unbounded_send((text.to_string(), reply))
            .map_err(|_| Error::Closed)?;
        answer.recv().map_err(|_| Error::Closed)?
    }

    /// Kills the agent. A prompt waiting on it returns `Closed`.
    pub fn close(&self) {
        let cancel = self
            .cancel
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(cancel) = cancel {
            let _ = cancel.send(());
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// `env -u NAME …` in front of the program when variables must be removed: the ACP
/// launcher can only add to the inherited environment.
fn config(launch: &Launch) -> AcpAgentConfig {
    let config = if launch.env_remove.is_empty() {
        AcpAgentConfig::new(&launch.program).args(launch.args.iter().cloned())
    } else {
        let mut args = Vec::new();
        for name in &launch.env_remove {
            args.extend(["-u".to_string(), name.clone()]);
        }
        args.push(launch.program.to_string_lossy().into_owned());
        args.extend(launch.args.iter().cloned());
        AcpAgentConfig::new("env").args(args)
    };
    config.envs(launch.env.clone())
}

fn answer(
    permissions: Permissions,
    request: &RequestPermissionRequest,
) -> RequestPermissionResponse {
    let wanted: &[PermissionOptionKind] = match permissions {
        Permissions::Allow => &[
            PermissionOptionKind::AllowOnce,
            PermissionOptionKind::AllowAlways,
        ],
        Permissions::Reject => &[
            PermissionOptionKind::RejectOnce,
            PermissionOptionKind::RejectAlways,
        ],
    };
    let chosen = wanted
        .iter()
        .find_map(|kind| request.options.iter().find(|option| option.kind == *kind));
    let outcome = match chosen {
        Some(option) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(
            option.option_id.clone(),
        )),
        None => RequestPermissionOutcome::Cancelled,
    };
    RequestPermissionResponse::new(outcome)
}

async fn run(
    agent: AcpAgent,
    options: SessionOptions,
    ready: mpsc::Sender<Result<(String, AgentInfo), Error>>,
    mut prompts: async_mpsc::UnboundedReceiver<(String, Reply)>,
) -> Result<(), agent_client_protocol::Error> {
    let permissions = options.permissions;
    Client
        .builder()
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, _connection| {
                responder.respond(answer(permissions, &request))
            },
            on_receive_request!(),
        )
        .connect_with(agent, async |connection: ConnectionTo<Agent>| {
            let init = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let info = init
                .agent_info
                .map(|i| AgentInfo {
                    name: i.name,
                    version: i.version,
                })
                .unwrap_or_default();
            let request = NewSessionRequest::new(options.cwd.clone()).meta(options.meta.clone());
            let mut session = connection
                .build_session_from(request)
                .block_task()
                .start_session()
                .await?;
            let _ = ready.send(Ok((session.session_id().to_string(), info)));
            while let Some((text, reply)) = prompts.next().await {
                let _ = reply.send(turn(&mut session, text).await);
            }
            Ok(())
        })
        .await
}

async fn turn(session: &mut ActiveSession<'static, Agent>, text: String) -> Result<Turn, Error> {
    let failed = |e: agent_client_protocol::Error| Error::Protocol(e.to_string());
    session.send_prompt(text).map_err(failed)?;
    let mut output = String::new();
    loop {
        match session.read_update().await.map_err(failed)? {
            SessionMessage::SessionMessage(dispatch) => MatchDispatch::new(dispatch)
                .if_notification(async |note: SessionNotification| {
                    if let SessionUpdate::AgentMessageChunk(ContentChunk {
                        content: ContentBlock::Text(text),
                        ..
                    }) = note.update
                    {
                        output.push_str(&text.text);
                    }
                    Ok(())
                })
                .await
                .otherwise_ignore()
                .map_err(failed)?,
            SessionMessage::StopReason(reason) => {
                let stop_reason = serde_json::to_value(reason)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| "unknown".into());
                return Ok(Turn {
                    stop_reason,
                    text: output,
                });
            }
            _ => {}
        }
    }
}

/// The sessions an engine module keeps open, under keys of its own: two agents may well
/// name their sessions the same.
#[derive(Default)]
pub struct Sessions {
    live: Mutex<HashMap<String, Arc<Session>>>,
    next: AtomicU64,
}

impl Sessions {
    fn live(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Session>>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Starts a session and returns its key with who answered.
    pub fn start(
        &self,
        launch: Launch,
        options: SessionOptions,
    ) -> Result<(String, AgentInfo), Error> {
        let session = Session::start(launch, options)?;
        let key = format!("s{}", self.next.fetch_add(1, Ordering::Relaxed) + 1);
        let agent = session.agent().clone();
        self.live().insert(key.clone(), Arc::new(session));
        Ok((key, agent))
    }

    pub fn prompt(&self, key: &str, text: &str) -> Result<Turn, Error> {
        let session = self.live().get(key).cloned().ok_or(Error::Closed)?;
        session.prompt(text)
    }

    /// Closes a session; a prompt in flight on it returns `Closed`. False if unknown.
    pub fn stop(&self, key: &str) -> bool {
        let session = self.live().remove(key);
        session.map(|s| s.close()).is_some()
    }

    pub fn stop_all(&self) {
        let all: Vec<_> = self.live().drain().map(|(_, s)| s).collect();
        for session in all {
            session.close();
        }
    }
}
