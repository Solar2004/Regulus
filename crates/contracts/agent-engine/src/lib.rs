//! The `agent.engine@1` capability (docs/12-agents.md §3): the methods the Agent box
//! calls on whatever engine is wired into it, and their arguments.
//!
//! The format belongs to the capability, not to any module, so the box and every engine
//! depend on this crate and on nothing of each other. Changing a shape here means a new
//! capability version (`agent.engine@2`), not a silent break.

use std::collections::BTreeMap;
use std::path::PathBuf;

pub use regulus_contract_llm_endpoint::LlmEndpoint;
pub use regulus_contract_skill::Skill;
pub use regulus_contract_toolset::Mount;
pub use regulus_proto::Hook;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CAPABILITY: &str = "agent.engine@1";

/// Installations of the engine's CLI found on this machine → [`Detected`].
pub const DETECT: &str = "engine.detect";
/// Launches the CLI and opens a session: [`StartArgs`] → [`Started`].
pub const SESSION_START: &str = "engine.session.start";
/// One turn: [`PromptArgs`] → [`Turn`].
pub const SESSION_PROMPT: &str = "engine.session.prompt";
/// Where a session stands: [`StatusArgs`] → [`Status`]. Optional: an engine without it
/// answers `UnknownMethod` and the box reports the state as unknown.
pub const SESSION_STATUS: &str = "engine.session.status";
/// Ends a session and its process: [`StopArgs`] → `null`.
pub const SESSION_STOP: &str = "engine.session.stop";
/// Asks the turn in flight to end (ACP `session/cancel`): [`CancelArgs`] → `null`. The
/// turn still answers, usually with stop reason `cancelled`. Optional: an engine without
/// it answers `UnknownMethod` and the box drops the session instead.
pub const SESSION_CANCEL: &str = "engine.session.cancel";
/// What happened in a session since a cursor: [`EventsArgs`] → [`Events`]. Optional: an
/// engine without it answers `UnknownMethod` and the box only sees each turn's final text.
pub const SESSION_EVENTS: &str = "engine.session.events";
/// Answers a pending permission request (a [`SessionEventKind::PermissionRequest`]):
/// [`DecideArgs`] → `null`. Optional, like the events.
pub const SESSION_DECIDE: &str = "engine.session.decide";

/// The Agent box's provider slots besides the main one (docs/12-agents.md §6.5): each
/// takes a provider for one purpose. A slot an engine does not know is left out with a
/// warning, never a failure.
pub mod slot {
    /// Background work: titles, summaries, quick checks.
    pub const FAST: &str = "fast";
    /// The subagents the CLI spawns on its own.
    pub const SUBAGENT: &str = "subagent";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Installation {
    pub path: PathBuf,
    /// What `--version` printed, if it answered.
    pub version: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Detected {
    pub installations: Vec<Installation>,
}

/// What the session answers when the agent asks for permission.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permissions {
    Allow,
    #[default]
    Reject,
    /// Each request waits as a `permission_request` event until
    /// [`SESSION_DECIDE`] answers it, the turn is cancelled or the engine's own time
    /// limit runs out (then the reject option is chosen).
    Ask,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartArgs {
    pub cwd: PathBuf,
    #[serde(default)]
    pub permissions: Permissions,
    /// The model endpoint wired into the box, if any (docs/12-agents.md §6). Optional, so
    /// it extends `@1` without breaking engines that ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<LlmEndpoint>,
    /// Providers for other purposes, keyed by [`slot`]. Optional, so it extends `@1`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub slots: BTreeMap<String, LlmEndpoint>,
    /// The skills wired into the box, for the engine to place in the profile. Optional,
    /// so it extends `@1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<Skill>,
    /// Tool servers to start with the session, each in the protocol it says. One the
    /// engine does not speak is left out with a warning. Optional, so it extends `@1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<Mount>,
    /// The hooks wired into the box (docs/13-blocks.md §4), for the engine to write into
    /// its CLI's hook settings. One the engine cannot place is left out with a warning.
    /// Optional, so it extends `@1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookMount>,
    /// A native session id from an earlier [`Started::native_session_id`] to load
    /// instead of opening a new session (ACP `session/load`). An engine that cannot load
    /// sessions opens a new one with a warning. Optional, so it extends `@1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
}

/// One hook for the engine's CLI: when to run the bridge and how. The engine appends
/// [`HOOK_FORMAT_FLAG`] and the name of its own hook protocol to `args`, so the bridge
/// reads the CLI's payload and answers in kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookMount {
    /// The hook node, for messages.
    pub node: String,
    #[serde(flatten)]
    pub hook: Hook,
    pub command: PathBuf,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
}

/// The bridge's flag naming the hook protocol of the CLI that runs it.
pub const HOOK_FORMAT_FLAG: &str = "--format";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub name: String,
    pub version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub session_id: String,
    pub agent: Agent,
    /// What was wired in but left out, and why. Optional, so it extends `@1`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// What the engine and its CLI can do. Optional, so it extends `@1`: an engine that
    /// does not say can do none of it.
    #[serde(default)]
    pub capabilities: EngineCapabilities,
    /// The CLI's own id for the session, the one [`StartArgs::resume`] takes. Optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_session_id: Option<String>,
    /// Whether the session is the one `resume` named, loaded by the CLI itself. False
    /// for a new session, also when a resume was asked and could not be done.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resumed: bool,
}

/// What an engine can do beyond the required methods.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineCapabilities {
    /// The CLI can load an earlier session by its native id ([`StartArgs::resume`]).
    #[serde(default)]
    pub load_session: bool,
    /// The engine serves [`SESSION_EVENTS`] (and [`SESSION_DECIDE`]).
    #[serde(default)]
    pub events: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromptArgs {
    pub session_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub stop_reason: String,
    pub text: String,
    /// The tokens the turn took, when the engine knows. Optional, so it extends `@1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default, alias = "inputTokens")]
    pub input_tokens: u64,
    #[serde(default, alias = "outputTokens")]
    pub output_tokens: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StopArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CancelArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusArgs {
    pub session_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionState {
    /// Waiting for a prompt.
    Idle,
    /// In a turn.
    Busy,
    /// The session or its process is gone.
    Ended,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Status {
    pub state: SessionState,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventsArgs {
    pub session_id: String,
    /// The `next` of the previous answer; 0: from the oldest event the engine keeps.
    #[serde(default)]
    pub since: u64,
}

/// The events after `since`, oldest first. Engines keep a bounded number: a cursor older
/// than the oldest kept gets what is left.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Events {
    pub events: Vec<SessionEvent>,
    /// The cursor for the next call: the `seq` of the last event given, or `since`.
    pub next: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DecideArgs {
    pub session_id: String,
    /// The `id` of the `permission_request` event.
    pub request_id: String,
    /// One of the request's option ids; `None` cancels the request (the agent is told it
    /// was cancelled).
    #[serde(default)]
    pub option: Option<String>,
}

/// Something that happened in a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionEvent {
    /// Grows by one per event of the session, from 1.
    pub seq: u64,
    /// Milliseconds since the Unix epoch.
    pub at: u64,
    /// The engine's turn number in this session (from 1), if it happened inside a turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn: Option<u64>,
    #[serde(flatten)]
    pub kind: SessionEventKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEventKind {
    TurnStarted,
    /// A piece of the agent's answer, as it streams.
    Text {
        text: String,
    },
    /// A piece of the agent's reasoning, as it streams.
    Thought {
        text: String,
    },
    ToolCall {
        id: String,
        title: String,
        /// The CLI's own kind of tool (`read`, `edit`, `execute`…), if it says.
        #[serde(rename = "tool_kind", default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        status: String,
    },
    ToolUpdate {
        id: String,
        status: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        content: Option<Value>,
    },
    Plan {
        entries: Value,
    },
    PermissionRequest {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
        title: String,
        options: Vec<PermissionOption>,
    },
    PermissionResolved {
        id: String,
        /// The option chosen; `None`: the request was cancelled.
        #[serde(default)]
        option: Option<String>,
    },
    TurnEnded {
        stop_reason: String,
    },
    State {
        state: SessionState,
    },
}

/// One answer a permission request offers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionOption {
    pub id: String,
    /// `allow_once`, `allow_always`, `reject_once` or `reject_always`.
    pub kind: String,
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_flat_with_a_kind_tag() {
        let event = SessionEvent {
            seq: 3,
            at: 10,
            turn: Some(1),
            kind: SessionEventKind::ToolCall {
                id: "t1".into(),
                title: "read".into(),
                kind: Some("read".into()),
                status: "pending".into(),
            },
        };
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["kind"], "tool_call");
        assert_eq!(value["tool_kind"], "read");
        assert_eq!(
            serde_json::from_value::<SessionEvent>(value).unwrap(),
            event
        );
    }

    #[test]
    fn an_older_start_answer_still_reads() {
        let started: Started = serde_json::from_value(serde_json::json!({
            "session_id": "s1", "agent": { "name": "a", "version": "1" }
        }))
        .unwrap();
        assert_eq!(started.capabilities, EngineCapabilities::default());
        assert!(!started.resumed);
    }
}
