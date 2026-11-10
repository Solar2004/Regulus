//! What the coding screen knows and how keys change it (docs/04-client.md §12). No drawing
//! and no I/O here: keys, pasted text, the clock and what the core answers go in, requests
//! to the core come out, so all of it is testable.
//!
//! The screen goes through stages: it looks at what the core has and decides what a launch
//! does (`session::plan`), shows the setup form when a profile is to be written, and then
//! is a conversation with the session's agent, with its workspace in a side panel.

use std::collections::HashSet;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use regulus_proto::{
    ExecutionState, ExecutionTree, ModuleSummary, TemplateRequire, TemplateSummary,
};
use serde_json::Value;

use super::composer::Composer;
use super::inspect::{self, Action, Ask, Found, Listing, Summary, Text};
use super::launch::{self, Failure, Survey};
use super::link::{Event, Request};
use super::mascot::Mood;
use super::session::{candidates, Candidate, Plan, Session, Step, Wanted, ROOT};
use super::settings::{OnExit, Settings, SetupForm};
use crate::conversation::{
    answer_for, asks_about, open_permissions, Answer, PermissionRequest, Record, SYSTEM, USER,
};
use crate::talk;
use crate::tui::app::{node_state, NodeState, Tone};
use crate::tui::core::{self, describe, Connection, Pending};
use crate::tui::keys::Options;
use crate::tui::talk::{AgentLog, Choice, PermissionPicker};
use crate::tui::templates::{require_state, Form, FormKey, RequireState};

/// Something asked of the core: `None` until it answers.
pub type Loaded<T> = Option<Result<T, Failure>>;

/// The var a credential goes in, and how it must be written: a secret's name.
const CREDENTIAL: &str = "credential";
const SECRET_PREFIX: &str = "secret:";
/// Lines a hit of a search is shown below the top of the file it opens.
const HIT_CONTEXT: u64 = 3;

/// The tabs of the side panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Tab {
    /// The session, the plan, the tools of the turn and what waits for the person.
    #[default]
    Activity,
    /// The workspace's files, a file, or what a search found.
    Files,
    /// What git says changed, and the diff.
    Changes,
    /// The executions the session's nodes started.
    Workers,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Activity, Tab::Files, Tab::Changes, Tab::Workers];

    pub fn title(self) -> &'static str {
        match self {
            Tab::Activity => "Activity",
            Tab::Files => "Files",
            Tab::Changes => "Changes",
            Tab::Workers => "Workers",
        }
    }

    fn step(self, by: isize) -> Self {
        let at = Self::ALL.iter().position(|t| *t == self).unwrap_or(0) as isize;
        Self::ALL[(at + by).rem_euclid(Self::ALL.len() as isize) as usize]
    }
}

/// Where the keys go in a conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Composer,
    Side,
}

/// A file of the workspace, open in the Files tab.
#[derive(Debug, Clone, PartialEq)]
pub struct Viewer {
    pub path: String,
    pub text: Loaded<Text>,
    /// Lines scrolled down.
    pub scroll: usize,
    /// The line to show once the text is there: the hit of a search.
    pub line: Option<u64>,
}

/// A search of the workspace: the query while it is typed, then what it found.
#[derive(Debug, Clone, PartialEq)]
pub struct Search {
    pub query: String,
    pub typing: bool,
    pub found: Loaded<Found>,
    pub index: usize,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Files {
    /// The folder shown, relative to the workspace's root; empty is the root.
    pub path: String,
    pub listing: Loaded<Listing>,
    pub index: usize,
    pub search: Option<Search>,
    pub viewer: Option<Viewer>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Side {
    pub shown: bool,
    pub tab: Tab,
    pub summary: Loaded<Summary>,
    pub files: Files,
    pub diff: Loaded<Text>,
    pub tree: Loaded<ExecutionTree>,
    /// Lines scrolled down in Activity, Changes and Workers.
    pub scroll: usize,
}

/// A permission request that has the keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asking {
    /// The request's id.
    pub request: String,
    pub index: usize,
    /// When it took the keys: for a moment after, what was being typed answers nothing.
    pub since: u128,
}

/// A conversation with a session's agent.
#[derive(Debug, Clone, PartialEq)]
pub struct Chat {
    pub session: Session,
    /// `module.list`, once the core answered: what the nodes' states are read from.
    pub modules: Option<Vec<ModuleSummary>>,
    /// What the Agent box recorded, as far as it was read.
    pub log: AgentLog,
    pub composer: Composer,
    pub focus: Focus,
    /// Lines of the transcript scrolled back from the newest; 0 follows it.
    pub scroll: usize,
    pub side: Side,
    /// `human.list`: where the decisions that answer permission requests wait.
    pub decisions: Loaded<Vec<Pending>>,
    pub asking: Option<Asking>,
    /// Requests the person put off or answered: they do not take the keys again.
    pub settled: HashSet<String>,
    /// What a person should know about the session: how it came to be, what is off in it.
    pub notes: Vec<String>,
    /// Goes up whenever what the transcript shows changes: the screen lays it out again.
    pub version: u64,
    /// Another session is being attached to from this one.
    pub switching: bool,
    /// The engine session the last page named: a new one means the model's context is gone.
    engine_session: Option<String>,
    /// The newest prompt of the person already offered to the composer's history.
    remembered: u64,
    /// The workspace changed since it was read: a turn or a tool ended.
    stale: bool,
    was_busy: bool,
    tools_done: usize,
    /// When the history was asked for and not answered yet.
    history_asked: Option<u128>,
    read_at: u128,
    status_at: u128,
    inspected_at: u128,
}

/// The form a profile is written from.
#[derive(Debug, Clone, PartialEq)]
pub struct Setup {
    pub form: Form,
    pub description: String,
    /// What the template needs and how each module stands.
    pub requires: Vec<(TemplateRequire, RequireState)>,
    /// Why the last try did not go through.
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// Reaching the core and looking at what it has.
    Opening,
    Setup(Box<Setup>),
    /// A step is being carried out. `back`: the form to return to if it fails.
    Launching {
        what: String,
        back: Option<Box<Setup>>,
    },
    /// It cannot go on: why, and what to do about it.
    Blocked(String),
    Chat(Box<Chat>),
}

/// One row of the sessions list.
#[derive(Debug, Clone, PartialEq)]
pub enum PickRow {
    /// A new session of the profile this one runs.
    New(String),
    Session(Box<Candidate>),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    Help,
    /// The sessions to attach to; the cursor.
    Sessions(usize),
    /// Stop the execution?
    ConfirmStop,
    /// Leaving: stop the execution, or leave it running?
    ConfirmLeave,
}

/// How the screen was left, to say after the terminal is back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leave {
    Quiet,
    /// The execution goes on.
    Kept(String),
    Stopped(String),
}

#[derive(Debug)]
pub struct App {
    pub connection: Connection,
    pub wanted: Wanted,
    pub options: Options,
    pub settings: Settings,
    /// `--yes`: no setup form unless the core refuses.
    pub skip_form: bool,
    /// `client.refresh_ms`.
    pub refresh_ms: u128,
    pub stage: Stage,
    pub overlay: Option<Overlay>,
    pub notice: Option<(Tone, String)>,
    /// Set when the person leaves: the screen closes.
    pub leave: Option<Leave>,
    /// Milliseconds since the screen opened.
    pub now: u128,
    /// Rows a page key moves by: the screen says how tall it is.
    pub page: usize,
    /// The last look at the core: what the setup form and the sessions list are made of.
    pub survey: Option<Survey>,
    /// The execution is being stopped to leave.
    leaving: bool,
}

fn step(index: usize, len: usize, by: isize) -> usize {
    if len == 0 {
        return 0;
    }
    index.saturating_add_signed(by).min(len - 1)
}

impl Setup {
    fn new(
        template: &TemplateSummary,
        wanted: &Wanted,
        profile: &str,
        force: bool,
        modules: &[ModuleSummary],
    ) -> Self {
        // What has no default comes first: it is what the person has to say.
        let mut ordered = template.clone();
        ordered.vars.sort_by_key(|var| var.default.is_some());
        let mut form = Form::new(&ordered);
        for (var, value) in form.vars.iter().zip(form.values.iter_mut()) {
            if var.name == ROOT {
                *value = wanted.root.clone().unwrap_or_default();
            } else if let Some(set) = wanted.vars.get(&var.name) {
                *value = set.clone();
            }
        }
        // What the person named, which may be a file: the core finds it the same way.
        form.template = wanted.template.clone();
        form.id = profile.to_string();
        form.force = force;
        let unknown: Vec<&str> = wanted
            .vars
            .keys()
            .filter(|name| !template.vars.iter().any(|var| var.name == **name))
            .map(String::as_str)
            .collect();
        let error = (!unknown.is_empty()).then(|| {
            format!(
                "the template has no var {}: it was left out",
                unknown.join(", ")
            )
        });
        Self {
            form,
            description: template.description.clone(),
            requires: template
                .requires
                .iter()
                .map(|require| (require.clone(), require_state(require, modules)))
                .collect(),
            error,
        }
    }

    /// The step the form asks for, or what it still lacks.
    fn step(&self) -> Result<Step, String> {
        let missing = self.form.missing();
        if !missing.is_empty() {
            return Err(format!("fill in: {}", missing.join(", ")));
        }
        for (var, value) in self.form.vars.iter().zip(&self.form.values) {
            let value = value.trim();
            if var.name == CREDENTIAL && !value.is_empty() && !value.starts_with(SECRET_PREFIX) {
                return Err(format!(
                    "{CREDENTIAL} is a secret's name, `{SECRET_PREFIX}<name>` (stored with \
                     `regulus secret set <name>`), never the secret itself"
                ));
            }
        }
        match self.form.request() {
            core::Request::Apply {
                id: Some(profile),
                template,
                vars,
                force,
            } => Ok(Step::Apply {
                template,
                vars,
                profile,
                force,
            }),
            _ => Err("fill in: the profile id".into()),
        }
    }
}

impl Chat {
    fn new(session: Session, now: u128) -> Self {
        let notes = launch::notes(&session);
        Self {
            session,
            modules: None,
            log: AgentLog::default(),
            composer: Composer::default(),
            focus: Focus::Composer,
            scroll: 0,
            side: Side {
                shown: true,
                ..Side::default()
            },
            decisions: None,
            asking: None,
            settled: HashSet::new(),
            notes,
            version: 0,
            switching: false,
            engine_session: None,
            remembered: 0,
            stale: false,
            was_busy: false,
            tools_done: 0,
            history_asked: Some(now),
            read_at: now,
            status_at: now,
            inspected_at: now,
        }
    }

    fn execution(&self) -> &str {
        &self.session.execution.id
    }

    /// A turn is in flight: a prompt sent and not recorded yet, a session that says it is
    /// busy, or a prompt of the person with no answer after it.
    pub fn busy(&self) -> bool {
        let unanswered = self
            .log
            .records
            .iter()
            .rev()
            .find_map(|record| match record {
                Record::Entry { role, .. } => Some(role == USER),
                Record::Activity { .. } => None,
            })
            .unwrap_or(false);
        self.log.busy() || unanswered
    }

    /// The agent node's state, once the modules are known and if the workflow lists it.
    pub fn agent_state(&self) -> Option<NodeState> {
        let modules = self.modules.as_deref()?;
        let node = self.session.workflow()?.node(&self.session.agent)?;
        Some(node_state(node, &self.session.execution, modules))
    }

    /// Why the agent takes no prompt now, if it does not.
    pub fn unwritable(&self) -> Option<String> {
        let execution = &self.session.execution;
        let state = match &execution.state {
            ExecutionState::Starting | ExecutionState::Running => None,
            ExecutionState::Stopped => Some("stopped".to_string()),
            ExecutionState::Failed { reason } => Some(format!("failed ({reason})")),
            ExecutionState::Interrupted { reason } => Some(format!("interrupted ({reason})")),
        };
        if let Some(state) = state {
            return Some(format!(
                "execution {} is {state}: it takes no prompts",
                execution.id
            ));
        }
        match self.agent_state()? {
            NodeState::Disabled(missing) => Some(format!(
                "the agent is disabled: nothing usable is wired into {}",
                missing.join(", ")
            )),
            NodeState::NotInstalled => Some("the agent's module is not installed".into()),
            NodeState::Broken(reason) => Some(format!("the agent's module is broken: {reason}")),
            _ => None,
        }
    }

    /// The permission requests no answer reached yet, oldest first.
    pub fn open_requests(&self) -> Vec<PermissionRequest> {
        open_permissions(&self.log.records)
    }

    /// The request that has the keys, with its choices: every option, and a plain
    /// rejection when none of them rejects.
    pub fn asked(&self) -> Option<(PermissionRequest, Vec<Choice>, usize)> {
        let asking = self.asking.as_ref()?;
        let request = self
            .open_requests()
            .into_iter()
            .find(|request| request.id == asking.request)?;
        let choices = choices(&request);
        let index = asking.index.min(choices.len().saturating_sub(1));
        Some((request, choices, index))
    }

    /// The decision a Decider opened for a request, if `human.list` has it.
    fn decision_for(&self, request: &PermissionRequest) -> Option<String> {
        let node = request.node.as_deref().unwrap_or(&self.session.agent);
        let list = self.decisions.as_ref()?.as_ref().ok()?;
        list.iter()
            .find(|p| asks_about(&p.kind, &p.value, self.execution(), node, &request.id))
            .map(|p| p.id.clone())
    }

    fn history_request(&self, limit: usize) -> Request {
        Request::Core(core::Request::AgentHistory {
            execution: self.execution().to_string(),
            node: self.session.agent.clone(),
            since: self.log.next,
            limit,
        })
    }

    /// A question to the workspace; nothing without a workspace node.
    fn inspect(&self, ask: Ask) -> Option<Request> {
        Some(Request::Inspect {
            execution: self.execution().to_string(),
            node: self.session.workspace.clone()?,
            ask,
        })
    }

    fn tree_request(&self) -> Request {
        Request::Tree {
            execution: self.execution().to_string(),
        }
    }

    /// What the tab shown is made of, asked again.
    fn tab_requests(&self) -> Vec<Request> {
        match self.side.tab {
            Tab::Activity => Vec::new(),
            Tab::Files => {
                let viewer = self.side.files.viewer.as_ref();
                let ask = match (viewer, &self.side.files.search) {
                    (Some(viewer), _) => Ask::read(&viewer.path),
                    (None, Some(search)) if !search.typing => self.search_ask(&search.query),
                    _ => Ask::list(&self.side.files.path),
                };
                self.inspect(ask).into_iter().collect()
            }
            Tab::Changes => self.inspect(Ask::diff()).into_iter().collect(),
            Tab::Workers => vec![self.tree_request()],
        }
    }

    /// The workspace's summary and the tab shown, read again.
    fn refresh(&self) -> Vec<Request> {
        let mut requests: Vec<Request> = self.inspect(Ask::summary()).into_iter().collect();
        if self.side.shown {
            requests.extend(self.tab_requests());
        }
        requests
    }

    /// A search from the folder shown down; from the root, everywhere.
    fn search_ask(&self, query: &str) -> Ask {
        let mut ask = Ask::search(query);
        if !self.side.files.path.is_empty() {
            ask.path = Some(self.side.files.path.clone());
        }
        ask
    }

    fn show(&mut self, tab: Tab) -> Vec<Request> {
        self.side.shown = true;
        if self.side.tab != tab {
            self.side.tab = tab;
            self.side.scroll = 0;
        }
        self.tab_requests()
    }

    fn open_folder(&mut self, path: &str) -> Vec<Request> {
        let files = &mut self.side.files;
        files.path = path.trim_matches('/').to_string();
        files.listing = None;
        files.index = 0;
        files.search = None;
        files.viewer = None;
        self.show(Tab::Files)
    }

    fn open_file(&mut self, path: &str, line: Option<u64>) -> Vec<Request> {
        self.side.files.viewer = Some(Viewer {
            path: path.trim_matches('/').to_string(),
            text: None,
            scroll: 0,
            line,
        });
        self.show(Tab::Files)
    }

    fn search(&mut self, query: &str) -> Vec<Request> {
        self.side.files.viewer = None;
        self.side.files.search = Some(Search {
            query: query.to_string(),
            typing: false,
            found: None,
            index: 0,
        });
        self.show(Tab::Files)
    }

    /// What a page of the history changed besides the records: prompts to bring back, a
    /// new engine session, a request that takes the keys, a workspace to read again.
    fn absorb(&mut self, now: u128, may_ask: bool) -> Option<(Tone, String)> {
        let mut notice = None;
        for record in &self.log.records {
            if let Record::Entry {
                seq, role, text, ..
            } = record
            {
                if role == USER && *seq > self.remembered {
                    self.composer.remember(text);
                    self.remembered = *seq;
                }
            }
        }
        let session = self.log.session["engine_session_id"]
            .as_str()
            .map(str::to_string);
        if let (Some(before), Some(after)) = (&self.engine_session, &session) {
            if before != after {
                notice = Some((
                    Tone::Info,
                    format!(
                        "the engine opened a new session ({after}): the model no longer has \
                         the earlier context; the transcript is Regulus' record"
                    ),
                ));
            }
        }
        if session.is_some() {
            self.engine_session = session;
        }
        let open = self.open_requests();
        if let Some(asking) = &self.asking {
            if !open.iter().any(|request| request.id == asking.request) {
                self.asking = None;
            }
        }
        self.settled
            .retain(|id| open.iter().any(|request| request.id == *id));
        let waiting = open
            .iter()
            .find(|request| !self.settled.contains(&request.id));
        if let (None, Some(request), true) = (&self.asking, waiting, may_ask) {
            if self.composer.is_empty() {
                self.asking = Some(Asking {
                    request: request.id.clone(),
                    index: 0,
                    since: now,
                });
            }
        }
        let done = self
            .log
            .records
            .iter()
            .filter(|record| {
                matches!(record, Record::Activity { event, .. }
                    if event["kind"] == "tool_update"
                        && matches!(event["status"].as_str(), Some("completed" | "failed")))
            })
            .count();
        let busy = self.busy();
        if done != self.tools_done || (self.was_busy && !busy) {
            self.stale = true;
        }
        self.tools_done = done;
        self.was_busy = busy;
        notice
    }
}

/// The choices of a permission request, as the terminal UI's picker makes them.
pub fn choices(request: &PermissionRequest) -> Vec<Choice> {
    PermissionPicker::new(
        String::new(),
        request.id.clone(),
        request.title.clone(),
        &request.options,
    )
    .choices
}

impl App {
    pub fn new(
        wanted: Wanted,
        options: Options,
        settings: Settings,
        refresh_ms: u64,
        skip_form: bool,
    ) -> Self {
        Self {
            connection: Connection::Connecting,
            wanted,
            options,
            settings,
            skip_form,
            refresh_ms: u128::from(refresh_ms.max(1)),
            stage: Stage::Opening,
            overlay: None,
            notice: None,
            leave: None,
            now: 0,
            page: 10,
            survey: None,
            leaving: false,
        }
    }

    /// The first request: look at what the core has. Sending it is what reaches the core.
    pub fn open(&mut self) -> Vec<Request> {
        self.stage = Stage::Opening;
        vec![Request::Survey(Box::new(self.wanted.clone()))]
    }

    pub fn chat(&self) -> Option<&Chat> {
        match &self.stage {
            Stage::Chat(chat) => Some(chat),
            _ => None,
        }
    }

    fn chat_mut(&mut self) -> Option<&mut Chat> {
        match &mut self.stage {
            Stage::Chat(chat) => Some(chat),
            _ => None,
        }
    }

    fn say(&mut self, tone: Tone, text: impl Into<String>) {
        self.notice = Some((tone, text.into()));
    }

    /// What the mascot's face says.
    pub fn mood(&self) -> Mood {
        if matches!(self.connection, Connection::Failed(_)) {
            return Mood::Failed;
        }
        match &self.stage {
            Stage::Opening | Stage::Launching { .. } => Mood::Working,
            Stage::Setup(_) => Mood::Idle,
            Stage::Blocked(_) => Mood::Failed,
            Stage::Chat(chat) => {
                let failed = matches!(chat.log.records.last(),
                    Some(Record::Entry { role, .. }) if role == SYSTEM);
                if !chat.session.execution.state.is_live() {
                    Mood::Asleep
                } else if !chat.open_requests().is_empty() {
                    Mood::Asking
                } else if chat.busy() {
                    Mood::Working
                } else if failed {
                    Mood::Failed
                } else {
                    Mood::Idle
                }
            }
        }
    }

    /// The rows of the sessions list: a new session of this profile, then every execution
    /// with an agent, the newest first.
    pub fn pick_rows(&self) -> Vec<PickRow> {
        let mut rows = Vec::new();
        if let Some(chat) = self.chat() {
            rows.push(PickRow::New(chat.session.profile.id.clone()));
        }
        if let Some(survey) = &self.survey {
            let found = candidates(&survey.profiles, &survey.executions, &survey.manifests);
            rows.extend(
                found
                    .into_iter()
                    .rev()
                    .map(|candidate| PickRow::Session(Box::new(candidate))),
            );
        }
        rows
    }

    /// What the screen measured when it last drew: how far the transcript and the side
    /// panel can scroll and how many rows a page is. The scrolls are held inside them.
    pub fn settle(&mut self, transcript_room: usize, side_room: usize, page: usize) {
        self.page = page.max(1);
        if let Some(chat) = self.chat_mut() {
            chat.scroll = chat.scroll.min(transcript_room);
            chat.side.scroll = chat.side.scroll.min(side_room);
            if let Some(viewer) = &mut chat.side.files.viewer {
                viewer.scroll = viewer.scroll.min(side_room);
            }
        }
    }

    // ── What the core answers ─────────────────────────────────────────────────────────

    pub fn apply(&mut self, event: Event) -> Vec<Request> {
        match event {
            Event::Core(event) => self.core(event),
            Event::Surveyed(result) => self.surveyed(result),
            Event::Launched(result) => self.launched(result),
            Event::Inspected {
                execution,
                node,
                ask,
                result,
            } => {
                self.inspected(&execution, &node, ask, result);
                Vec::new()
            }
            Event::Tree { execution, result } => {
                if let Some(chat) = self.chat_mut() {
                    if chat.execution() == execution {
                        chat.side.tree = Some(result);
                    }
                }
                Vec::new()
            }
        }
    }

    fn core(&mut self, event: core::Event) -> Vec<Request> {
        use core::Event as E;
        match event {
            E::Connection(connection) => {
                self.connection = connection;
                // Whatever was asked on the old line will not be answered.
                if let Some(chat) = self.chat_mut() {
                    chat.history_asked = None;
                }
                Vec::new()
            }
            E::Status {
                executions,
                modules,
            } => {
                let mut said = None;
                if let Some(chat) = self.chat_mut() {
                    let now = executions.iter().find(|e| e.id == chat.execution());
                    if let Some(now) = now {
                        let was_live = chat.session.execution.state.is_live();
                        if was_live && !now.state.is_live() {
                            said = Some(match &now.state {
                                ExecutionState::Failed { reason } => {
                                    format!("execution {} failed: {reason}", now.id)
                                }
                                ExecutionState::Interrupted { reason } => {
                                    format!("execution {} was interrupted: {reason}", now.id)
                                }
                                _ => format!("execution {} stopped", now.id),
                            });
                        }
                        chat.session.execution = now.clone();
                    }
                    chat.modules = Some(modules);
                }
                if let Some(said) = said {
                    self.say(Tone::Info, said);
                }
                Vec::new()
            }
            E::AgentHistory {
                execution,
                node,
                since,
                result,
            } => self.history(&execution, &node, since, result),
            E::AgentSent {
                execution,
                node,
                text,
                result,
            } => {
                let limit = self.options.tuning.event_page;
                let now = self.now;
                let Some(chat) = self.chat_mut() else {
                    return Vec::new();
                };
                if chat.execution() != execution || chat.session.agent != node {
                    return Vec::new();
                }
                let at = chat
                    .log
                    .queued
                    .iter()
                    .position(|(turn, queued)| turn.is_empty() && *queued == text);
                chat.version += 1;
                match (result, at) {
                    (Ok(turn), Some(at)) => chat.log.queued[at].0 = turn,
                    (Ok(_), None) => {}
                    (Err(failure), at) => {
                        if let Some(at) = at {
                            chat.log.queued.remove(at);
                        }
                        // What was typed is not lost with the send.
                        if chat.composer.is_empty() {
                            chat.composer.set(&text);
                        }
                        self.say(Tone::Error, describe(&failure));
                        return Vec::new();
                    }
                }
                chat.history_asked = Some(now);
                vec![chat.history_request(limit)]
            }
            E::AgentCancelled(result) => {
                match result {
                    Ok(()) => self.say(Tone::Info, "asked the agent to cancel its turn"),
                    Err(failure) => self.say(Tone::Error, describe(&failure)),
                }
                Vec::new()
            }
            E::AllDecisions(result) => {
                if let Some(chat) = self.chat_mut() {
                    chat.decisions = Some(result);
                }
                Vec::new()
            }
            E::Decided(result) => {
                match result {
                    Ok(()) => self.say(Tone::Info, "answered"),
                    Err(failure) => {
                        // The answer did not go through: the request may be asked again.
                        if let Some(chat) = self.chat_mut() {
                            chat.settled.clear();
                        }
                        self.say(Tone::Error, describe(&failure));
                    }
                }
                let limit = self.options.tuning.event_page;
                let Some(chat) = self.chat() else {
                    return Vec::new();
                };
                vec![
                    chat.history_request(limit),
                    Request::Core(core::Request::AllDecisions),
                ]
            }
            E::Stopped(Ok(execution)) => {
                let id = execution.id.clone();
                if let Some(chat) = self.chat_mut() {
                    if chat.execution() == id {
                        chat.session.execution = execution;
                    }
                }
                if self.leaving {
                    self.leave = Some(Leave::Stopped(id));
                } else {
                    self.say(Tone::Info, format!("stopped {id}"));
                }
                Vec::new()
            }
            E::Stopped(Err(error)) => {
                self.leaving = false;
                self.say(Tone::Error, error);
                Vec::new()
            }
            _ => Vec::new(),
        }
    }

    fn surveyed(&mut self, result: Result<Box<Survey>, Failure>) -> Vec<Request> {
        let survey = match result {
            Ok(survey) => *survey,
            Err(failure) => {
                if matches!(self.stage, Stage::Opening) {
                    // The line itself says more than "it does not answer".
                    let why = match &self.connection {
                        Connection::Failed(reason) => reason.clone(),
                        _ => describe(&failure),
                    };
                    self.stage = Stage::Blocked(why);
                } else {
                    self.say(Tone::Error, describe(&failure));
                }
                return Vec::new();
            }
        };
        let requests = if matches!(self.stage, Stage::Opening) {
            self.decide(&survey)
        } else {
            Vec::new()
        };
        self.survey = Some(survey);
        requests
    }

    /// What the launch does with what the core has: go on, show the form, or stop.
    fn decide(&mut self, survey: &Survey) -> Vec<Request> {
        let plan = launch::decide(&self.wanted, survey);
        let (profile, force) = match &plan {
            Plan::Refuse(why) => {
                self.stage = Stage::Blocked(why.clone());
                return Vec::new();
            }
            Plan::Apply { profile, force } => (profile, *force),
            Plan::Attach { .. } | Plan::Start { .. } => {
                let step = launch::step(&plan, &self.wanted).expect("a plan that goes on");
                return self.launch(step, None);
            }
        };
        let template = match &survey.template {
            Ok(template) => template,
            Err(why) => {
                self.stage = Stage::Blocked(why.clone());
                return Vec::new();
            }
        };
        let mut setup = Setup::new(template, &self.wanted, profile, force, &survey.modules);
        let straight = self.skip_form || self.settings.setup == SetupForm::Needed;
        match setup.step() {
            Ok(step) if straight && setup.error.is_none() => {
                self.launch(step, Some(Box::new(setup)))
            }
            Ok(_) => {
                self.stage = Stage::Setup(Box::new(setup));
                Vec::new()
            }
            Err(lacks) => {
                // Only said when the form was meant to be skipped: on a form it is plain.
                if straight {
                    setup.error = Some(lacks);
                }
                self.stage = Stage::Setup(Box::new(setup));
                Vec::new()
            }
        }
    }

    fn launch(&mut self, step: Step, back: Option<Box<Setup>>) -> Vec<Request> {
        let what = match &step {
            Step::Attach { execution } => format!("attaching to {execution}"),
            Step::Start { profile } => format!("starting {profile}"),
            Step::Apply { profile, .. } => format!("writing {profile} and starting it"),
        };
        self.stage = Stage::Launching { what, back };
        vec![Request::Launch {
            step,
            wanted: Box::new(self.wanted.clone()),
        }]
    }

    fn launched(&mut self, result: Result<Box<Session>, Failure>) -> Vec<Request> {
        let failure = match result {
            Ok(session) => return self.enter(*session),
            Err(failure) => failure,
        };
        let said = describe(&failure);
        match std::mem::replace(&mut self.stage, Stage::Opening) {
            Stage::Launching {
                back: Some(mut setup),
                ..
            } => {
                setup.error = Some(said);
                self.stage = Stage::Setup(setup);
            }
            Stage::Chat(mut chat) => {
                chat.switching = false;
                self.stage = Stage::Chat(chat);
                self.say(Tone::Error, said);
            }
            _ => self.stage = Stage::Blocked(said),
        }
        Vec::new()
    }

    /// A session to talk to: its conversation, what runs, its workspace and its workers.
    fn enter(&mut self, session: Session) -> Vec<Request> {
        let chat = Chat::new(session, self.now);
        let mut requests = vec![
            chat.history_request(self.options.tuning.event_page),
            Request::Core(core::Request::Status),
            chat.tree_request(),
        ];
        requests.extend(chat.inspect(Ask::summary()));
        requests.extend(chat.inspect(Ask::list("")));
        if let Some(first) = chat.notes.first() {
            self.notice = Some((Tone::Info, first.clone()));
        }
        self.stage = Stage::Chat(Box::new(chat));
        self.overlay = None;
        requests
    }

    fn history(
        &mut self,
        execution: &str,
        node: &str,
        since: u64,
        result: Result<Value, Failure>,
    ) -> Vec<Request> {
        let keep = self.options.tuning.conversation_keep;
        let limit = self.options.tuning.event_page;
        let now = self.now;
        let may_ask = self.overlay.is_none();
        let Some(chat) = self.chat_mut() else {
            return Vec::new();
        };
        if chat.execution() != execution || chat.session.agent != node {
            return Vec::new();
        }
        chat.history_asked = None;
        let page = match result {
            Ok(page) => page,
            Err(failure) => {
                if chat.log.error.as_ref() != Some(&failure) {
                    chat.version += 1;
                }
                chat.log.error = Some(failure);
                return Vec::new();
            }
        };
        let before = (
            chat.log.next,
            chat.log.queued.len(),
            chat.log.session.clone(),
        );
        let more = chat.log.page(since, &page, keep);
        if before
            != (
                chat.log.next,
                chat.log.queued.len(),
                chat.log.session.clone(),
            )
        {
            chat.version += 1;
        }
        let notice = chat.absorb(now, may_ask && chat.focus == Focus::Composer);
        // A page that brought something may have more behind it: ask at once.
        let requests = if more {
            chat.history_asked = Some(now);
            vec![chat.history_request(limit)]
        } else {
            Vec::new()
        };
        if notice.is_some() {
            self.notice = notice;
        }
        requests
    }

    fn inspected(&mut self, execution: &str, node: &str, ask: Ask, result: Result<Value, Failure>) {
        let Some(chat) = self.chat_mut() else {
            return;
        };
        if chat.execution() != execution || chat.session.workspace.as_deref() != Some(node) {
            return;
        }
        let files = &mut chat.side.files;
        match ask.action {
            Action::Summary => chat.side.summary = Some(result.map(|v| inspect::summary(&v))),
            Action::Diff => chat.side.diff = Some(result.map(|v| inspect::file(&v))),
            Action::List => {
                if ask.path.as_deref().unwrap_or_default() == files.path {
                    let listing = result.map(|v| inspect::listing(&v));
                    let len = listing.as_ref().map_or(0, |l| l.entries.len());
                    files.index = step(files.index, len, 0);
                    files.listing = Some(listing);
                }
            }
            Action::Read => {
                if let Some(viewer) = files
                    .viewer
                    .as_mut()
                    .filter(|viewer| Some(&viewer.path) == ask.path.as_ref())
                {
                    viewer.text = Some(result.map(|v| inspect::file(&v)));
                    if let Some(line) = viewer.line.take() {
                        viewer.scroll = line.saturating_sub(1 + HIT_CONTEXT) as usize;
                    }
                }
            }
            Action::Search => {
                if let Some(search) = files
                    .search
                    .as_mut()
                    .filter(|search| Some(&search.query) == ask.query.as_ref())
                {
                    let found = result.map(|v| inspect::found(&v));
                    let len = found.as_ref().map_or(0, |f| f.matches.len());
                    search.index = step(search.index, len, 0);
                    search.found = Some(found);
                }
            }
        }
    }

    // ── The clock ─────────────────────────────────────────────────────────────────────

    /// What to keep fresh now: the conversation (often while a turn is in flight), what
    /// runs, the decisions while a permission waits, and the workspace when it changed.
    pub fn tick(&mut self, now: u128) -> Vec<Request> {
        self.now = now;
        let refresh = self.refresh_ms;
        let busy_every = self.options.tuning.code_busy_refresh_ms as u128;
        let limit = self.options.tuning.event_page;
        let inspect_every = u128::from(self.settings.inspect_refresh_ms);
        let Some(chat) = self.chat_mut() else {
            return Vec::new();
        };
        let mut requests = Vec::new();
        let every = if chat.busy() {
            busy_every.min(refresh)
        } else {
            refresh
        };
        // An answer that never came is not waited for longer than this.
        let patience = (refresh * 10).max(5000);
        let waiting = chat.history_asked.is_some_and(|at| now < at + patience);
        if !waiting && now >= chat.read_at + every {
            requests.push(chat.history_request(limit));
            chat.history_asked = Some(now);
            chat.read_at = now;
        }
        if now >= chat.status_at + refresh {
            requests.push(Request::Core(core::Request::Status));
            if !chat.open_requests().is_empty() {
                requests.push(Request::Core(core::Request::AllDecisions));
            }
            requests.push(chat.tree_request());
            chat.status_at = now;
        }
        let due = inspect_every > 0 && now >= chat.inspected_at + inspect_every;
        if chat.stale || due {
            requests.extend(chat.refresh());
            chat.inspected_at = now;
            chat.stale = false;
        }
        requests
    }

    // ── Keys ──────────────────────────────────────────────────────────────────────────

    pub fn key(&mut self, key: KeyEvent) -> Vec<Request> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        self.notice = None;
        if let Some(overlay) = self.overlay.take() {
            return self.overlay_key(overlay, key);
        }
        let keys = self.options.keys.clone();
        match &self.stage {
            Stage::Chat(_) => self.chat_key(key),
            Stage::Setup(_) => self.setup_key(key),
            Stage::Blocked(_) => {
                if keys.code_quit.matches(&key) || keys.back.matches(&key) {
                    self.leave = Some(Leave::Quiet);
                    Vec::new()
                } else if keys.code_refresh.matches(&key) || keys.open.matches(&key) {
                    self.open()
                } else {
                    Vec::new()
                }
            }
            Stage::Opening | Stage::Launching { .. } => {
                if keys.code_quit.matches(&key) {
                    self.leave = Some(Leave::Quiet);
                }
                Vec::new()
            }
        }
    }

    /// Text that arrives at once goes where typing goes, and never answers anything.
    pub fn paste(&mut self, text: &str) {
        if self.overlay.is_some() {
            return;
        }
        let keys = self.options.keys.clone();
        match &mut self.stage {
            Stage::Setup(setup) => {
                for c in text.chars().filter(|c| !c.is_control()) {
                    let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
                    setup.form.key(key, &keys);
                }
            }
            Stage::Chat(chat) if chat.asking.is_none() => {
                let search = chat.side.files.search.as_mut().filter(|s| s.typing);
                match (chat.focus, search) {
                    (Focus::Side, Some(search)) => search
                        .query
                        .extend(text.chars().filter(|c| !c.is_control())),
                    (Focus::Composer, _) => chat.composer.insert_str(text),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn setup_key(&mut self, key: KeyEvent) -> Vec<Request> {
        let keys = self.options.keys.clone();
        if keys.code_quit.matches(&key) {
            self.leave = Some(Leave::Quiet);
            return Vec::new();
        }
        let Stage::Setup(setup) = &mut self.stage else {
            return Vec::new();
        };
        match setup.form.key(key, &keys) {
            FormKey::Edited => Vec::new(),
            FormKey::Leave => {
                self.leave = Some(Leave::Quiet);
                Vec::new()
            }
            FormKey::Submit => match setup.step() {
                Ok(step) => {
                    setup.error = None;
                    let Stage::Setup(setup) = std::mem::replace(&mut self.stage, Stage::Opening)
                    else {
                        return Vec::new();
                    };
                    self.launch(step, Some(setup))
                }
                Err(lacks) => {
                    setup.error = Some(lacks);
                    Vec::new()
                }
            },
        }
    }

    fn overlay_key(&mut self, overlay: Overlay, key: KeyEvent) -> Vec<Request> {
        let keys = self.options.keys.clone();
        match overlay {
            // Any key closes the help.
            Overlay::Help => Vec::new(),
            Overlay::ConfirmStop => {
                if keys.yes.matches(&key) {
                    return self.stop();
                }
                Vec::new()
            }
            Overlay::ConfirmLeave => {
                if keys.yes.matches(&key) {
                    self.leaving = true;
                    return self.stop();
                }
                if keys.no.matches(&key) {
                    if let Some(chat) = self.chat() {
                        self.leave = Some(Leave::Kept(chat.execution().to_string()));
                    }
                }
                Vec::new()
            }
            Overlay::Sessions(index) => {
                let rows = self.pick_rows();
                if keys.up.matches(&key) || keys.down.matches(&key) {
                    let by = if keys.up.matches(&key) { -1 } else { 1 };
                    self.overlay = Some(Overlay::Sessions(step(index, rows.len(), by)));
                    return Vec::new();
                }
                if !keys.open.matches(&key) {
                    if !keys.back.matches(&key) && !keys.code_sessions.matches(&key) {
                        self.overlay = Some(Overlay::Sessions(index));
                    }
                    return Vec::new();
                }
                match rows.get(index) {
                    Some(PickRow::New(profile)) => {
                        let profile = profile.clone();
                        self.switch(
                            Step::Start {
                                profile: profile.clone(),
                            },
                            format!(
                                "starting a new session of {profile}: the model starts \
                                 without this one's context"
                            ),
                        )
                    }
                    Some(PickRow::Session(candidate)) => {
                        let execution = candidate.execution.id.clone();
                        if self.chat().is_some_and(|c| c.execution() == execution) {
                            return Vec::new();
                        }
                        self.switch(
                            Step::Attach {
                                execution: execution.clone(),
                            },
                            format!("attaching to {execution}"),
                        )
                    }
                    None => Vec::new(),
                }
            }
        }
    }

    /// Goes to another session from this one; this one stays on screen until it is there.
    fn switch(&mut self, step: Step, said: String) -> Vec<Request> {
        // The agent and the workspace asked for were this workflow's.
        let wanted = Wanted {
            agent: None,
            workspace: None,
            ..self.wanted.clone()
        };
        if let Some(chat) = self.chat_mut() {
            chat.switching = true;
        }
        self.say(Tone::Info, said);
        vec![Request::Launch {
            step,
            wanted: Box::new(wanted),
        }]
    }

    fn stop(&mut self) -> Vec<Request> {
        let Some(chat) = self.chat() else {
            return Vec::new();
        };
        let execution = chat.execution().to_string();
        self.say(Tone::Info, format!("stopping {execution}"));
        vec![Request::Core(core::Request::Stop { execution })]
    }

    fn cancel(&mut self) -> Vec<Request> {
        let Some(chat) = self.chat() else {
            return Vec::new();
        };
        if !chat.busy() {
            self.say(Tone::Info, "no turn is in flight");
            return Vec::new();
        }
        vec![Request::Core(core::Request::AgentCancel {
            execution: chat.execution().to_string(),
            node: chat.session.agent.clone(),
        })]
    }

    /// Clears what is typed; with nothing typed, leaves as `[client.code] on_exit` says.
    fn quit(&mut self) -> Vec<Request> {
        let on_exit = self.settings.on_exit;
        let leaving = self.leaving;
        let Some(chat) = self.chat_mut() else {
            self.leave = Some(Leave::Quiet);
            return Vec::new();
        };
        if !chat.composer.is_empty() {
            chat.composer.clear();
            self.say(Tone::Info, "cleared; again to leave");
            return Vec::new();
        }
        let execution = chat.execution().to_string();
        if !chat.session.execution.state.is_live() {
            self.leave = Some(Leave::Quiet);
            return Vec::new();
        }
        match on_exit {
            // A stop that does not answer is not waited for twice.
            OnExit::Stop if !leaving => {
                self.leaving = true;
                return self.stop();
            }
            OnExit::Ask if !leaving => self.overlay = Some(Overlay::ConfirmLeave),
            _ => self.leave = Some(Leave::Kept(execution)),
        }
        Vec::new()
    }

    fn open_sessions(&mut self) -> Vec<Request> {
        self.overlay = Some(Overlay::Sessions(0));
        vec![Request::Survey(Box::new(self.wanted.clone()))]
    }

    fn confirm_stop(&mut self) {
        match self.chat() {
            Some(chat) if chat.session.execution.state.is_live() => {
                self.overlay = Some(Overlay::ConfirmStop)
            }
            Some(chat) => {
                let said = format!("execution {} is not running", chat.execution());
                self.say(Tone::Info, said);
            }
            None => {}
        }
    }

    /// Gives the keys to the oldest permission request, also one put off before.
    fn ask(&mut self) {
        let now = self.now;
        let Some(chat) = self.chat_mut() else {
            return;
        };
        match chat.open_requests().first() {
            Some(request) => {
                chat.settled.remove(&request.id);
                chat.asking = Some(Asking {
                    request: request.id.clone(),
                    index: 0,
                    since: now,
                });
            }
            None => self.say(Tone::Info, "no permission request waits"),
        }
    }

    fn chat_key(&mut self, key: KeyEvent) -> Vec<Request> {
        let keys = self.options.keys.clone();
        let page = self.page.max(1) as isize;
        if self.chat().is_some_and(|chat| chat.asking.is_some()) {
            return self.asking_key(key);
        }
        if keys.code_quit.matches(&key) {
            return self.quit();
        }
        if keys.cancel_turn.matches(&key) {
            return self.cancel();
        }
        if keys.code_help.matches(&key) {
            self.overlay = Some(Overlay::Help);
            return Vec::new();
        }
        if keys.code_sessions.matches(&key) {
            return self.open_sessions();
        }
        if keys.code_stop.matches(&key) {
            self.confirm_stop();
            return Vec::new();
        }
        if keys.code_permission.matches(&key) {
            self.ask();
            return Vec::new();
        }
        let Some(chat) = self.chat_mut() else {
            return Vec::new();
        };
        if keys.code_side.matches(&key) {
            chat.side.shown = !chat.side.shown;
            if !chat.side.shown {
                chat.focus = Focus::Composer;
                return Vec::new();
            }
            return chat.refresh();
        }
        if keys.code_focus.matches(&key) {
            chat.focus = match chat.focus {
                Focus::Composer => Focus::Side,
                Focus::Side => Focus::Composer,
            };
            if chat.focus == Focus::Side && !chat.side.shown {
                chat.side.shown = true;
                return chat.refresh();
            }
            return Vec::new();
        }
        let paging = if keys.code_page_up.matches(&key) {
            Some(-page)
        } else if keys.code_page_down.matches(&key) {
            Some(page)
        } else {
            None
        };
        match (chat.focus, paging) {
            // The transcript counts back from the newest: up is further back.
            (Focus::Composer, Some(by)) => {
                chat.scroll = chat.scroll.saturating_add_signed(-by);
                Vec::new()
            }
            (Focus::Side, Some(by)) => {
                self.side_move(by);
                Vec::new()
            }
            (Focus::Composer, None) => self.composer_key(key),
            (Focus::Side, None) => self.side_key(key),
        }
    }

    /// A permission request has the keys: a choice by its number, by the cursor, or yes
    /// and no; `back` puts it off.
    fn asking_key(&mut self, key: KeyEvent) -> Vec<Request> {
        let keys = self.options.keys.clone();
        let grace = self.options.tuning.code_permission_grace_ms as u128;
        let now = self.now;
        if keys.code_quit.matches(&key) {
            return self.quit();
        }
        if keys.cancel_turn.matches(&key) {
            return self.cancel();
        }
        let Some(chat) = self.chat_mut() else {
            return Vec::new();
        };
        let Some((request, choices, index)) = chat.asked() else {
            chat.asking = None;
            return Vec::new();
        };
        if keys.back.matches(&key) {
            chat.settled.insert(request.id.clone());
            chat.asking = None;
            let again = keys.code_permission.label();
            self.say(Tone::Info, format!("put off: {again} answers it"));
            return Vec::new();
        }
        // What was being typed when it appeared answers nothing.
        let since = chat.asking.as_ref().map_or(0, |asking| asking.since);
        if now < since + grace {
            return Vec::new();
        }
        if keys.up.matches(&key) || keys.down.matches(&key) {
            let by = if keys.up.matches(&key) { -1 } else { 1 };
            if let Some(asking) = &mut chat.asking {
                asking.index = step(index, choices.len(), by);
            }
            return Vec::new();
        }
        let picked = match key.code {
            _ if keys.open.matches(&key) => choices.get(index),
            KeyCode::Char(digit @ '1'..='9') => choices.get(digit as usize - '1' as usize),
            _ => None,
        };
        let answer = match picked {
            Some(Choice::Option(option)) => Some(Answer::Option(option.id.clone())),
            Some(Choice::Reject) => Some(Answer::Reject),
            None if keys.yes.matches(&key) => match answer_for("allow", &request.options) {
                Some(answer) => Some(answer),
                None => {
                    self.say(Tone::Info, "no option of this request allows: pick one");
                    return Vec::new();
                }
            },
            None if keys.no.matches(&key) => answer_for("reject", &request.options),
            None => None,
        };
        match answer {
            Some(answer) => self.answer(&request, answer),
            None => Vec::new(),
        }
    }

    /// Answers a request through the decision a Decider opened for it.
    fn answer(&mut self, request: &PermissionRequest, answer: Answer) -> Vec<Request> {
        let Some(chat) = self.chat_mut() else {
            return Vec::new();
        };
        let Some(id) = chat.decision_for(request) else {
            let said = match &chat.decisions {
                Some(Err(failure)) => format!(
                    "nobody can be answered from here: asking who waits said {}",
                    describe(failure)
                ),
                _ => format!(
                    "nobody was asked about {} yet: no decision waits for it (a Decider \
                     wired into the agent's `decisions` asks)",
                    request.id
                ),
            };
            self.say(Tone::Info, said);
            return vec![Request::Core(core::Request::AllDecisions)];
        };
        chat.settled.insert(request.id.clone());
        chat.asking = None;
        let option = match answer {
            Answer::Option(option) => Some(option),
            Answer::Reject => None,
        };
        vec![Request::Core(core::Request::Choose { id, option })]
    }

    /// A word typed after `/` that is not a command: an answer to the oldest request.
    fn answer_word(&mut self, word: &str, request: Option<&str>) -> Vec<Request> {
        let Some(chat) = self.chat() else {
            return Vec::new();
        };
        let open = chat.open_requests();
        let Some(asking) = open.iter().find(|r| request.is_none_or(|id| r.id == id)) else {
            let said = match request {
                Some(id) => format!("no open permission request `{id}`"),
                None => format!("unknown command `/{word}`: /help lists them"),
            };
            self.say(Tone::Error, said);
            return Vec::new();
        };
        match answer_for(word, &asking.options) {
            Some(answer) => {
                let asking = asking.clone();
                self.answer(&asking, answer)
            }
            None => {
                let ids: Vec<&str> = asking.options.iter().map(|o| o.id.as_str()).collect();
                let said = format!(
                    "`{word}` answers nothing of request {}: its options are {}, or allow / \
                     reject",
                    asking.id,
                    ids.join(", ")
                );
                self.say(Tone::Error, said);
                Vec::new()
            }
        }
    }

    fn composer_key(&mut self, key: KeyEvent) -> Vec<Request> {
        let keys = self.options.keys.clone();
        if keys.code_send.matches(&key) {
            return self.submit();
        }
        let Some(chat) = self.chat_mut() else {
            return Vec::new();
        };
        let composer = &mut chat.composer;
        if keys.code_newline.matches(&key) {
            composer.newline();
            return Vec::new();
        }
        if keys.back.matches(&key) {
            // Back to the newest of the transcript.
            chat.scroll = 0;
            return Vec::new();
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        match key.code {
            KeyCode::Char(c) if !ctrl && !alt => composer.insert(c),
            KeyCode::Char('a') if ctrl => composer.home(),
            KeyCode::Char('e') if ctrl => composer.end(),
            KeyCode::Char('u') if ctrl => composer.delete_to_home(),
            KeyCode::Char('w') if ctrl => composer.delete_word(),
            KeyCode::Char('b') if alt => composer.word_left(),
            KeyCode::Char('f') if alt => composer.word_right(),
            KeyCode::Backspace if alt => composer.delete_word(),
            KeyCode::Backspace => composer.backspace(),
            KeyCode::Delete => composer.delete(),
            KeyCode::Left if ctrl => composer.word_left(),
            KeyCode::Right if ctrl => composer.word_right(),
            KeyCode::Left => composer.left(),
            KeyCode::Right => composer.right(),
            KeyCode::Home => composer.home(),
            KeyCode::End => composer.end(),
            // Past the first and the last line: the prompts sent before.
            KeyCode::Up => {
                let _ = composer.up() || composer.older();
            }
            KeyCode::Down => {
                let _ = composer.down() || composer.newer();
            }
            _ => {}
        }
        Vec::new()
    }

    /// Enter: a line that starts with `/` is a command (`//x` sends `/x`); anything else
    /// goes to the agent.
    fn submit(&mut self) -> Vec<Request> {
        let Some(chat) = self.chat_mut() else {
            return Vec::new();
        };
        if chat.composer.is_blank() {
            return Vec::new();
        }
        let line = chat.composer.text().trim().to_string();
        if line.starts_with('/') && !line.starts_with("//") {
            chat.composer.take();
            return self.command(&line);
        }
        if let Some(why) = chat.unwritable() {
            self.say(Tone::Error, why);
            return Vec::new();
        }
        chat.composer.take();
        let text = match line.strip_prefix("//") {
            Some(rest) => format!("/{rest}"),
            None => line,
        };
        chat.log.queued.push((String::new(), text.clone()));
        chat.scroll = 0;
        chat.version += 1;
        vec![Request::Core(core::Request::AgentSend {
            execution: chat.execution().to_string(),
            node: chat.session.agent.clone(),
            text,
        })]
    }

    /// A line that starts with `/`: what the side panel shows, the session, and the
    /// commands of `regulus talk` (`/cancel`, `/quit`, an answer to a permission request).
    fn command(&mut self, line: &str) -> Vec<Request> {
        let rest = line.trim_start_matches('/');
        let (word, argument) = match rest.split_once(char::is_whitespace) {
            Some((word, argument)) => (word, argument.trim()),
            None => (rest, ""),
        };
        let needs = |what: &str| (Tone::Error, format!("/{word} needs {what}"));
        let Some(chat) = self.chat_mut() else {
            return Vec::new();
        };
        match word {
            "files" | "ls" => chat.open_folder(argument),
            "read" | "open" if argument.is_empty() => {
                self.notice = Some(needs("a file"));
                Vec::new()
            }
            "read" | "open" => chat.open_file(argument, None),
            "search" | "grep" if argument.is_empty() => {
                self.notice = Some(needs("what to look for"));
                Vec::new()
            }
            "search" | "grep" => chat.search(argument),
            "diff" | "changes" => chat.show(Tab::Changes),
            "workers" => chat.show(Tab::Workers),
            "activity" => chat.show(Tab::Activity),
            "sessions" => self.open_sessions(),
            "stop" => {
                self.confirm_stop();
                Vec::new()
            }
            _ => match talk::command(line) {
                talk::Command::Quit => self.quit(),
                talk::Command::Help => {
                    self.overlay = Some(Overlay::Help);
                    Vec::new()
                }
                // The whole conversation is on screen: go to where it starts.
                talk::Command::History => {
                    chat.scroll = usize::MAX;
                    Vec::new()
                }
                talk::Command::Cancel => self.cancel(),
                talk::Command::Answer(word, request) => self.answer_word(&word, request.as_deref()),
                talk::Command::Send(_) | talk::Command::Nothing => Vec::new(),
            },
        }
    }

    /// Moves in the side panel by `by` rows: the cursor of a list, the scroll of a text.
    fn side_move(&mut self, by: isize) {
        let Some(chat) = self.chat_mut() else {
            return;
        };
        let side = &mut chat.side;
        if side.tab != Tab::Files {
            side.scroll = side.scroll.saturating_add_signed(by);
            return;
        }
        let files = &mut side.files;
        if let Some(viewer) = &mut files.viewer {
            viewer.scroll = viewer.scroll.saturating_add_signed(by);
        } else if let Some(search) = &mut files.search {
            let len = match &search.found {
                Some(Ok(found)) => found.matches.len(),
                _ => 0,
            };
            search.index = step(search.index, len, by);
        } else {
            let len = match &files.listing {
                Some(Ok(listing)) => listing.entries.len(),
                _ => 0,
            };
            files.index = step(files.index, len, by);
        }
    }

    fn side_key(&mut self, key: KeyEvent) -> Vec<Request> {
        let keys = self.options.keys.clone();
        let Some(chat) = self.chat_mut() else {
            return Vec::new();
        };
        // A search being typed takes every key.
        let typing = chat.side.tab == Tab::Files
            && chat.side.files.search.as_ref().is_some_and(|s| s.typing);
        if typing {
            let Some(search) = chat.side.files.search.as_mut() else {
                return Vec::new();
            };
            match key.code {
                _ if keys.back.matches(&key) => chat.side.files.search = None,
                KeyCode::Enter if search.query.trim().is_empty() => chat.side.files.search = None,
                KeyCode::Enter => {
                    let query = search.query.trim().to_string();
                    return chat.search(&query);
                }
                KeyCode::Backspace => {
                    search.query.pop();
                }
                KeyCode::Char(c)
                    if !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                {
                    search.query.push(c)
                }
                _ => {}
            }
            return Vec::new();
        }
        if keys.code_next_tab.matches(&key) || keys.code_previous_tab.matches(&key) {
            let by = if keys.code_next_tab.matches(&key) {
                1
            } else {
                -1
            };
            let tab = chat.side.tab.step(by);
            return chat.show(tab);
        }
        if keys.code_refresh.matches(&key) {
            return chat.refresh();
        }
        if keys.up.matches(&key) || keys.down.matches(&key) {
            let by = if keys.up.matches(&key) { -1 } else { 1 };
            self.side_move(by);
            return Vec::new();
        }
        let files = &mut chat.side.files;
        let in_files = chat.side.tab == Tab::Files;
        // Out of what is open, one step at a time; from the list, back to typing.
        if keys.back.matches(&key) || (in_files && keys.code_parent.matches(&key)) {
            if in_files && files.viewer.take().is_some() {
                return Vec::new();
            }
            if in_files && files.search.take().is_some() {
                return Vec::new();
            }
            if keys.back.matches(&key) {
                chat.focus = Focus::Composer;
                return Vec::new();
            }
            if files.path.is_empty() {
                return Vec::new();
            }
            let above = inspect::parent(&files.path);
            return chat.open_folder(&above);
        }
        if !in_files || files.viewer.is_some() {
            return Vec::new();
        }
        if keys.code_search.matches(&key) {
            files.search = Some(Search {
                query: String::new(),
                typing: true,
                found: None,
                index: 0,
            });
            return Vec::new();
        }
        if !keys.open.matches(&key) {
            return Vec::new();
        }
        if let Some(search) = &files.search {
            let hit = match &search.found {
                Some(Ok(found)) => found.matches.get(search.index).cloned(),
                _ => None,
            };
            return match hit {
                Some(hit) => chat.open_file(&hit.path, Some(hit.line)),
                None => Vec::new(),
            };
        }
        let entry = match &files.listing {
            Some(Ok(listing)) => listing.entries.get(files.index).cloned(),
            _ => None,
        };
        match entry {
            Some(entry) if entry.directory => chat.open_folder(&entry.path),
            Some(entry) => chat.open_file(&entry.path, None),
            None => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code::fixtures::*;
    use crate::code::session::profile_id;
    use crate::rpc::CallError;
    use regulus_proto::code;
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn a_first_launch_shows_the_form_and_enter_writes_the_profile() {
        let mut app = app_for(wanted(), false);
        assert_eq!(app.open(), vec![Request::Survey(Box::new(wanted()))]);
        assert!(app
            .apply(Event::Surveyed(Ok(Box::new(survey()))))
            .is_empty());
        let id = profile_id(Path::new("/work/app"));
        let Stage::Setup(setup) = &app.stage else {
            panic!("{:?}", app.stage)
        };
        // What has no default is asked first, already filled with the folder.
        assert_eq!(setup.form.vars[0].name, "root");
        assert_eq!(setup.form.values[0], "/work/app");
        assert_eq!(setup.form.id, id);
        assert!(!setup.form.force);
        assert_eq!(setup.requires[0].1, RequireState::Missing);
        // Down to the model, and another one typed over the default.
        for _ in 0..3 {
            press(&mut app, KeyCode::Down);
        }
        for _ in 0.."qwen3:4b".len() {
            press(&mut app, KeyCode::Backspace);
        }
        typed(&mut app, "big");
        let launch = press(&mut app, KeyCode::Enter);
        let [Request::Launch {
            step:
                Step::Apply {
                    template,
                    vars,
                    profile,
                    force,
                },
            ..
        }] = launch.as_slice()
        else {
            panic!("{launch:?}")
        };
        assert_eq!(template, "regulus-code");
        assert_eq!(profile, &id);
        assert!(!force);
        // An optional var left empty is not sent: the core takes its default.
        assert_eq!(
            Value::Object(vars.clone()),
            json!({ "root": "/work/app", "model": "big", "preset": "ollama" })
        );
        assert!(matches!(&app.stage, Stage::Launching { back: Some(_), .. }));
        // The core refuses: back on the form, with what it said by its code.
        let unmet = CallError {
            code: Some(code::TEMPLATE_UNMET),
            message: "regulus/code is not installed".into(),
        };
        app.apply(Event::Launched(Err(unmet)));
        let Stage::Setup(setup) = &app.stage else {
            panic!("{:?}", app.stage)
        };
        let said = setup.error.clone().unwrap();
        assert!(said.contains("a required module is missing"), "{said}");
        assert_eq!(setup.form.values[3], "big", "what was typed is still there");
        // Again, and it goes through: the conversation, what runs, the workspace.
        press(&mut app, KeyCode::Enter);
        let first = app.apply(Event::Launched(Ok(Box::new(session(EXEC)))));
        assert!(matches!(
            &first[0],
            Request::Core(core::Request::AgentHistory { since: 0, node, .. }) if node == "agent"
        ));
        assert!(first.contains(&Request::Core(core::Request::Status)));
        assert!(first.contains(&Request::Tree {
            execution: EXEC.into()
        }));
        assert_eq!(asked(&first), vec![&Ask::summary(), &Ask::list("")]);
        assert!(app.chat().is_some());
    }

    #[test]
    fn flags_skip_the_form_and_what_cannot_be_is_said() {
        // --yes: straight to the launch.
        let mut app = app_for(wanted(), true);
        let launch = app.apply(Event::Surveyed(Ok(Box::new(survey()))));
        assert!(matches!(
            launch.as_slice(),
            [Request::Launch {
                step: Step::Apply { .. },
                ..
            }]
        ));
        // A credential that is not a secret's name stops on the form, never sent.
        let mut secret = wanted();
        secret.vars.insert("credential".into(), "sk-live-1".into());
        let mut app = app_for(secret, true);
        assert!(app
            .apply(Event::Surveyed(Ok(Box::new(survey()))))
            .is_empty());
        let Stage::Setup(setup) = &app.stage else {
            panic!("{:?}", app.stage)
        };
        assert!(setup.error.clone().unwrap().contains("secret:<name>"));
        // A var the template does not have is said, not dropped in silence.
        let mut unknown = wanted();
        unknown.vars.insert("colour".into(), "blue".into());
        let mut app = app_for(unknown, true);
        app.apply(Event::Surveyed(Ok(Box::new(survey()))));
        let Stage::Setup(setup) = &app.stage else {
            panic!("{:?}", app.stage)
        };
        assert!(setup.error.clone().unwrap().contains("no var colour"));
        // No template where it is looked for: why, and a key to try again.
        let mut app = app_for(wanted(), false);
        let missing = Survey {
            template: Err("no template `regulus-code` in /h/templates".into()),
            ..survey()
        };
        app.apply(Event::Surveyed(Ok(Box::new(missing))));
        assert_eq!(
            app.stage,
            Stage::Blocked("no template `regulus-code` in /h/templates".into())
        );
        assert_eq!(app.mood(), Mood::Failed);
        assert_eq!(
            press(&mut app, KeyCode::Char('r')),
            vec![Request::Survey(Box::new(wanted()))]
        );
        assert_eq!(app.stage, Stage::Opening);
        // A profile that is written is run as it is; setup flags without --reset stop.
        let id = profile_id(Path::new("/work/app"));
        let written = Survey {
            profiles: vec![profile(&id)],
            ..survey()
        };
        let mut app = app_for(wanted(), false);
        let run = app.apply(Event::Surveyed(Ok(Box::new(written.clone()))));
        assert!(
            matches!(run.as_slice(), [Request::Launch { step: Step::Start { profile }, .. }]
            if *profile == id)
        );
        let mut model = wanted();
        model.vars.insert("model".into(), "big".into());
        let mut app = app_for(model, false);
        app.apply(Event::Surveyed(Ok(Box::new(written))));
        assert!(matches!(&app.stage, Stage::Blocked(why) if why.contains("pass --reset")));
        // No core: what the line said, not a generic failure.
        let mut app = app_for(wanted(), false);
        app.apply(Event::Core(core::Event::Connection(Connection::Failed(
            "start one with: regulus serve".into(),
        ))));
        app.apply(Event::Surveyed(Err(CallError {
            code: None,
            message: "the core does not answer".into(),
        })));
        assert_eq!(
            app.stage,
            Stage::Blocked("start one with: regulus serve".into())
        );
    }

    #[test]
    fn a_prompt_is_queued_sent_and_read_back() {
        let mut app = chatting();
        let version = chat(&app).version;
        let sent = say(&mut app, "fix the test");
        assert_eq!(
            sent,
            vec![Request::Core(core::Request::AgentSend {
                execution: EXEC.into(),
                node: "agent".into(),
                text: "fix the test".into()
            })]
        );
        assert_eq!(
            chat(&app).log.queued,
            vec![(String::new(), "fix the test".into())]
        );
        assert!(chat(&app).version > version);
        assert!(chat(&app).composer.is_empty());
        assert!(chat(&app).busy());
        assert_eq!(app.mood(), Mood::Working);
        // The box answers with the turn, and the history is asked for at once.
        let read = app.apply(Event::Core(core::Event::AgentSent {
            execution: EXEC.into(),
            node: "agent".into(),
            text: "fix the test".into(),
            result: Ok("t-1".into()),
        }));
        assert!(matches!(
            read.as_slice(),
            [Request::Core(core::Request::AgentHistory { since: 0, .. })]
        ));
        // Its entry arrives: no longer queued, and still a turn in flight until answered,
        // whatever the engine says of itself.
        let more = app.apply(history(
            0,
            1,
            json!([entry(1, "user", "fix the test", "t-1")]),
            json!([]),
            "idle",
        ));
        assert!(matches!(
            more.as_slice(),
            [Request::Core(core::Request::AgentHistory { since: 1, .. })]
        ));
        assert!(chat(&app).log.queued.is_empty());
        assert!(chat(&app).busy());
        app.apply(history(
            1,
            2,
            json!([entry(2, "agent", "done", "t-1")]),
            json!([]),
            "idle",
        ));
        assert!(!chat(&app).busy());
        assert_eq!(app.mood(), Mood::Idle);
        // What was sent comes back with the up key.
        press(&mut app, KeyCode::Up);
        assert_eq!(chat(&app).composer.text(), "fix the test");
        ctrl(&mut app, 'c');
        // A send the core refuses puts the text back where it was typed.
        say(&mut app, "again");
        app.apply(Event::Core(core::Event::AgentSent {
            execution: EXEC.into(),
            node: "agent".into(),
            text: "again".into(),
            result: Err(CallError {
                code: Some(code::MODULE_UNAVAILABLE),
                message: "the agent is gone".into(),
            }),
        }));
        assert!(chat(&app).log.queued.is_empty());
        assert_eq!(chat(&app).composer.text(), "again");
        assert!(matches!(&app.notice, Some((Tone::Error, said)) if said.contains("unavailable")));
    }

    #[test]
    fn typing_edits_at_the_cursor_and_alt_enter_is_a_new_line() {
        let mut app = chatting();
        typed(&mut app, "one");
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        typed(&mut app, "two");
        assert_eq!(chat(&app).composer.text(), "one\ntwo");
        press(&mut app, KeyCode::Up);
        press(&mut app, KeyCode::Home);
        typed(&mut app, "> ");
        ctrl(&mut app, 'e');
        typed(&mut app, "!");
        assert_eq!(chat(&app).composer.text(), "> one!\ntwo");
        ctrl(&mut app, 'w');
        ctrl(&mut app, 'u');
        assert_eq!(chat(&app).composer.text(), "\ntwo");
        // A paste keeps its lines and sends nothing.
        ctrl(&mut app, 'c');
        app.paste("fn main() {\n\tprintln!();\n}");
        assert_eq!(
            chat(&app).composer.text(),
            "fn main() {\n    println!();\n}"
        );
        // Only spaces: nothing to send.
        ctrl(&mut app, 'c');
        assert!(say(&mut app, "   ").is_empty());
    }

    #[test]
    fn slash_commands_show_the_workspace_and_drive_the_session() {
        let mut app = chatting();
        let files = say(&mut app, "/files src/");
        assert_eq!(asked(&files), vec![&Ask::list("src")]);
        assert_eq!(chat(&app).side.tab, Tab::Files);
        assert_eq!(chat(&app).side.files.path, "src");
        let read = say(&mut app, "/read src/main.rs");
        assert_eq!(asked(&read), vec![&Ask::read("src/main.rs")]);
        assert!(chat(&app).side.files.viewer.is_some());
        // A search starts from the folder shown.
        let search = say(&mut app, "/search fn main");
        let scoped = Ask {
            path: Some("src".into()),
            ..Ask::search("fn main")
        };
        assert_eq!(asked(&search), vec![&scoped]);
        let diff = say(&mut app, "/diff");
        assert_eq!(asked(&diff), vec![&Ask::diff()]);
        assert_eq!(chat(&app).side.tab, Tab::Changes);
        assert_eq!(
            say(&mut app, "/workers"),
            vec![Request::Tree {
                execution: EXEC.into()
            }]
        );
        assert!(say(&mut app, "/read").is_empty());
        assert!(matches!(&app.notice, Some((Tone::Error, said)) if said == "/read needs a file"));
        // Not a command and nothing to answer: said, not sent to the agent.
        assert!(say(&mut app, "/nope").is_empty());
        assert!(
            matches!(&app.notice, Some((Tone::Error, said)) if said.contains("unknown command `/nope`"))
        );
        // Two slashes send one.
        assert_eq!(
            say(&mut app, "//etc/hosts"),
            vec![Request::Core(core::Request::AgentSend {
                execution: EXEC.into(),
                node: "agent".into(),
                text: "/etc/hosts".into()
            })]
        );
        say(&mut app, "/help");
        assert_eq!(app.overlay, Some(Overlay::Help));
        press(&mut app, KeyCode::Char('x'));
        assert_eq!(app.overlay, None);
        assert!(
            chat(&app).composer.is_empty(),
            "the key that closes is not typed"
        );
        say(&mut app, "/stop");
        assert_eq!(app.overlay, Some(Overlay::ConfirmStop));
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            vec![Request::Core(core::Request::Stop {
                execution: EXEC.into()
            })]
        );
        say(&mut app, "/quit");
        assert_eq!(app.leave, Some(Leave::Kept(EXEC.into())));
    }

    #[test]
    fn a_permission_request_takes_the_keys_and_is_answered_through_its_decision() {
        let mut app = chatting();
        app.tick(1000);
        app.apply(history(
            0,
            2,
            json!([entry(1, "user", "edit it", "t-1")]),
            json!([asks(2, "p1")]),
            "busy",
        ));
        assert_eq!(app.mood(), Mood::Asking);
        let (request, choices, index) = chat(&app).asked().expect("it has the keys");
        assert_eq!((request.id.as_str(), choices.len(), index), ("p1", 3, 0));
        // What was being typed when it appeared answers nothing.
        app.tick(1200);
        assert!(press(&mut app, KeyCode::Char('y')).is_empty());
        assert!(chat(&app).asking.is_some());
        // Nobody opened a decision for it yet: said, and asked for again.
        app.tick(1500);
        assert_eq!(
            press(&mut app, KeyCode::Char('y')),
            vec![Request::Core(core::Request::AllDecisions)]
        );
        assert!(
            matches!(&app.notice, Some((_, said)) if said.contains("nobody was asked about p1"))
        );
        // The Decider's row arrives, from whichever execution it lives in.
        app.apply(Event::Core(core::Event::AllDecisions(Ok(vec![
            decision("h-0", "p0"),
            decision("h-1", "p1"),
        ]))));
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Down);
        press(&mut app, KeyCode::Up);
        assert_eq!(chat(&app).asked().unwrap().2, 1);
        assert_eq!(
            press(&mut app, KeyCode::Enter),
            vec![Request::Core(core::Request::Choose {
                id: "h-1".into(),
                option: Some("always".into())
            })]
        );
        assert!(chat(&app).asking.is_none());
        // Answered and not resolved yet: it does not take the keys again.
        app.apply(history(2, 2, json!([]), json!([]), "busy"));
        assert!(chat(&app).asking.is_none());
        let after = app.apply(Event::Core(core::Event::Decided(Ok(()))));
        assert!(after.contains(&Request::Core(core::Request::AllDecisions)));
        // The other ways to answer: a number, yes and no, and a slash command.
        for (key, option) in [('1', "yes"), ('3', "no"), ('y', "yes"), ('n', "no")] {
            let mut app = chatting();
            app.apply(history(0, 2, json!([]), json!([asks(2, "p1")]), "busy"));
            app.apply(Event::Core(core::Event::AllDecisions(Ok(vec![decision(
                "h-1", "p1",
            )]))));
            app.tick(1000);
            assert_eq!(
                press(&mut app, KeyCode::Char(key)),
                vec![Request::Core(core::Request::Choose {
                    id: "h-1".into(),
                    option: Some(option.into())
                })],
                "{key}"
            );
        }
        let mut app = chatting();
        typed(&mut app, "/al");
        app.apply(history(0, 2, json!([]), json!([asks(2, "p1")]), "busy"));
        // Something is being typed: the request waits, the keys stay.
        assert!(chat(&app).asking.is_none());
        app.apply(Event::Core(core::Event::AllDecisions(Ok(vec![decision(
            "h-1", "p1",
        )]))));
        assert_eq!(
            say(&mut app, "ways"),
            vec![Request::Core(core::Request::Choose {
                id: "h-1".into(),
                option: Some("always".into())
            })]
        );
    }

    #[test]
    fn a_request_put_off_comes_back_when_asked_for() {
        let mut app = chatting();
        app.apply(history(0, 2, json!([]), json!([asks(2, "p1")]), "busy"));
        app.tick(1000);
        press(&mut app, KeyCode::Esc);
        assert!(chat(&app).asking.is_none());
        assert!(matches!(&app.notice, Some((_, said)) if said.contains("Ctrl+P answers it")));
        // Put off: the next page does not bring it back on its own, and typing works.
        app.apply(history(2, 2, json!([]), json!([]), "busy"));
        assert!(chat(&app).asking.is_none());
        typed(&mut app, "y");
        assert_eq!(chat(&app).composer.text(), "y");
        ctrl(&mut app, 'p');
        assert_eq!(chat(&app).asking.as_ref().unwrap().request, "p1");
        // A paste never answers.
        app.tick(5000);
        app.paste("y");
        assert!(chat(&app).asking.is_some());
        // Resolved elsewhere: the bar goes.
        app.apply(history(
            2,
            3,
            json!([]),
            json!([activity(
                3,
                "t-1",
                json!({ "kind": "permission_resolved", "id": "p1", "option": "no" })
            )]),
            "busy",
        ));
        assert!(chat(&app).asking.is_none());
        ctrl(&mut app, 'p');
        assert!(matches!(&app.notice, Some((_, said)) if said == "no permission request waits"));
        // Without the module that asks people, it says so by the code.
        let mut app = chatting();
        app.apply(history(0, 2, json!([]), json!([asks(2, "p1")]), "busy"));
        app.apply(Event::Core(core::Event::AllDecisions(Err(CallError {
            code: Some(core::METHOD_NOT_FOUND),
            message: "method not found".into(),
        }))));
        app.tick(1000);
        press(&mut app, KeyCode::Char('y'));
        assert!(matches!(&app.notice, Some((_, said)) if said.contains("□ not installed")));
    }

    #[test]
    fn the_clock_reads_often_only_while_a_turn_is_in_flight() {
        let mut app = chatting();
        let history_asked = |requests: &[Request]| {
            requests
                .iter()
                .any(|r| matches!(r, Request::Core(core::Request::AgentHistory { .. })))
        };
        // Idle: everything at `client.refresh_ms`.
        assert!(app.tick(500).is_empty());
        let due = app.tick(1000);
        assert!(history_asked(&due));
        assert!(due.contains(&Request::Core(core::Request::Status)));
        assert!(due.contains(&Request::Tree {
            execution: EXEC.into()
        }));
        assert!(!due.contains(&Request::Core(core::Request::AllDecisions)));
        // Asked and not answered: not asked again on every tick.
        assert!(!history_asked(&app.tick(2100)));
        app.apply(history(0, 0, json!([]), json!([]), "idle"));
        // A turn in flight: the conversation at the busy pace, the rest as before.
        say(&mut app, "go");
        app.apply(history(
            0,
            2,
            json!([entry(1, "user", "go", "t-1")]),
            json!([asks(2, "p1")]),
            "busy",
        ));
        app.apply(history(2, 2, json!([]), json!([]), "busy"));
        let soon = app.tick(2400);
        assert!(history_asked(&soon), "{soon:?}");
        assert!(!soon.contains(&Request::Core(core::Request::Status)));
        app.apply(history(2, 2, json!([]), json!([]), "busy"));
        // While a permission waits, who answers it is asked along with what runs.
        let due = app.tick(3200);
        assert!(due.contains(&Request::Core(core::Request::AllDecisions)));
        // An answer that never comes is waited for only so long: ten refreshes.
        assert!(!history_asked(&app.tick(4000)));
        assert!(!history_asked(&app.tick(13_000)));
        assert!(history_asked(&app.tick(13_300)));
    }

    #[test]
    fn the_workspace_is_read_again_when_a_tool_or_a_turn_ends() {
        let mut app = chatting();
        app.settings.inspect_refresh_ms = 0;
        assert!(asked(&app.tick(100)).is_empty());
        app.apply(history(
            0,
            3,
            json!([entry(1, "user", "edit", "t-1")]),
            json!([
                activity(
                    2,
                    "t-1",
                    json!({ "kind": "tool_call", "id": "c1", "title": "write",
                    "status": "pending" })
                ),
                activity(
                    3,
                    "t-1",
                    json!({ "kind": "tool_update", "id": "c1",
                    "status": "completed" })
                )
            ]),
            "busy",
        ));
        // The summary always; the tab shown too (Activity has nothing of its own).
        assert_eq!(asked(&app.tick(200)), vec![&Ask::summary()]);
        assert!(asked(&app.tick(300)).is_empty());
        press(&mut app, KeyCode::Tab);
        press(&mut app, KeyCode::Right);
        press(&mut app, KeyCode::Right);
        assert_eq!(chat(&app).side.tab, Tab::Changes);
        app.apply(history(
            3,
            4,
            json!([entry(4, "agent", "done", "t-1")]),
            json!([]),
            "idle",
        ));
        assert_eq!(asked(&app.tick(400)), vec![&Ask::summary(), &Ask::diff()]);
        // And on its own clock, when `[client.code] inspect_refresh_ms` says so.
        app.settings.inspect_refresh_ms = 4000;
        assert!(asked(&app.tick(4300)).is_empty());
        assert_eq!(asked(&app.tick(4400)), vec![&Ask::summary(), &Ask::diff()]);
    }

    #[test]
    fn the_side_panel_browses_opens_and_searches() {
        let mut app = chatting();
        press(&mut app, KeyCode::Tab);
        assert_eq!(chat(&app).focus, Focus::Side);
        let list = press(&mut app, KeyCode::Right);
        assert_eq!(asked(&list), vec![&Ask::list("")]);
        app.apply(inspected(
            Ask::list(""),
            json!({ "path": "", "entries": [{ "path": "notes.md", "kind": "file" },
                { "path": "src", "kind": "directory" }] }),
        ));
        // Folders first: Enter goes into `src`.
        let into = press(&mut app, KeyCode::Enter);
        assert_eq!(asked(&into), vec![&Ask::list("src")]);
        // An answer for a folder no longer shown is dropped.
        app.apply(inspected(Ask::list(""), json!({ "entries": [] })));
        assert!(chat(&app).side.files.listing.is_none());
        app.apply(inspected(
            Ask::list("src"),
            json!({ "path": "src", "entries": [{ "path": "src/main.rs", "kind": "file" }] }),
        ));
        let open = press(&mut app, KeyCode::Enter);
        assert_eq!(asked(&open), vec![&Ask::read("src/main.rs")]);
        // Backspace: out of the file, then up a folder; at the root it stays.
        assert!(press(&mut app, KeyCode::Backspace).is_empty());
        assert!(chat(&app).side.files.viewer.is_none());
        let up = press(&mut app, KeyCode::Backspace);
        assert_eq!(asked(&up), vec![&Ask::list("")]);
        assert!(press(&mut app, KeyCode::Backspace).is_empty());
        // A search is typed in the panel, not in the box below.
        press(&mut app, KeyCode::Char('/'));
        typed(&mut app, "needle");
        assert!(chat(&app).composer.is_empty());
        let search = press(&mut app, KeyCode::Enter);
        assert_eq!(asked(&search), vec![&Ask::search("needle")]);
        app.apply(inspected(
            Ask::search("needle"),
            json!({ "matches": [{ "path": "notes.md", "line": 1, "text": "a needle" },
                { "path": "src/main.rs", "line": 40, "text": "// needle" }] }),
        ));
        press(&mut app, KeyCode::Down);
        let hit = press(&mut app, KeyCode::Enter);
        assert_eq!(asked(&hit), vec![&Ask::read("src/main.rs")]);
        app.apply(inspected(
            Ask::read("src/main.rs"),
            json!({ "path": "src/main.rs", "text": "…" }),
        ));
        // The hit is shown with a few lines above it.
        assert_eq!(chat(&app).side.files.viewer.as_ref().unwrap().scroll, 36);
        // Esc: out of the file, out of the results, back to typing.
        press(&mut app, KeyCode::Esc);
        assert!(chat(&app).side.files.search.is_some());
        press(&mut app, KeyCode::Esc);
        assert!(chat(&app).side.files.search.is_none());
        press(&mut app, KeyCode::Esc);
        assert_eq!(chat(&app).focus, Focus::Composer);
        // Hidden and shown again, it is read again.
        ctrl(&mut app, 'b');
        assert!(!chat(&app).side.shown);
        let shown = ctrl(&mut app, 'b');
        assert_eq!(asked(&shown), vec![&Ask::summary(), &Ask::list("")]);
    }

    #[test]
    fn leaving_does_what_on_exit_says() {
        // Something typed: the first press clears it.
        let mut app = chatting();
        typed(&mut app, "half a thought");
        ctrl(&mut app, 'c');
        assert_eq!(app.leave, None);
        assert!(chat(&app).composer.is_empty());
        ctrl(&mut app, 'c');
        assert_eq!(app.leave, Some(Leave::Kept(EXEC.into())));
        // `on_exit = "stop"`: it stops, then leaves once the core says so.
        let mut app = chatting();
        app.settings.on_exit = OnExit::Stop;
        assert_eq!(
            ctrl(&mut app, 'd'),
            vec![Request::Core(core::Request::Stop {
                execution: EXEC.into()
            })]
        );
        assert_eq!(app.leave, None);
        app.apply(Event::Core(core::Event::Stopped(Ok(execution(
            EXEC, "code-app", "stopped",
        )))));
        assert_eq!(app.leave, Some(Leave::Stopped(EXEC.into())));
        // A stop that does not answer is not waited for twice.
        let mut app = chatting();
        app.settings.on_exit = OnExit::Stop;
        ctrl(&mut app, 'c');
        ctrl(&mut app, 'c');
        assert_eq!(app.leave, Some(Leave::Kept(EXEC.into())));
        // `on_exit = "ask"`.
        let mut app = chatting();
        app.settings.on_exit = OnExit::Ask;
        ctrl(&mut app, 'c');
        assert_eq!(app.overlay, Some(Overlay::ConfirmLeave));
        press(&mut app, KeyCode::Char('n'));
        assert_eq!(app.leave, Some(Leave::Kept(EXEC.into())));
        let mut app = chatting();
        app.settings.on_exit = OnExit::Ask;
        ctrl(&mut app, 'c');
        assert_eq!(press(&mut app, KeyCode::Char('y')).len(), 1);
        // Esc stays.
        let mut app = chatting();
        app.settings.on_exit = OnExit::Ask;
        ctrl(&mut app, 'c');
        press(&mut app, KeyCode::Esc);
        assert_eq!((app.leave.clone(), app.overlay.clone()), (None, None));
    }

    #[test]
    fn a_session_that_is_not_running_is_read_not_written() {
        let mut app = chatting();
        app.apply(Event::Core(core::Event::Status {
            executions: vec![execution(EXEC, "code-app", "stopped")],
            modules: Vec::new(),
        }));
        assert!(matches!(&app.notice, Some((_, said)) if said == "execution exec-1 stopped"));
        assert_eq!(app.mood(), Mood::Asleep);
        assert!(say(&mut app, "hello?").is_empty());
        assert!(
            matches!(&app.notice, Some((Tone::Error, said)) if said.contains("takes no prompts"))
        );
        assert_eq!(
            chat(&app).composer.text(),
            "hello?",
            "nothing typed is lost"
        );
        // Nothing to stop, and leaving has nothing to keep.
        ctrl(&mut app, 't');
        assert_eq!(app.overlay, None);
        ctrl(&mut app, 'c');
        ctrl(&mut app, 'c');
        assert_eq!(app.leave, Some(Leave::Quiet));
        // An agent left off by the execution says what it misses.
        let mut app = chatting();
        let mut off = execution(EXEC, "code-app", "running");
        off.disabled = vec![regulus_proto::DisabledNode {
            node_id: "agent".into(),
            missing: vec!["engine".into()],
        }];
        let agent: ModuleSummary = serde_json::from_value(json!({
            "id": "regulus/agent@0.1.0", "name": "agent", "node_types": [],
            "state": { "status": "active", "since": 0 }
        }))
        .unwrap();
        app.apply(Event::Core(core::Event::Status {
            executions: vec![off],
            modules: vec![agent],
        }));
        assert!(say(&mut app, "hi").is_empty());
        assert!(
            matches!(&app.notice, Some((Tone::Error, said)) if said.contains("nothing usable is wired into engine"))
        );
    }

    #[test]
    fn another_session_is_attached_to_without_losing_this_one() {
        let mut app = chatting();
        assert_eq!(
            ctrl(&mut app, 'o'),
            vec![Request::Survey(Box::new(wanted()))]
        );
        assert_eq!(app.overlay, Some(Overlay::Sessions(0)));
        let listed = Survey {
            profiles: vec![profile("code-app")],
            executions: vec![
                execution(EXEC, "code-app", "running"),
                execution("exec-2", "code-app", "running"),
            ],
            ..survey()
        };
        // The list arrives while this conversation stays where it is.
        assert!(app.apply(Event::Surveyed(Ok(Box::new(listed)))).is_empty());
        assert!(app.chat().is_some());
        let rows = app.pick_rows();
        assert!(matches!(&rows[0], PickRow::New(profile) if profile == "code-app"));
        assert!(matches!(&rows[1], PickRow::Session(c) if c.execution.id == "exec-2"));
        assert!(matches!(&rows[2], PickRow::Session(c) if c.execution.id == EXEC));
        press(&mut app, KeyCode::Down);
        let attach = press(&mut app, KeyCode::Enter);
        assert!(matches!(
            attach.as_slice(),
            [Request::Launch { step: Step::Attach { execution }, .. }] if execution == "exec-2"
        ));
        assert!(chat(&app).switching);
        // It fails: said, and this one is still here.
        app.apply(Event::Launched(Err(CallError {
            code: None,
            message: "more than one agent".into(),
        })));
        assert_eq!(chat(&app).session.execution.id, EXEC);
        assert!(!chat(&app).switching);
        assert!(matches!(&app.notice, Some((Tone::Error, _))));
        // It works: the other conversation takes the screen.
        app.apply(Event::Launched(Ok(Box::new(session("exec-2")))));
        assert_eq!(chat(&app).session.execution.id, "exec-2");
        // The first row starts a new session of the same profile; this one's row does
        // nothing.
        ctrl(&mut app, 'o');
        let fresh = press(&mut app, KeyCode::Enter);
        assert!(matches!(
            fresh.as_slice(),
            [Request::Launch { step: Step::Start { profile }, .. }] if profile == "code-app"
        ));
        assert!(
            matches!(&app.notice, Some((_, said)) if said.contains("without this one's context"))
        );
    }

    #[test]
    fn a_new_engine_session_is_said_because_the_context_went_with_the_old_one() {
        let mut app = chatting();
        app.apply(history(
            0,
            1,
            json!([entry(1, "user", "hi", "t-1")]),
            json!([]),
            "idle",
        ));
        // The same session page after page says nothing.
        assert!(!matches!(&app.notice, Some((_, said)) if said.contains("new session (")));
        app.apply(Event::Core(core::Event::AgentHistory {
            execution: EXEC.into(),
            node: "agent".into(),
            since: 1,
            result: Ok(json!({ "entries": [], "activity": [], "next": 1,
                "session": { "engine_session_id": "native-2", "state": "idle" } })),
        }));
        assert!(matches!(&app.notice, Some((_, said))
            if said.contains("new session (native-2)") && said.contains("no longer has")));
        // An answer about another conversation changes nothing here.
        let before = chat(&app).version;
        app.apply(Event::Core(core::Event::AgentHistory {
            execution: "exec-7".into(),
            node: "agent".into(),
            since: 0,
            result: Ok(json!({ "entries": [entry(9, "user", "x", "t-9")], "next": 9 })),
        }));
        assert_eq!(chat(&app).version, before);
        // What cannot be read is kept to show, by its code.
        app.apply(Event::Core(core::Event::AgentHistory {
            execution: EXEC.into(),
            node: "agent".into(),
            since: 1,
            result: Err(CallError {
                code: Some(core::METHOD_NOT_FOUND),
                message: "method not found".into(),
            }),
        }));
        assert!(chat(&app).log.error.is_some());
    }

    #[test]
    fn scrolling_stays_inside_what_the_screen_measured() {
        let mut app = chatting();
        app.settle(40, 0, 10);
        press(&mut app, KeyCode::PageUp);
        assert_eq!(chat(&app).scroll, 10);
        for _ in 0..9 {
            press(&mut app, KeyCode::PageUp);
        }
        app.settle(40, 0, 10);
        assert_eq!(chat(&app).scroll, 40);
        press(&mut app, KeyCode::PageDown);
        assert_eq!(chat(&app).scroll, 30);
        // Esc goes back to the newest; so does sending.
        press(&mut app, KeyCode::Esc);
        assert_eq!(chat(&app).scroll, 0);
        say(&mut app, "/history");
        app.settle(40, 0, 10);
        assert_eq!(chat(&app).scroll, 40);
        say(&mut app, "go on");
        assert_eq!(chat(&app).scroll, 0);
    }
}
