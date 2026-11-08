//! What the terminal UI knows and how keys change it. No drawing and no I/O here: keys
//! and core events go in, requests to the core come out, so all of it is testable.

use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use regulus_proto::{
    BusEvent, BusEventKind, Direction, Execution, ExecutionState, Manifest, ModuleState,
    ModuleSummary, Node, Port, Profile, Workflow,
};
use serde_json::Value;

use super::core::{describe, ChannelState, Connection, Event, Failure, Pending, Request};
use super::document::unique_id;
use super::edit::{Ctx, Editor, Step};
use super::keys::Options;
use super::templates::{Templates, Then};

/// Capabilities that tell what a node is, never its module (CLAUDE.md, invariant 2).
const AGENT_ENGINE: &str = "agent.engine";
const PARTICIPANT: &str = "participant";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Profiles,
    Executions,
    Nodes,
    Input,
    Events,
    Decisions,
}

/// Which view is on screen.
#[derive(Debug, Clone, PartialEq)]
pub enum Mode {
    Operate,
    Edit(Box<Editor>),
    Templates(Box<Templates>),
}

/// A line typed in Operate.
#[derive(Debug, Clone, PartialEq)]
pub enum Prompt {
    /// The name of a new profile.
    NewProfile(String),
    /// The node the events are filtered by; empty shows all.
    Filter(String),
    /// A value for one of the workflow's own inputs (`workflow.send`).
    Send {
        execution: String,
        ports: Vec<Port>,
        index: usize,
        value: String,
    },
}

/// The bus events of one execution seen so far, and where to go on from.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EventLog {
    pub events: Vec<BusEvent>,
    pub next: u64,
    pub error: Option<Failure>,
}

impl EventLog {
    /// Adds a page: no duplicates, in `seq` order, the newest `keep` only.
    pub fn merge(&mut self, page: Vec<BusEvent>, next: u64, keep: usize) {
        let mut seen: HashSet<u64> = self.events.iter().map(|e| e.seq).collect();
        self.events
            .extend(page.into_iter().filter(|e| seen.insert(e.seq)));
        self.events.sort_by_key(|e| e.seq);
        if keep > 0 && self.events.len() > keep {
            let extra = self.events.len() - keep;
            self.events.drain(..extra);
        }
        self.next = self.next.max(next);
        self.error = None;
    }
}

/// Whether an event left from or went to a node.
pub fn touches(event: &BusEvent, node: &str) -> bool {
    event.from.node_id == node
        || matches!(&event.kind, BusEventKind::Delivered { to } | BusEventKind::Failed { to, .. } if to.node_id == node)
}

/// A value on one line: text as text, the rest as JSON, cut at `max` characters.
pub fn compact(value: &Value, max: usize) -> String {
    let line = match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    };
    let line = line.lines().map(str::trim).collect::<Vec<_>>().join(" ⏎ ");
    if max > 0 && line.chars().count() > max {
        let cut: String = line.chars().take(max).collect();
        format!("{cut}…")
    } else {
        line
    }
}

/// One event as the CLI's `regulus events` prints it.
pub fn event_line(event: &BusEvent, max: usize) -> String {
    let from = format!("{}.{}", event.from.node_id, event.from.port_id);
    match &event.kind {
        BusEventKind::Emitted { value } => format!(
            "#{} {from} emitted {} {}",
            event.seq,
            event.capability,
            compact(value, max)
        ),
        BusEventKind::Delivered { to } => {
            format!(
                "#{} {from} → {}.{} delivered",
                event.seq, to.node_id, to.port_id
            )
        }
        BusEventKind::Failed { to, error } => format!(
            "#{} {from} → {}.{} failed: {error}",
            event.seq, to.node_id, to.port_id
        ),
        BusEventKind::Cut { max_depth } => {
            format!("#{} {from} cut past depth {max_depth}", event.seq)
        }
    }
}

/// What `workflow.send` carries: a `text@` port takes the text, any other JSON.
pub fn send_value(capability: &str, raw: &str) -> Option<Value> {
    if capability.split('@').next() == Some("text") {
        Some(Value::from(raw))
    } else {
        serde_json::from_str(raw).ok()
    }
}

/// What a node lets a person do from here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeKind {
    /// Takes prompts: it has an agent engine wired in.
    Agent,
    /// Carries messages between participants.
    Channel,
    Other,
}

/// A node's state as docs/04-client.md §3 names them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeState {
    Running,
    /// Part of the workflow with no module: it takes part as it is.
    Resource,
    NotInstalled,
    Disabled(Vec<String>),
    Broken(String),
    /// The execution is not running.
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRow {
    pub id: String,
    pub name: String,
    pub node_type: String,
    pub kind: NodeKind,
    pub state: NodeState,
}

/// One exchange with an agent, kept for this session only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Turn {
    Prompt(String),
    Answer(String),
    Error(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Info,
    Error,
}

/// A node of an execution: `(execution id, node id)`.
pub type NodeKey = (String, String);

#[derive(Debug)]
pub struct App {
    pub connection: Connection,
    pub profiles: Vec<Profile>,
    pub executions: Vec<Execution>,
    pub modules: Vec<ModuleSummary>,
    pub focus: Focus,
    pub profile: usize,
    pub execution: usize,
    pub node: usize,
    pub input: String,
    pub channels: HashMap<NodeKey, Result<ChannelState, String>>,
    pub turns: HashMap<NodeKey, Vec<Turn>>,
    /// Agents whose turn is still running.
    pub thinking: HashSet<NodeKey>,
    pub notice: Option<(Tone, String)>,
    pub quit: bool,
    pub mode: Mode,
    pub options: Options,
    /// `module.manifest` of every module, asked for when Edit opens.
    pub manifests: Option<Result<Vec<Manifest>, Failure>>,
    pub events: HashMap<String, EventLog>,
    /// Executions with an `event.list` call on its way.
    pub events_asked: HashSet<String>,
    /// Events scrolled back from the newest.
    pub event_offset: usize,
    pub filter: String,
    pub decisions: HashMap<String, Result<Vec<Pending>, Failure>>,
    /// The id of the pending decision under the cursor: what a key answers, whatever the
    /// list did since it was drawn.
    pub decision: Option<String>,
    pub prompt: Option<Prompt>,
    /// A profile to open in Edit once the profiles come back with it.
    pub pending_edit: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new(Options::default())
    }
}

impl App {
    pub fn new(options: Options) -> Self {
        Self {
            connection: Connection::Connecting,
            profiles: Vec::new(),
            executions: Vec::new(),
            modules: Vec::new(),
            focus: Focus::Profiles,
            profile: 0,
            execution: 0,
            node: 0,
            input: String::new(),
            channels: HashMap::new(),
            turns: HashMap::new(),
            thinking: HashSet::new(),
            notice: None,
            quit: false,
            mode: Mode::Operate,
            options,
            manifests: None,
            events: HashMap::new(),
            events_asked: HashSet::new(),
            event_offset: 0,
            filter: String::new(),
            decisions: HashMap::new(),
            decision: None,
            prompt: None,
            pending_edit: None,
        }
    }
}

fn has_input(node: &Node, capability: &str) -> bool {
    node.ports.iter().any(|port| {
        port.direction == Direction::Input && port.capability.split('@').next() == Some(capability)
    })
}

pub fn kind(node: &Node) -> NodeKind {
    if has_input(node, AGENT_ENGINE) {
        NodeKind::Agent
    } else if has_input(node, PARTICIPANT) {
        NodeKind::Channel
    } else {
        NodeKind::Other
    }
}

/// Moves `index` by `step` inside `0..len`, stopping at the ends.
fn step(index: usize, len: usize, step: isize) -> usize {
    if len == 0 {
        return 0;
    }
    index.saturating_add_signed(step).min(len - 1)
}

impl App {
    pub fn connected(&self) -> bool {
        matches!(self.connection, Connection::Connected { .. })
    }

    pub fn selected_profile(&self) -> Option<&Profile> {
        self.profiles.get(self.profile)
    }

    pub fn selected_execution(&self) -> Option<&Execution> {
        self.executions.get(self.execution)
    }

    pub fn workflow(&self, execution: &Execution) -> Option<&Workflow> {
        self.profiles
            .iter()
            .find(|profile| profile.id == execution.profile_id)
            .and_then(|profile| profile.workflow(&execution.workflow_id))
    }

    /// The nodes of the selected execution, with their state.
    pub fn nodes(&self) -> Vec<NodeRow> {
        let Some(execution) = self.selected_execution() else {
            return Vec::new();
        };
        let Some(workflow) = self.workflow(execution) else {
            return Vec::new();
        };
        let running = matches!(
            execution.state,
            ExecutionState::Running | ExecutionState::Starting
        );
        workflow
            .nodes
            .iter()
            .map(|node| {
                let module = node
                    .module_id
                    .as_deref()
                    .map(|id| self.modules.iter().find(|module| module.id == id));
                let disabled = execution.disabled.iter().find(|d| d.node_id == node.id);
                let state = match (module, disabled) {
                    (None, _) => NodeState::Resource,
                    (Some(None), _) => NodeState::NotInstalled,
                    (_, Some(disabled)) => NodeState::Disabled(disabled.missing.clone()),
                    (Some(Some(module)), None) => match &module.state {
                        ModuleState::Broken { reason } => NodeState::Broken(reason.clone()),
                        ModuleState::Incompatible { requires, have } => NodeState::Broken(format!(
                            "needs protocol {requires}, the core has {have}"
                        )),
                        _ if running => NodeState::Running,
                        _ => NodeState::Stopped,
                    },
                };
                NodeRow {
                    id: node.id.clone(),
                    name: node.name.clone(),
                    node_type: node.node_type.clone(),
                    kind: kind(node),
                    state,
                }
            })
            .collect()
    }

    pub fn selected_node(&self) -> Option<NodeRow> {
        self.nodes().into_iter().nth(self.node)
    }

    /// The selected node, if a person can talk to it now: an agent or a channel of a
    /// running execution.
    pub fn talk_target(&self) -> Option<(NodeKey, NodeKind)> {
        let execution = self.selected_execution()?;
        let node = self.selected_node()?;
        (node.kind != NodeKind::Other && node.state == NodeState::Running)
            .then(|| ((execution.id.clone(), node.id), node.kind))
    }

    /// What to keep fresh besides the status: the selected channel's messages.
    pub fn watched_channel(&self) -> Option<Request> {
        match self.talk_target()? {
            ((execution, node), NodeKind::Channel) => Some(Request::Channel { execution, node }),
            _ => None,
        }
    }

    pub fn apply(&mut self, event: Event) -> Vec<Request> {
        let mut requests = Vec::new();
        match event {
            Event::Connection(connection) => {
                let fresh = !self.connected() && matches!(connection, Connection::Connected { .. });
                self.connection = connection;
                if fresh {
                    requests.extend([Request::Profiles, Request::Status]);
                }
            }
            Event::Profiles(profiles) => {
                self.profiles = profiles;
                self.profile = step(self.profile, self.profiles.len(), 0);
                // One chance: a profile that did not come back is not waited for.
                if let Some(id) = self.pending_edit.take() {
                    match self.profiles.iter().position(|p| p.id == id) {
                        Some(at) => {
                            self.profile = at;
                            requests.extend(self.open_editor());
                        }
                        None => {
                            self.notice = Some((Tone::Error, format!("profile {id} is not loaded")))
                        }
                    }
                }
            }
            Event::Status {
                executions,
                modules,
            } => {
                let selected = self.selected_execution().map(|e| e.id.clone());
                // An execution of a profile not seen yet: the profiles changed on disk.
                if executions
                    .iter()
                    .any(|e| !self.profiles.iter().any(|p| p.id == e.profile_id))
                {
                    requests.push(Request::Profiles);
                }
                self.executions = executions;
                self.modules = modules;
                self.execution = selected
                    .as_deref()
                    .and_then(|id| self.executions.iter().position(|e| e.id == id))
                    .unwrap_or_else(|| step(self.execution, self.executions.len(), 0));
                if self.selected_execution().map(|e| e.id.as_str()) != selected.as_deref() {
                    self.execution_changed();
                }
                self.node = step(self.node, self.nodes().len(), 0);
            }
            Event::Channel {
                execution,
                node,
                state,
            } => {
                self.channels.insert((execution, node), state);
            }
            Event::Started(Ok(execution)) => {
                let disabled: Vec<String> = execution
                    .disabled
                    .iter()
                    .map(|d| format!("{} (needs {})", d.node_id, d.missing.join(", ")))
                    .collect();
                let mut said = format!("started {}", execution.id);
                if !disabled.is_empty() {
                    said.push_str(&format!(" · disabled: {}", disabled.join("; ")));
                }
                self.notice = Some((Tone::Info, said));
                if let ExecutionState::Failed { reason } = &execution.state {
                    self.notice = Some((Tone::Error, format!("{}: {reason}", execution.id)));
                }
                // From Templates, show what runs; Edit stays where it is.
                if matches!(self.mode, Mode::Templates(_)) {
                    self.mode = Mode::Operate;
                }
                let id = execution.id.clone();
                self.executions.push(execution);
                self.execution = self.executions.iter().position(|e| e.id == id).unwrap_or(0);
                self.node = 0;
                self.focus = Focus::Nodes;
                requests.push(Request::Status);
            }
            Event::Stopped(Ok(execution)) => {
                self.notice = Some((Tone::Info, format!("stopped {}", execution.id)));
                requests.push(Request::Status);
            }
            Event::Started(Err(error)) | Event::Stopped(Err(error)) => {
                self.notice = Some((Tone::Error, error));
            }
            Event::Answer {
                execution,
                node,
                answer,
            } => {
                let key = (execution, node);
                self.thinking.remove(&key);
                let turn = match answer {
                    Ok(text) => Turn::Answer(text),
                    Err(error) => Turn::Error(error),
                };
                self.turns.entry(key).or_default().push(turn);
            }
            Event::Sent {
                execution,
                node,
                result,
            } => match result {
                Ok(()) => requests.push(Request::Channel { execution, node }),
                Err(error) => self.notice = Some((Tone::Error, error)),
            },
            Event::Manifests(result) => self.manifests = Some(result),
            Event::Saved { profile, result } => {
                if let Mode::Edit(editor) = &mut self.mode {
                    if editor.doc.id == profile.id {
                        let step = editor.saved(*profile, result);
                        requests.extend(self.take(step));
                    }
                }
            }
            Event::Checked { profile, result } => {
                if let Mode::Edit(editor) = &mut self.mode {
                    if editor.doc.id == profile {
                        let step = editor.checked(result);
                        requests.extend(self.take(step));
                    }
                }
            }
            Event::Templates(result) => {
                if let Mode::Templates(view) = &mut self.mode {
                    view.listed(result);
                }
            }
            Event::Applied(result) => {
                if let Mode::Templates(view) = &mut self.mode {
                    let step = view.applied(result);
                    requests.extend(self.take(step));
                }
            }
            Event::Events {
                execution,
                since,
                result,
            } => {
                self.events_asked.remove(&execution);
                let keep = self.options.tuning.event_keep;
                let log = self.events.entry(execution).or_default();
                match result {
                    Ok(page) if since == log.next || page.next > log.next => {
                        log.merge(page.events, page.next, keep)
                    }
                    Ok(_) => {}
                    Err(failure) => log.error = Some(failure),
                }
            }
            Event::Decisions { execution, result } => {
                let selected = self.selected_execution().map(|e| e.id == execution);
                self.decisions.insert(execution, result);
                if selected == Some(true) {
                    self.settle_decision();
                }
            }
            Event::Decided(result) => {
                match result {
                    Ok(()) => self.notice = Some((Tone::Info, "decided".into())),
                    Err(failure) => self.notice = Some((Tone::Error, describe(&failure))),
                }
                requests.extend(self.poll());
            }
            Event::WorkflowSent(result) => {
                match result {
                    Ok(seq) => self.notice = Some((Tone::Info, format!("sent as event #{seq}"))),
                    Err(failure) => self.notice = Some((Tone::Error, describe(&failure))),
                }
                requests.extend(self.poll());
            }
        }
        requests
    }

    /// Applies what a view's key or event did: its notice and whether it leaves.
    fn take(&mut self, step: Step) -> Vec<Request> {
        let mut requests = step.requests;
        if let Some(notice) = step.notice {
            self.notice = Some(notice);
        }
        if step.leave {
            self.mode = Mode::Operate;
            requests.push(Request::Profiles);
        }
        if step.quit {
            self.quit = true;
        }
        requests
    }

    /// Another execution is under the cursor: what was scrolled or picked in the last
    /// one means nothing here.
    fn execution_changed(&mut self) {
        self.node = 0;
        self.event_offset = 0;
        self.decision = None;
        self.settle_decision();
    }

    /// Keeps the decision cursor on an id the list has, or on its first item.
    fn settle_decision(&mut self) {
        let pending = self.pending();
        let kept = self
            .decision
            .as_deref()
            .is_some_and(|id| pending.iter().any(|p| p.id == id));
        if !kept {
            self.decision = pending.first().map(|p| p.id.clone());
        }
    }

    /// Where the decision cursor is in the list, for the screen.
    pub fn decision_index(&self) -> Option<usize> {
        let id = self.decision.as_deref()?;
        self.pending().iter().position(|p| p.id == id)
    }

    /// What to keep fresh for the selected execution: its next events and its pending
    /// decisions.
    pub fn poll(&mut self) -> Vec<Request> {
        let Some(execution) = self.selected_execution().map(|e| e.id.clone()) else {
            return Vec::new();
        };
        let mut requests = Vec::new();
        if self.events_asked.insert(execution.clone()) {
            let since = self.events.get(&execution).map_or(0, |log| log.next);
            requests.push(Request::Events {
                execution: execution.clone(),
                since,
                limit: self.options.tuning.event_page,
            });
        }
        requests.push(Request::Decisions { execution });
        requests
    }

    /// Changes when [`App::poll`] should run at once rather than on the next refresh.
    pub fn poll_key(&self) -> Option<String> {
        self.selected_execution().map(|e| e.id.clone())
    }

    /// The selected execution's events, filtered by node if a filter is set.
    pub fn shown_events(&self) -> (Vec<&BusEvent>, Option<&Failure>) {
        let Some(log) = self
            .selected_execution()
            .and_then(|e| self.events.get(&e.id))
        else {
            return (Vec::new(), None);
        };
        let filter = self.filter.trim();
        let events = log
            .events
            .iter()
            .filter(|e| filter.is_empty() || touches(e, filter))
            .collect();
        (events, log.error.as_ref())
    }

    pub fn decisions_of_selected(&self) -> Option<&Result<Vec<Pending>, Failure>> {
        self.decisions.get(&self.selected_execution()?.id)
    }

    pub fn pending(&self) -> &[Pending] {
        match self.decisions_of_selected() {
            Some(Ok(list)) => list,
            _ => &[],
        }
    }

    /// Opens the selected profile in Edit.
    fn open_editor(&mut self) -> Vec<Request> {
        let Some(profile) = self.selected_profile().cloned() else {
            return Vec::new();
        };
        self.mode = Mode::Edit(Box::new(Editor::open(profile)));
        vec![Request::Manifests]
    }

    fn edit_key(&mut self, key: KeyEvent) -> Vec<Request> {
        let Mode::Edit(editor) = &mut self.mode else {
            return Vec::new();
        };
        let manifests = match &self.manifests {
            Some(Ok(list)) => list.as_slice(),
            _ => &[],
        };
        let ctx = Ctx {
            keys: &self.options.keys,
            tuning: &self.options.tuning,
            manifests,
            modules: &self.modules,
        };
        let step = editor.key(key, &ctx);
        self.take(step)
    }

    fn templates_key(&mut self, key: KeyEvent) -> Vec<Request> {
        let Mode::Templates(view) = &mut self.mode else {
            return Vec::new();
        };
        let (step, then) = view.key(key, &self.options.keys);
        let mut requests = self.take(step);
        match then {
            Then::Nothing => {}
            Then::Leave => self.mode = Mode::Operate,
            Then::Edit(id) => {
                self.mode = Mode::Operate;
                self.pending_edit = Some(id);
                requests.push(Request::Profiles);
            }
        }
        requests
    }

    fn prompt_key(&mut self, prompt: Prompt, key: KeyEvent) -> Vec<Request> {
        let keys = &self.options.keys;
        if keys.back.matches(&key) {
            return Vec::new();
        }
        let typed = match key.code {
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                Some(c)
            }
            _ => None,
        };
        match prompt {
            Prompt::NewProfile(mut name) => match key.code {
                KeyCode::Enter if !name.trim().is_empty() => {
                    let taken: Vec<&str> = self.profiles.iter().map(|p| p.id.as_str()).collect();
                    let id = unique_id(&name, taken, "profile");
                    self.mode = Mode::Edit(Box::new(Editor::new_profile(&id, name.trim())));
                    return vec![Request::Manifests];
                }
                KeyCode::Backspace => {
                    name.pop();
                    self.prompt = Some(Prompt::NewProfile(name));
                }
                _ => {
                    name.extend(typed);
                    self.prompt = Some(Prompt::NewProfile(name));
                }
            },
            Prompt::Filter(mut text) => match key.code {
                KeyCode::Enter => {
                    self.filter = text.trim().to_string();
                    self.event_offset = 0;
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.prompt = Some(Prompt::Filter(text));
                }
                _ => {
                    text.extend(typed);
                    self.prompt = Some(Prompt::Filter(text));
                }
            },
            Prompt::Send {
                execution,
                ports,
                mut index,
                mut value,
            } => {
                if keys.up.matches(&key) || keys.down.matches(&key) {
                    let by = if keys.up.matches(&key) { -1 } else { 1 };
                    index = step(index, ports.len(), by);
                } else if key.code == KeyCode::Enter {
                    let port = &ports[index];
                    match send_value(&port.capability, &value) {
                        Some(parsed) => {
                            return vec![Request::WorkflowSend {
                                execution,
                                port: port.id.clone(),
                                value: parsed,
                            }]
                        }
                        None => {
                            self.notice = Some((
                                Tone::Error,
                                format!("{} takes JSON ({})", port.id, port.capability),
                            ))
                        }
                    }
                } else if key.code == KeyCode::Backspace {
                    value.pop();
                } else {
                    value.extend(typed);
                }
                self.prompt = Some(Prompt::Send {
                    execution,
                    ports,
                    index,
                    value,
                });
            }
        }
        Vec::new()
    }

    /// The new keys of Operate (docs/04-client.md §7, second version). `None`: not one.
    fn operate_key(&mut self, key: KeyEvent) -> Option<Vec<Request>> {
        let keys = self.options.keys.clone();
        if self.focus == Focus::Decisions {
            let approve = keys.approve.matches(&key);
            if approve || keys.reject.matches(&key) {
                // The id drawn under the cursor, not its place in a list that may have
                // moved since; with nothing to decide, the key means what it does elsewhere.
                if let Some(id) = self.decision_index().and_then(|_| self.decision.clone()) {
                    return Some(vec![Request::Decide { id, approve }]);
                }
            }
        }
        if self.focus == Focus::Events && (keys.up.matches(&key) || keys.down.matches(&key)) {
            self.event_offset = if keys.up.matches(&key) {
                self.event_offset
                    .saturating_add(1)
                    .min(self.shown_events().0.len())
            } else {
                self.event_offset.saturating_sub(1)
            };
            return Some(Vec::new());
        }
        if keys.edit.matches(&key) {
            return Some(self.open_editor());
        }
        if keys.new_profile.matches(&key) {
            self.prompt = Some(Prompt::NewProfile(String::new()));
            return Some(Vec::new());
        }
        if keys.templates.matches(&key) {
            self.mode = Mode::Templates(Box::default());
            return Some(vec![Request::Templates, Request::Status]);
        }
        if keys.filter.matches(&key) {
            self.prompt = Some(Prompt::Filter(self.filter.clone()));
            return Some(Vec::new());
        }
        if keys.send.matches(&key) {
            let execution = self.selected_execution()?.clone();
            let ports: Vec<Port> = self
                .workflow(&execution)
                .map(|w| {
                    w.ports
                        .iter()
                        .filter(|p| p.direction == Direction::Input)
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
            if ports.is_empty() {
                self.notice = Some((
                    Tone::Info,
                    "the workflow has no inputs of its own to send to".into(),
                ));
            } else if !execution.state.is_live() {
                self.notice = Some((Tone::Info, "the execution is not running".into()));
            } else {
                self.prompt = Some(Prompt::Send {
                    execution: execution.id,
                    ports,
                    index: 0,
                    value: String::new(),
                });
            }
            return Some(Vec::new());
        }
        None
    }

    pub fn key(&mut self, key: KeyEvent) -> Vec<Request> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        self.notice = None;
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            if let Mode::Edit(editor) = &mut self.mode {
                let step = editor.quit();
                return self.take(step);
            }
            self.quit = true;
            return Vec::new();
        }
        match self.mode {
            Mode::Edit(_) => return self.edit_key(key),
            Mode::Templates(_) => return self.templates_key(key),
            Mode::Operate => {}
        }
        if let Some(prompt) = self.prompt.take() {
            return self.prompt_key(prompt, key);
        }
        if self.focus == Focus::Input {
            return self.type_key(key);
        }
        if let Some(requests) = self.operate_key(key) {
            return requests;
        }
        let mut requests = Vec::new();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.quit = true,
            KeyCode::Tab => self.cycle(1),
            KeyCode::BackTab => self.cycle(-1),
            KeyCode::Left | KeyCode::Char('h') => self.cycle(-1),
            KeyCode::Right | KeyCode::Char('l') => self.cycle(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_selection(1),
            KeyCode::Char('r') => requests.extend([Request::Profiles, Request::Status]),
            KeyCode::Enter => match self.focus {
                Focus::Profiles => {
                    if let Some(profile) = self.selected_profile() {
                        requests.push(Request::Start {
                            profile: profile.id.clone(),
                            workflow: None,
                        });
                    }
                }
                Focus::Executions => self.focus = Focus::Nodes,
                Focus::Nodes => self.start_typing(),
                Focus::Input | Focus::Events | Focus::Decisions => {}
            },
            KeyCode::Char('i') => self.start_typing(),
            KeyCode::Char('s') if self.focus == Focus::Executions => {
                if let Some(execution) = self.selected_execution() {
                    requests.push(Request::Stop {
                        execution: execution.id.clone(),
                    });
                }
            }
            _ => {}
        }
        requests
    }

    fn start_typing(&mut self) {
        if self.talk_target().is_some() {
            self.focus = Focus::Input;
        } else if let Some(node) = self.selected_node() {
            self.notice = Some((
                Tone::Info,
                format!("{} takes no prompts or messages", node.name),
            ));
        }
    }

    fn type_key(&mut self, key: KeyEvent) -> Vec<Request> {
        // Esc and a key pressed right after it reach us as Alt+key: leave, then the key.
        if key.modifiers.contains(KeyModifiers::ALT) {
            self.focus = Focus::Nodes;
            return self.key(KeyEvent::new(key.code, key.modifiers - KeyModifiers::ALT));
        }
        match key.code {
            KeyCode::Esc => self.focus = Focus::Nodes,
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Char(c) => self.input.push(c),
            KeyCode::Enter => return self.submit(),
            _ => {}
        }
        Vec::new()
    }

    fn submit(&mut self) -> Vec<Request> {
        let text = self.input.trim().to_string();
        let Some(((execution, node), kind)) = self.talk_target() else {
            self.focus = Focus::Nodes;
            return Vec::new();
        };
        if text.is_empty() {
            return Vec::new();
        }
        self.input.clear();
        let key = (execution.clone(), node.clone());
        match kind {
            NodeKind::Agent => {
                self.turns
                    .entry(key.clone())
                    .or_default()
                    .push(Turn::Prompt(text.clone()));
                self.thinking.insert(key);
                vec![Request::Prompt {
                    execution,
                    node,
                    text,
                }]
            }
            NodeKind::Channel => vec![Request::Send {
                execution,
                node,
                text,
            }],
            NodeKind::Other => Vec::new(),
        }
    }

    fn cycle(&mut self, by: isize) {
        const ORDER: [Focus; 5] = [
            Focus::Profiles,
            Focus::Executions,
            Focus::Nodes,
            Focus::Events,
            Focus::Decisions,
        ];
        let at = ORDER.iter().position(|f| *f == self.focus).unwrap_or(0);
        self.focus = ORDER[(at as isize + by).rem_euclid(ORDER.len() as isize) as usize];
    }

    fn move_selection(&mut self, by: isize) {
        match self.focus {
            Focus::Profiles => self.profile = step(self.profile, self.profiles.len(), by),
            Focus::Executions => {
                let before = self.execution;
                self.execution = step(self.execution, self.executions.len(), by);
                if before != self.execution {
                    self.execution_changed();
                }
            }
            Focus::Nodes => self.node = step(self.node, self.nodes().len(), by),
            Focus::Decisions => {
                let at = step(self.decision_index().unwrap_or(0), self.pending().len(), by);
                self.decision = self.pending().get(at).map(|p| p.id.clone());
            }
            Focus::Input | Focus::Events => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::edit::{ConfirmFor, Modal, TreeRow};
    use super::*;
    use regulus_proto::{DisabledNode, Port, WireKind};

    fn port(id: &str, capability: &str, direction: Direction) -> Port {
        serde_json::from_value(serde_json::json!({
            "id": id, "name": id, "capability": capability,
            "direction": direction, "kind": WireKind::Uses,
        }))
        .unwrap()
    }

    fn node(id: &str, module: Option<&str>, ports: Vec<Port>) -> Node {
        let mut node: Node = serde_json::from_value(serde_json::json!({
            "id": id, "type": id, "name": id, "module_id": module,
        }))
        .unwrap();
        node.ports = ports;
        node
    }

    fn profile() -> Profile {
        let workflow = serde_json::json!({
            "id": "main", "name": "main", "kind": "main",
            "layout": { "x": 0, "y": 0, "width": 0, "height": 0 },
        });
        let mut workflow: Workflow = serde_json::from_value(workflow).unwrap();
        workflow.nodes = vec![
            node(
                "dev",
                Some("regulus/agent@0.1.0"),
                vec![port("engine", "agent.engine@1", Direction::Input)],
            ),
            node(
                "room",
                Some("regulus/communication@0.1.0"),
                vec![port("participants", "participant@1", Direction::Input)],
            ),
            node("notes", None, vec![]),
            node("gone", Some("someone/missing@1"), vec![]),
        ];
        Profile {
            id: "team".into(),
            name: "Team".into(),
            project_id: None,
            extends: None,
            main_workflow_id: "main".into(),
            workflows: vec![workflow],
        }
    }

    fn module(id: &str) -> ModuleSummary {
        ModuleSummary {
            id: id.into(),
            name: id.into(),
            node_types: vec![],
            workflow_id: None,
            state: ModuleState::Active { since: 0 },
        }
    }

    fn execution(state: ExecutionState) -> Execution {
        Execution {
            id: "exec-1".into(),
            profile_id: "team".into(),
            workflow_id: "main".into(),
            project_id: None,
            state,
            disabled: vec![],
            parent: None,
        }
    }

    fn running_app() -> App {
        let mut app = App::default();
        app.apply(Event::Connection(Connection::Connected {
            core: "regulus/0".into(),
            proto: "1.4".into(),
        }));
        app.apply(Event::Profiles(vec![profile()]));
        app.apply(Event::Status {
            executions: vec![execution(ExecutionState::Running)],
            modules: vec![
                module("regulus/agent@0.1.0"),
                module("regulus/communication@0.1.0"),
            ],
        });
        app
    }

    fn press(app: &mut App, code: KeyCode) -> Vec<Request> {
        app.key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn connecting_asks_for_everything_once() {
        let mut app = App::default();
        let connected = Event::Connection(Connection::Connected {
            core: "c".into(),
            proto: "1.4".into(),
        });
        assert_eq!(
            app.apply(connected.clone()),
            vec![Request::Profiles, Request::Status]
        );
        assert!(app.apply(connected).is_empty());
    }

    #[test]
    fn nodes_are_told_apart_by_capability_and_show_their_state() {
        let app = running_app();
        let rows = app.nodes();
        let summary: Vec<(&str, NodeKind, NodeState)> = rows
            .iter()
            .map(|r| (r.id.as_str(), r.kind, r.state.clone()))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("dev", NodeKind::Agent, NodeState::Running),
                ("room", NodeKind::Channel, NodeState::Running),
                ("notes", NodeKind::Other, NodeState::Resource),
                ("gone", NodeKind::Other, NodeState::NotInstalled),
            ]
        );
    }

    #[test]
    fn a_disabled_node_says_what_it_misses() {
        let mut app = running_app();
        let mut execution = execution(ExecutionState::Running);
        execution.disabled = vec![DisabledNode {
            node_id: "dev".into(),
            missing: vec!["engine".into()],
        }];
        app.apply(Event::Status {
            executions: vec![execution],
            modules: app.modules.clone(),
        });
        assert_eq!(
            app.nodes()[0].state,
            NodeState::Disabled(vec!["engine".into()])
        );
        assert_eq!(app.talk_target(), None);
    }

    #[test]
    fn enter_on_a_profile_starts_it() {
        let mut app = running_app();
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            vec![Request::Start {
                profile: "team".into(),
                workflow: None,
            }]
        );
    }

    #[test]
    fn a_prompt_goes_to_the_agent_and_waits_for_its_answer() {
        let mut app = running_app();
        app.focus = Focus::Nodes;
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Input);
        for c in "hi".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let sent = press(&mut app, KeyCode::Enter);
        assert_eq!(
            sent,
            vec![Request::Prompt {
                execution: "exec-1".into(),
                node: "dev".into(),
                text: "hi".into()
            }]
        );
        let key = ("exec-1".to_string(), "dev".to_string());
        assert!(app.thinking.contains(&key));
        app.apply(Event::Answer {
            execution: key.0.clone(),
            node: key.1.clone(),
            answer: Ok("hello".into()),
        });
        assert!(!app.thinking.contains(&key));
        assert_eq!(
            app.turns[&key],
            vec![Turn::Prompt("hi".into()), Turn::Answer("hello".into())]
        );
    }

    #[test]
    fn a_message_goes_to_the_channel_and_q_is_typed_not_quit() {
        let mut app = running_app();
        app.focus = Focus::Nodes;
        press(&mut app, KeyCode::Down);
        assert_eq!(
            app.watched_channel(),
            Some(Request::Channel {
                execution: "exec-1".into(),
                node: "room".into()
            })
        );
        press(&mut app, KeyCode::Char('i'));
        press(&mut app, KeyCode::Char('q'));
        assert!(!app.quit);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            vec![Request::Send {
                execution: "exec-1".into(),
                node: "room".into(),
                text: "q".into()
            }]
        );
        press(&mut app, KeyCode::Esc);
        assert_eq!(app.focus, Focus::Nodes);
        press(&mut app, KeyCode::Char('q'));
        assert!(app.quit);
    }

    #[test]
    fn esc_typed_quickly_before_a_key_still_leaves_the_input() {
        let mut app = running_app();
        app.focus = Focus::Nodes;
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        app.key(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::ALT));
        assert!(app.input.is_empty());
        assert!(app.quit);
    }

    #[test]
    fn a_stopped_execution_takes_no_input() {
        let mut app = running_app();
        app.apply(Event::Status {
            executions: vec![execution(ExecutionState::Stopped)],
            modules: app.modules.clone(),
        });
        app.focus = Focus::Nodes;
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.focus, Focus::Nodes);
        assert_eq!(app.nodes()[0].state, NodeState::Stopped);
    }

    fn manifests() -> Vec<Manifest> {
        vec![serde_json::from_value(serde_json::json!({
            "module": { "id": "regulus/hello@0.1.0", "name": "Hello" },
            "node": [
                { "type": "greeter", "name": "Greeter",
                  "port": [{ "id": "tick", "capability": "text@1", "direction": "output", "kind": "then" }],
                  "field": [{ "id": "who", "type": "text", "default": "world" }] },
                { "type": "sink", "name": "Sink",
                  "port": [{ "id": "in", "capability": "text@1", "direction": "input", "kind": "then" }] }
            ]
        }))
        .unwrap()]
    }

    fn typing(app: &mut App, text: &str) {
        for c in text.chars() {
            press(app, KeyCode::Char(c));
        }
    }

    #[test]
    fn a_new_profile_is_built_wired_and_saved_without_a_terminal() {
        let mut app = running_app();
        app.modules.push(module("regulus/hello@0.1.0"));
        press(&mut app, KeyCode::Char('n'));
        typing(&mut app, "My Flow");
        assert_eq!(press(&mut app, KeyCode::Enter), vec![Request::Manifests]);
        app.apply(Event::Manifests(Ok(manifests())));
        // Two nodes from the palette: filter, then add.
        press(&mut app, KeyCode::Char('a'));
        typing(&mut app, "greet");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Char('a'));
        typing(&mut app, "sink");
        press(&mut app, KeyCode::Enter);
        // A wire: the only output, then the only input.
        press(&mut app, KeyCode::Char('c'));
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Enter);
        // Edit a field in the inspector: select the greeter, then its `who`.
        let Mode::Edit(editor) = &mut app.mode else {
            panic!("not editing")
        };
        editor.row = 1;
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Enter);
        for _ in 0.."world".len() {
            press(&mut app, KeyCode::Backspace);
        }
        typing(&mut app, "Ana");
        press(&mut app, KeyCode::Enter);
        let saved = app.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL));
        let [Request::Save { profile, force }] = saved.as_slice() else {
            panic!("{saved:?}")
        };
        assert!(!force, "a new profile is saved without force");
        assert_eq!(profile.id, "my-flow");
        let main = profile.workflow("main").unwrap();
        let ids: Vec<&str> = main.nodes.iter().map(|n| n.id.as_str()).collect();
        assert_eq!(ids, vec!["greeter", "sink"]);
        assert_eq!(main.nodes[0].config["who"], "Ana");
        assert_ne!(main.nodes[0].position, main.nodes[1].position);
        assert_eq!(main.wires.len(), 1);
        assert_eq!(main.wires[0].from.port_id, "tick");
        assert!(regulus_core::graph::check_profile(profile, |_| true)
            .iter()
            .all(|d| d.severity != regulus_proto::Severity::Error));
        // Saved: Esc leaves at once, nothing to lose.
        app.apply(Event::Saved {
            profile: profile.clone(),
            result: Ok(regulus_proto::ProfileSaved {
                profile_id: "my-flow".into(),
                path: "/p/my-flow.toml".into(),
                diagnostics: vec![],
            }),
        });
        assert_eq!(press(&mut app, KeyCode::Esc), vec![Request::Profiles]);
        assert_eq!(app.mode, Mode::Operate);
    }

    #[test]
    fn leaving_with_changes_asks_first() {
        let mut app = running_app();
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Char('w'));
        typing(&mut app, "extra");
        press(&mut app, KeyCode::Enter);
        press(&mut app, KeyCode::Esc);
        assert!(matches!(app.mode, Mode::Edit(_)));
        press(&mut app, KeyCode::Char('n'));
        assert!(matches!(app.mode, Mode::Edit(_)));
        press(&mut app, KeyCode::Esc);
        press(&mut app, KeyCode::Char('y'));
        assert_eq!(app.mode, Mode::Operate);
    }

    #[test]
    fn a_refused_wire_says_why_and_is_not_added() {
        let mut app = running_app();
        app.modules.push(module("regulus/hello@0.1.0"));
        press(&mut app, KeyCode::Char('n'));
        typing(&mut app, "x");
        press(&mut app, KeyCode::Enter);
        app.apply(Event::Manifests(Ok(manifests())));
        for query in ["sink", "greet"] {
            press(&mut app, KeyCode::Char('a'));
            typing(&mut app, query);
            press(&mut app, KeyCode::Enter);
        }
        let Mode::Edit(editor) = &mut app.mode else {
            panic!()
        };
        let from = editor.sources()[0].0.clone();
        let wired = |app: &App| match &app.mode {
            Mode::Edit(e) => e.wires().len(),
            _ => 0,
        };
        // Wire once, then try the same wire again.
        for _ in 0..2 {
            press(&mut app, KeyCode::Char('c'));
            press(&mut app, KeyCode::Enter);
            press(&mut app, KeyCode::Enter);
        }
        assert_eq!(wired(&app), 1);
        let (_, notice) = app.notice.clone().unwrap();
        assert!(notice.contains("already there"), "{notice} from {from:?}");
    }

    #[test]
    fn a_node_whose_module_is_missing_is_shown_not_added() {
        let mut app = running_app();
        press(&mut app, KeyCode::Char('n'));
        typing(&mut app, "x");
        press(&mut app, KeyCode::Enter);
        app.apply(Event::Manifests(Ok(manifests())));
        press(&mut app, KeyCode::Char('a'));
        press(&mut app, KeyCode::Enter);
        let Mode::Edit(editor) = &app.mode else {
            panic!()
        };
        assert!(editor.doc.workflow("main").unwrap().nodes.is_empty());
        assert!(app.notice.as_ref().unwrap().1.contains("not installed"));
    }

    #[test]
    fn templates_apply_with_their_vars_and_offer_to_run() {
        let mut app = running_app();
        assert_eq!(
            press(&mut app, KeyCode::Char('t')),
            vec![Request::Templates, Request::Status]
        );
        let template: regulus_proto::TemplateSummary = serde_json::from_value(serde_json::json!({
            "name": "greeting", "description": "d", "path": "/t/greeting.toml",
            "vars": [{ "name": "who" }, { "name": "file", "default": "greeting.txt" }],
            "requires": [{ "module": "regulus/hello", "version": ">=0.1" }]
        }))
        .unwrap();
        app.apply(Event::Templates(Ok(vec![template])));
        press(&mut app, KeyCode::Enter);
        // `who` is required: Enter without it says so.
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(app.notice.as_ref().unwrap().1.contains("who"));
        typing(&mut app, "Ana");
        let applied = press(&mut app, KeyCode::Enter);
        let mut vars = serde_json::Map::new();
        vars.insert("who".into(), "Ana".into());
        vars.insert("file".into(), "greeting.txt".into());
        assert_eq!(
            applied,
            vec![Request::Apply {
                template: "greeting".into(),
                vars,
                id: None,
                force: false
            }]
        );
        let failed = app.apply(Event::Applied(Err(Failure {
            code: Some(regulus_proto::code::PROFILE_EXISTS),
            message: "greeting exists".into(),
        })));
        assert!(failed.is_empty());
        assert!(app
            .notice
            .as_ref()
            .unwrap()
            .1
            .starts_with("✗ already there"));
        app.apply(Event::Applied(Ok(regulus_proto::TemplateApplied {
            profile_id: "greeting".into(),
            path: "/p/greeting.toml".into(),
            diagnostics: vec![],
        })));
        assert_eq!(
            app.key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            vec![Request::Start {
                profile: "greeting".into(),
                workflow: None
            }]
        );
        assert_eq!(press(&mut app, KeyCode::Char('e')), vec![Request::Profiles]);
        assert_eq!(app.pending_edit.as_deref(), Some("greeting"));
    }

    fn bus(seq: u64, node: &str, value: serde_json::Value) -> BusEvent {
        serde_json::from_value(serde_json::json!({
            "seq": seq, "at": 0, "execution_id": "exec-1", "project_id": null, "event": seq,
            "from": { "node_id": node, "port_id": "out" }, "capability": "text@1", "depth": 0,
            "kind": "emitted", "value": value
        }))
        .unwrap()
    }

    #[test]
    fn events_follow_the_cursor_and_filter_by_node() {
        let mut app = running_app();
        let polled = app.poll();
        assert_eq!(
            polled,
            vec![
                Request::Events {
                    execution: "exec-1".into(),
                    since: 0,
                    limit: 200
                },
                Request::Decisions {
                    execution: "exec-1".into()
                }
            ]
        );
        // While a page is on its way, no second one is asked for.
        assert_eq!(app.poll().len(), 1);
        app.apply(Event::Events {
            execution: "exec-1".into(),
            since: 0,
            result: Ok(regulus_proto::EventPage {
                events: vec![
                    bus(1, "dev", "hi".into()),
                    bus(2, "room", serde_json::json!({"a": 1})),
                ],
                next: 2,
                pending: 0,
            }),
        });
        assert!(matches!(app.poll()[0], Request::Events { since: 2, .. }));
        assert_eq!(app.shown_events().0.len(), 2);
        assert_eq!(
            event_line(app.shown_events().0[1], 240),
            "#2 room.out emitted text@1 {\"a\":1}"
        );
        assert_eq!(
            event_line(app.shown_events().0[0], 240),
            "#1 dev.out emitted text@1 hi"
        );
        press(&mut app, KeyCode::Char('/'));
        typing(&mut app, "room");
        press(&mut app, KeyCode::Enter);
        assert_eq!(app.shown_events().0.len(), 1);
        assert_eq!(compact(&serde_json::json!("a\nb"), 240), "a ⏎ b");
        assert_eq!(compact(&serde_json::json!("abcdef"), 3), "abc…");
    }

    #[test]
    fn decisions_are_answered_and_a_missing_module_is_named() {
        let mut app = running_app();
        app.apply(Event::Decisions {
            execution: "exec-1".into(),
            result: Err(Failure {
                code: Some(super::super::core::METHOD_NOT_FOUND),
                message: "method not found".into(),
            }),
        });
        let Some(Err(failure)) = app.decisions_of_selected() else {
            panic!()
        };
        assert_eq!(describe(failure), "□ not installed");
        let unavailable = Failure {
            code: Some(regulus_proto::code::MODULE_UNAVAILABLE),
            message: "human is broken".into(),
        };
        assert_eq!(describe(&unavailable), "✗ unavailable: human is broken");
        let pending: Pending = serde_json::from_value(serde_json::json!({
            "id": "h-1", "kind": "approval", "execution_id": "exec-1", "node_id": "gate",
            "value": "deploy", "prompt": "ok?"
        }))
        .unwrap();
        app.apply(Event::Decisions {
            execution: "exec-1".into(),
            result: Ok(vec![pending]),
        });
        app.focus = Focus::Decisions;
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            vec![Request::Decide {
                id: "h-1".into(),
                approve: true
            }]
        );
        assert_eq!(
            press(&mut app, KeyCode::Char('n')),
            vec![Request::Decide {
                id: "h-1".into(),
                approve: false
            }]
        );
    }

    #[test]
    fn a_value_goes_to_the_workflow_input_as_its_capability_says() {
        let mut app = running_app();
        let mut profile = profile();
        profile.workflows[0].ports = vec![
            port("ask", "text@1", Direction::Input),
            port("n", "number@1", Direction::Input),
        ];
        app.apply(Event::Profiles(vec![profile]));
        press(&mut app, KeyCode::Char('w'));
        typing(&mut app, "hello");
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            vec![Request::WorkflowSend {
                execution: "exec-1".into(),
                port: "ask".into(),
                value: "hello".into()
            }]
        );
        press(&mut app, KeyCode::Char('w'));
        press(&mut app, KeyCode::Down);
        typing(&mut app, "nope");
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert!(app.prompt.is_some(), "a bad value keeps the form open");
        assert_eq!(send_value("number@1", "3"), Some(serde_json::json!(3)));
    }

    fn ctrl(app: &mut App, c: char) -> Vec<Request> {
        app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
    }

    fn editor(app: &App) -> &Editor {
        let Mode::Edit(editor) = &app.mode else {
            panic!("not editing: {:?}", app.mode)
        };
        editor
    }

    /// Opens the selected profile in Edit and adds a workflow, so it has unsaved changes.
    fn dirty_editor() -> App {
        let mut app = running_app();
        app.apply(Event::Manifests(Ok(manifests())));
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Char('w'));
        typing(&mut app, "extra");
        press(&mut app, KeyCode::Enter);
        assert!(editor(&app).dirty());
        app
    }

    fn pending(id: &str) -> Pending {
        serde_json::from_value(serde_json::json!({
            "id": id, "kind": "approval", "execution_id": "exec-1", "node_id": "gate",
            "value": "deploy", "prompt": "ok?"
        }))
        .unwrap()
    }

    #[test]
    fn edits_made_while_a_save_is_in_flight_stay_unsaved() {
        let mut app = dirty_editor();
        let saved = ctrl(&mut app, 's');
        let [Request::Save { profile: sent, .. }] = saved.as_slice() else {
            panic!("{saved:?}")
        };
        let sent = sent.clone();
        // Typed while the core writes: a second workflow.
        press(&mut app, KeyCode::Char('w'));
        typing(&mut app, "later");
        press(&mut app, KeyCode::Enter);
        let done = regulus_proto::ProfileSaved {
            profile_id: "team".into(),
            path: "/p/team.toml".into(),
            diagnostics: vec![],
        };
        // A late answer for another profile changes nothing here.
        let mut other = (*sent).clone();
        other.id = "someone-else".into();
        app.apply(Event::Saved {
            profile: Box::new(other),
            result: Ok(done.clone()),
        });
        assert_eq!(
            editor(&app).saved.as_ref().map(|p| p.workflows.len()),
            Some(1)
        );
        app.apply(Event::Saved {
            profile: sent.clone(),
            result: Ok(done),
        });
        let editor = editor(&app);
        assert_eq!(
            editor.saved.as_ref(),
            Some(&*sent),
            "clean is what was sent"
        );
        assert!(editor.dirty(), "the workflow added meanwhile is not saved");
        assert_eq!(editor.doc.workflows.len(), 3);
    }

    #[test]
    fn ctrl_c_with_unsaved_changes_asks_first() {
        let mut app = dirty_editor();
        ctrl(&mut app, 'c');
        assert!(!app.quit);
        assert_eq!(editor(&app).modal, Some(Modal::Confirm(ConfirmFor::Quit)));
        press(&mut app, KeyCode::Char('n'));
        assert!(!app.quit);
        assert!(matches!(app.mode, Mode::Edit(_)));
        ctrl(&mut app, 'c');
        press(&mut app, KeyCode::Char('y'));
        assert!(app.quit);
        // Nothing to lose: it quits at once.
        let mut clean = running_app();
        press(&mut clean, KeyCode::Char('e'));
        ctrl(&mut clean, 'c');
        assert!(clean.quit);
    }

    #[test]
    fn a_decision_is_answered_by_the_id_under_the_cursor() {
        let mut app = running_app();
        app.apply(Event::Decisions {
            execution: "exec-1".into(),
            result: Ok(vec![pending("h-1"), pending("h-2")]),
        });
        app.focus = Focus::Decisions;
        press(&mut app, KeyCode::Down);
        assert_eq!(app.decision.as_deref(), Some("h-2"));
        assert_eq!(app.decision_index(), Some(1));
        // Another execution's list arrives: the cursor here does not move.
        app.apply(Event::Decisions {
            execution: "exec-9".into(),
            result: Ok(vec![pending("x-1")]),
        });
        assert_eq!(app.decision.as_deref(), Some("h-2"));
        // h-1 was decided elsewhere before the key landed: h-2 is still what is answered.
        app.apply(Event::Decisions {
            execution: "exec-1".into(),
            result: Ok(vec![pending("h-2")]),
        });
        assert_eq!(app.decision_index(), Some(0));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            vec![Request::Decide {
                id: "h-2".into(),
                approve: true
            }]
        );
        // The one under the cursor is gone: the first remaining one takes its place.
        app.apply(Event::Decisions {
            execution: "exec-1".into(),
            result: Ok(vec![pending("h-3")]),
        });
        assert_eq!(app.decision.as_deref(), Some("h-3"));
    }

    #[test]
    fn with_nothing_to_reject_n_means_a_new_profile() {
        let mut app = running_app();
        app.apply(Event::Decisions {
            execution: "exec-1".into(),
            result: Ok(vec![]),
        });
        app.focus = Focus::Decisions;
        assert!(press(&mut app, KeyCode::Char('n')).is_empty());
        assert_eq!(app.prompt, Some(Prompt::NewProfile(String::new())));
    }

    #[test]
    fn changing_the_execution_resets_its_cursors() {
        let mut app = running_app();
        let mut second = execution(ExecutionState::Running);
        second.id = "exec-2".into();
        app.apply(Event::Status {
            executions: vec![execution(ExecutionState::Running), second],
            modules: vec![],
        });
        app.apply(Event::Decisions {
            execution: "exec-1".into(),
            result: Ok(vec![pending("h-1")]),
        });
        app.event_offset = 3;
        assert_eq!(app.decision.as_deref(), Some("h-1"));
        app.focus = Focus::Executions;
        press(&mut app, KeyCode::Down);
        assert_eq!(
            app.selected_execution().map(|e| e.id.as_str()),
            Some("exec-2")
        );
        assert_eq!(app.event_offset, 0);
        assert_eq!(app.decision, None);
        // The selected execution vanishes from a status: the cursors reset as well.
        app.event_offset = 2;
        app.apply(Event::Status {
            executions: vec![execution(ExecutionState::Running)],
            modules: vec![],
        });
        assert_eq!(
            app.selected_execution().map(|e| e.id.as_str()),
            Some("exec-1")
        );
        assert_eq!(app.event_offset, 0);
        assert_eq!(
            app.decision.as_deref(),
            Some("h-1"),
            "settled on what exec-1 has"
        );
    }

    #[test]
    fn a_profile_that_does_not_come_back_is_not_waited_for() {
        let mut app = running_app();
        app.pending_edit = Some("ghost".into());
        app.apply(Event::Profiles(vec![profile()]));
        assert_eq!(app.pending_edit, None);
        assert_eq!(app.mode, Mode::Operate);
        assert!(matches!(&app.notice, Some((Tone::Error, text)) if text.contains("ghost")));
        // A later refresh does not open anything on its own.
        app.apply(Event::Profiles(vec![profile()]));
        assert_eq!(app.mode, Mode::Operate);
    }

    #[test]
    fn deleting_resets_the_inspector_and_wire_cursors() {
        let mut app = running_app();
        app.apply(Event::Manifests(Ok(manifests())));
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Down);
        let Mode::Edit(opened) = &mut app.mode else {
            panic!()
        };
        assert_eq!(opened.tree()[1], TreeRow::Node("main".into(), "dev".into()));
        opened.field = 2;
        opened.wire = 1;
        press(&mut app, KeyCode::Char('d'));
        press(&mut app, KeyCode::Char('y'));
        let editor = editor(&app);
        assert!(editor.doc.workflow("main").unwrap().node("dev").is_none());
        assert_eq!((editor.field, editor.wire), (0, 0));
    }

    #[test]
    fn a_rename_to_nothing_is_refused() {
        let mut app = running_app();
        app.apply(Event::Manifests(Ok(manifests())));
        press(&mut app, KeyCode::Char('e'));
        press(&mut app, KeyCode::Char('r'));
        assert!(matches!(editor(&app).modal, Some(Modal::Text { .. })));
        for _ in 0..8 {
            press(&mut app, KeyCode::Backspace);
        }
        press(&mut app, KeyCode::Enter);
        let editor = editor(&app);
        assert!(
            matches!(editor.modal, Some(Modal::Text { .. })),
            "the dialog stays"
        );
        assert_eq!(editor.doc.workflow("main").unwrap().name, "main");
        assert!(matches!(&app.notice, Some((Tone::Info, text)) if text.contains("empty")));
    }

    #[test]
    fn enter_in_the_tree_does_not_run_anything() {
        let mut app = running_app();
        app.apply(Event::Manifests(Ok(manifests())));
        press(&mut app, KeyCode::Char('e'));
        assert!(press(&mut app, KeyCode::Enter).is_empty());
        assert_eq!(
            ctrl(&mut app, 'r'),
            vec![Request::Start {
                profile: "team".into(),
                workflow: Some("main".into()),
            }]
        );
    }
}
