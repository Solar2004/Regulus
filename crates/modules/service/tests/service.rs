//! The service against a real core: a key calls an exposed workflow over HTTP and gets
//! its answer; an unknown key gets 401, one out of scope 403, one past its policy 429;
//! an async call gives a job to poll; every request lands in the log.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use regulus_core::config::StoreConfig;
use regulus_core::{Core, Module, ProfileStore, Storage, Store};
use regulus_module_hello::Hello;
use regulus_module_human::Human;
use regulus_module_service::{Service, ENDPOINTS, KEY_LIST, KEY_NEW, KEY_REVOKE, LOG};
use regulus_proto::{method, Profile};
use serde_json::{json, Value};

fn temp_home(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-service-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn json_port(id: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": "json@1", "direction": direction, "kind": "then" })
}

fn profile(policies: Value) -> Profile {
    let then = |id: &str, direction: &str| json!({ "id": id, "capability": "text@1", "direction": direction, "kind": "then" });
    let engine = |direction: &str| {
        json!({ "id": "engine", "capability": "service.engine@1", "direction": direction,
                "kind": "uses", "required": direction == "input" })
    };
    serde_json::from_value(json!({
        "id": "p", "project_id": "proj", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main",
              "nodes": [
                { "id": "http", "type": "http", "module_id": "regulus/service@0.1.0",
                  "config": { "bind": "127.0.0.1:0" }, "ports": [engine("output")] },
                { "id": "door", "type": "endpoint", "module_id": "regulus/service@0.1.0",
                  "config": { "workflows": ["greet", "hold"], "policies": policies,
                              "timeout_ms": 5000 },
                  "ports": [engine("input")] }
              ],
              "wires": [{ "id": "e", "kind": "uses",
                          "from": { "node_id": "http", "port_id": "engine" },
                          "to": { "node_id": "door", "port_id": "engine" } }] },
            { "id": "greet", "kind": "extra",
              "ports": [then("ask", "input"), then("reply", "output")],
              "nodes": [{ "id": "hello", "type": "greeter", "module_id": "regulus/hello@0.1.0",
                          "ports": [then("echo", "input"), then("reply", "output")] }],
              "wires": [
                { "id": "a", "kind": "then", "from": { "node_id": "self", "port_id": "ask" },
                  "to": { "node_id": "hello", "port_id": "echo" } },
                { "id": "b", "kind": "then", "from": { "node_id": "hello", "port_id": "reply" },
                  "to": { "node_id": "self", "port_id": "reply" } }
              ] },
            // A request that stays in flight until the test approves it.
            { "id": "hold", "kind": "extra",
              "ports": [json_port("ask", "input"), json_port("reply", "output")],
              "nodes": [{ "id": "wait", "type": "approval", "module_id": "regulus/human@0.1.0",
                          "ports": [json_port("in", "input"), json_port("approved", "output")] }],
              "wires": [
                { "id": "a", "kind": "then", "from": { "node_id": "self", "port_id": "ask" },
                  "to": { "node_id": "wait", "port_id": "in" } },
                { "id": "b", "kind": "then", "from": { "node_id": "wait", "port_id": "approved" },
                  "to": { "node_id": "self", "port_id": "reply" } }
              ] }
        ]
    }))
    .unwrap()
}

fn core(name: &str, policies: Value) -> Arc<Core> {
    let home = temp_home(name);
    let mut profiles = ProfileStore::default();
    profiles.insert(profile(policies)).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![
        Box::new(Service::new()),
        Box::new(Hello::new()),
        Box::new(Human::new()),
    ];
    let storage = Storage {
        store: Some(Arc::new(
            Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
        )),
        data_dir: Some(home.join("data")),
        ..Storage::default()
    };
    Core::build_with(modules, &[], profiles, storage)
}

fn start(core: &Core) -> (String, String) {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    let endpoints = core.dispatch(ENDPOINTS, json!({})).unwrap();
    let url = endpoints[0]["url"].as_str().unwrap();
    (
        started["id"].as_str().unwrap().to_string(),
        url.trim_start_matches("http://").to_string(),
    )
}

/// One HTTP request; returns status, `retry-after` and the JSON body.
fn request(
    addr: &str,
    method: &str,
    path: &str,
    key: Option<&str>,
    body: &str,
) -> (u16, Option<u64>, Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let auth = key
        .map(|k| format!("authorization: Bearer {k}\r\n"))
        .unwrap_or_default();
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nhost: {addr}\r\n{auth}content-length: {}\r\n\r\n{body}",
        body.len()
    )
    .unwrap();
    let mut text = String::new();
    stream.read_to_string(&mut text).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap();
    let status = head[9..12].parse().unwrap();
    let retry = head
        .lines()
        .find_map(|l| l.strip_prefix("retry-after: "))
        .map(|s| s.trim().parse().unwrap());
    (
        status,
        retry,
        serde_json::from_str(body).unwrap_or(Value::Null),
    )
}

fn new_key(core: &Core, scope: &str, policy: Value) -> (String, String) {
    let key = core
        .dispatch(
            KEY_NEW,
            json!({ "scope": [scope], "name": "test", "policy": policy }),
        )
        .unwrap();
    (
        key["id"].as_str().unwrap().to_string(),
        key["key"].as_str().unwrap().to_string(),
    )
}

#[test]
fn a_key_calls_a_workflow_and_the_door_says_no_when_it_must() {
    let core = core(
        "call",
        json!({ "greet": { "requests": 2, "window_ms": 60000 } }),
    );
    let (_, addr) = start(&core);
    let (id, key) = new_key(&core, "proj/greet", json!({}));
    assert!(core.dispatch(KEY_LIST, json!({})).unwrap()[0]
        .get("hash")
        .is_none());

    let (status, _, body) = request(
        &addr,
        "POST",
        "/v1/workflows/greet",
        Some(&key),
        r#"{"ask":"Ada"}"#,
    );
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["outputs"]["reply"], "Hello, Ada!");

    assert_eq!(
        request(&addr, "POST", "/v1/workflows/greet", None, "{}").0,
        401
    );
    assert_eq!(
        request(&addr, "POST", "/v1/workflows/greet", Some("rgk_nope"), "{}").0,
        401
    );
    let (_, other) = new_key(&core, "elsewhere", json!({}));
    assert_eq!(
        request(&addr, "POST", "/v1/workflows/greet", Some(&other), "{}").0,
        403
    );
    assert_eq!(
        request(
            &addr,
            "POST",
            "/v1/workflows/greet",
            Some(&key),
            r#"{"nope":1}"#
        )
        .0,
        400
    );

    // The window lets two in; the third waits.
    assert_eq!(
        request(
            &addr,
            "POST",
            "/v1/workflows/greet",
            Some(&key),
            r#"{"ask":"Bo"}"#
        )
        .0,
        200
    );
    let (status, retry, _) = request(
        &addr,
        "POST",
        "/v1/workflows/greet",
        Some(&key),
        r#"{"ask":"Cy"}"#,
    );
    assert_eq!(status, 429);
    assert!(retry.is_some_and(|s| s > 0 && s <= 60), "{retry:?}");

    core.dispatch(KEY_REVOKE, json!({ "id": id })).unwrap();
    assert_eq!(
        request(
            &addr,
            "POST",
            "/v1/workflows/greet",
            Some(&key),
            r#"{"ask":"Di"}"#
        )
        .0,
        401
    );

    let log = core.dispatch(LOG, json!({ "key": id })).unwrap();
    let statuses: Vec<u64> = log
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["status"].as_u64().unwrap())
        .collect();
    assert!(
        statuses.contains(&200) && statuses.contains(&429) && statuses.contains(&400),
        "{statuses:?}"
    );
    let ok = log
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["status"] == 200)
        .unwrap();
    assert_eq!(ok["workflow"], "greet");
    assert_eq!(ok["project"], "proj");
    assert!(!log.to_string().contains(&key), "the log never holds a key");
}

#[test]
fn an_async_call_gives_a_job_to_poll_and_a_key_policy_has_a_cooldown() {
    let core = core("async", json!({}));
    let (_, addr) = start(&core);
    let (_, key) = new_key(&core, "*", json!({ "cooldown_ms": 60000 }));
    let (status, _, body) = request(
        &addr,
        "POST",
        "/v1/workflows/greet?async=1",
        Some(&key),
        r#"{"ask":"Eve"}"#,
    );
    assert_eq!(status, 202, "{body}");
    let job = body["job"].as_str().unwrap().to_string();
    let mut polled = Value::Null;
    for _ in 0..200 {
        polled = request(&addr, "GET", &format!("/v1/jobs/{job}"), Some(&key), "").2;
        if polled["status"] != "running" {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(polled["status"], "done", "{polled}");
    assert_eq!(polled["outputs"]["reply"], "Hello, Eve!");

    let (status, retry, _) = request(
        &addr,
        "POST",
        "/v1/workflows/greet",
        Some(&key),
        r#"{"ask":"F"}"#,
    );
    assert_eq!((status, retry), (429, Some(60)));
    let (_, stranger) = new_key(&core, "*", json!({}));
    assert_eq!(
        request(
            &addr,
            "GET",
            &format!("/v1/jobs/{job}"),
            Some(&stranger),
            ""
        )
        .0,
        404
    );
}

#[test]
fn stopping_the_execution_closes_the_listener() {
    let core = core("stop", json!({}));
    let (id, addr) = start(&core);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert!(TcpStream::connect(&addr).is_err());
}

/// Waits until `count` requests wait in `hold`, and returns their ids.
fn held(core: &Core, count: usize) -> Vec<String> {
    for _ in 0..500 {
        let list = core.dispatch("human.list", json!({})).unwrap();
        let ids: Vec<String> = list
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["id"].as_str().unwrap().to_string())
            .collect();
        if ids.len() >= count {
            return ids;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("{count} requests never reached `hold`");
}

fn release(core: &Core, id: &str) {
    core.dispatch("human.answer", json!({ "id": id, "decision": "approve" }))
        .unwrap();
}

fn hold(addr: &str, key: &str) -> (u16, Option<u64>, Value) {
    request(
        addr,
        "POST",
        "/v1/workflows/hold?async=1",
        Some(key),
        r#"{"ask":{"n":1}}"#,
    )
}

fn wait_job(addr: &str, key: &str, job: &str) -> Value {
    for _ in 0..500 {
        let polled = request(addr, "GET", &format!("/v1/jobs/{job}"), Some(key), "").2;
        if polled["status"] != "running" {
            return polled;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("job {job} never ended");
}

#[test]
fn concurrent_requests_cannot_pass_a_budget_or_a_concurrency_limit() {
    let core = core(
        "budget",
        json!({ "hold": { "budget_requests": 2 }, "greet": { "concurrency": 1 } }),
    );
    let (_, addr) = start(&core);
    let (_, key) = new_key(&core, "proj", json!({}));
    // Four at once against a budget of two: two run, two are refused, none charged yet.
    let answers: Vec<u16> = std::thread::scope(|s| {
        let calls: Vec<_> = (0..4).map(|_| s.spawn(|| hold(&addr, &key).0)).collect();
        calls.into_iter().map(|c| c.join().unwrap()).collect()
    });
    assert_eq!(
        answers.iter().filter(|s| **s == 202).count(),
        2,
        "{answers:?}"
    );
    assert_eq!(
        answers.iter().filter(|s| **s == 429).count(),
        2,
        "{answers:?}"
    );
    for id in held(&core, 2) {
        release(&core, &id);
    }
    // Spent now, and still spent: no refill when they end.
    std::thread::sleep(Duration::from_millis(200));
    let (status, retry, _) = hold(&addr, &key);
    assert_eq!((status, retry), (429, None));

    // `greet` lets one in at a time; quick answers free the slot each time.
    let answers: Vec<u16> = std::thread::scope(|s| {
        let calls: Vec<_> = (0..3)
            .map(|_| {
                s.spawn(|| {
                    request(
                        &addr,
                        "POST",
                        "/v1/workflows/greet",
                        Some(&key),
                        r#"{"ask":"x"}"#,
                    )
                    .0
                })
            })
            .collect();
        calls.into_iter().map(|c| c.join().unwrap()).collect()
    });
    assert!(
        answers.iter().all(|s| matches!(s, 200 | 429)),
        "{answers:?}"
    );
    assert!(answers.contains(&200));
}

#[test]
fn stopping_with_a_request_in_flight_frees_its_slot_and_fails_its_job() {
    let core = core("inflight", json!({}));
    let (id, addr) = start(&core);
    // The key's own limit is shared by every endpoint: a leak would block it for good.
    let (kid, key) = new_key(&core, "proj", json!({ "concurrency": 1 }));
    let (status, _, body) = hold(&addr, &key);
    assert_eq!(status, 202, "{body}");
    held(&core, 1);
    assert_eq!(hold(&addr, &key).0, 429);
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();

    let (_, addr) = start(&core);
    let (status, _, body) = hold(&addr, &key);
    assert_eq!(status, 202, "the slot came back: {body}");
    let log = core.dispatch(LOG, json!({ "key": kid })).unwrap();
    assert!(
        log.as_array().unwrap().iter().any(|e| e["status"] == 503),
        "the stopped request is logged: {log}"
    );
    let job = body["job"].as_str().unwrap().to_string();
    let id = held(&core, 1).pop().unwrap();
    release(&core, &id);
    assert_eq!(wait_job(&addr, &key, &job)["status"], "done");
}

#[test]
fn a_sync_call_that_runs_out_of_time_becomes_a_job() {
    let core = core("late", json!({}));
    let (_, addr) = start(&core);
    let (_, key) = new_key(&core, "*", json!({}));
    let started = std::time::Instant::now();
    let caller = {
        let (addr, key) = (addr.clone(), key.clone());
        std::thread::spawn(move || {
            request(
                &addr,
                "POST",
                "/v1/workflows/hold",
                Some(&key),
                r#"{"ask":{"n":2}}"#,
            )
        })
    };
    let id = held(&core, 1).pop().unwrap();
    let (status, _, body) = caller.join().unwrap();
    assert_eq!(status, 504, "{body}");
    assert!(started.elapsed() >= Duration::from_secs(5));
    let job = body["job"].as_str().unwrap().to_string();
    release(&core, &id);
    let polled = wait_job(&addr, &key, &job);
    assert_eq!(polled["outputs"]["reply"], json!({ "n": 2 }), "{polled}");
}

#[test]
fn unknown_keys_are_not_logged() {
    let core = core("quiet", json!({}));
    let (_, addr) = start(&core);
    for _ in 0..3 {
        assert_eq!(
            request(&addr, "POST", "/v1/workflows/anything", Some("rgk_x"), "{}").0,
            401
        );
    }
    assert_eq!(core.dispatch(LOG, json!({})).unwrap(), json!([]));
}
