//! `complete` against a real core and a fake model server, on both protocols: what goes
//! over the wire, usage, retries of what may pass, a 401 that is not retried and never
//! shows the key.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore};
use regulus_module_inference::Inference;
use regulus_module_providers::Providers;
use regulus_proto::{method, BusEventKind, EventPage, Profile};
use serde_json::{json, Value};

use common::{completion, serve};

fn port(id: &str, capability: &str, direction: &str, kind: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
}

/// `self.ask` → `complete` with `config`, its provider at `base_url` speaking `protocol`.
fn profile(protocol: &str, base_url: &str, config: Value) -> Profile {
    let auth = if protocol == "anthropic" {
        "x-api-key"
    } else {
        "bearer"
    };
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "ports": [port("ask", "json@1", "input", "then")],
            "nodes": [
                { "id": "model", "type": "provider", "module_id": "regulus/providers@0.1.0",
                  "config": { "protocol": protocol, "base_url": base_url, "auth": auth,
                              "credential": "sk-secret-key", "model": "fake-1" },
                  "ports": [port("endpoint", "llm.endpoint@1", "output", "uses")] },
                { "id": "c", "type": "complete", "module_id": "regulus/inference@0.1.0",
                  "config": config, "ports": [
                    { "id": "model", "capability": "llm.endpoint@1", "direction": "input",
                      "kind": "uses", "required": true },
                    port("in", "json@1", "input", "then"),
                    port("out", "text@1", "output", "then"),
                    port("usage", "json@1", "output", "then"),
                    port("error", "error@1", "output", "then"),
                  ] },
            ],
            "wires": [
                { "id": "w1", "kind": "uses", "from": { "node_id": "model", "port_id": "endpoint" },
                  "to": { "node_id": "c", "port_id": "model" } },
                { "id": "w2", "kind": "then", "from": { "node_id": "self", "port_id": "ask" },
                  "to": { "node_id": "c", "port_id": "in" } },
            ],
        }]
    }))
    .unwrap()
}

fn core(profile: Profile) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> =
        vec![Box::new(Inference::new()), Box::new(Providers::new())];
    Core::build(modules, &[], profiles)
}

fn run(core: &Core, value: Value) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    let id = started["id"].as_str().unwrap().to_string();
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": "ask", "value": value }),
    )
    .unwrap();
    id
}

/// Every port `c` emitted on, with the value, once `out` or `error` came.
fn wait(core: &Core, id: &str) -> Vec<(String, Value)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        let emitted: Vec<(String, Value)> = page
            .events
            .into_iter()
            .filter(|e| e.from.node_id == "c")
            .filter_map(|e| match e.kind {
                BusEventKind::Emitted { value } => Some((e.from.port_id, value)),
                _ => None,
            })
            .collect();
        if emitted.iter().any(|(p, _)| p == "out" || p == "error") {
            return emitted;
        }
        assert!(Instant::now() < deadline, "nothing yet: {emitted:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn openai_chat_completions_with_system_and_usage() {
    let fake = serve(|_, _, _| {
        let mut answer = completion("Hello there.");
        answer["usage"] = json!({ "prompt_tokens": 12, "completion_tokens": 3 });
        (200, answer)
    });
    let config = json!({ "system": "Be brief.", "max_tokens": 50 });
    let core = core(profile("openai", &format!("{}/v1", fake.url), config));
    let id = run(&core, json!("Say hello"));
    let emitted = wait(&core, &id);
    let usage = &emitted[0];
    assert_eq!(usage.0, "usage");
    assert_eq!(usage.1["input_tokens"], 12);
    assert_eq!(usage.1["total_tokens"], 15);
    assert_eq!(usage.1["model"], "fake-1");
    assert_eq!(usage.1["attempts"], 1);
    assert_eq!(emitted[1], ("out".to_string(), json!("Hello there.")));
    let seen = fake.seen.lock().unwrap();
    let (path, body, headers) = &seen[0];
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(
        body["messages"],
        json!([{ "role": "system", "content": "Be brief." },
               { "role": "user", "content": "Say hello" }])
    );
    assert_eq!(
        (&body["max_tokens"], &body["temperature"]),
        (&json!(50), &json!(0.2))
    );
    assert!(headers
        .iter()
        .any(|h| h == "authorization: bearer sk-secret-key"));
}

#[test]
fn anthropic_messages_take_the_json_shape() {
    let fake = serve(|_, _, _| {
        (
            200,
            json!({ "content": [{ "type": "text", "text": "Hi " }, { "type": "text", "text": "you." }],
                      "usage": { "input_tokens": 7, "output_tokens": 2 } }),
        )
    });
    let core = core(profile(
        "anthropic",
        &fake.url,
        json!({ "system": "ignored" }),
    ));
    let id = run(
        &core,
        json!({ "system": "Be kind.", "prompt": "Greet me", "max_tokens": 9 }),
    );
    let emitted = wait(&core, &id);
    assert_eq!(
        emitted.last().unwrap(),
        &("out".to_string(), json!("Hi you."))
    );
    assert_eq!(emitted[0].1["output_tokens"], 2);
    let seen = fake.seen.lock().unwrap();
    let (path, body, headers) = &seen[0];
    assert_eq!(path, "/v1/messages");
    assert_eq!(body["system"], "Be kind.");
    assert_eq!(body["max_tokens"], 9);
    assert_eq!(
        body["messages"],
        json!([{ "role": "user", "content": "Greet me" }])
    );
    assert!(headers.iter().any(|h| h == "anthropic-version: 2023-06-01"));
    assert!(headers.iter().any(|h| h == "x-api-key: sk-secret-key"));
}

#[test]
fn a_server_error_is_retried_and_counted() {
    let fake = serve(|count, _, _| match count {
        1 => (503, json!({ "error": "busy" })),
        _ => (200, completion("second time")),
    });
    let config = json!({ "retries": 1, "backoff_ms": 1 });
    let core = core(profile("openai", &format!("{}/v1", fake.url), config));
    let id = run(&core, json!("again?"));
    let emitted = wait(&core, &id);
    assert_eq!(emitted.last().unwrap().1, json!("second time"));
    assert_eq!(emitted[0].1["attempts"], 2);
    assert_eq!(fake.seen.lock().unwrap().len(), 2);
}

#[test]
fn a_401_is_not_retried_and_hides_the_key() {
    let fake = serve(|_, _, _| (401, json!({ "error": "bad key sk-secret-key" })));
    let config = json!({ "retries": 3, "backoff_ms": 1 });
    let core = core(profile("openai", &format!("{}/v1", fake.url), config));
    let id = run(&core, json!("hi"));
    let emitted = wait(&core, &id);
    let (port, error) = emitted.last().unwrap();
    assert_eq!(port, "error");
    let message = error["message"].as_str().unwrap();
    assert!(message.contains("401"), "{message}");
    assert!(!message.contains("sk-secret-key"), "{message}");
    // error@1, as guards' `error` node gives it.
    assert_eq!(error["value"], "hi");
    assert_eq!(
        error["from"],
        json!({ "node_id": "self", "port_id": "ask" })
    );
    assert!(error["at"].as_u64().unwrap() > 0);
    assert_eq!(fake.seen.lock().unwrap().len(), 1);
}

#[test]
fn a_value_without_a_prompt_and_a_dead_server_go_to_error() {
    let core = core(profile(
        "openai",
        "http://127.0.0.1:1/v1",
        json!({ "retries": 1, "backoff_ms": 1 }),
    ));
    let id = run(&core, json!({ "system": "x" }));
    let emitted = wait(&core, &id);
    assert!(emitted[0].1["message"]
        .as_str()
        .unwrap()
        .contains("`prompt`"));
    let id = run(&core, json!("hello"));
    let emitted = wait(&core, &id);
    let message = emitted[0].1["message"].as_str().unwrap();
    assert!(message.contains("2 attempt(s)"), "{message}");
}

#[test]
fn without_a_provider_the_node_is_disabled_not_broken() {
    let mut profile: Value =
        serde_json::to_value(profile("openai", "http://x/v1", json!({}))).unwrap();
    profile["workflows"][0]["wires"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    let core = core(serde_json::from_value(profile).unwrap());
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    assert_eq!(
        started["disabled"],
        json!([{ "node_id": "c", "missing": ["model"] }]),
        "{started}"
    );
}
