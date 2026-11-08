//! The Edit view's state: a profile edited in memory with [`document`] and saved with
//! `profile.save`, as the app's canvas does (docs/04-client.md §9). Keys and core events
//! go in; requests come out. No drawing and no I/O here.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use regulus_proto::{
    Diagnostic, Direction, Manifest, ModuleSummary, Node, PortRef, Profile, ProfileSaved, Severity,
    Wire, Workflow,
};
use serde_json::Value;

use super::app::Tone;
use super::catalog::{self, Control, FieldRow, PaletteEntry};
use super::core::{describe, Failure, Request};
use super::document as doc;
use super::keys::{Keys, Tuning};

/// What the editor reads besides its profile.
pub struct Ctx<'a> {
    pub keys: &'a Keys,
    pub tuning: &'a Tuning,
    pub manifests: &'a [Manifest],
    pub modules: &'a [ModuleSummary],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Tree,
    Inspector,
    Wires,
}

const PANES: [Pane; 3] = [Pane::Tree, Pane::Inspector, Pane::Wires];

/// A row of the tree: a workflow, or a node of one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeRow {
    Workflow(String),
    Node(String, String),
}

/// A row of the inspector.
#[derive(Debug, Clone, PartialEq)]
pub enum InspectorRow {
    Name,
    Field(FieldRow),
    /// The workflow inside a node.
    Internal(Option<String>),
    /// A port of the workflow itself (`self`).
    Port(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextFor {
    RenameNode {
        workflow: String,
        node: String,
    },
    RenameWorkflow(String),
    NewWorkflow,
    Field {
        workflow: String,
        node: String,
        field: String,
    },
    NewPort(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChooseFor {
    Field {
        workflow: String,
        node: String,
        field: String,
    },
    Internal {
        workflow: String,
        node: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmFor {
    DeleteNode { workflow: String, node: String },
    DeleteWorkflow(String),
    Leave,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Modal {
    Palette {
        query: String,
        index: usize,
    },
    Text {
        purpose: TextFor,
        input: String,
    },
    Choose {
        purpose: ChooseFor,
        options: Vec<(String, Option<Value>)>,
        index: usize,
    },
    /// Picking a wire: first where it leaves from, then where it goes.
    Connect {
        from: Option<PortRef>,
        index: usize,
    },
    Confirm(ConfirmFor),
}

/// What a key or an event did, for the screen around the editor.
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub requests: Vec<Request>,
    pub notice: Option<(Tone, String)>,
    /// Back to Operate.
    pub leave: bool,
}

impl Step {
    pub fn notice(tone: Tone, text: impl Into<String>) -> Self {
        Self {
            notice: Some((tone, text.into())),
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Editor {
    pub doc: Profile,
    /// What the core has; `None` until a new profile is first saved.
    pub saved: Option<Profile>,
    pub pane: Pane,
    pub row: usize,
    pub field: usize,
    pub wire: usize,
    pub modal: Option<Modal>,
    /// What the last save or check found.
    pub diagnostics: Vec<Diagnostic>,
}

/// A connect target and whether it may take the wire.
pub type Target = (
    PortRef,
    regulus_proto::Port,
    Result<regulus_proto::WireKind, doc::Refusal>,
);

fn step_in(index: usize, len: usize, by: isize) -> usize {
    if len == 0 {
        return 0;
    }
    index.saturating_add_signed(by).min(len - 1)
}

fn typed(key: &KeyEvent) -> Option<char> {
    match key.code {
        KeyCode::Char(c)
            if !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
        {
            Some(c)
        }
        _ => None,
    }
}

pub fn end_name(end: &PortRef) -> String {
    format!("{}.{}", end.node_id, end.port_id)
}

impl Editor {
    /// Edits a profile the core has.
    pub fn open(profile: Profile) -> Self {
        Self::with(profile.clone(), Some(profile))
    }

    /// A profile that is not saved yet.
    pub fn new_profile(id: &str, name: &str) -> Self {
        Self::with(doc::new_profile(id, name, "Main"), None)
    }

    fn with(doc: Profile, saved: Option<Profile>) -> Self {
        Self {
            doc,
            saved,
            pane: Pane::Tree,
            row: 0,
            field: 0,
            wire: 0,
            modal: None,
            diagnostics: Vec::new(),
        }
    }

    pub fn dirty(&self) -> bool {
        self.saved.as_ref() != Some(&self.doc)
    }

    pub fn tree(&self) -> Vec<TreeRow> {
        let mut rows = Vec::new();
        for workflow in &self.doc.workflows {
            rows.push(TreeRow::Workflow(workflow.id.clone()));
            for node in &workflow.nodes {
                rows.push(TreeRow::Node(workflow.id.clone(), node.id.clone()));
            }
        }
        rows
    }

    fn selected_row(&self) -> Option<TreeRow> {
        self.tree().into_iter().nth(self.row)
    }

    /// The workflow the selection is in.
    pub fn workflow_id(&self) -> String {
        match self.selected_row() {
            Some(TreeRow::Workflow(id) | TreeRow::Node(id, _)) => id,
            None => self.doc.main_workflow_id.clone(),
        }
    }

    pub fn workflow(&self) -> Option<&Workflow> {
        self.doc.workflow(&self.workflow_id())
    }

    pub fn node(&self) -> Option<&Node> {
        match self.selected_row()? {
            TreeRow::Node(workflow, node) => self.doc.workflow(&workflow)?.node(&node),
            TreeRow::Workflow(_) => None,
        }
    }

    pub fn wires(&self) -> &[Wire] {
        self.workflow().map_or(&[], |w| w.wires.as_slice())
    }

    fn select(&mut self, wanted: &TreeRow) {
        if let Some(at) = self.tree().iter().position(|row| row == wanted) {
            self.row = at;
        }
        self.field = 0;
        self.wire = 0;
    }

    fn workflow_ids(&self) -> Vec<String> {
        self.doc.workflows.iter().map(|w| w.id.clone()).collect()
    }

    pub fn inspector(&self, ctx: &Ctx) -> Vec<InspectorRow> {
        let mut rows = vec![InspectorRow::Name];
        if let Some(node) = self.node() {
            let fields = catalog::type_of(ctx.manifests, node)
                .map_or(&[][..], |kind| kind.fields.as_slice());
            let workflows: Vec<String> = self
                .workflow_ids()
                .into_iter()
                .filter(|id| *id != self.workflow_id())
                .collect();
            rows.extend(
                catalog::field_rows(fields, &node.config, &workflows)
                    .into_iter()
                    .map(InspectorRow::Field),
            );
            rows.push(InspectorRow::Internal(node.internal_workflow_id.clone()));
        } else if let Some(workflow) = self.workflow() {
            rows.extend(
                workflow
                    .ports
                    .iter()
                    .map(|p| InspectorRow::Port(p.id.clone())),
            );
        }
        rows
    }

    pub fn palette(&self, ctx: &Ctx) -> Vec<PaletteEntry> {
        catalog::palette(ctx.manifests, ctx.modules, &self.doc, &self.workflow_id())
    }

    /// The choices of the connect dialog, at its current stage.
    pub fn sources(&self) -> Vec<(PortRef, regulus_proto::Port)> {
        doc::ends(&self.doc, &self.workflow_id(), Direction::Output)
    }

    pub fn targets(&self, from: &PortRef) -> Vec<Target> {
        let workflow = self.workflow_id();
        doc::ends(&self.doc, &workflow, Direction::Input)
            .into_iter()
            .map(|(to, port)| {
                let verdict = doc::can_connect(&self.doc, &workflow, from, &to);
                (to, port, verdict)
            })
            .collect()
    }

    pub fn key(&mut self, key: KeyEvent, ctx: &Ctx) -> Step {
        if let Some(modal) = self.modal.take() {
            return self.modal_key(modal, key, ctx);
        }
        let keys = ctx.keys;
        if keys.back.matches(&key) {
            if self.dirty() {
                self.modal = Some(Modal::Confirm(ConfirmFor::Leave));
                return Step::default();
            }
            return Step {
                leave: true,
                ..Step::default()
            };
        }
        if keys.save.matches(&key) {
            return Step {
                requests: vec![Request::Save {
                    profile: Box::new(self.doc.clone()),
                    force: self.saved.is_some(),
                }],
                notice: Some((Tone::Info, format!("saving {}…", self.doc.id))),
                leave: false,
            };
        }
        if keys.check.matches(&key) {
            if self.saved.is_none() {
                return Step::notice(Tone::Info, "save first: the check reads the saved profile");
            }
            return Step {
                requests: vec![Request::Check {
                    profile: self.doc.id.clone(),
                }],
                ..Step::default()
            };
        }
        if keys.next_pane.matches(&key) || keys.previous_pane.matches(&key) {
            let by = if keys.next_pane.matches(&key) { 1 } else { -1 };
            let at = PANES.iter().position(|p| *p == self.pane).unwrap_or(0) as isize;
            self.pane = PANES[(at + by).rem_euclid(PANES.len() as isize) as usize];
            return Step::default();
        }
        if keys.up.matches(&key) || keys.down.matches(&key) {
            let by = if keys.up.matches(&key) { -1 } else { 1 };
            self.move_selection(by, ctx);
            return Step::default();
        }
        if keys.add_node.matches(&key) {
            self.modal = Some(Modal::Palette {
                query: String::new(),
                index: 0,
            });
            return Step::default();
        }
        if keys.new_workflow.matches(&key) {
            self.modal = Some(Modal::Text {
                purpose: TextFor::NewWorkflow,
                input: String::new(),
            });
            return Step::default();
        }
        if keys.connect.matches(&key) {
            if self.sources().is_empty() {
                return Step::notice(Tone::Info, "nothing in this workflow sends a value yet");
            }
            self.modal = Some(Modal::Connect {
                from: None,
                index: 0,
            });
            return Step::default();
        }
        if keys.rename.matches(&key) {
            self.rename();
            return Step::default();
        }
        match self.pane {
            Pane::Inspector => {
                if keys.open.matches(&key) {
                    return self.open_row(ctx);
                }
                if keys.reset.matches(&key) {
                    return self.reset_row(ctx);
                }
                if keys.add_port.matches(&key) {
                    if self.node().is_some() {
                        return Step::notice(Tone::Info, "select a workflow to add its ports");
                    }
                    self.modal = Some(Modal::Text {
                        purpose: TextFor::NewPort(self.workflow_id()),
                        input: String::new(),
                    });
                    return Step::default();
                }
                if keys.remove.matches(&key) {
                    if let Some(InspectorRow::Port(port)) = self.inspector(ctx).get(self.field) {
                        let workflow = self.workflow_id();
                        doc::remove_workflow_port(&mut self.doc, &workflow, port);
                        self.field = self.field.saturating_sub(1);
                        return Step::notice(Tone::Info, format!("removed port self.{port}"));
                    }
                }
            }
            Pane::Tree => {
                if keys.delete.matches(&key) {
                    return self.ask_delete();
                }
            }
            Pane::Wires => {
                if keys.remove.matches(&key) {
                    if let Some(wire) = self.wires().get(self.wire).map(|w| w.id.clone()) {
                        let workflow = self.workflow_id();
                        doc::remove_wire(&mut self.doc, &workflow, &wire);
                        self.wire = step_in(self.wire, self.wires().len(), 0);
                        return Step::notice(Tone::Info, format!("removed wire {wire}"));
                    }
                }
            }
        }
        if keys.run.matches(&key) {
            return self.run();
        }
        Step::default()
    }

    fn run(&self) -> Step {
        if self.saved.is_none() {
            return Step::notice(Tone::Info, "save the profile before running it");
        }
        let mut step = Step {
            requests: vec![Request::Start {
                profile: self.doc.id.clone(),
                workflow: Some(self.workflow_id()),
            }],
            ..Step::default()
        };
        if self.dirty() {
            step.notice = Some((
                Tone::Info,
                "runs the saved profile: unsaved changes are not in it".into(),
            ));
        }
        step
    }

    fn move_selection(&mut self, by: isize, ctx: &Ctx) {
        match self.pane {
            Pane::Tree => {
                let before = self.row;
                self.row = step_in(self.row, self.tree().len(), by);
                if before != self.row {
                    self.field = 0;
                    self.wire = 0;
                }
            }
            Pane::Inspector => self.field = step_in(self.field, self.inspector(ctx).len(), by),
            Pane::Wires => self.wire = step_in(self.wire, self.wires().len(), by),
        }
    }

    fn rename(&mut self) {
        let (purpose, input) = match self.selected_row() {
            Some(TreeRow::Node(workflow, node)) => {
                let name = self.node().map(|n| n.name.clone()).unwrap_or_default();
                (TextFor::RenameNode { workflow, node }, name)
            }
            Some(TreeRow::Workflow(workflow)) => {
                let name = self.workflow().map(|w| w.name.clone()).unwrap_or_default();
                (TextFor::RenameWorkflow(workflow), name)
            }
            None => return,
        };
        self.modal = Some(Modal::Text { purpose, input });
    }

    fn ask_delete(&mut self) -> Step {
        match self.selected_row() {
            Some(TreeRow::Node(workflow, node)) => {
                self.modal = Some(Modal::Confirm(ConfirmFor::DeleteNode { workflow, node }));
                Step::default()
            }
            Some(TreeRow::Workflow(workflow)) if workflow == self.doc.main_workflow_id => {
                Step::notice(Tone::Info, "the main workflow stays")
            }
            Some(TreeRow::Workflow(workflow)) => {
                self.modal = Some(Modal::Confirm(ConfirmFor::DeleteWorkflow(workflow)));
                Step::default()
            }
            None => Step::default(),
        }
    }

    fn open_row(&mut self, ctx: &Ctx) -> Step {
        let workflow = self.workflow_id();
        let Some(row) = self.inspector(ctx).into_iter().nth(self.field) else {
            return Step::default();
        };
        let node = self.node().map(|n| n.id.clone());
        match (row, node) {
            (InspectorRow::Name, _) => self.rename(),
            (InspectorRow::Field(field), Some(node)) => {
                let shown = catalog::display(field.value.as_ref().or(field.default.as_ref()));
                if matches!(field.control, Control::Select | Control::Workflow) {
                    let options: Vec<(String, Option<Value>)> = field
                        .options
                        .iter()
                        .map(|o| (catalog::display(Some(o)), Some(o.clone())))
                        .collect();
                    if options.is_empty() {
                        return Step::notice(
                            Tone::Info,
                            format!("{} has nothing to choose", field.id),
                        );
                    }
                    let index = options.iter().position(|(l, _)| *l == shown).unwrap_or(0);
                    self.modal = Some(Modal::Choose {
                        purpose: ChooseFor::Field {
                            workflow,
                            node,
                            field: field.id,
                        },
                        options,
                        index,
                    });
                } else {
                    self.modal = Some(Modal::Text {
                        purpose: TextFor::Field {
                            workflow,
                            node,
                            field: field.id,
                        },
                        input: shown,
                    });
                }
            }
            (InspectorRow::Internal(current), Some(node)) => {
                let mut options = vec![("(none)".to_string(), None)];
                options.extend(
                    self.workflow_ids()
                        .into_iter()
                        .filter(|id| *id != workflow)
                        .map(|id| (id.clone(), Some(Value::from(id)))),
                );
                let index = current
                    .and_then(|c| options.iter().position(|(l, _)| *l == c))
                    .unwrap_or(0);
                self.modal = Some(Modal::Choose {
                    purpose: ChooseFor::Internal { workflow, node },
                    options,
                    index,
                });
            }
            _ => {}
        }
        Step::default()
    }

    fn reset_row(&mut self, ctx: &Ctx) -> Step {
        let Some(InspectorRow::Field(field)) = self.inspector(ctx).into_iter().nth(self.field)
        else {
            return Step::default();
        };
        let Some(node) = self.node().map(|n| n.id.clone()) else {
            return Step::default();
        };
        let workflow = self.workflow_id();
        doc::set_config(
            &mut self.doc,
            &workflow,
            &node,
            &field.id,
            field.default.clone(),
        );
        Step::notice(Tone::Info, format!("{} back to its default", field.id))
    }

    fn modal_key(&mut self, modal: Modal, key: KeyEvent, ctx: &Ctx) -> Step {
        let keys = ctx.keys;
        match modal {
            Modal::Confirm(purpose) => {
                if keys.yes.matches(&key) {
                    return self.confirm(purpose);
                }
                if !(keys.no.matches(&key) || keys.back.matches(&key)) {
                    self.modal = Some(Modal::Confirm(purpose));
                }
                Step::default()
            }
            Modal::Text { purpose, mut input } => {
                if keys.back.matches(&key) {
                    return Step::default();
                }
                match key.code {
                    KeyCode::Enter => return self.commit_text(purpose, input, ctx),
                    KeyCode::Backspace => {
                        input.pop();
                    }
                    _ => {
                        if let Some(c) = typed(&key) {
                            input.push(c);
                        }
                    }
                }
                self.modal = Some(Modal::Text { purpose, input });
                Step::default()
            }
            Modal::Choose {
                purpose,
                options,
                mut index,
            } => {
                if keys.back.matches(&key) {
                    return Step::default();
                }
                if keys.up.matches(&key) || keys.down.matches(&key) {
                    let by = if keys.up.matches(&key) { -1 } else { 1 };
                    index = step_in(index, options.len(), by);
                } else if key.code == KeyCode::Enter {
                    let value = options.get(index).and_then(|(_, v)| v.clone());
                    return self.commit_choice(purpose, value);
                }
                self.modal = Some(Modal::Choose {
                    purpose,
                    options,
                    index,
                });
                Step::default()
            }
            Modal::Palette {
                mut query,
                mut index,
            } => {
                if keys.back.matches(&key) {
                    return Step::default();
                }
                let entries = self.palette(ctx);
                let shown = catalog::filter_palette(&entries, &query).len();
                if keys.up.matches(&key) || keys.down.matches(&key) {
                    let by = if keys.up.matches(&key) { -1 } else { 1 };
                    index = step_in(index, shown, by);
                } else if key.code == KeyCode::Enter {
                    let picked = catalog::filter_palette(&entries, &query)
                        .get(index)
                        .map(|e| (*e).clone());
                    match picked {
                        Some(entry) if entry.state.addable() => return self.place(&entry, ctx),
                        Some(entry) => {
                            self.modal = Some(Modal::Palette { query, index });
                            let (symbol, word) = entry.state.label();
                            return Step::notice(
                                Tone::Error,
                                format!("{}: {symbol} {word}, it cannot be added", entry.name),
                            );
                        }
                        None => {}
                    }
                } else if key.code == KeyCode::Backspace {
                    query.pop();
                    index = 0;
                } else if let Some(c) = typed(&key) {
                    query.push(c);
                    index = 0;
                }
                self.modal = Some(Modal::Palette { query, index });
                Step::default()
            }
            Modal::Connect { from, mut index } => {
                if keys.back.matches(&key) {
                    if from.is_some() {
                        self.modal = Some(Modal::Connect {
                            from: None,
                            index: 0,
                        });
                    }
                    return Step::default();
                }
                let len = match &from {
                    None => self.sources().len(),
                    Some(from) => self.targets(from).len(),
                };
                if keys.up.matches(&key) || keys.down.matches(&key) {
                    let by = if keys.up.matches(&key) { -1 } else { 1 };
                    index = step_in(index, len, by);
                } else if key.code == KeyCode::Enter {
                    match &from {
                        None => {
                            if let Some((end, _)) = self.sources().into_iter().nth(index) {
                                self.modal = Some(Modal::Connect {
                                    from: Some(end),
                                    index: 0,
                                });
                                return Step::default();
                            }
                        }
                        Some(source) => match self.targets(source).into_iter().nth(index) {
                            Some((to, _, Ok(kind))) => {
                                let workflow = self.workflow_id();
                                doc::add_wire(
                                    &mut self.doc,
                                    &workflow,
                                    kind,
                                    source.clone(),
                                    to.clone(),
                                );
                                self.wire = self.wires().len().saturating_sub(1);
                                return Step::notice(
                                    Tone::Info,
                                    format!("wired {} → {}", end_name(source), end_name(&to)),
                                );
                            }
                            Some((to, _, Err(refusal))) => {
                                self.modal = Some(Modal::Connect { from, index });
                                return Step::notice(
                                    Tone::Error,
                                    format!(
                                        "{} cannot take it: {}",
                                        end_name(&to),
                                        refusal.reason()
                                    ),
                                );
                            }
                            None => {}
                        },
                    }
                }
                self.modal = Some(Modal::Connect { from, index });
                Step::default()
            }
        }
    }

    fn place(&mut self, entry: &PaletteEntry, ctx: &Ctx) -> Step {
        let workflow = self.workflow_id();
        let Some(flow) = self.doc.workflow(&workflow) else {
            return Step::default();
        };
        let t = ctx.tuning;
        let at = doc::cascade_position(flow, t.cascade_step, t.node_width, t.node_height);
        let taken: Vec<&str> = flow.nodes.iter().map(|n| n.id.as_str()).collect();
        let Some(node) = catalog::node_for(ctx.manifests, &self.doc, &entry.piece, at, &taken)
        else {
            return Step::notice(
                Tone::Error,
                format!("{} is not in any manifest", entry.name),
            );
        };
        let id = node.id.clone();
        doc::add_node(&mut self.doc, &workflow, node);
        self.select(&TreeRow::Node(workflow, id.clone()));
        Step::notice(Tone::Info, format!("added {id}"))
    }

    fn confirm(&mut self, purpose: ConfirmFor) -> Step {
        match purpose {
            ConfirmFor::DeleteNode { workflow, node } => {
                doc::remove_node(&mut self.doc, &workflow, &node);
                self.row = step_in(self.row, self.tree().len(), 0);
                Step::notice(Tone::Info, format!("deleted {node}"))
            }
            ConfirmFor::DeleteWorkflow(workflow) => {
                doc::remove_workflow(&mut self.doc, &workflow);
                self.row = 0;
                Step::notice(Tone::Info, format!("deleted workflow {workflow}"))
            }
            ConfirmFor::Leave => Step {
                leave: true,
                ..Step::default()
            },
        }
    }

    fn commit_text(&mut self, purpose: TextFor, input: String, ctx: &Ctx) -> Step {
        let text = input.trim().to_string();
        match purpose {
            TextFor::RenameNode { workflow, node } => {
                doc::rename_node(&mut self.doc, &workflow, &node, &text);
            }
            TextFor::RenameWorkflow(workflow) => {
                doc::rename_workflow(&mut self.doc, &workflow, &text);
            }
            TextFor::NewWorkflow => {
                if text.is_empty() {
                    self.modal = Some(Modal::Text {
                        purpose: TextFor::NewWorkflow,
                        input,
                    });
                    return Step::notice(Tone::Info, "a workflow needs a name");
                }
                let id = doc::add_workflow(&mut self.doc, &text);
                self.select(&TreeRow::Workflow(id.clone()));
                return Step::notice(Tone::Info, format!("added workflow {id}"));
            }
            TextFor::NewPort(workflow) => {
                let mut words = text.split_whitespace();
                let id = words.next().unwrap_or_default().to_string();
                let direction = match words.next() {
                    None | Some("in" | "input") => Direction::Input,
                    Some("out" | "output") => Direction::Output,
                    Some(other) => {
                        self.modal = Some(Modal::Text {
                            purpose: TextFor::NewPort(workflow),
                            input,
                        });
                        return Step::notice(Tone::Error, format!("`{other}`: in or out"));
                    }
                };
                let capability = words.next().unwrap_or_default();
                let added =
                    doc::add_workflow_port(&mut self.doc, &workflow, &id, direction, capability);
                return Step::notice(
                    Tone::Info,
                    format!("added port self.{}", added.unwrap_or_default()),
                );
            }
            TextFor::Field {
                workflow,
                node,
                field,
            } => {
                let row = self
                    .doc
                    .workflow(&workflow)
                    .and_then(|w| w.node(&node))
                    .and_then(|n| {
                        let fields = catalog::type_of(ctx.manifests, n)
                            .map_or(&[][..], |k| k.fields.as_slice());
                        catalog::field_rows(fields, &n.config, &self.workflow_ids())
                            .into_iter()
                            .find(|r| r.id == field)
                    });
                let value = match row {
                    Some(row) => catalog::coerce(&row, &input),
                    None => Ok(Value::from(input.clone())),
                };
                match value {
                    Ok(value) => {
                        doc::set_config(&mut self.doc, &workflow, &node, &field, Some(value))
                    }
                    Err(error) => {
                        self.modal = Some(Modal::Text {
                            purpose: TextFor::Field {
                                workflow,
                                node,
                                field,
                            },
                            input,
                        });
                        return Step::notice(Tone::Error, error);
                    }
                }
            }
        }
        Step::default()
    }

    fn commit_choice(&mut self, purpose: ChooseFor, value: Option<Value>) -> Step {
        match purpose {
            ChooseFor::Field {
                workflow,
                node,
                field,
            } => doc::set_config(&mut self.doc, &workflow, &node, &field, value),
            ChooseFor::Internal { workflow, node } => {
                let internal = value.as_ref().and_then(Value::as_str);
                doc::set_internal_workflow(&mut self.doc, &workflow, &node, internal);
            }
        }
        Step::default()
    }

    pub fn saved(&mut self, result: Result<ProfileSaved, Failure>) -> Step {
        match result {
            Ok(saved) => {
                self.saved = Some(self.doc.clone());
                self.diagnostics = saved.diagnostics;
                let found = if self.diagnostics.is_empty() {
                    String::new()
                } else {
                    format!(" · {}", summary(&self.diagnostics))
                };
                Step {
                    requests: vec![Request::Profiles],
                    notice: Some((Tone::Info, format!("saved {}{found}", saved.path))),
                    leave: false,
                }
            }
            Err(failure) => Step::notice(Tone::Error, describe(&failure)),
        }
    }

    pub fn checked(&mut self, result: Result<Vec<Diagnostic>, Failure>) -> Step {
        match result {
            Ok(found) => {
                self.diagnostics = found;
                let text = if self.diagnostics.is_empty() {
                    "✓ check: nothing to report".to_string()
                } else {
                    format!("check: {}", summary(&self.diagnostics))
                };
                Step::notice(Tone::Info, text)
            }
            Err(failure) => Step::notice(Tone::Error, describe(&failure)),
        }
    }
}

/// `2 errors, 1 warning`.
pub fn summary(found: &[Diagnostic]) -> String {
    let count = |severity| found.iter().filter(|d| d.severity == severity).count();
    let mut parts = Vec::new();
    for (severity, word) in [
        (Severity::Error, "error"),
        (Severity::Warning, "warning"),
        (Severity::Info, "note"),
    ] {
        match count(severity) {
            0 => {}
            1 => parts.push(format!("1 {word}")),
            n => parts.push(format!("{n} {word}s")),
        }
    }
    parts.join(", ")
}
