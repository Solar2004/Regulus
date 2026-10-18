//! The `participant@1` capability (docs/11-composition.md §3): someone a channel delivers
//! messages to. An agent, a person at a terminal or a script; communication only knows
//! participants, never agents, so it does not depend on the agent module.
//!
//! The format belongs to the capability, not to any module. Adding an optional field
//! keeps `@1`; changing a shape means `participant@2`.

use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "participant@1";

/// Hands a message to the participant: `NodeCall` with [`Delivery`] args → [`Reply`].
/// It may take as long as the participant needs to answer, an agent's whole turn.
pub const DELIVER: &str = "participant.deliver";

/// Who the participant is and whether it can take a message now: `NodeCall` → [`Identity`].
/// Optional: a participant without it is named after its node, in an unknown state.
pub const DESCRIBE: &str = "participant.describe";

/// Whether a participant can take a message right now. The channel waits while it is
/// busy, so a message never lands in the middle of someone else's turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum State {
    Idle,
    Busy,
    /// Not running: nothing can be delivered.
    Gone,
    /// It does not say; the channel delivers without waiting.
    #[default]
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    /// The name others mention it by, `@name`. Unique in a channel.
    pub name: String,
    /// What it is: `agent`, `person`, `script`… Informative only.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    #[serde(default)]
    pub state: State,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Delivery {
    /// The channel node the message went through.
    pub channel: String,
    /// The sender's name in that channel.
    pub from: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reply {
    /// What the participant answered, if anything. The channel records it; it does not
    /// deliver it, so two agents cannot answer each other forever.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub text: String,
}
