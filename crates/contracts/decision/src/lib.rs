//! The `decision@1` capability (docs/12-agents.md §5): a node that asks someone (a
//! person, a judge, a rule) to choose one of a few options, and later says what was
//! chosen. The Agent box uses it for an engine's permission requests; who decides and how
//! is the offering node's business.
//!
//! The format belongs to the capability, not to any module. Adding an optional field
//! keeps `@1`; changing a shape means `decision@2`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CAPABILITY: &str = "decision@1";

/// Asks for a decision: `NodeCall` with [`AskArgs`] → [`Asked`]. Returns at once.
pub const ASK: &str = "decision.ask";
/// Where a decision stands: `NodeCall` with [`PollArgs`] → [`Poll`].
pub const POLL: &str = "decision.poll";

/// One option to choose from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Choice {
    pub id: String,
    pub name: String,
    /// What kind of answer it is, as the asker understands it (`allow_once`,
    /// `reject_once`…). A decider that is told "reject" picks a choice whose kind starts
    /// with `reject`, if there is one. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskArgs {
    /// The question, for whoever decides.
    pub prompt: String,
    pub options: Vec<Choice>,
    /// Anything that helps decide: the tool, the agent, the execution…
    #[serde(default)]
    pub context: Value,
    /// Milliseconds before the decider gives up; `None`: the decider's own setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asked {
    /// What [`POLL`] takes.
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PollArgs {
    pub id: String,
}

/// Where a decision stands. Pending: nothing else holds. Decided: `option` is the
/// choice, or `None` when it was turned down with no reject option to pick, or when
/// nobody decided in time (`timed_out`) and there was no default.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Poll {
    pub pending: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub timed_out: bool,
}
