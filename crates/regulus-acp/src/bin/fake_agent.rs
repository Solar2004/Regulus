//! A scripted ACP agent for the tests, written against the wire format rather than the
//! ACP crate so the client is checked against the protocol itself. Each prompt is a
//! command:
//!
//! - `cwd` answers the session's working directory
//! - `env NAME` answers that environment variable, or `<unset>`
//! - `permission` asks the client for permission and answers the option it chose
//! - `provider` answers what `providers/set` received before the session, or `<unset>`
//! - `meta` answers the `_meta` of `session/new`
//! - `exit` kills the process mid-turn, saying so on stderr
//! - `hang` answers nothing until `session/cancel`, then ends the turn as `cancelled`
//! - `deaf` never answers, cancel or not
//! - `tool` reports a tool call, its completion and then a text
//! - `ask` reports a tool call, asks permission for it and answers `chose <option id>`
//!   (`chose cancelled` if the client cancelled the request)
//! - `think` streams a thought chunk, then a text
//! - `session` answers the session id the prompt came in
//! - anything else is echoed back in two chunks
//!
//! With `REGULUS_FAKE_LOAD_SESSION` set it advertises `loadSession`: `session/load`
//! replays an old exchange (a user chunk and an agent chunk) before answering, as ACP
//! agents do, and the session keeps the id it was loaded by.

use std::io::{self, BufRead, Write};

use serde_json::{json, Value};

fn send(out: &mut impl Write, message: Value) {
    writeln!(out, "{message}").expect("stdout is open");
    out.flush().expect("stdout is open");
}

fn chunk(out: &mut impl Write, session: &str, text: &str) {
    let update = json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } });
    send(
        out,
        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": session, "update": update } }),
    );
}

fn update(out: &mut impl Write, session: &str, update: Value) {
    send(
        out,
        json!({ "jsonrpc": "2.0", "method": "session/update", "params": { "sessionId": session, "update": update } }),
    );
}

/// Sends a permission request and waits for its answer; returns the chosen option id,
/// or the outcome (`cancelled`) when none was chosen.
fn ask(
    out: &mut impl Write,
    lines: &mut impl Iterator<Item = io::Result<String>>,
    session: &str,
    request: &str,
    tool_call: Value,
) -> String {
    let options = json!([
        { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
        { "optionId": "always", "name": "Always allow", "kind": "allow_always" },
        { "optionId": "no", "name": "Reject", "kind": "reject_once" }
    ]);
    send(
        out,
        json!({ "jsonrpc": "2.0", "id": request, "method": "session/request_permission",
        "params": { "sessionId": session, "toolCall": tool_call, "options": options } }),
    );
    let chosen = lines
        .by_ref()
        .map_while(Result::ok)
        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
        .find(|m| m["id"] == request)
        .map(|m| m["result"]["outcome"].clone())
        .unwrap_or(Value::Null);
    chosen["optionId"]
        .as_str()
        .or(chosen["outcome"].as_str())
        .unwrap_or("none")
        .to_string()
}

fn main() {
    let loads = std::env::var_os("REGULUS_FAKE_LOAD_SESSION").is_some();
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut out = io::stdout();
    let mut cwd = String::new();
    let mut provider = Value::Null;
    let mut meta = Value::Null;
    while let Some(Ok(line)) = lines.next() {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message["id"].clone();
        let params = &message["params"];
        let result = match message["method"].as_str() {
            Some("initialize") => json!({
                "protocolVersion": 1,
                "agentCapabilities": { "loadSession": loads },
                "agentInfo": { "name": "fake-agent", "version": "0.0.1" }
            }),
            Some("session/new") => {
                cwd = params["cwd"].as_str().unwrap_or_default().to_string();
                meta = params["_meta"].clone();
                json!({ "sessionId": "fake-session" })
            }
            Some("session/load") if loads => {
                cwd = params["cwd"].as_str().unwrap_or_default().to_string();
                let session = params["sessionId"].as_str().unwrap_or_default().to_string();
                for (kind, text) in [
                    ("user_message_chunk", "old question"),
                    ("agent_message_chunk", "old answer"),
                ] {
                    let replayed = json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } });
                    update(&mut out, &session, replayed);
                }
                json!({})
            }
            Some("providers/set") => {
                provider = params.clone();
                json!({})
            }
            Some("session/prompt") => {
                let session = params["sessionId"].as_str().unwrap_or_default().to_string();
                let text = params["prompt"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if text == "deaf" {
                    continue;
                }
                if text == "hang" {
                    let cancelled = lines
                        .by_ref()
                        .map_while(Result::ok)
                        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
                        .any(|m| m["method"] == "session/cancel");
                    if !cancelled {
                        return;
                    }
                    chunk(&mut out, &session, "hung until cancelled");
                    send(
                        &mut out,
                        json!({ "jsonrpc": "2.0", "id": id, "result": { "stopReason": "cancelled" } }),
                    );
                    continue;
                }
                if text == "exit" {
                    eprintln!("fake-agent: told to exit");
                    std::process::exit(3);
                } else if text == "cwd" {
                    chunk(&mut out, &session, &cwd);
                } else if let Some(name) = text.strip_prefix("env ") {
                    let value = std::env::var(name).unwrap_or_else(|_| "<unset>".into());
                    chunk(&mut out, &session, &value);
                } else if text == "provider" {
                    let answer = match &provider {
                        Value::Null => "<unset>".to_string(),
                        set => set.to_string(),
                    };
                    chunk(&mut out, &session, &answer);
                } else if text == "tool" {
                    update(
                        &mut out,
                        &session,
                        json!({ "sessionUpdate": "tool_call", "toolCallId": "call-1",
                                "title": "read a file", "kind": "read", "status": "pending" }),
                    );
                    update(
                        &mut out,
                        &session,
                        json!({ "sessionUpdate": "tool_call_update", "toolCallId": "call-1",
                                "status": "completed", "content": [{ "type": "content",
                                "content": { "type": "text", "text": "file body" } }] }),
                    );
                    chunk(&mut out, &session, "tool done");
                } else if text == "ask" {
                    let tool_call = json!({ "toolCallId": "call-2", "title": "write a file",
                                            "kind": "edit", "status": "pending" });
                    update(
                        &mut out,
                        &session,
                        json!({ "sessionUpdate": "tool_call", "toolCallId": "call-2",
                                "title": "write a file", "kind": "edit", "status": "pending" }),
                    );
                    let picked = ask(&mut out, &mut lines, &session, "ask-2", tool_call);
                    chunk(&mut out, &session, &format!("chose {picked}"));
                } else if text == "think" {
                    let thought = json!({ "sessionUpdate": "agent_thought_chunk",
                                          "content": { "type": "text", "text": "thinking it over" } });
                    update(&mut out, &session, thought);
                    chunk(&mut out, &session, "thought done");
                } else if text == "session" {
                    chunk(&mut out, &session, &session);
                } else if text == "meta" {
                    chunk(&mut out, &session, &meta.to_string());
                } else if text == "permission" {
                    let options = json!([
                        { "optionId": "yes", "name": "Allow", "kind": "allow_once" },
                        { "optionId": "no", "name": "Reject", "kind": "reject_once" }
                    ]);
                    let tool_call = json!({ "toolCallId": "t1", "title": "write a file" });
                    send(
                        &mut out,
                        json!({ "jsonrpc": "2.0", "id": "ask-1", "method": "session/request_permission",
                        "params": { "sessionId": session, "toolCall": tool_call, "options": options } }),
                    );
                    let chosen = lines
                        .by_ref()
                        .map_while(Result::ok)
                        .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
                        .find(|m| m["id"] == "ask-1")
                        .map(|m| m["result"]["outcome"].clone())
                        .unwrap_or(Value::Null);
                    let picked = chosen["optionId"]
                        .as_str()
                        .or(chosen["outcome"].as_str())
                        .unwrap_or("none");
                    chunk(&mut out, &session, picked);
                } else {
                    chunk(&mut out, &session, "echo: ");
                    chunk(&mut out, &session, &text);
                }
                json!({ "stopReason": "end_turn" })
            }
            _ if id.is_null() => continue,
            _ => {
                send(
                    &mut out,
                    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": -32601, "message": "Method not found" } }),
                );
                continue;
            }
        };
        send(
            &mut out,
            json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        );
    }
}
