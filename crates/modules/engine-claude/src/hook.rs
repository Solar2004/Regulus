//! Claude Code's side of the `hooks` slot (docs/13-blocks.md §4): its hook events, what
//! it writes on a hook's stdin and how a hook answers it. The bridge (`regulus hook
//! --format claude`) uses this to turn Claude's payload into a `HookRequest` and the
//! combined `HookAnswer` back into an exit code and output.
//!
//! The protocol is Claude Code's, as documented at https://code.claude.com/docs/en/hooks
//! (read 2026-11-07): stdin is one JSON object with `hook_event_name`, `session_id`,
//! `cwd` and per-event fields (`tool_name`, `tool_input` for tool events, `prompt` for
//! `UserPromptSubmit`, `last_assistant_message` for `Stop`). Exit 2 blocks with stderr
//! as the reason on `PreToolUse`, `UserPromptSubmit`, `Stop` (Claude keeps working) and
//! `PreCompact`; on `PostToolUse` it shows stderr to Claude after the tool ran; on
//! `SessionStart` only the user sees it. Context is exit 0 with
//! `{"hookSpecificOutput": {"hookEventName", "additionalContext"}}` on stdout, except on
//! `PreCompact`, which takes no `additionalContext` and reads plain stdout as
//! instructions for the summary.

use regulus_proto::{hook_event, HookAnswer, HookRequest};
use serde_json::{json, Value};

/// The `--format` the bridge is called with by this engine.
pub const FORMAT: &str = "claude";

/// Engine-neutral event → Claude Code's.
pub const EVENTS: &[(&str, &str)] = &[
    (hook_event::PRE_TOOL, "PreToolUse"),
    (hook_event::POST_TOOL, "PostToolUse"),
    (hook_event::STOP, "Stop"),
    (hook_event::SESSION_START, "SessionStart"),
    (hook_event::PROMPT, "UserPromptSubmit"),
    (hook_event::PRE_COMPACT, "PreCompact"),
];

/// The events where an exit 2 does not stop anything (docs above): a veto there only
/// reaches Claude, or only the user, as a message.
const VETO_IS_FEEDBACK: &[&str] = &[hook_event::POST_TOOL, hook_event::SESSION_START];

/// Claude Code's name for an engine-neutral event, if it has one.
pub fn native(event: &str) -> Option<&'static str> {
    EVENTS.iter().find(|(e, _)| *e == event).map(|(_, n)| *n)
}

/// Whether a veto on `event` really stops something in Claude Code.
pub fn veto_blocks(event: &str) -> bool {
    native(event).is_some() && !VETO_IS_FEEDBACK.contains(&event)
}

/// Claude's stdin as a request for `event`; the payload goes whole under `raw`.
pub fn request(event: &str, payload: Value) -> HookRequest {
    let text = |key: &str| payload.get(key).and_then(Value::as_str).map(String::from);
    HookRequest {
        event: event.into(),
        session_id: text("session_id"),
        cwd: text("cwd"),
        tool: text("tool_name"),
        input: payload.get("tool_input").cloned(),
        text: text("prompt").or_else(|| text("last_assistant_message")),
        raw: payload,
    }
}

/// What the bridge prints and exits with.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Output {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

/// The answer as Claude Code reads it, for the event the bridge was called for. The
/// event name in the output is the one Claude sent, else the one `event` maps to.
pub fn output(event: &str, payload: &Value, answer: &HookAnswer) -> Output {
    match answer {
        HookAnswer::Nothing => Output::default(),
        HookAnswer::Veto { reason } => Output {
            code: 2,
            stderr: reason.clone(),
            ..Output::default()
        },
        HookAnswer::Context { text } if event == hook_event::PRE_COMPACT => Output {
            stdout: text.clone(),
            ..Output::default()
        },
        HookAnswer::Context { text } => {
            let name = payload
                .get("hook_event_name")
                .and_then(Value::as_str)
                .or_else(|| native(event))
                .unwrap_or(event);
            let out = json!({ "hookSpecificOutput": {
                "hookEventName": name, "additionalContext": text,
            }});
            Output {
                stdout: out.to_string(),
                ..Output::default()
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_payloads_become_requests_and_keep_everything_in_raw() {
        let payload = json!({
            "session_id": "s1", "cwd": "/work", "hook_event_name": "PreToolUse",
            "tool_name": "Bash", "tool_input": { "command": "ls" },
        });
        let request = request(hook_event::PRE_TOOL, payload.clone());
        assert_eq!(request.tool.as_deref(), Some("Bash"));
        assert_eq!(request.input, Some(json!({ "command": "ls" })));
        assert_eq!(request.session_id.as_deref(), Some("s1"));
        assert_eq!(request.raw, payload);
        let stop = super::request("stop", json!({ "last_assistant_message": "done" }));
        assert_eq!(stop.text.as_deref(), Some("done"));
        let prompt = super::request("prompt", json!({ "prompt": "hi" }));
        assert_eq!(prompt.text.as_deref(), Some("hi"));
    }

    #[test]
    fn answers_become_exit_codes_and_output() {
        let payload = json!({ "hook_event_name": "SessionStart" });
        assert_eq!(
            output("session_start", &payload, &HookAnswer::Nothing),
            Output::default()
        );
        let veto = output(
            "pre_tool",
            &json!({}),
            &HookAnswer::Veto {
                reason: "no Bash".into(),
            },
        );
        assert_eq!((veto.code, veto.stderr.as_str()), (2, "no Bash"));
        let context = HookAnswer::Context {
            text: "be brief".into(),
        };
        let out = output("session_start", &payload, &context);
        assert_eq!(out.code, 0);
        let parsed: Value = serde_json::from_str(&out.stdout).unwrap();
        assert_eq!(
            parsed,
            json!({ "hookSpecificOutput": {
                "hookEventName": "SessionStart", "additionalContext": "be brief" } })
        );
        // Without the name in the payload, the mapped one; PreCompact takes plain text.
        let prompt: Value =
            serde_json::from_str(&output("prompt", &json!({}), &context).stdout).unwrap();
        assert_eq!(
            prompt["hookSpecificOutput"]["hookEventName"],
            "UserPromptSubmit"
        );
        assert_eq!(
            output("pre_compact", &json!({}), &context).stdout,
            "be brief"
        );
    }

    #[test]
    fn every_neutral_event_has_a_claude_name() {
        for event in hook_event::ALL {
            assert!(native(event).is_some(), "{event}");
        }
        assert_eq!(native("on_idle"), None);
        assert!(veto_blocks("pre_tool") && veto_blocks("stop"));
        assert!(!veto_blocks("session_start") && !veto_blocks("on_idle"));
    }
}
