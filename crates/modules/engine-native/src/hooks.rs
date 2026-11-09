//! The `hook@1` nodes wired into `context`, asked the way the core's `hook.dispatch` asks
//! for a CLI's bridge (docs/13-blocks.md §4): each matching hook on a thread of its own,
//! within its time limit; a late, failed or undeclared answer counts as nothing, or as a
//! veto when the hook says `fail = closed`; any veto wins, contexts add up.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use regulus_core::{Endpoint, ModuleCtx};
use regulus_proto::{
    hook_event, matches_tool, method, Hook, HookAnswer, HookDispatched, HookFailure, HookReply,
    HookRequest, HOOK_DESCRIBE,
};
use serde_json::Value;

/// The events the loop asks hooks about.
pub const HANDLED: &[&str] = &[
    hook_event::SESSION_START,
    hook_event::PROMPT,
    hook_event::PRE_TOOL,
    hook_event::POST_TOOL,
    hook_event::STOP,
];

/// A hook node wired into `context`, with what it said it listens to.
#[derive(Clone)]
pub struct Wired {
    pub endpoint: Endpoint,
    pub hook: Hook,
}

impl Wired {
    fn id(&self) -> &str {
        &self.endpoint.node().node_id
    }

    fn applies(&self, request: &HookRequest) -> bool {
        self.hook.event == request.event
            && (!hook_event::WITH_TOOL.contains(&request.event.as_str())
                || matches_tool(self.hook.matcher.as_deref(), request.tool.as_deref()))
    }
}

/// Asks every endpoint what it listens to. One that does not say is left out, with a
/// warning; so is one on an event the loop never reaches.
pub fn describe(ctx: &ModuleCtx, endpoints: Vec<Endpoint>) -> (Vec<Wired>, Vec<String>) {
    let mut wired = Vec::new();
    let mut warnings = Vec::new();
    for endpoint in endpoints {
        let id = endpoint.node().node_id.clone();
        let hook = ctx
            .call(&endpoint, HOOK_DESCRIBE, Value::Null)
            .map_err(|e| e.to_string())
            .and_then(|v| serde_json::from_value::<Hook>(v).map_err(|e| e.to_string()));
        match hook {
            Ok(hook) if HANDLED.contains(&hook.event.as_str()) => {
                wired.push(Wired { endpoint, hook })
            }
            Ok(hook) => warnings.push(format!(
                "hook `{id}` listens to `{}`, which the native engine never reaches: left out",
                hook.event
            )),
            Err(error) => warnings.push(format!(
                "hook `{id}` did not say what it listens to ({error}): left out"
            )),
        }
    }
    (wired, warnings)
}

/// Asks the hooks that apply to `request` and combines their answers.
pub fn dispatch(
    ctx: &ModuleCtx,
    hooks: &[Wired],
    request: &HookRequest,
    default_timeout: Duration,
) -> HookAnswer {
    let asked: Vec<&Wired> = hooks.iter().filter(|h| h.applies(request)).collect();
    if asked.is_empty() {
        return HookAnswer::Nothing;
    }
    let body = match serde_json::to_value(request) {
        Ok(body) => body,
        Err(_) => return HookAnswer::Nothing,
    };
    let started = Instant::now();
    let limits: Vec<Duration> = asked
        .iter()
        .map(|h| {
            h.hook
                .timeout_ms
                .map_or(default_timeout, Duration::from_millis)
                .max(Duration::from_millis(1))
        })
        .collect();
    let (sender, answers) = mpsc::channel();
    let mut got: Vec<Option<Result<Value, String>>> = asked.iter().map(|_| None).collect();
    for (index, wired) in asked.iter().enumerate() {
        let (ctx, tx, endpoint, body) = (
            ctx.clone(),
            sender.clone(),
            wired.endpoint.clone(),
            body.clone(),
        );
        let spawned = std::thread::Builder::new()
            .name("regulus-native-hook".into())
            .spawn(move || {
                let answer = ctx
                    .call(&endpoint, method::NODE_HOOK, body)
                    .map_err(|e| e.to_string());
                let _ = tx.send((index, answer));
            });
        if let Err(error) = spawned {
            got[index] = Some(Err(error.to_string()));
        }
    }
    drop(sender);
    let deadline = started + limits.iter().copied().max().unwrap_or(default_timeout);
    while got.iter().any(Option::is_none) {
        let left = deadline.saturating_duration_since(Instant::now());
        match answers.recv_timeout(left) {
            Ok((index, answer)) if started.elapsed() <= limits[index] => got[index] = Some(answer),
            Ok((index, _)) => {
                got[index] = Some(Err(format!(
                    "no answer within {} ms",
                    limits[index].as_millis()
                )))
            }
            Err(_) => break,
        }
    }
    let replies = asked
        .iter()
        .zip(got)
        .zip(&limits)
        .map(|((wired, answer), limit)| {
            let answer = match answer {
                None => Err(format!("no answer within {} ms", limit.as_millis())),
                Some(Err(error)) => Err(error),
                Some(Ok(value)) => HookAnswer::from_value(value)
                    .map_err(|e| format!("bad answer: {e}"))
                    .and_then(|answer| match answer.effect() {
                        Some(effect)
                            if !wired.hook.returns.is_empty()
                                && !wired.hook.returns.contains(&effect) =>
                        {
                            Err(format!(
                                "answered `{}`, which it does not declare",
                                effect.name()
                            ))
                        }
                        _ => Ok(answer),
                    }),
            };
            let (answer, error) = match answer {
                Ok(answer) => (answer, None),
                Err(error) if wired.hook.fail == HookFailure::Closed => (
                    HookAnswer::Veto {
                        reason: format!("hook `{}` fails closed: {error}", wired.id()),
                    },
                    Some(error),
                ),
                Err(error) => {
                    eprintln!(
                        "regulus: native engine: hook `{}` on `{}`: {error}",
                        wired.id(),
                        request.event
                    );
                    (HookAnswer::Nothing, Some(error))
                }
            };
            HookReply {
                node_id: wired.id().to_string(),
                answer,
                error,
            }
        })
        .collect();
    HookDispatched { replies }.combined()
}
