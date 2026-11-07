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
//! instructions for the summary. The page lists the events that take `additionalContext`:
//! SessionStart, SubagentStart, UserPromptSubmit, UserPromptExpansion, PreToolUse,
//! PostToolUse, PostToolUseFailure, PostToolBatch, Stop, SubagentStop, PostModelSwitch.
//! On `Stop` it is "Non-error feedback for Claude. The conversation continues so Claude
//! can act on it, but unlike `decision: "block"` it is shown in the transcript as hook
//! feedback rather than a hook error", with the example
//! `{"hookSpecificOutput": {"hookEventName": "Stop", "additionalContext": "Please run the
//! test suite before finishing"}}`: context on `stop` keeps the agent working.

use std::path::Path;

use regulus_contract_agent_engine::{HookMount, HOOK_FORMAT_FLAG};
use regulus_proto::{hook_event, HookAnswer, HookEffect, HookRequest};
use serde_json::{json, Map, Value};

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

/// Inside CLAUDE_CONFIG_DIR, the settings an isolated profile reads (`settingSources:
/// ["user"]`), where the hooks go.
const SETTINGS_FILE: &str = "settings.json";
const HOOKS_KEY: &str = "hooks";
/// Claude Code's `timeout` is in whole seconds and kills the bridge when it runs out; the
/// limit that counts is the core's (`--timeout-ms`), which answers "nothing" on time. This
/// second only keeps Claude from killing the bridge first while it starts and connects
/// (milliseconds, measured): it is part of speaking Claude's protocol, not a setting.
const BRIDGE_SLACK_SECONDS: u64 = 1;

/// Claude Code's name for an engine-neutral event, if it has one.
pub fn native(event: &str) -> Option<&'static str> {
    EVENTS.iter().find(|(e, _)| *e == event).map(|(_, n)| *n)
}

/// Whether a veto on `event` really stops something in Claude Code.
pub fn veto_blocks(event: &str) -> bool {
    native(event).is_some() && !VETO_IS_FEEDBACK.contains(&event)
}

/// Writes `hooks` into the `settings.json` of the isolated profile at `dir`, keeping
/// every other key. Regulus owns the `hooks` key there: an isolated profile holds only
/// what is wired in, so what an earlier run wrote is replaced, and with no hooks the key
/// goes. Returns a warning for each hook left out or that cannot do what it says.
///
/// With no hooks it only removes a stale `hooks` key: a file it cannot read or parse is
/// then a warning, never a failure, so an agent without hooks starts whatever its
/// profile holds. With hooks to write, such a file is an error.
pub fn place(dir: &Path, hooks: &[HookMount]) -> Result<Vec<String>, String> {
    let path = dir.join(SETTINGS_FILE);
    let read = match std::fs::read_to_string(&path) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(map)) => Ok(map),
            _ => Err(format!("{} is not a JSON object", path.display())),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Map::new()),
        Err(e) => Err(format!("cannot read {}: {e}", path.display())),
    };
    if hooks.is_empty() {
        return match read {
            Ok(settings) if !settings.contains_key(HOOKS_KEY) => Ok(Vec::new()),
            Ok(mut settings) => {
                settings.remove(HOOKS_KEY);
                write(&path, settings).map(|()| Vec::new())
            }
            Err(error) => Ok(vec![format!("left as it is, no hooks to remove: {error}")]),
        };
    }
    let mut settings = read?;
    let mut warnings = Vec::new();
    let mut events: Map<String, Value> = Map::new();
    for mount in hooks {
        let (node, event) = (&mount.node, mount.hook.event.as_str());
        let Some(name) = native(event) else {
            warnings.push(format!(
                "Claude Code has no `{event}` hook: `{node}` was left out"
            ));
            continue;
        };
        if mount.hook.returns.contains(&HookEffect::Veto) && !veto_blocks(event) {
            warnings.push(format!(
                "a veto cannot stop `{event}` in Claude Code: `{node}`'s reason only \
                 reaches it as a message"
            ));
        }
        if event == hook_event::STOP && mount.hook.returns.contains(&HookEffect::Context) {
            warnings.push(format!(
                "context on `stop` keeps Claude Code working (\"Stop hook feedback\"): \
                 `{node}` will not let it stop while it answers with context"
            ));
        }
        let mut handler = Map::new();
        handler.insert("type".into(), json!("command"));
        handler.insert("command".into(), json!(command_line(mount)));
        if let Some(ms) = mount.hook.timeout_ms {
            handler.insert(
                "timeout".into(),
                json!(ms.div_ceil(1000) + BRIDGE_SLACK_SECONDS),
            );
        }
        let mut entry = Map::new();
        match claude_matcher(event, mount.hook.matcher.as_deref()) {
            Matcher::Every => {}
            Matcher::Names(names) => {
                entry.insert("matcher".into(), json!(names));
            }
            Matcher::LeftToHook(matcher) => warnings.push(format!(
                "Claude Code would read `{matcher}` as a pattern: it asks `{node}` for \
                 every tool and the hook matches the names itself"
            )),
        }
        entry.insert("hooks".into(), json!([handler]));
        let list = events.entry(name).or_insert_with(|| json!([]));
        if let Value::Array(list) = list {
            list.push(Value::Object(entry));
        }
    }
    if events.is_empty() {
        if settings.remove(HOOKS_KEY).is_some() {
            write(&path, settings)?;
        }
    } else {
        settings.insert(HOOKS_KEY.into(), Value::Object(events));
        write(&path, settings)?;
    }
    Ok(warnings)
}

/// Writes the settings next to the file and renames it over: Claude Code never reads
/// half a file.
fn write(path: &Path, settings: Map<String, Value>) -> Result<(), String> {
    let text = serde_json::to_string_pretty(&Value::Object(settings)).map_err(|e| e.to_string())?;
    let partial = path.with_extension("json.regulus-partial");
    std::fs::write(&partial, text + "\n")
        .and_then(|()| std::fs::rename(&partial, path))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// What Claude Code gets as a hook's matcher.
#[derive(Debug, PartialEq, Eq)]
enum Matcher {
    /// No matcher: every tool, or an event without tools.
    Every,
    /// Exact names joined by `|`, the form Claude Code reads as a list.
    Names(String),
    /// A name Claude Code would read as a regex: no matcher, the hook filters.
    LeftToHook(String),
}

/// The matcher for Claude Code, read as the hook reads it (`regulus_proto::matches_tool`):
/// names separated by `|`, each trimmed, empty ones dropped. Claude Code takes a matcher
/// of only letters, digits, `_`, `-`, spaces, `,` and `|` as exact names, and anything
/// else as an unanchored JavaScript regex (https://code.claude.com/docs/en/hooks), so
/// names are re-joined without spaces, and a name with any other character is left to
/// the hook rather than turned into a pattern that matches something else.
fn claude_matcher(event: &str, matcher: Option<&str>) -> Matcher {
    if !hook_event::WITH_TOOL.contains(&event) {
        return Matcher::Every;
    }
    let raw = matcher.map(str::trim).unwrap_or_default();
    let names: Vec<&str> = raw
        .split('|')
        .map(str::trim)
        .filter(|n| !n.is_empty())
        .collect();
    if names.is_empty() || names == ["*"] {
        return Matcher::Every;
    }
    let plain = |name: &&str| {
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
    };
    if names.iter().all(plain) {
        Matcher::Names(names.join("|"))
    } else {
        Matcher::LeftToHook(raw.to_string())
    }
}

/// The bridge as one shell command, every word quoted: Claude Code runs a hook's
/// `command` through the shell.
fn command_line(mount: &HookMount) -> String {
    let quote = |word: &str| format!("'{}'", word.replace('\'', "'\\''"));
    let program = mount.command.to_string_lossy();
    std::iter::once(quote(&program))
        .chain(mount.args.iter().map(|a| quote(a)))
        .chain([quote(HOOK_FORMAT_FLAG), quote(FORMAT)])
        .collect::<Vec<_>>()
        .join(" ")
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

    fn mount(node: &str, event: &str, matcher: Option<&str>, timeout_ms: Option<u64>) -> HookMount {
        HookMount {
            node: node.into(),
            hook: regulus_proto::Hook {
                event: event.into(),
                matcher: matcher.map(String::from),
                timeout_ms,
                returns: vec![HookEffect::Veto],
            },
            command: "/opt/it's/regulus".into(),
            args: vec!["--home".into(), "/h".into(), "hook".into()],
        }
    }

    #[test]
    fn hooks_go_into_the_profile_settings_and_unwired_ones_go_away() {
        let dir = std::env::temp_dir().join(format!("regulus-claude-hooks-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(SETTINGS_FILE);
        std::fs::write(&path, r#"{ "model": "opus" }"#).unwrap();
        let warnings = place(
            &dir,
            &[
                mount("no-bash", "pre_tool", Some("Bash"), Some(2500)),
                mount("brief", "session_start", Some("Bash"), None),
                mount("any", "pre_tool", Some("Edit.*"), None),
                mount("idle", "on_idle", None, None),
            ],
        )
        .unwrap();
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        assert!(warnings[0].contains("`session_start`"), "{warnings:?}");
        assert!(
            warnings[1].contains("`Edit.*` as a pattern"),
            "{warnings:?}"
        );
        assert!(warnings[2].contains("no `on_idle` hook"), "{warnings:?}");
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings["model"], "opus");
        let pre = &settings["hooks"]["PreToolUse"];
        assert_eq!(pre[0]["matcher"], "Bash");
        let handler = &pre[0]["hooks"][0];
        assert_eq!(handler["type"], "command");
        assert_eq!(handler["timeout"], 4);
        assert_eq!(
            handler["command"],
            r#"'/opt/it'\''s/regulus' '--home' '/h' 'hook' '--format' 'claude'"#
        );
        // A pattern Claude would read as a regex is left to the hook; no matcher off tools.
        assert!(pre[1].get("matcher").is_none());
        assert!(settings["hooks"]["SessionStart"][0]
            .get("matcher")
            .is_none());
        assert!(settings["hooks"]["SessionStart"][0]["hooks"][0]
            .get("timeout")
            .is_none());
        assert!(place(&dir, &[]).unwrap().is_empty());
        let settings: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(settings, json!({ "model": "opus" }));
        std::fs::remove_dir_all(&dir).unwrap();
        // An empty profile with nothing to write stays empty.
        let fresh = dir.join("fresh");
        std::fs::create_dir_all(&fresh).unwrap();
        place(&fresh, &[]).unwrap();
        assert!(!fresh.join(SETTINGS_FILE).exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn matchers_reach_claude_as_the_same_exact_names_the_hook_matches() {
        use regulus_proto::matches_tool;
        let names = |m: &str| claude_matcher("pre_tool", Some(m));
        // Spaces around `|` would make Claude read a regex (`Bash ` or ` Write`).
        for (written, sent) in [
            (" Bash | Write ", "Bash|Write"),
            ("Bash||Write|", "Bash|Write"),
            ("mcp__github__create_issue", "mcp__github__create_issue"),
        ] {
            assert_eq!(names(written), Matcher::Names(sent.into()), "{written}");
            for tool in sent.split('|') {
                assert!(
                    matches_tool(Some(written), Some(tool)),
                    "{written} / {tool}"
                );
            }
        }
        // A `.` would be a regex wildcard in Claude: no matcher, the hook filters exactly.
        assert_eq!(
            names("Bash|my.tool"),
            Matcher::LeftToHook("Bash|my.tool".into())
        );
        assert!(!matches_tool(Some("my.tool"), Some("myXtool")));
        for every in ["", " * ", "|"] {
            assert_eq!(names(every), Matcher::Every, "{every:?}");
        }
        assert_eq!(claude_matcher("stop", Some("Bash")), Matcher::Every);
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("regulus-claude-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn an_odd_settings_file_stops_only_an_agent_with_hooks_to_write() {
        let dir = scratch("odd");
        let path = dir.join(SETTINGS_FILE);
        for odd in ["", "// jsonc\n{}", "[]"] {
            std::fs::write(&path, odd).unwrap();
            let warnings = place(&dir, &[]).unwrap();
            assert_eq!(warnings.len(), 1, "{odd:?}: {warnings:?}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), odd, "untouched");
            let hooks = [mount("no-bash", "pre_tool", Some("Bash"), None)];
            let error = place(&dir, &hooks).unwrap_err();
            assert!(error.contains("is not a JSON object"), "{error}");
        }
        // No hooks and nothing stale: the file is not even rewritten.
        std::fs::write(&path, "{ \"model\": \"opus\" }").unwrap();
        assert!(place(&dir, &[]).unwrap().is_empty());
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "{ \"model\": \"opus\" }"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn each_event_gets_the_output_shape_claude_reads_for_it() {
        let context = HookAnswer::Context {
            text: "note".into(),
        };
        let veto = HookAnswer::Veto {
            reason: "why".into(),
        };
        for (event, name) in EVENTS {
            let out = output(event, &json!({}), &context);
            assert_eq!(out.code, 0, "{event}");
            assert!(out.stderr.is_empty(), "{event}");
            if *event == hook_event::PRE_COMPACT {
                // No `additionalContext` on PreCompact: plain stdout steers the summary.
                assert_eq!(out.stdout, "note");
            } else {
                let parsed: Value = serde_json::from_str(&out.stdout).unwrap();
                assert_eq!(
                    parsed,
                    json!({ "hookSpecificOutput": {
                        "hookEventName": name, "additionalContext": "note" } }),
                    "{event}"
                );
            }
            let out = output(event, &json!({}), &veto);
            assert_eq!(
                (out.code, out.stderr.as_str(), out.stdout.as_str()),
                (2, "why", "")
            );
            assert_eq!(
                output(event, &json!({}), &HookAnswer::Nothing),
                Output::default()
            );
        }
    }

    #[test]
    fn context_on_stop_is_written_with_a_warning() {
        let dir = scratch("stop");
        let mut hook = mount("more", "stop", None, None);
        hook.hook.returns = vec![HookEffect::Context];
        let warnings = place(&dir, &[hook]).unwrap();
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("keeps Claude Code working"),
            "{warnings:?}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
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
