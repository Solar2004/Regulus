//! The Templates view's state: `template.list`, the form of `template.apply` and what
//! came of it, as the app's Templates view (docs/04-client.md §8). No drawing, no I/O.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use regulus_proto::{
    ModuleState, ModuleSummary, TemplateApplied, TemplateRequire, TemplateSummary, TemplateVar,
};
use serde_json::{Map, Value};

use super::app::Tone;
use super::catalog::find_module;
use super::core::{describe, Failure, Request};
use super::edit::Step;
use super::keys::Keys;

/// How a module a template requires stands; the version range is the core's to judge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequireState {
    Installed,
    Missing,
    Broken,
}

impl RequireState {
    pub fn label(self) -> (&'static str, &'static str) {
        match self {
            Self::Installed => ("✓", "installed"),
            Self::Missing => ("□", "not installed"),
            Self::Broken => ("▲", "broken"),
        }
    }
}

pub fn require_state(require: &TemplateRequire, modules: &[ModuleSummary]) -> RequireState {
    match find_module(modules, &require.module).map(|m| &m.state) {
        None => RequireState::Missing,
        Some(ModuleState::Broken { .. } | ModuleState::Incompatible { .. }) => RequireState::Broken,
        Some(_) => RequireState::Installed,
    }
}

pub fn is_required(var: &TemplateVar) -> bool {
    var.default.is_none()
}

/// The form of `template.apply`: one value per var, then `id` and `force`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Form {
    pub template: String,
    pub vars: Vec<TemplateVar>,
    pub values: Vec<String>,
    pub id: String,
    pub force: bool,
    pub row: usize,
}

/// A row of the form.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormRow {
    Var(usize),
    Id,
    Force,
}

impl Form {
    pub fn new(template: &TemplateSummary) -> Self {
        Self {
            template: template.name.clone(),
            values: template
                .vars
                .iter()
                .map(|v| v.default.clone().unwrap_or_default())
                .collect(),
            vars: template.vars.clone(),
            id: String::new(),
            force: false,
            row: 0,
        }
    }

    pub fn rows(&self) -> Vec<FormRow> {
        let mut rows: Vec<FormRow> = (0..self.vars.len()).map(FormRow::Var).collect();
        rows.extend([FormRow::Id, FormRow::Force]);
        rows
    }

    /// Required vars still empty, in the template's order.
    pub fn missing(&self) -> Vec<String> {
        self.vars
            .iter()
            .zip(&self.values)
            .filter(|(var, value)| is_required(var) && value.trim().is_empty())
            .map(|(var, _)| var.name.clone())
            .collect()
    }

    /// What was typed; an emptied optional var takes its default in the core.
    pub fn request(&self) -> Request {
        let mut vars = Map::new();
        for (var, value) in self.vars.iter().zip(&self.values) {
            if !value.trim().is_empty() || is_required(var) {
                vars.insert(var.name.clone(), Value::from(value.clone()));
            }
        }
        let id = self.id.trim();
        Request::Apply {
            template: self.template.clone(),
            vars,
            id: (!id.is_empty()).then(|| id.to_string()),
            force: self.force,
        }
    }

    fn text(&mut self) -> Option<&mut String> {
        match self.rows().get(self.row)? {
            FormRow::Var(i) => self.values.get_mut(*i),
            FormRow::Id => Some(&mut self.id),
            FormRow::Force => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Templates {
    pub list: Option<Result<Vec<TemplateSummary>, Failure>>,
    pub index: usize,
    pub form: Option<Form>,
    /// The last apply, kept to open or run what it wrote.
    pub applied: Option<Result<TemplateApplied, Failure>>,
}

/// What the screen around the view does on a key, besides requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Then {
    Nothing,
    Leave,
    /// Open this profile in Edit.
    Edit(String),
}

impl Templates {
    pub fn templates(&self) -> &[TemplateSummary] {
        match &self.list {
            Some(Ok(list)) => list,
            _ => &[],
        }
    }

    pub fn selected(&self) -> Option<&TemplateSummary> {
        self.templates().get(self.index)
    }

    pub fn listed(&mut self, result: Result<Vec<TemplateSummary>, Failure>) {
        self.list = Some(result);
        self.index = self.index.min(self.templates().len().saturating_sub(1));
    }

    pub fn applied(&mut self, result: Result<TemplateApplied, Failure>) -> Step {
        let step = match &result {
            Ok(done) => {
                self.form = None;
                Step {
                    requests: vec![Request::Profiles],
                    notice: Some((
                        Tone::Info,
                        format!("applied: profile {} in {}", done.profile_id, done.path),
                    )),
                    leave: false,
                    quit: false,
                }
            }
            Err(failure) => Step {
                notice: Some((Tone::Error, describe(failure))),
                ..Step::default()
            },
        };
        self.applied = Some(result);
        step
    }

    pub fn key(&mut self, key: KeyEvent, keys: &Keys) -> (Step, Then) {
        if let Some(form) = &mut self.form {
            if keys.back.matches(&key) {
                self.form = None;
                return (Step::default(), Then::Nothing);
            }
            let rows = form.rows();
            if keys.up.matches(&key) || key.code == KeyCode::BackTab {
                form.row = form.row.saturating_sub(1);
            } else if keys.down.matches(&key) || key.code == KeyCode::Tab {
                form.row = (form.row + 1).min(rows.len() - 1);
            } else if key.code == KeyCode::Enter {
                let missing = form.missing();
                if !missing.is_empty() {
                    let text = format!("fill in: {}", missing.join(", "));
                    return (Step::notice(Tone::Info, text), Then::Nothing);
                }
                let request = form.request();
                return (
                    Step {
                        requests: vec![request],
                        notice: Some((Tone::Info, format!("applying {}…", form.template))),
                        leave: false,
                        quit: false,
                    },
                    Then::Nothing,
                );
            } else if rows.get(form.row) == Some(&FormRow::Force)
                && matches!(key.code, KeyCode::Char(' '))
            {
                form.force = !form.force;
            } else if key.code == KeyCode::Backspace {
                if let Some(text) = form.text() {
                    text.pop();
                }
            } else if let KeyCode::Char(c) = key.code {
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                {
                    if let Some(text) = form.text() {
                        text.push(c);
                    }
                }
            }
            return (Step::default(), Then::Nothing);
        }
        if keys.back.matches(&key) {
            return (Step::default(), Then::Leave);
        }
        if keys.up.matches(&key) || keys.down.matches(&key) {
            let by: isize = if keys.up.matches(&key) { -1 } else { 1 };
            let len = self.templates().len();
            if len > 0 {
                self.index = self.index.saturating_add_signed(by).min(len - 1);
            }
            // Moving on leaves the last result: Enter opens a form again.
            self.applied = None;
            return (Step::default(), Then::Nothing);
        }
        // Right after an apply, the result offers to edit or run what it wrote.
        if let Some(Ok(done)) = &self.applied {
            let profile = done.profile_id.clone();
            if keys.open_in_editor.matches(&key) {
                return (Step::default(), Then::Edit(profile));
            }
            if keys.run.matches(&key) {
                let step = Step {
                    requests: vec![Request::Start {
                        profile,
                        workflow: None,
                    }],
                    ..Step::default()
                };
                return (step, Then::Nothing);
            }
        }
        if keys.open.matches(&key) {
            match self.selected() {
                Some(template) if template.error.is_some() => {
                    return (
                        Step::notice(
                            Tone::Error,
                            format!(
                                "{} cannot be applied: {}",
                                template.name,
                                template.error.clone().unwrap_or_default()
                            ),
                        ),
                        Then::Nothing,
                    );
                }
                Some(template) => self.form = Some(Form::new(template)),
                None => {}
            }
        }
        (Step::default(), Then::Nothing)
    }
}
