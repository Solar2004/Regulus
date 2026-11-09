//! The ACP channel engine modules share (docs/12-agents.md §4). It is a library, not a
//! module: an engine decides how to launch its CLI and hands the result here.
//!
//! One [`Session`] is one agent process with one ACP session. The protocol is async; a
//! session runs it on a thread of its own, so modules, whose calls are synchronous, can
//! use it without a runtime. Dropping a session kills the agent's whole process group.
//!
//! Each session keeps what happens in it as [`SessionEvent`]s in a bounded buffer
//! (`SessionOptions::events_keep`): streamed text and reasoning, tool calls and their
//! updates, plans, permission requests and how they were answered, turn boundaries and
//! state changes. Updates of a kind it does not map are left out of the buffer.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub mod engine;

use agent_client_protocol::schema::v1::{
    CancelNotification, ContentBlock, ContentChunk, EnvVariable, InitializeRequest,
    LoadSessionRequest, McpServer, McpServerStdio, NewSessionRequest, PermissionOptionKind,
    RequestPermissionOutcome, RequestPermissionRequest, RequestPermissionResponse,
    SelectedPermissionOutcome, SessionNotification, SessionUpdate,
};
use agent_client_protocol::schema::ProtocolVersion;
use agent_client_protocol::util::MatchDispatch;
use agent_client_protocol::{
    on_receive_request, AcpAgent, AcpAgentConfig, ActiveSession, Agent, Client, ConnectionTo,
    SessionMessage, UntypedMessage,
};
use futures::channel::{mpsc as async_mpsc, oneshot};
use futures::{FutureExt, StreamExt};
pub use regulus_contract_agent_engine::{
    PermissionOption, SessionEvent, SessionEventKind, SessionState,
};
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

/// What the session answers when the agent asks for permission.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permissions {
    Allow,
    #[default]
    Reject,
    /// The request waits as a `permission_request` event until [`Session::decide`]
    /// answers it, a cancel ends the turn or `permission_timeout` runs out (then the
    /// reject option is chosen, or the request is cancelled if it offers none).
    Ask,
}

/// How many events a session keeps when its options do not say.
pub const DEFAULT_EVENTS_KEEP: usize = 2000;
/// How long an asked permission waits when the options do not say.
pub const DEFAULT_PERMISSION_TIMEOUT: Duration = Duration::from_secs(600);

fn default_events_keep() -> usize {
    DEFAULT_EVENTS_KEEP
}

fn default_permission_timeout() -> Duration {
    DEFAULT_PERMISSION_TIMEOUT
}

/// A request sent after `initialize` and before `session/new`, for agent-specific setup
/// such as `providers/set`. The channel does not know what it means.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetupRequest {
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionOptions {
    pub cwd: PathBuf,
    #[serde(default)]
    pub permissions: Permissions,
    /// Sent as `_meta` of `session/new`: agent-specific options (docs/12-agents.md §3).
    #[serde(default)]
    pub meta: Option<Map<String, Value>>,
    /// Sent in order before the session opens; the first one to fail fails the start.
    #[serde(default)]
    pub setup: Vec<SetupRequest>,
    /// How long the agent may take to start and open the session.
    pub startup: Duration,
    /// MCP servers over stdio, passed in `session/new`; every ACP agent accepts them.
    #[serde(default)]
    pub mcp_servers: Vec<StdioServer>,
    /// How many events the session keeps; the oldest go first.
    #[serde(default = "default_events_keep")]
    pub events_keep: usize,
    /// How long a permission request waits for [`Session::decide`] with
    /// [`Permissions::Ask`].
    #[serde(default = "default_permission_timeout")]
    pub permission_timeout: Duration,
    /// A native session id to load (`session/load`) instead of opening a new session.
    /// Done only when the agent advertises `loadSession`; otherwise, or if loading fails,
    /// a new session opens and [`Session::warnings`] says why.
    #[serde(default)]
    pub resume: Option<String>,
}

/// An MCP server the agent starts as a process and talks to over stdio.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StdioServer {
    pub name: String,
    pub command: PathBuf,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Who answered `initialize`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentInfo {
    pub name: String,
    pub version: String,
}

/// How the session opened: what the agent can do and whether it loaded an old session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Opened {
    id: String,
    agent: AgentInfo,
    load_session: bool,
    resumed: bool,
    warnings: Vec<String>,
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
    /// The agent process ended on its own; what it said on the way out.
    #[error("the agent ended: {0}")]
    Ended(String),
    #[error("{0}")]
    Protocol(String),
}

/// Where a session stands, as far as its client can tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    /// Waiting for a prompt.
    Idle,
    /// In a turn, maybe with more prompts queued behind it.
    Busy,
    /// The agent process is gone.
    Ended,
}

type Reply = mpsc::Sender<Result<Turn, Error>>;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The session's events, bounded, and the turn they belong to.
struct Events {
    keep: usize,
    state: Mutex<EventsState>,
}

#[derive(Default)]
struct EventsState {
    kept: VecDeque<SessionEvent>,
    seq: u64,
    turns: u64,
    turn: Option<u64>,
}

impl Events {
    fn new(keep: usize) -> Self {
        Self {
            keep: keep.max(1),
            state: Mutex::default(),
        }
    }

    fn push(&self, kind: SessionEventKind) {
        let mut state = lock(&self.state);
        state.seq += 1;
        let event = SessionEvent {
            seq: state.seq,
            at: now_ms(),
            turn: state.turn,
            kind,
        };
        state.kept.push_back(event);
        while state.kept.len() > self.keep {
            state.kept.pop_front();
        }
    }

    fn begin_turn(&self) {
        {
            let mut state = lock(&self.state);
            state.turns += 1;
            state.turn = Some(state.turns);
        }
        self.push(SessionEventKind::TurnStarted);
        self.push(SessionEventKind::State {
            state: SessionState::Busy,
        });
    }

    fn end_turn(&self, stop_reason: Option<&str>) {
        if let Some(stop_reason) = stop_reason {
            self.push(SessionEventKind::TurnEnded {
                stop_reason: stop_reason.to_string(),
            });
        }
        lock(&self.state).turn = None;
        self.push(SessionEventKind::State {
            state: SessionState::Idle,
        });
    }

    /// The events after `since`, and the cursor for the next call.
    fn since(&self, since: u64) -> (Vec<SessionEvent>, u64) {
        let state = lock(&self.state);
        let events: Vec<SessionEvent> = state
            .kept
            .iter()
            .filter(|e| e.seq > since)
            .cloned()
            .collect();
        let next = events.last().map_or(since, |e| e.seq);
        (events, next)
    }
}

/// A permission request waiting for [`Session::decide`].
struct Parked {
    answer: oneshot::Sender<Option<String>>,
    options: Vec<PermissionOption>,
    /// Dropped with the entry: wakes the request's timer, which then has nothing to do.
    _alive: mpsc::Sender<()>,
}

/// The permission requests waiting in a session, and its events to report them in.
struct Asks {
    events: Arc<Events>,
    parked: Mutex<HashMap<String, Parked>>,
    next: AtomicU64,
    timeout: Duration,
}

impl Asks {
    /// Parks `request`, says so in the events and returns where its answer will arrive.
    fn park(
        self: &Arc<Self>,
        request: &RequestPermissionRequest,
    ) -> oneshot::Receiver<Option<String>> {
        let id = format!("p{}", self.next.fetch_add(1, Ordering::Relaxed) + 1);
        let options: Vec<PermissionOption> = request
            .options
            .iter()
            .map(|o| PermissionOption {
                id: o.option_id.0.to_string(),
                kind: serde_json::to_value(o.kind)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_default(),
                name: o.name.clone(),
            })
            .collect();
        let call = serde_json::to_value(&request.tool_call).unwrap_or_default();
        let text = |key: &str| call.get(key).and_then(Value::as_str).map(String::from);
        let (answer, answered) = oneshot::channel();
        let (alive, gone) = mpsc::channel::<()>();
        lock(&self.parked).insert(
            id.clone(),
            Parked {
                answer,
                options: options.clone(),
                _alive: alive,
            },
        );
        self.events.push(SessionEventKind::PermissionRequest {
            id: id.clone(),
            tool: text("name").or_else(|| text("kind")),
            title: text("title").unwrap_or_else(|| "permission".into()),
            options,
        });
        let asks = Arc::downgrade(self);
        let timeout = self.timeout;
        let spawned = std::thread::Builder::new()
            .name("regulus-acp-ask".into())
            .spawn(move || {
                if gone.recv_timeout(timeout) == Err(mpsc::RecvTimeoutError::Timeout) {
                    if let Some(asks) = asks.upgrade() {
                        asks.expire(&id);
                    }
                }
            });
        if let Err(error) = spawned {
            eprintln!("regulus: acp: no timer for a permission request: {error}");
        }
        answered
    }

    /// Answers request `id` with `option` (`None`: cancelled). An option it did not offer
    /// is refused and the request keeps waiting.
    fn resolve(&self, id: &str, option: Option<String>) -> Result<(), String> {
        let parked = {
            let mut parked = lock(&self.parked);
            let found = parked
                .get(id)
                .ok_or_else(|| format!("no pending permission request `{id}`"))?;
            if let Some(chosen) = &option {
                if !found.options.iter().any(|o| &o.id == chosen) {
                    return Err(format!("request `{id}` has no option `{chosen}`"));
                }
            }
            parked.remove(id)
        };
        if let Some(parked) = parked {
            self.events.push(SessionEventKind::PermissionResolved {
                id: id.to_string(),
                option: option.clone(),
            });
            let _ = parked.answer.send(option);
        }
        Ok(())
    }

    /// Nobody answered in time: the reject option, else cancelled.
    fn expire(&self, id: &str) {
        let reject = lock(&self.parked).get(id).and_then(|p| {
            ["reject_once", "reject_always"]
                .iter()
                .find_map(|kind| p.options.iter().find(|o| o.kind == *kind))
                .map(|o| o.id.clone())
        });
        let _ = self.resolve(id, reject);
    }

    /// Every waiting request is cancelled, as ACP asks of a client that cancels a turn.
    fn cancel_all(&self) {
        let ids: Vec<String> = lock(&self.parked).keys().cloned().collect();
        for id in ids {
            let _ = self.resolve(&id, None);
        }
    }
}

/// Why the session's thread finished, set before it is marked done. A prompt that loses
/// its agent waits for it, so the agent's last words are not lost to a race.
#[derive(Default)]
struct Outcome {
    state: Mutex<(bool, Option<String>)>,
    done: Condvar,
}

impl Outcome {
    fn finish(&self, reason: Option<String>) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        *state = (true, reason);
        self.done.notify_all();
    }

    fn reason(&self) -> Option<String> {
        let state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let state = self
            .done
            .wait_while(state, |(done, _)| !*done)
            .unwrap_or_else(PoisonError::into_inner);
        state.1.clone()
    }

    fn is_done(&self) -> bool {
        self.state.lock().unwrap_or_else(PoisonError::into_inner).0
    }
}

pub struct Session {
    id: String,
    agent: AgentInfo,
    load_session: bool,
    resumed: bool,
    warnings: Vec<String>,
    events: Arc<Events>,
    asks: Arc<Asks>,
    prompts: async_mpsc::UnboundedSender<(String, Reply)>,
    /// Asks the turn in flight to end (`session/cancel`).
    cancels: async_mpsc::UnboundedSender<()>,
    cancel: Mutex<Option<oneshot::Sender<()>>>,
    outcome: Arc<Outcome>,
    /// Prompts sent and not answered yet, the one in its turn included.
    pending: AtomicUsize,
    thread: Option<JoinHandle<()>>,
}

impl Session {
    /// Starts the agent, says `initialize` and opens a session in `options.cwd`.
    pub fn start(launch: Launch, options: SessionOptions) -> Result<Self, Error> {
        let startup = options.startup;
        let (ready_tx, ready_rx) = mpsc::channel();
        let (prompts, prompt_rx) = async_mpsc::unbounded();
        let (cancels, cancels_rx) = async_mpsc::unbounded();
        let (cancel, cancel_rx) = oneshot::channel();
        let agent = AcpAgent::new(config(&launch));
        let outcome = Arc::new(Outcome::default());
        let finished = outcome.clone();
        let events = Arc::new(Events::new(options.events_keep));
        let asks = Arc::new(Asks {
            events: events.clone(),
            parked: Mutex::default(),
            next: AtomicU64::new(0),
            timeout: options.permission_timeout,
        });
        let (ended, asked) = (events.clone(), asks.clone());
        let thread = std::thread::Builder::new()
            .name("regulus-acp".into())
            .spawn(move || {
                let failed = ready_tx.clone();
                let channels = Channels {
                    ready: ready_tx,
                    prompts: prompt_rx,
                    cancels: cancels_rx,
                    asks: asked.clone(),
                };
                let run = Box::pin(run(agent, options, channels));
                // Losing the race to `cancel` drops the connection, which kills the agent.
                let result = futures::executor::block_on(futures::future::select(run, cancel_rx));
                let reason = match result {
                    futures::future::Either::Left((Err(error), _)) => {
                        let _ = failed.send(Err(Error::Protocol(error.to_string())));
                        Some(error.to_string())
                    }
                    _ => None,
                };
                asked.cancel_all();
                ended.push(SessionEventKind::State {
                    state: SessionState::Ended,
                });
                finished.finish(reason);
            })
            .map_err(|e| Error::Protocol(format!("cannot start the session thread: {e}")))?;
        let mut session = Session {
            id: String::new(),
            agent: AgentInfo::default(),
            load_session: false,
            resumed: false,
            warnings: Vec::new(),
            events,
            asks,
            prompts,
            cancels,
            cancel: Mutex::new(Some(cancel)),
            outcome,
            pending: AtomicUsize::new(0),
            thread: Some(thread),
        };
        match ready_rx.recv_timeout(startup) {
            Ok(Ok(opened)) => {
                session.id = opened.id;
                session.agent = opened.agent;
                session.load_session = opened.load_session;
                session.resumed = opened.resumed;
                session.warnings = opened.warnings;
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

    /// Whether the agent said it can load sessions (`agentCapabilities.loadSession`).
    pub fn load_session(&self) -> bool {
        self.load_session
    }

    /// Whether this session is the one `SessionOptions::resume` named, loaded by the agent.
    pub fn resumed(&self) -> bool {
        self.resumed
    }

    /// What was asked at start and could not be done (a resume, mostly).
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    /// The events after `since` (0: from the oldest kept) and the cursor for the next call.
    pub fn events(&self, since: u64) -> (Vec<SessionEvent>, u64) {
        self.events.since(since)
    }

    /// Answers a pending permission request with one of its options; `None` cancels it.
    pub fn decide(&self, request_id: &str, option: Option<String>) -> Result<(), String> {
        self.asks.resolve(request_id, option)
    }

    /// Sends a prompt and waits for the end of the turn. Prompts to one session queue up.
    /// If the agent dies meanwhile, the error carries what it said on the way out.
    pub fn prompt(&self, text: &str) -> Result<Turn, Error> {
        let (reply, answer) = mpsc::channel();
        self.pending.fetch_add(1, Ordering::SeqCst);
        let sent = self.prompts.unbounded_send((text.to_string(), reply));
        let answered = sent.ok().and_then(|()| answer.recv().ok());
        self.pending.fetch_sub(1, Ordering::SeqCst);
        match answered {
            Some(turn) => turn,
            None => Err(self.outcome.reason().map_or(Error::Closed, Error::Ended)),
        }
    }

    pub fn state(&self) -> State {
        if self.outcome.is_done() {
            State::Ended
        } else if self.pending.load(Ordering::SeqCst) > 0 {
            State::Busy
        } else {
            State::Idle
        }
    }

    /// Asks the agent to end the turn in flight (ACP `session/cancel`); the prompt waiting
    /// on it returns when the agent answers, usually with stop reason `cancelled`. With no
    /// turn in flight it does nothing. False if the session is gone.
    pub fn cancel(&self) -> bool {
        if self.outcome.is_done() {
            return false;
        }
        // A request waiting for an answer belongs to the turn being cancelled.
        self.asks.cancel_all();
        self.cancels.unbounded_send(()).is_ok()
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
        Permissions::Reject | Permissions::Ask => &[
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

fn chosen(option: Option<String>) -> RequestPermissionResponse {
    let outcome = match option {
        Some(id) => RequestPermissionOutcome::Selected(SelectedPermissionOutcome::new(id)),
        None => RequestPermissionOutcome::Cancelled,
    };
    RequestPermissionResponse::new(outcome)
}

/// What the session's thread talks to the [`Session`] through.
struct Channels {
    ready: mpsc::Sender<Result<Opened, Error>>,
    prompts: async_mpsc::UnboundedReceiver<(String, Reply)>,
    cancels: async_mpsc::UnboundedReceiver<()>,
    asks: Arc<Asks>,
}

async fn run(
    agent: AcpAgent,
    options: SessionOptions,
    channels: Channels,
) -> Result<(), agent_client_protocol::Error> {
    let Channels {
        ready,
        mut prompts,
        mut cancels,
        asks,
    } = channels;
    let permissions = options.permissions;
    let events = asks.events.clone();
    Client
        .builder()
        .on_receive_request(
            async move |request: RequestPermissionRequest, responder, connection| {
                if permissions != Permissions::Ask {
                    return responder.respond(answer(permissions, &request));
                }
                // The answer comes from another thread, later: the connection goes on
                // meanwhile.
                let answered = asks.park(&request);
                connection.spawn(async move {
                    let option = answered.await.unwrap_or(None);
                    responder.respond(chosen(option))
                })
            },
            on_receive_request!(),
        )
        .connect_with(agent, async |connection: ConnectionTo<Agent>| {
            let init = connection
                .send_request(InitializeRequest::new(ProtocolVersion::V1))
                .block_task()
                .await?;
            let load_session = init.agent_capabilities.load_session;
            let info = init
                .agent_info
                .map(|i| AgentInfo {
                    name: i.name,
                    version: i.version,
                })
                .unwrap_or_default();
            for setup in &options.setup {
                let request = UntypedMessage::new(&setup.method, &setup.params)?;
                connection.send_request(request).block_task().await?;
            }
            let servers: Vec<McpServer> = options
                .mcp_servers
                .iter()
                .map(|s| {
                    let env = s.env.iter().map(|(k, v)| EnvVariable::new(k, v)).collect();
                    McpServer::Stdio(
                        McpServerStdio::new(&s.name, &s.command)
                            .args(s.args.clone())
                            .env(env),
                    )
                })
                .collect();
            let mut warnings = Vec::new();
            let mut loaded = None;
            match &options.resume {
                Some(id) if load_session => {
                    let request = LoadSessionRequest::new(id.clone(), options.cwd.clone())
                        .meta(options.meta.clone())
                        .mcp_servers(servers.clone());
                    let restored = connection
                        .load_session_from(request)
                        .block_task()
                        .start_session()
                        .await;
                    match restored {
                        Ok(restored) => loaded = Some(restored.into_session()),
                        Err(error) => warnings.push(format!(
                            "could not load session `{id}` ({error}): a new one was opened"
                        )),
                    }
                }
                Some(id) => warnings.push(format!(
                    "the agent cannot load sessions: `{id}` was not resumed, a new one was opened"
                )),
                None => {}
            }
            let resumed = loaded.is_some();
            let mut session = match loaded {
                Some(mut session) => {
                    // The agent replays the loaded conversation before answering
                    // `session/load`: that history is not news, so it stays out of the
                    // events. What is queued now is all of it.
                    while let Some(Ok(_)) = session.read_update().now_or_never() {}
                    session
                }
                None => {
                    let request = NewSessionRequest::new(options.cwd.clone())
                        .meta(options.meta.clone())
                        .mcp_servers(servers);
                    connection
                        .build_session_from(request)
                        .block_task()
                        .start_session()
                        .await?
                }
            };
            let _ = ready.send(Ok(Opened {
                id: session.session_id().to_string(),
                agent: info,
                load_session,
                resumed,
                warnings,
            }));
            events.push(SessionEventKind::State {
                state: SessionState::Idle,
            });
            while let Some((text, reply)) = prompts.next().await {
                // A cancel asked between turns is for a turn that already ended.
                while cancels.try_recv().is_ok() {}
                events.begin_turn();
                let answer = turn(&mut session, &mut cancels, &events, text).await;
                events.end_turn(answer.as_ref().ok().map(|t| t.stop_reason.as_str()));
                let _ = reply.send(answer);
            }
            Ok(())
        })
        .await
}

/// The event an update of the session stands for, if it is a kind the buffer keeps.
/// Read from the update's wire form, so a field the typed schema hides is still seen.
fn event_of(update: &SessionUpdate) -> Option<SessionEventKind> {
    let wire = serde_json::to_value(update).ok()?;
    let text = |key: &str| wire.get(key).and_then(Value::as_str).map(String::from);
    let chunk = || {
        let content = wire.get("content")?;
        (content.get("type").and_then(Value::as_str) == Some("text"))
            .then(|| {
                content
                    .get("text")
                    .and_then(Value::as_str)
                    .map(String::from)
            })
            .flatten()
    };
    Some(match wire.get("sessionUpdate")?.as_str()? {
        "agent_message_chunk" => SessionEventKind::Text { text: chunk()? },
        "agent_thought_chunk" => SessionEventKind::Thought { text: chunk()? },
        "tool_call" => SessionEventKind::ToolCall {
            id: text("toolCallId")?,
            title: text("title").unwrap_or_default(),
            kind: text("kind"),
            status: text("status").unwrap_or_else(|| "pending".into()),
        },
        "tool_call_update" => SessionEventKind::ToolUpdate {
            id: text("toolCallId")?,
            status: text("status").unwrap_or_default(),
            content: wire
                .get("content")
                .or_else(|| wire.get("rawOutput"))
                .filter(|c| !c.is_null())
                .cloned(),
        },
        "plan" => SessionEventKind::Plan {
            entries: wire.get("entries").cloned().unwrap_or(Value::Null),
        },
        _ => return None,
    })
}

async fn turn(
    session: &mut ActiveSession<'static, Agent>,
    cancels: &mut async_mpsc::UnboundedReceiver<()>,
    events: &Events,
    text: String,
) -> Result<Turn, Error> {
    let failed = |e: agent_client_protocol::Error| Error::Protocol(e.to_string());
    session.send_prompt(text).map_err(failed)?;
    let mut output = String::new();
    let mut cancellable = true;
    loop {
        // Reading an update is cancel-safe (a channel), so a cancel can cut in between.
        let update = if cancellable {
            let read = session.read_update();
            futures::pin_mut!(read);
            match futures::future::select(read, cancels.next()).await {
                futures::future::Either::Left((update, _)) => Ok(update),
                futures::future::Either::Right((asked, _)) => Err(asked),
            }
        } else {
            Ok(session.read_update().await)
        };
        let update = match update {
            Ok(update) => update,
            Err(Some(())) => {
                let id = session.session_id().clone();
                session
                    .connection()
                    .send_notification(CancelNotification::new(id))
                    .map_err(failed)?;
                continue;
            }
            // Nobody can cancel anymore: stop asking.
            Err(None) => {
                cancellable = false;
                continue;
            }
        };
        match update.map_err(failed)? {
            SessionMessage::SessionMessage(dispatch) => MatchDispatch::new(dispatch)
                .if_notification(async |note: SessionNotification| {
                    if let Some(event) = event_of(&note.update) {
                        events.push(event);
                    }
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

    /// Starts a session and returns its key with the session itself.
    pub fn start(
        &self,
        launch: Launch,
        options: SessionOptions,
    ) -> Result<(String, Arc<Session>), Error> {
        let session = Arc::new(Session::start(launch, options)?);
        let key = format!("s{}", self.next.fetch_add(1, Ordering::Relaxed) + 1);
        self.live().insert(key.clone(), session.clone());
        Ok((key, session))
    }

    /// The events of a session after `since`; `None` if the session is unknown.
    pub fn events(&self, key: &str, since: u64) -> Option<(Vec<SessionEvent>, u64)> {
        self.live().get(key).map(|s| s.events(since))
    }

    /// Answers a pending permission request of a session.
    pub fn decide(
        &self,
        key: &str,
        request_id: &str,
        option: Option<String>,
    ) -> Result<(), String> {
        let session = self
            .live()
            .get(key)
            .cloned()
            .ok_or_else(|| format!("no live session `{key}`"))?;
        session.decide(request_id, option)
    }

    pub fn prompt(&self, key: &str, text: &str) -> Result<Turn, Error> {
        let session = self.live().get(key).cloned().ok_or(Error::Closed)?;
        session.prompt(text)
    }

    /// Asks the session's turn in flight to end. False if the session is unknown or gone.
    pub fn cancel(&self, key: &str) -> bool {
        self.live().get(key).is_some_and(|s| s.cancel())
    }

    /// Where a session stands; a session closed or never opened has ended.
    pub fn state(&self, key: &str) -> State {
        self.live().get(key).map_or(State::Ended, |s| s.state())
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
