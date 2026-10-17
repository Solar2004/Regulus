//! A scripted ACP agent for the tests, written against the wire format rather than the
//! ACP crate so the client is checked against the protocol itself. Each prompt is a
//! command:
//!
//! - `cwd` answers the session's working directory
//! - `env NAME` answers that environment variable, or `<unset>`
//! - `permission` asks the client for permission and answers the option it chose
//! - `exit` kills the process mid-turn
//! - anything else is echoed back in two chunks

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

fn main() {
    let stdin = io::stdin();
    let mut lines = stdin.lock().lines();
    let mut out = io::stdout();
    let mut cwd = String::new();
    while let Some(Ok(line)) = lines.next() {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        let id = message["id"].clone();
        let params = &message["params"];
        let result = match message["method"].as_str() {
            Some("initialize") => json!({
                "protocolVersion": 1,
                "agentCapabilities": {},
                "agentInfo": { "name": "fake-agent", "version": "0.0.1" }
            }),
            Some("session/new") => {
                cwd = params["cwd"].as_str().unwrap_or_default().to_string();
                json!({ "sessionId": "fake-session" })
            }
            Some("session/prompt") => {
                let session = params["sessionId"].as_str().unwrap_or_default().to_string();
                let text = params["prompt"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                if text == "exit" {
                    std::process::exit(3);
                } else if text == "cwd" {
                    chunk(&mut out, &session, &cwd);
                } else if let Some(name) = text.strip_prefix("env ") {
                    let value = std::env::var(name).unwrap_or_else(|_| "<unset>".into());
                    chunk(&mut out, &session, &value);
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
