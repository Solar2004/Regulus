//! What the tests of the coding screen share: a session, what the core answers about it,
//! and keys pressed on an [`App`].

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use regulus_proto::{Execution, TemplateSummary};
use serde_json::{json, Value};

use super::app::{App, Chat};
use super::inspect::Ask;
use super::launch::Survey;
use super::link::{Event, Request};
use super::session::{Session, Wanted};
use super::settings::Settings;
use crate::tui::core::{self, Pending};
use crate::tui::keys::Options;

pub const EXEC: &str = "exec-1";

pub fn port(id: &str, capability: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": "uses" })
}

pub fn profile(id: &str) -> regulus_proto::Profile {
    serde_json::from_value(json!({
        "id": id, "main_workflow_id": "code",
        "workflows": [{ "id": "code", "kind": "main", "nodes": [
            { "id": "agent", "type": "agent", "module_id": "regulus/agent@0.1.0",
              "ports": [port("engine", "agent.engine@1", "input")] },
            { "id": "workspace", "type": "workspace", "module_id": "regulus/code@0.1.0",
              "config": { "root": "/work/app" },
              "ports": [port("workspace", "code.workspace@1", "output")] }] }]
    }))
    .unwrap()
}

pub fn execution(id: &str, profile: &str, status: &str) -> Execution {
    serde_json::from_value(json!({
        "id": id, "profile_id": profile, "workflow_id": "code", "project_id": null,
        "state": { "status": status }
    }))
    .unwrap()
}

pub fn session(id: &str) -> Session {
    Session {
        execution: execution(id, "code-app", "running"),
        profile: profile("code-app"),
        agent: "agent".into(),
        workspace: Some("workspace".into()),
        notes: Vec::new(),
        origin: crate::code::session::Origin::Attached,
    }
}

pub fn wanted() -> Wanted {
    Wanted {
        root: Some("/work/app".into()),
        template: "regulus-code".into(),
        ..Wanted::default()
    }
}

pub fn template() -> TemplateSummary {
    serde_json::from_value(json!({
        "name": "regulus-code", "description": "A coding harness", "path": "/t/c.toml",
        "vars": [
            { "name": "base_url", "description": "Where the provider answers", "default": "" },
            { "name": "credential", "description": "secret:<name>", "default": "" },
            { "name": "model", "description": "The model", "default": "qwen3:4b" },
            { "name": "preset", "description": "The provider", "default": "ollama" },
            { "name": "root", "description": "The folder" }],
        "requires": [{ "module": "regulus/code", "version": ">=0.1" }]
    }))
    .unwrap()
}

pub fn survey() -> Survey {
    Survey {
        profiles: Vec::new(),
        executions: Vec::new(),
        modules: Vec::new(),
        manifests: Vec::new(),
        template: Ok(template()),
        in_folder: Vec::new(),
    }
}

pub fn app_for(wanted: Wanted, skip_form: bool) -> App {
    App::new(
        wanted,
        Options::default(),
        Settings::default(),
        1000,
        skip_form,
    )
}

/// An app talking to `exec-1`, with what entering it asks already answered.
pub fn chatting() -> App {
    let mut app = app_for(wanted(), false);
    app.apply(Event::Launched(Ok(Box::new(session(EXEC)))));
    app.apply(history(0, 0, json!([]), json!([]), "idle"));
    app
}

pub fn chat(app: &App) -> &Chat {
    app.chat().expect("a conversation")
}

pub fn press(app: &mut App, code: KeyCode) -> Vec<Request> {
    app.key(KeyEvent::new(code, KeyModifiers::NONE))
}

pub fn ctrl(app: &mut App, c: char) -> Vec<Request> {
    app.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL))
}

pub fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        press(app, KeyCode::Char(c));
    }
}

pub fn say(app: &mut App, text: &str) -> Vec<Request> {
    typed(app, text);
    press(app, KeyCode::Enter)
}

pub fn history(since: u64, next: u64, entries: Value, activity: Value, state: &str) -> Event {
    Event::Core(core::Event::AgentHistory {
        execution: EXEC.into(),
        node: "agent".into(),
        since,
        result: Ok(json!({
            "entries": entries, "activity": activity, "next": next,
            "session": { "engine_session_id": "native-1", "native_resume": false,
                         "state": state }
        })),
    })
}

pub fn entry(seq: u64, role: &str, text: &str, turn: &str) -> Value {
    json!({ "seq": seq, "role": role, "text": text, "turn": turn })
}

pub fn activity(seq: u64, turn: &str, event: Value) -> Value {
    json!({ "seq": seq, "turn": turn, "event": event })
}

pub fn asks(seq: u64, id: &str) -> Value {
    activity(
        seq,
        "t-1",
        json!({ "kind": "permission_request", "id": id, "tool": "code_write_file",
            "title": "Write src/main.rs", "options": [
                { "id": "yes", "kind": "allow_once", "name": "Allow once" },
                { "id": "always", "kind": "allow_always", "name": "Always allow" },
                { "id": "no", "kind": "reject_once", "name": "Reject" }] }),
    )
}

pub fn decision(id: &str, request: &str) -> Pending {
    serde_json::from_value(json!({
        "id": id, "kind": "decision", "execution_id": "exec-9", "node_id": "decider",
        "value": { "context": { "execution_id": EXEC, "node_id": "agent",
                                "request_id": request } }
    }))
    .unwrap()
}

pub fn inspected(ask: Ask, result: Value) -> Event {
    Event::Inspected {
        execution: EXEC.into(),
        node: "workspace".into(),
        ask,
        result: Ok(result),
    }
}

pub fn asked(requests: &[Request]) -> Vec<&Ask> {
    requests
        .iter()
        .filter_map(|request| match request {
            Request::Inspect { ask, .. } => Some(ask),
            _ => None,
        })
        .collect()
}
