//! The judge nodes against a real core and a fake model server: what each routes where,
//! what goes over the wire, retries, a plugged-in engine, and the core without the module.

mod common;

use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ModuleCtx, ModuleError, ProfileStore};
use regulus_module_judge::{Ask, Judge, ASK};
use regulus_module_providers::Providers;
use regulus_proto::{method, BusEventKind, EventPage, Manifest, NodeCall, Profile};
use serde_json::{json, Value};

use common::{completion, serve};

fn port(id: &str, capability: &str, direction: &str, kind: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
}

/// `self.ask` → a judge-module node of `kind` with `config`, its provider at `url`, and
/// the outputs it declares.
fn profile(kind: &str, url: &str, config: Value, outputs: &[(&str, &str)]) -> Profile {
    let mut ports = vec![
        port("model", "llm.endpoint@1", "input", "uses"),
        port("engine", "judge.engine@1", "input", "uses"),
        port("in", "text@1", "input", "then"),
        port("gate", "json@1", "input", "then"),
    ];
    ports.extend(
        outputs
            .iter()
            .map(|(id, cap)| port(id, cap, "output", "then")),
    );
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "ports": [port("ask", "text@1", "input", "then"), port("check", "json@1", "input", "then")],
            "nodes": [
                { "id": "model", "type": "provider", "module_id": "regulus/providers@0.1.0",
                  "config": { "protocol": "openai", "base_url": format!("{url}/v1"),
                              "credential": "sk-test", "model": "fake-1" },
                  "ports": [port("endpoint", "llm.endpoint@1", "output", "uses")] },
                { "id": "j", "type": kind, "module_id": "regulus/judge@0.1.0",
                  "config": config, "ports": ports },
            ],
            "wires": [
                { "id": "w1", "kind": "uses", "from": { "node_id": "model", "port_id": "endpoint" },
                  "to": { "node_id": "j", "port_id": "model" } },
                { "id": "w2", "kind": "then", "from": { "node_id": "self", "port_id": "ask" },
                  "to": { "node_id": "j", "port_id": "in" } },
                { "id": "w3", "kind": "then", "from": { "node_id": "self", "port_id": "check" },
                  "to": { "node_id": "j", "port_id": "gate" } },
            ],
        }]
    }))
    .unwrap()
}

fn core_with(profile: Profile, mut modules: Vec<Box<dyn Module>>, without: &[String]) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    modules.push(Box::new(Judge::new()));
    modules.push(Box::new(Providers::new()));
    Core::build(modules, without, profiles)
}

fn start(core: &Core) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    started["id"].as_str().unwrap().to_string()
}

fn send(core: &Core, id: &str, port: &str, value: Value) {
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": port, "value": value }),
    )
    .unwrap();
}

/// Waits until node `j` has emitted on `port`, and returns every port it emitted on
/// with the value, in order.
fn wait_for(core: &Core, id: &str, port: &str) -> Vec<(String, Value)> {
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
            .filter(|e| e.from.node_id == "j")
            .filter_map(|e| match e.kind {
                BusEventKind::Emitted { value } => Some((e.from.port_id, value)),
                _ => None,
            })
            .collect();
        if emitted.iter().any(|(p, _)| p == port) {
            return emitted;
        }
        assert!(
            Instant::now() < deadline,
            "nothing on `{port}`: {emitted:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

const JUDGE_OUT: &[(&str, &str)] = &[
    ("pass", "text@1"),
    ("fail", "text@1"),
    ("feedback", "text@1"),
    ("error", "text@1"),
];

#[test]
fn the_judge_routes_the_value_to_its_verdict_with_a_strict_schema() {
    let fake = serve(|_, _, _| {
        (
            200,
            completion(r#"{"verdict":"fail","feedback":"no tests"}"#),
        )
    });
    let config = json!({ "criteria": "has tests", "verdicts": "pass, fail" });
    let core = core_with(profile("judge", &fake.url, config, JUDGE_OUT), vec![], &[]);
    let id = start(&core);
    send(&core, &id, "ask", json!("fn main() {}"));
    let emitted = wait_for(&core, &id, "fail");
    assert_eq!(
        emitted,
        [
            ("feedback".to_string(), json!("no tests")),
            ("fail".to_string(), json!("fn main() {}"))
        ]
    );
    let seen = fake.seen.lock().unwrap();
    let (path, body, headers) = &seen[0];
    assert_eq!(path, "/v1/chat/completions");
    assert_eq!(body["model"], "fake-1");
    let format = &body["response_format"]["json_schema"];
    assert_eq!(format["strict"], true);
    assert_eq!(
        format["schema"]["properties"]["verdict"]["enum"],
        json!(["pass", "fail"])
    );
    assert!(body["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("has tests"));
    assert!(headers.iter().any(|h| h == "authorization: bearer sk-test"));
}

#[test]
fn an_answer_out_of_schema_is_retried_once_then_fails() {
    let fake = serve(|count, _, _| match count {
        1 => (200, completion("I think it passes")),
        _ => (200, completion(r#"{"verdict":"maybe","feedback":""}"#)),
    });
    let core = core_with(
        profile("judge", &fake.url, json!({}), JUDGE_OUT),
        vec![],
        &[],
    );
    let id = start(&core);
    send(&core, &id, "ask", json!("x"));
    let emitted = wait_for(&core, &id, "error");
    assert_eq!(emitted.len(), 1, "{emitted:?}");
    let error = emitted[0].1.as_str().unwrap();
    assert!(
        error.contains("2 attempt(s)") && error.contains("maybe"),
        "{error}"
    );
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen.len(), 2);
    // The second attempt is told why the first was rejected.
    let retry = seen[1].1["messages"][1]["content"].as_str().unwrap();
    assert!(retry.contains("rejected"), "{retry}");
}

#[test]
fn a_server_that_refuses_json_schema_gets_a_forced_tool_call() {
    let fake = serve(|_, _, body| {
        if body.get("response_format").is_some() {
            return (400, json!({ "error": "response_format unsupported" }));
        }
        let call = json!({ "function": { "name": "verdict",
            "arguments": r#"{"verdict":"pass","feedback":"fine"}"# } });
        (
            200,
            json!({ "choices": [{ "message": { "tool_calls": [call] } }] }),
        )
    });
    let core = core_with(
        profile("judge", &fake.url, json!({}), JUDGE_OUT),
        vec![],
        &[],
    );
    let id = start(&core);
    send(&core, &id, "ask", json!("x"));
    wait_for(&core, &id, "pass");
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen[1].1["tool_choice"]["function"]["name"], "verdict");
}

#[test]
fn the_classifier_routes_by_label_and_the_extractor_emits_json() {
    let fake = serve(|_, _, body| {
        let name = &body["response_format"]["json_schema"]["name"];
        if name == "label" {
            let labels = &body["response_format"]["json_schema"]["schema"]["properties"]["label"];
            assert_eq!(labels["enum"], json!(["bug", "feature", "other"]));
            return (200, completion(r#"{"label":"bug"}"#));
        }
        (
            200,
            completion(r#"{"title":"crash on start","severity":3}"#),
        )
    });
    let outputs = [
        ("bug", "text@1"),
        ("feature", "text@1"),
        ("other", "text@1"),
        ("error", "text@1"),
    ];
    let config = json!({ "labels": "bug, feature" });
    let core = core_with(
        profile("classifier", &fake.url, config, &outputs),
        vec![],
        &[],
    );
    let id = start(&core);
    send(&core, &id, "ask", json!("it crashes"));
    assert_eq!(
        wait_for(&core, &id, "bug"),
        [("bug".to_string(), json!("it crashes"))]
    );

    let schema = json!({ "type": "object", "additionalProperties": false,
        "required": ["title", "severity"],
        "properties": { "title": { "type": "string" }, "severity": { "type": "integer" } } });
    let outputs = [("out", "json@1"), ("error", "text@1")];
    let config = json!({ "schema": schema });
    let core = core_with(
        profile("extractor", &fake.url, config, &outputs),
        vec![],
        &[],
    );
    let id = start(&core);
    send(&core, &id, "ask", json!("it crashes on start, severity 3"));
    assert_eq!(
        wait_for(&core, &id, "out"),
        [(
            "out".to_string(),
            json!({ "title": "crash on start", "severity": 3 })
        )]
    );
}

#[test]
fn the_rubric_mixes_gates_and_judged_leaves() {
    let fake = serve(|_, _, body| {
        let prompt = body["messages"][1]["content"].as_str().unwrap_or_default();
        let verdict = if prompt.contains("readable") {
            "pass"
        } else {
            "fail"
        };
        (
            200,
            completion(&format!(r#"{{"verdict":"{verdict}","feedback":"ok"}}"#)),
        )
    });
    let rubric = json!({ "id": "paper", "children": [
        { "id": "tests", "weight": 2, "gate": "tests" },
        { "id": "prose", "children": [
            { "id": "readable", "criteria": "readable" },
            { "id": "novel", "criteria": "novel" },
        ]},
    ]});
    let outputs = [
        ("score", "json@1"),
        ("results", "json@1"),
        ("pass", "text@1"),
        ("fail", "text@1"),
        ("error", "text@1"),
    ];
    let config = json!({ "rubric": rubric, "threshold": 0.8 });
    let core = core_with(profile("rubric", &fake.url, config, &outputs), vec![], &[]);
    let id = start(&core);
    send(
        &core,
        &id,
        "check",
        json!({ "name": "tests", "exit_code": 0 }),
    );
    // The gate result must be in before the value: the bus delivers in order.
    send(&core, &id, "ask", json!("draft"));
    let emitted = wait_for(&core, &id, "pass");
    let score = emitted[0].1.as_f64().unwrap();
    assert!((score - 2.5 / 3.0).abs() < 1e-9, "{score}");
    let results = &emitted[1].1;
    assert_eq!(results[0]["by"], "gate");
    assert_eq!(results[0]["pass"], true);
    assert_eq!(results[2]["id"], "novel");
    assert_eq!(results[2]["pass"], false);
    assert_eq!(emitted[2], ("pass".to_string(), json!("draft")));
    assert_eq!(fake.seen.lock().unwrap().len(), 2);
}

/// An engine of someone else's: answers every question with the first enum value.
struct FirstChoice(Manifest);

impl Module for FirstChoice {
    fn manifest(&self) -> &Manifest {
        &self.0
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        if name != ASK {
            return Err(ModuleError::UnknownMethod(name.into()));
        }
        let call: NodeCall = serde_json::from_value(params).unwrap();
        let ask: Ask = serde_json::from_value(call.args).unwrap();
        let first = &ask.schema["properties"]["verdict"]["enum"][0];
        Ok(json!({ "verdict": first, "feedback": "by the first choice" }))
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

#[test]
fn a_wired_engine_replaces_the_llm_and_without_the_module_the_rest_runs() {
    let fake = serve(|_, _, _| panic!("the LLM must not be asked"));
    let mut profile = profile(
        "judge",
        &fake.url,
        json!({ "verdicts": "ship, hold" }),
        &[("ship", "text@1"), ("feedback", "text@1")],
    );
    let workflow = &mut profile.workflows[0];
    workflow.nodes.push(
        serde_json::from_value(
            json!({ "id": "mine", "type": "engine", "module_id": "t/first@1.0.0",
            "ports": [port("engine", "judge.engine@1", "output", "uses")] }),
        )
        .unwrap(),
    );
    workflow.wires.push(
        serde_json::from_value(json!({ "id": "w4", "kind": "uses",
            "from": { "node_id": "mine", "port_id": "engine" },
            "to": { "node_id": "j", "port_id": "engine" } }))
        .unwrap(),
    );
    let manifest: Manifest = toml::from_str(
        "[module]\nid = \"t/first@1.0.0\"\nname = \"first\"\n[[node]]\ntype = \"engine\"\nname = \"e\"\n",
    )
    .unwrap();
    let engine = || -> Vec<Box<dyn Module>> { vec![Box::new(FirstChoice(manifest.clone()))] };
    let core = core_with(profile.clone(), engine(), &[]);
    let id = start(&core);
    send(&core, &id, "ask", json!("v1"));
    assert_eq!(
        wait_for(&core, &id, "ship"),
        [
            ("feedback".to_string(), json!("by the first choice")),
            ("ship".to_string(), json!("v1"))
        ]
    );

    let core = core_with(profile, engine(), &["regulus/judge".to_string()]);
    let id = start(&core);
    send(&core, &id, "ask", json!("v1"));
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        let refused = page.events.iter().any(|e| {
            matches!(&e.kind, BusEventKind::Failed { error, .. } if error.contains("not running"))
        });
        if refused {
            break;
        }
        assert!(Instant::now() < deadline, "{page:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_anthropic_protocol_gets_a_forced_tool_use() {
    let fake = serve(|_, _, _| {
        let block = json!({ "type": "tool_use", "name": "verdict",
            "input": { "verdict": "pass", "feedback": "good" } });
        (
            200,
            json!({ "content": [{ "type": "text", "text": "" }, block] }),
        )
    });
    let mut profile = profile("judge", &fake.url, json!({}), JUDGE_OUT);
    let provider = &mut profile.workflows[0].nodes[0].config;
    provider.insert("protocol".into(), json!("anthropic"));
    provider.insert("base_url".into(), json!(fake.url));
    let core = core_with(profile, vec![], &[]);
    let id = start(&core);
    send(&core, &id, "ask", json!("x"));
    wait_for(&core, &id, "pass");
    let seen = fake.seen.lock().unwrap();
    let (path, body, headers) = &seen[0];
    assert_eq!(path, "/v1/messages");
    assert_eq!(
        body["tool_choice"],
        json!({ "type": "tool", "name": "verdict" })
    );
    assert!(headers.iter().any(|h| h.starts_with("anthropic-version:")));
}
