//! The triggers against a real core: Cron ticks, Webhook takes HTTP, Idle fires on
//! silence, Threshold counts, Bus event filters, Chat keeps a transcript; and every
//! timer and listener stops with its execution.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore, Storage, Store};
use regulus_module_hello::Hello;
use regulus_module_triggers::{Triggers, TRANSCRIPT, WEBHOOKS};
use regulus_proto::{method, BusEventKind, EventPage, Profile};
use serde_json::{json, Value};

fn port(id: &str, capability: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": "then",
            "cardinality": if direction == "input" { "many" } else { "one" } })
}

fn node(id: &str, kind: &str, config: Value, ports: Vec<Value>) -> Value {
    json!({ "id": id, "type": kind, "module_id": "regulus/triggers@0.1.0",
            "config": config, "ports": ports })
}

fn wire(id: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": "then",
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

fn profile(ports: Vec<Value>, nodes: Vec<Value>, wires: Vec<Value>) -> Profile {
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "ports": ports, "nodes": nodes, "wires": wires }]
    }))
    .unwrap()
}

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-triggers-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn core(profile: Profile, store: Option<&str>) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Triggers::new()), Box::new(Hello::new())];
    let storage = Storage {
        store: store.map(|name| {
            let config = regulus_core::config::StoreConfig::default();
            Arc::new(Store::open(&temp_home(name), &config).unwrap())
        }),
        ..Storage::default()
    };
    Core::build_with(modules, &[], profiles, storage)
}

fn start(core: &Core) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    started["id"].as_str().unwrap().to_string()
}

fn stop(core: &Core, id: &str) {
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
}

fn send(core: &Core, id: &str, port: &str, value: Value) {
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": port, "value": value }),
    )
    .unwrap();
}

/// What left `node.port` so far.
fn emitted(core: &Core, id: &str, node: &str, port: &str) -> Vec<Value> {
    let page: EventPage = serde_json::from_value(
        core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
            .unwrap(),
    )
    .unwrap();
    page.events
        .into_iter()
        .filter(|e| e.from.node_id == node && e.from.port_id == port)
        .filter_map(|e| match e.kind {
            BusEventKind::Emitted { value } => Some(value),
            _ => None,
        })
        .collect()
}

/// Waits until `node.port` emitted at least `count` values, and returns them.
fn wait_for(core: &Core, id: &str, node: &str, port: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let values = emitted(core, id, node, port);
        if values.len() >= count {
            return values;
        }
        assert!(
            Instant::now() < deadline,
            "{node}.{port}: {} of {count}",
            values.len()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn cron_ticks_until_its_execution_stops() {
    let cron = node(
        "cron",
        "cron",
        json!({ "schedule": "* * * * * *", "timezone": "utc" }),
        vec![port("tick", "text@1", "output")],
    );
    let core = core(profile(vec![], vec![cron], vec![]), None);
    let id = start(&core);
    let ticks = wait_for(&core, &id, "cron", "tick", 2);
    let first = ticks[0].as_str().unwrap();
    assert!(first.ends_with("+00:00"), "{first}");
    stop(&core, &id);
    let after = emitted(&core, &id, "cron", "tick").len();
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(emitted(&core, &id, "cron", "tick").len(), after);
}

/// An HTTP request by hand; returns the status code.
fn post(url: &str, token: Option<&str>, body: &str) -> Result<u16, std::io::Error> {
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_at(rest.find('/').unwrap());
    let mut stream = TcpStream::connect(host)?;
    let auth = token.map_or(String::new(), |t| format!("authorization: Bearer {t}\r\n"));
    write!(
        stream,
        "POST {path} HTTP/1.1\r\nhost: {host}\r\n{auth}content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut answer = String::new();
    stream.read_to_string(&mut answer)?;
    Ok(answer.split_whitespace().nth(1).unwrap().parse().unwrap())
}

#[test]
fn a_webhook_takes_authorized_posts_and_closes_with_its_node() {
    let hook = node(
        "hook",
        "webhook",
        json!({ "bind": "127.0.0.1:0", "path": "/in", "token": "s3cret", "max_body_bytes": 64 }),
        vec![
            port("body", "text@1", "output"),
            port("json", "json@1", "output"),
        ],
    );
    let core = core(profile(vec![], vec![hook], vec![]), None);
    let id = start(&core);
    let listed = core.dispatch(WEBHOOKS, json!({})).unwrap();
    assert_eq!(listed[0]["node_id"], "hook");
    assert_eq!(listed[0]["token"], true);
    let url = listed[0]["url"].as_str().unwrap().to_string();

    assert_eq!(post(&url, None, "{}").unwrap(), 401);
    assert_eq!(post(&url, Some("wrong!"), "{}").unwrap(), 401);
    let other = url.replace("/in", "/elsewhere");
    assert_eq!(post(&other, Some("s3cret"), "{}").unwrap(), 404);
    assert_eq!(post(&url, Some("s3cret"), &"x".repeat(65)).unwrap(), 413);
    assert_eq!(
        post(&url, Some("s3cret"), r#"{"deploy":"v2"}"#).unwrap(),
        202
    );
    assert_eq!(post(&url, Some("s3cret"), "plain words").unwrap(), 202);

    assert_eq!(
        wait_for(&core, &id, "hook", "body", 2),
        [json!(r#"{"deploy":"v2"}"#), json!("plain words")]
    );
    assert_eq!(
        emitted(&core, &id, "hook", "json"),
        [json!({ "deploy": "v2" })]
    );

    stop(&core, &id);
    assert_eq!(core.dispatch(WEBHOOKS, json!({})).unwrap(), json!([]));
    assert!(post(&url, Some("s3cret"), "{}").is_err(), "still listening");
}

#[test]
fn idle_fires_once_per_silence_and_threshold_counts() {
    let nodes = vec![
        node(
            "quiet",
            "idle",
            json!({ "after_ms": 300 }),
            vec![
                port("in", "text@1", "input"),
                port("idle", "text@1", "output"),
            ],
        ),
        node(
            "third",
            "threshold",
            json!({ "count": 3 }),
            vec![
                port("in", "text@1", "input"),
                port("reached", "json@1", "output"),
            ],
        ),
    ];
    let wires = vec![
        wire("w1", ("self", "ping"), ("quiet", "in")),
        wire("w2", ("self", "ping"), ("third", "in")),
    ];
    let ports = vec![port("ping", "text@1", "input")];
    let core = core(profile(ports, nodes, wires), None);
    let id = start(&core);
    let started = Instant::now();
    wait_for(&core, &id, "quiet", "idle", 1);
    assert!(started.elapsed() >= Duration::from_millis(300));
    // Once per silence.
    std::thread::sleep(Duration::from_millis(500));
    assert_eq!(emitted(&core, &id, "quiet", "idle").len(), 1);

    for n in 1..=7 {
        send(&core, &id, "ping", json!(format!("p{n}")));
    }
    let reached = wait_for(&core, &id, "third", "reached", 2);
    assert_eq!(
        reached,
        [
            json!({ "count": 3, "value": "p3" }),
            json!({ "count": 3, "value": "p6" })
        ]
    );
    // The pings broke the silence: it fires again after it.
    wait_for(&core, &id, "quiet", "idle", 2);
    stop(&core, &id);
}

#[test]
fn bus_event_fires_only_on_what_matches() {
    let watch = node(
        "watch",
        "bus_event",
        json!({ "from_node": "self", "contains": "deploy" }),
        vec![
            port("in", "text@1", "input"),
            port("fired", "json@1", "output"),
        ],
    );
    let wires = vec![wire("w1", ("self", "log"), ("watch", "in"))];
    let core = core(
        profile(vec![port("log", "text@1", "input")], vec![watch], wires),
        None,
    );
    let id = start(&core);
    send(&core, &id, "log", json!("build ok"));
    send(&core, &id, "log", json!("deploy v2"));
    let fired = wait_for(&core, &id, "watch", "fired", 1);
    assert_eq!(fired[0]["value"], "deploy v2");
    assert_eq!(
        fired[0]["from"],
        json!({ "node_id": "self", "port_id": "log" })
    );
    assert_eq!(fired[0]["capability"], "text@1");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(emitted(&core, &id, "watch", "fired").len(), 1);
}

#[test]
fn chat_relays_both_ways_and_keeps_the_transcript_in_the_store() {
    let chat = node(
        "chat",
        "chat",
        json!({ "history_turns": 2 }),
        vec![
            port("user", "text@1", "input"),
            port("message", "text@1", "output"),
            port("reply", "text@1", "input"),
            port("out", "text@1", "output"),
        ],
    );
    // Hello stands in for the agent: it greets whatever it is told.
    let agent = json!({ "id": "agent", "type": "greeter", "module_id": "regulus/hello@0.1.0",
        "ports": [port("echo", "text@1", "input"), port("reply", "text@1", "output")] });
    let wires = vec![
        wire("w1", ("self", "chat"), ("chat", "user")),
        wire("w2", ("chat", "message"), ("agent", "echo")),
        wire("w3", ("agent", "reply"), ("chat", "reply")),
        wire("w4", ("chat", "out"), ("self", "reply")),
    ];
    let ports = vec![
        port("chat", "text@1", "input"),
        port("reply", "text@1", "output"),
    ];
    let core = core(profile(ports, vec![chat, agent], wires), Some("chat"));
    let id = start(&core);
    send(&core, &id, "chat", json!("Ada"));
    assert_eq!(
        wait_for(&core, &id, "self", "reply", 1),
        [json!("Hello, Ada!")]
    );
    send(&core, &id, "chat", json!("Bob"));
    let replies = wait_for(&core, &id, "self", "reply", 2);
    // The second message carried the last two turns before it.
    assert_eq!(
        replies[1],
        json!("Hello, user: Ada\nassistant: Hello, Ada!\nuser: Bob!")
    );
    let turns = core
        .dispatch(TRANSCRIPT, json!({ "execution_id": id, "node_id": "chat" }))
        .unwrap();
    let roles: Vec<&str> = turns
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["role"].as_str().unwrap())
        .collect();
    assert_eq!(roles, ["user", "assistant", "user", "assistant"]);
    assert_eq!(turns[0]["text"], "Ada");
    stop(&core, &id);
    // The store keeps it after the execution stopped.
    let kept = core
        .dispatch(TRANSCRIPT, json!({ "execution_id": id, "node_id": "chat" }))
        .unwrap();
    assert_eq!(kept.as_array().unwrap().len(), 4);
}

#[test]
fn the_core_shutting_down_stops_timers_and_listeners() {
    let nodes = vec![
        node(
            "hook",
            "webhook",
            json!({ "bind": "127.0.0.1:0" }),
            vec![port("body", "text@1", "output")],
        ),
        node(
            "cron",
            "cron",
            json!({ "schedule": "* * * * * *" }),
            vec![port("tick", "text@1", "output")],
        ),
    ];
    let core = core(profile(vec![], nodes, vec![]), None);
    let id = start(&core);
    let url = core.dispatch(WEBHOOKS, json!({})).unwrap()[0]["url"]
        .as_str()
        .unwrap()
        .to_string();
    assert_eq!(post(&url, None, "hi").unwrap(), 202);
    let started = Instant::now();
    core.shutdown();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(post(&url, None, "hi").is_err(), "still listening");
    let ticks = emitted(&core, &id, "cron", "tick").len();
    std::thread::sleep(Duration::from_millis(1200));
    assert_eq!(emitted(&core, &id, "cron", "tick").len(), ticks);
}

#[test]
fn the_example_checks_clean() {
    let example: Profile = toml::from_str(include_str!("../examples/triggers.toml")).unwrap();
    let id = example.id.clone();
    let core = core(example, None);
    let check = core
        .dispatch(method::PROFILE_CHECK, json!({ "id": id }))
        .unwrap();
    assert_eq!(check, json!([]));
}
