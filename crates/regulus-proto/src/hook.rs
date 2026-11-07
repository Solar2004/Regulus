//! The `hook@1` capability (docs/13-blocks.md §4): a node that an agent's engine asks,
//! at points of its life cycle, whether to go on. The engine's CLI calls a bridge, the
//! bridge calls `hook.dispatch` on the core, and the core relays the request as
//! `node/hook` to every `hook@1` node wired into the agent, each within its time limit.
//!
//! The core only relays: it does not know events, matchers or what a veto means. Names
//! are engine-neutral; each engine maps them to its own and leaves out, with a warning,
//! the ones it has no equivalent for. Adding an optional field keeps `@1`; changing a
//! shape means `hook@2`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const HOOK_CAPABILITY: &str = "hook@1";

/// What a hook listens to and may answer: `NodeCall` with no args → [`Hook`]. Asked by
/// the node it is wired into, over the "uses" wire, like any capability method.
pub const HOOK_DESCRIBE: &str = "hook";

/// Engine-neutral event names. An event is a plain string, so a new one is not a new
/// version; an engine that does not know it says so.
pub mod hook_event {
    /// Before a tool runs. A veto stops the call; the reason goes to the agent.
    pub const PRE_TOOL: &str = "pre_tool";
    /// After a tool ran. A veto cannot undo it: its reason goes to the agent as feedback.
    pub const POST_TOOL: &str = "post_tool";
    /// The agent finished its turn. A veto keeps it working, with the reason as why.
    pub const STOP: &str = "stop";
    /// A session starts or resumes. Context only.
    pub const SESSION_START: &str = "session_start";
    /// The user sent a prompt, before the agent sees it. A veto drops it.
    pub const PROMPT: &str = "prompt";
    /// Before the conversation is compacted. A veto skips it; context steers the summary.
    pub const PRE_COMPACT: &str = "pre_compact";
    /// Every name above.
    pub const ALL: &[&str] = &[
        PRE_TOOL,
        POST_TOOL,
        STOP,
        SESSION_START,
        PROMPT,
        PRE_COMPACT,
    ];
    /// The events that carry a tool, where a matcher applies.
    pub const WITH_TOOL: &[&str] = &[PRE_TOOL, POST_TOOL];
}

/// What a hook may answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HookEffect {
    Veto,
    Context,
}

/// A hook as it describes itself to the node it is wired into.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Hook {
    /// One of [`hook_event`], or a name some engine knows.
    pub event: String,
    /// For [`hook_event::WITH_TOOL`]: the tool names it applies to, see
    /// [`matches_tool`]. None: every tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub matcher: Option<String>,
    /// How long it may take to answer. None: the core's `[hooks] timeout_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    /// What it may answer, so an engine can warn about an effect it cannot honor on
    /// that event. Empty: unknown.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub returns: Vec<HookEffect>,
}

/// What the engine reported, normalized just enough to decide on without knowing the
/// engine. Everything it sent is in `raw`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct HookRequest {
    /// The engine-neutral event name.
    pub event: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The tool, for tool events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    /// The tool's input, for tool events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Value>,
    /// The text of the event: the prompt, or the agent's last message on `stop`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// The payload exactly as the engine sent it.
    #[serde(default)]
    pub raw: Value,
}

/// A hook's answer to `node/hook`. A module may also answer `null`, which is
/// [`HookAnswer::Nothing`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "lowercase")]
pub enum HookAnswer {
    /// Go on as if there were no hook.
    #[default]
    #[serde(rename = "none")]
    Nothing,
    /// Stop what the event is about, telling the agent why.
    Veto { reason: String },
    /// Go on, with this text added to what the agent sees.
    Context { text: String },
}

impl HookAnswer {
    /// A module's answer: `null` is nothing.
    pub fn from_value(value: Value) -> Result<Self, serde_json::Error> {
        if value.is_null() {
            return Ok(Self::Nothing);
        }
        serde_json::from_value(value)
    }
}

/// `hook.dispatch`: relays `request` as `node/hook` to the `hook@1` nodes wired over
/// "uses" into `node_id` of a running execution, and waits for their answers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HookDispatchParams {
    pub execution_id: String,
    /// The node the hooks are wired into: the agent.
    pub node_id: String,
    /// Only this hook node, which must be wired into `node_id`. None: all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hook_id: Option<String>,
    /// How long each hook may take. None: `[hooks] timeout_ms`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    pub request: HookRequest,
}

/// One hook's answer. One that failed, timed out or does not answer hooks counts as
/// [`HookAnswer::Nothing`], with `error` saying why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookReply {
    pub node_id: String,
    pub answer: HookAnswer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The answer of `hook.dispatch`, in wire order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookDispatched {
    pub replies: Vec<HookReply>,
}

impl HookDispatched {
    /// What the engine should do: any veto wins, with every veto's reason; otherwise
    /// every context, in order; otherwise nothing.
    pub fn combined(&self) -> HookAnswer {
        let join = |texts: Vec<&str>| texts.join("\n\n");
        let vetoes: Vec<&str> = self
            .replies
            .iter()
            .filter_map(|r| match &r.answer {
                HookAnswer::Veto { reason } => Some(reason.as_str()),
                _ => None,
            })
            .collect();
        if !vetoes.is_empty() {
            return HookAnswer::Veto {
                reason: join(vetoes),
            };
        }
        let contexts: Vec<&str> = self
            .replies
            .iter()
            .filter_map(|r| match &r.answer {
                HookAnswer::Context { text } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        if contexts.is_empty() {
            HookAnswer::Nothing
        } else {
            HookAnswer::Context {
                text: join(contexts),
            }
        }
    }
}

/// Whether a matcher applies to `tool`: empty or `*` matches any tool; otherwise exact
/// names separated by `|` (`Bash|Write`). This is the plain form Claude Code reads the
/// same way; anything that would make it a pattern there is still compared literally.
pub fn matches_tool(matcher: Option<&str>, tool: Option<&str>) -> bool {
    let matcher = matcher.map(str::trim).unwrap_or_default();
    if matcher.is_empty() || matcher == "*" {
        return true;
    }
    let Some(tool) = tool else {
        return false;
    };
    matcher.split('|').any(|name| name.trim() == tool)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn reply(answer: HookAnswer) -> HookReply {
        HookReply {
            node_id: "h".into(),
            answer,
            error: None,
        }
    }

    #[test]
    fn answers_are_tagged_by_decision() {
        let veto = HookAnswer::Veto {
            reason: "no".into(),
        };
        assert_eq!(
            serde_json::to_value(&veto).unwrap(),
            json!({ "decision": "veto", "reason": "no" })
        );
        assert_eq!(
            serde_json::to_value(HookAnswer::Nothing).unwrap(),
            json!({ "decision": "none" })
        );
        let context: HookAnswer =
            serde_json::from_value(json!({ "decision": "context", "text": "hi" })).unwrap();
        assert_eq!(context, HookAnswer::Context { text: "hi".into() });
        assert_eq!(
            HookAnswer::from_value(Value::Null).unwrap(),
            HookAnswer::Nothing
        );
        assert!(HookAnswer::from_value(json!({ "decision": "maybe" })).is_err());
    }

    #[test]
    fn a_veto_wins_and_contexts_add_up() {
        let context = |t: &str| HookAnswer::Context { text: t.into() };
        let both = HookDispatched {
            replies: vec![
                reply(context("a")),
                reply(HookAnswer::Veto {
                    reason: "no".into(),
                }),
                reply(HookAnswer::Nothing),
            ],
        };
        assert_eq!(
            both.combined(),
            HookAnswer::Veto {
                reason: "no".into()
            }
        );
        let contexts = HookDispatched {
            replies: vec![reply(context("a")), reply(context("b"))],
        };
        assert_eq!(contexts.combined(), context("a\n\nb"));
        assert_eq!(HookDispatched::default().combined(), HookAnswer::Nothing);
    }

    #[test]
    fn a_hook_and_a_request_keep_what_they_omit_out_of_the_json() {
        let hook: Hook = serde_json::from_value(json!({ "event": "pre_tool" })).unwrap();
        assert_eq!(hook.matcher, None);
        assert_eq!(
            serde_json::to_value(&hook).unwrap(),
            json!({ "event": "pre_tool" })
        );
        let request = HookRequest {
            event: "pre_tool".into(),
            tool: Some("Bash".into()),
            raw: json!({ "tool_name": "Bash" }),
            ..HookRequest::default()
        };
        let value = serde_json::to_value(&request).unwrap();
        assert_eq!(value["raw"]["tool_name"], "Bash");
        assert!(value.get("text").is_none());
    }

    #[test]
    fn matchers_are_exact_names_or_anything() {
        assert!(matches_tool(None, Some("Bash")));
        assert!(matches_tool(Some(" * "), None));
        assert!(matches_tool(Some("Write|Bash"), Some("Bash")));
        assert!(!matches_tool(Some("Bash"), Some("BashOutput")));
        assert!(!matches_tool(Some("Bash"), None));
        assert!(!matches_tool(Some("Edit.*"), Some("Edit")));
    }
}
