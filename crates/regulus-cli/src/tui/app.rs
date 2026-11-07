//! What the terminal UI knows and how keys change it. No drawing and no I/O here: keys
//! and core events go in, requests to the core come out, so all of it is testable.

use std::collections::{HashMap, HashSet};

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use regulus_proto::{
    Direction, Execution, ExecutionState, ModuleState, ModuleSummary, Node, Profile, Workflow,
};

use super::core::{ChannelState, Connection, Event, Request};

/// Capabilities that tell what a node is, never its module (CLAUDE.md, invariant 2).
const AGENT_ENGINE: &str = "agent.engine";
const PARTICIPANT: &str = "participant";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Profiles,
    Executions,
    Nodes,
    Input,
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
}

impl Default for App {
    fn default() -> Self {
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
                    .and_then(|id| self.executions.iter().position(|e| e.id == id))
                    .unwrap_or_else(|| step(self.execution, self.executions.len(), 0));
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
                self.notice = Some((Tone::Info, format!("started {}", execution.id)));
                if let ExecutionState::Failed { reason } = &execution.state {
                    self.notice = Some((Tone::Error, format!("{}: {reason}", execution.id)));
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
        }
        requests
    }

    pub fn key(&mut self, key: KeyEvent) -> Vec<Request> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        self.notice = None;
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return Vec::new();
        }
        if self.focus == Focus::Input {
            return self.type_key(key);
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
                        });
                    }
                }
                Focus::Executions => self.focus = Focus::Nodes,
                Focus::Nodes => self.start_typing(),
                Focus::Input => {}
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
        const ORDER: [Focus; 3] = [Focus::Profiles, Focus::Executions, Focus::Nodes];
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
                    self.node = 0;
                }
            }
            Focus::Nodes => self.node = step(self.node, self.nodes().len(), by),
            Focus::Input => {}
        }
    }
}

#[cfg(test)]
mod tests {
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
                profile: "team".into()
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
}
