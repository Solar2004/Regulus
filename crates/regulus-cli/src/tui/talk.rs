//! The conversation view of Operate (docs/04-client.md §7): what the terminal UI keeps of
//! an agent's history between refreshes, and the two pickers it opens, the binding of a
//! workflow and the answer to a permission request. The rules are `conversation.rs`.

use serde_json::Value;

use super::core::Failure;
use crate::conversation::{history_records, merge, PermissionOption, Record, USER};

/// An agent's history as far as it was read.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct AgentLog {
    pub records: Vec<Record>,
    /// The cursor: what `agent.history` takes as `since`.
    pub next: u64,
    /// `session` of the last page: `engine_session_id`, `native_resume`, `state`.
    pub session: Value,
    pub error: Option<Failure>,
    /// Prompts sent from here not in the history yet: `(turn, text)`; the turn is empty
    /// until `agent.send` answers.
    pub queued: Vec<(String, String)>,
}

impl AgentLog {
    /// Adds an `agent.history` page read from `since`. Says whether there may be more.
    pub fn page(&mut self, since: u64, page: &Value, keep: usize) -> bool {
        let next = page["next"].as_u64().unwrap_or(since);
        if since != self.next && next <= self.next {
            return false;
        }
        merge(&mut self.records, history_records(page), keep);
        self.next = self.next.max(next);
        self.session = page["session"].clone();
        self.error = None;
        let records = &self.records;
        self.queued.retain(|(turn, _)| {
            turn.is_empty()
                || !records.iter().any(
                    |r| matches!(r, Record::Entry { role, turn: Some(t), .. } if role == USER && t == turn),
                )
        });
        next > since
    }

    pub fn state(&self) -> &str {
        self.session["state"].as_str().unwrap_or("unknown")
    }

    /// A turn of this agent is in flight: sent and not answered, or the session busy.
    pub fn busy(&self) -> bool {
        !self.queued.is_empty() || self.state() == "busy"
    }
}

/// Choosing the binding of a workflow: one of its text inputs and one of its text outputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindPicker {
    pub execution: String,
    pub profile: String,
    pub workflow: String,
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
    pub input: usize,
    pub output: usize,
    /// 0: moving through the inputs, 1: the outputs.
    pub column: usize,
    /// There is a binding to forget.
    pub bound: bool,
}

/// One choice of a permission picker: an option, or a plain rejection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    Option(PermissionOption),
    Reject,
}

/// Answering a permission request through the decision a Decider opened for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionPicker {
    pub decision: String,
    pub request: String,
    pub title: String,
    pub choices: Vec<Choice>,
    pub index: usize,
}

impl PermissionPicker {
    /// Every option, and a rejection at the end when no option is of a `reject…` kind.
    pub fn new(
        decision: String,
        request: String,
        title: String,
        options: &[PermissionOption],
    ) -> Self {
        let mut choices: Vec<Choice> = options.iter().cloned().map(Choice::Option).collect();
        if !options.iter().any(|o| o.kind.starts_with("reject")) {
            choices.push(Choice::Reject);
        }
        Self {
            decision,
            request,
            title,
            choices,
            index: 0,
        }
    }
}
