//! Hooks (docs/13-blocks.md §4). Today one node, `rule`: a fixed answer for the Agent
//! box's `hooks` input. It tells the box what it listens to (`hook`) and answers the
//! core's `node/hook`: on its event, for the tools its matcher names, a veto with its
//! reason or its context; anything else, nothing.

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError};
use regulus_proto::{
    hook_event, matches_tool, method, Hook, HookAnswer, HookEffect, HookFailure, HookRequest,
    Manifest, NodeCall, HOOK_DESCRIBE,
};
use serde_json::Value;

const MANIFEST: &str = include_str!("../regulus-module.toml");
/// The `deny` value that makes the rule veto.
const YES: &str = "yes";

pub struct Hooks {
    manifest: Manifest,
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn json(value: impl serde::Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(|e| ModuleError::Failed(e.to_string()))
}

/// A rule's settings, defaults filled in.
struct Rule {
    event: String,
    matcher: Option<String>,
    answer: HookAnswer,
    timeout_ms: Option<u64>,
    fail: HookFailure,
}

impl Hooks {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("hooks manifest is valid"),
        }
    }

    fn rule(&self, call: &NodeCall) -> Result<Rule, ModuleError> {
        let text = |field: &str| {
            self.manifest
                .setting(&call.node, field)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        let event = text("event").ok_or_else(|| {
            ModuleError::InvalidParams(format!("rule `{}` names no event", call.node.id))
        })?;
        let answer = match (text("deny").as_deref() == Some(YES), text("context")) {
            (true, _) => HookAnswer::Veto {
                reason: text("reason")
                    .unwrap_or_else(|| format!("blocked by the rule `{}`", call.node.id)),
            },
            (false, Some(text)) => HookAnswer::Context { text },
            (false, None) => HookAnswer::Nothing,
        };
        let timeout_ms = self
            .manifest
            .setting(&call.node, "timeout_ms")
            .and_then(Value::as_u64)
            .filter(|ms| *ms > 0);
        let fail = match text("fail").as_deref() {
            None => HookFailure::Open,
            Some(fail) => serde_json::from_value(Value::String(fail.into())).map_err(|_| {
                ModuleError::InvalidParams(format!(
                    "rule `{}`: `fail` is open or closed, not `{fail}`",
                    call.node.id
                ))
            })?,
        };
        Ok(Rule {
            event,
            matcher: text("matcher"),
            answer,
            timeout_ms,
            fail,
        })
    }

    fn describe(&self, call: &NodeCall) -> Result<Value, ModuleError> {
        let rule = self.rule(call)?;
        let returns = match rule.answer {
            HookAnswer::Veto { .. } => vec![HookEffect::Veto],
            HookAnswer::Context { .. } => vec![HookEffect::Context],
            HookAnswer::Nothing => Vec::new(),
        };
        let with_tool = hook_event::WITH_TOOL.contains(&rule.event.as_str());
        json(Hook {
            matcher: rule.matcher.filter(|_| with_tool),
            event: rule.event,
            timeout_ms: rule.timeout_ms,
            returns,
            fail: rule.fail,
        })
    }

    fn answer(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let rule = self.rule(&call)?;
        let request: HookRequest = parse(call.args)?;
        let with_tool = hook_event::WITH_TOOL.contains(&request.event.as_str());
        let applies = request.event == rule.event
            && (!with_tool || matches_tool(rule.matcher.as_deref(), request.tool.as_deref()));
        json(if applies {
            rule.answer
        } else {
            HookAnswer::Nothing
        })
    }
}

impl Default for Hooks {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Hooks {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, _ctx: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            HOOK_DESCRIBE => self.describe(&parse(params)?),
            method::NODE_HOOK => self.answer(parse(params)?),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(name: &str, config: Value, args: Value) -> Value {
        let node = json!({ "id": "r", "type": "rule", "module_id": "regulus/hooks@0.1.0",
                           "config": config });
        Hooks::new()
            .call(
                name,
                json!({ "execution_id": "exec-1", "node": node, "args": args }),
            )
            .unwrap()
    }

    fn ask(config: Value, event: &str, tool: Option<&str>) -> Value {
        call(
            method::NODE_HOOK,
            config,
            json!({ "event": event, "tool": tool, "raw": {} }),
        )
    }

    #[test]
    fn a_deny_rule_vetoes_its_tools_on_its_event_only() {
        let rule = json!({ "matcher": "Bash|Write", "deny": "yes", "reason": "read only" });
        let veto = json!({ "decision": "veto", "reason": "read only" });
        let none = json!({ "decision": "none" });
        assert_eq!(ask(rule.clone(), "pre_tool", Some("Bash")), veto);
        assert_eq!(ask(rule.clone(), "pre_tool", Some("Write")), veto);
        assert_eq!(ask(rule.clone(), "pre_tool", Some("Read")), none);
        assert_eq!(ask(rule.clone(), "post_tool", Some("Bash")), none);
        let described = call(HOOK_DESCRIBE, rule, Value::Null);
        assert_eq!(
            described,
            json!({ "event": "pre_tool", "matcher": "Bash|Write", "returns": ["veto"] })
        );
        // Without a reason, the veto still says who blocked it.
        let bare = ask(json!({ "deny": "yes" }), "pre_tool", Some("Bash"));
        assert_eq!(bare["reason"], "blocked by the rule `r`");
    }

    #[test]
    fn a_context_rule_adds_its_text_and_ignores_matchers_off_tools() {
        let rule = json!({ "event": "session_start", "matcher": "Bash",
                           "context": "Answer in Spanish.", "timeout_ms": 500 });
        assert_eq!(
            ask(rule.clone(), "session_start", None),
            json!({ "decision": "context", "text": "Answer in Spanish." })
        );
        assert_eq!(
            ask(rule.clone(), "stop", None),
            json!({ "decision": "none" })
        );
        assert_eq!(
            call(HOOK_DESCRIBE, rule, Value::Null),
            json!({ "event": "session_start", "timeout_ms": 500, "returns": ["context"] })
        );
        let closed = call(
            HOOK_DESCRIBE,
            json!({ "deny": "yes", "fail": "closed" }),
            Value::Null,
        );
        assert_eq!(closed["fail"], "closed");
        let node = json!({ "id": "r", "type": "rule", "config": { "fail": "maybe" } });
        let bad = Hooks::new().call(HOOK_DESCRIBE, json!({ "execution_id": "e", "node": node }));
        assert!(bad.is_err());
        assert_eq!(
            ask(json!({}), "pre_tool", Some("Bash")),
            json!({ "decision": "none" })
        );
    }
}
