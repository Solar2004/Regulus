//! Tests of `regulus.rs`, the Rust helper, over in-memory stdin and stdout. The helper ships
//! without them: they are compiled into `crates/regulus-cli/tests/rust_node.rs`, so the
//! workspace's tests run them (`cargo test -p regulus-cli --test rust_node sdk::`).

use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use crate::regulus::{Context, Error, Module, FAILED, INVALID_PARAMS, METHOD_NOT_FOUND};

const MANIFEST: &str = "[[node]]\ntype = \"caps\"\nname = \"Caps\"\n";

/// The messages written, one JSON object per line; a line that is not JSON fails the test.
fn serve(module: Module, input: &[u8]) -> Vec<Value> {
    let mut output = Vec::new();
    module.serve(input, &mut output);
    output
        .split(|b| *b == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_slice(line)
                .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(line)))
        })
        .collect()
}

fn request(id: impl Into<Value>, method: &str, params: Value) -> String {
    let message = json!({ "jsonrpc": "2.0", "id": id.into(), "method": method, "params": params });
    format!("{message}\n")
}

/// `node/event`: `value` on the input `port` of `node` in `execution`.
fn event(id: impl Into<Value>, execution: &str, node: Value, port: &str, value: Value) -> String {
    let args = json!({ "port": port, "capability": "text@1", "value": value, "event": 3 });
    let call = json!({ "execution_id": execution, "node": node, "args": args });
    request(
        id,
        "call",
        json!({ "method": "node/event", "params": call }),
    )
}

/// A node of `node_type` with the ports `in` and `out`, as the core sends it.
fn node(id: &str, node_type: &str, config: Value) -> Value {
    json!({ "id": id, "type": node_type, "config": config, "ports": [
        { "id": "in", "capability": "text@1", "direction": "input", "kind": "then" },
        { "id": "out", "capability": "text@1", "direction": "output", "kind": "then" },
        { "id": "tool", "capability": "tool@1", "direction": "output", "kind": "uses" },
    ]})
}

fn caps(ctx: &Context, _port: &str, value: Value) -> Result<(), Error> {
    let text = value
        .as_str()
        .ok_or_else(|| Error::invalid("text@1 must be a string"))?;
    let prefix = ctx.config("prefix").as_str().unwrap_or("");
    ctx.emit("out", format!("{prefix}{}", text.to_uppercase()))
}

/// Where the answer to `id` is, and the answer.
fn answer<'a>(out: &'a [Value], id: &Value) -> (usize, &'a Value) {
    out.iter()
        .enumerate()
        .find(|(_, m)| m.get("method").is_none() && &m["id"] == id)
        .unwrap_or_else(|| panic!("no answer to {id}: {out:?}"))
}

fn error_of(out: &[Value], id: impl Into<Value>) -> (i64, &str) {
    let (_, answer) = answer(out, &id.into());
    let error = &answer["error"];
    (
        error["code"].as_i64().unwrap(),
        error["message"].as_str().unwrap(),
    )
}

#[test]
fn manifest_start_and_stop_are_answered_and_nothing_is_read_after_stop() {
    let module = Module::new(MANIFEST).node("caps", |ctx: &Context, _: &str, _: Value| {
        ctx.emit("out", format!("{} {}", ctx.project_id, ctx.root))
    });
    let input = [
        request(1, "manifest", json!({ "protocol": "1.17" })),
        request("s", "start", json!({ "project_id": "demo", "root": "/p" })),
        event(2, "e1", node("n", "caps", json!({})), "in", json!("x")),
        request(3, "stop", json!({})),
        event(4, "e1", node("n", "caps", json!({})), "in", json!("late")),
    ]
    .concat();
    let out = serve(module.workers(1), input.as_bytes());
    assert_eq!(answer(&out, &json!(1)).1["result"], MANIFEST);
    assert_eq!(answer(&out, &json!("s")).1["result"], Value::Null);
    assert_eq!(answer(&out, &json!(3)).1["result"], Value::Null);
    let (at, _) = answer(&out, &json!(2));
    assert_eq!(out[at - 1]["params"]["value"], "demo /p", "{out:?}");
    assert_eq!(out.len(), 5, "nothing after stop is read: {out:?}");
}

#[test]
fn an_event_is_emitted_before_its_answer_with_the_ids_it_came_with() {
    let module = Module::new(MANIFEST).node("caps", |ctx: &Context, port: &str, value: Value| {
        assert_eq!((port, ctx.node_type.as_str()), ("in", "caps"));
        assert_eq!(ctx.event["event"], 3);
        assert!(ctx.event.get("value").is_none(), "the value is handed over");
        caps(ctx, port, value)
    });
    let line = event(
        "req-α",
        "exec-1",
        node("n1", "caps", json!({ "prefix": "> " })),
        "in",
        json!("hi"),
    );
    let out = serve(module, line.as_bytes());
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(
        out[0],
        json!({ "jsonrpc": "2.0", "method": "emit", "params": {
            "execution_id": "exec-1", "node_id": "n1", "port": "out", "value": "> HI" } })
    );
    assert_eq!(
        out[1],
        json!({ "jsonrpc": "2.0", "id": "req-α", "result": null })
    );
}

#[test]
fn errors_are_answered_with_the_codes_the_core_maps_and_nothing_is_emitted() {
    let module = Module::new(MANIFEST)
        .node("caps", caps)
        .node("fails", |_: &Context, _: &str, _: Value| {
            Err(Error::failed("no luck"))
        })
        .node("parses", |_: &Context, _: &str, value: Value| {
            let _: u32 = value.as_str().unwrap_or_default().parse()?;
            Ok(())
        })
        .node("strays", |ctx: &Context, _: &str, _: Value| {
            ctx.emit("tool", 1)?; // a "uses" port is not an output a value goes on
            ctx.emit("out", 2)
        });
    let input = [
        event(1, "e", node("n", "caps", json!({})), "in", json!(3)),
        event(2, "e", node("n", "ghost", json!({})), "in", json!("x")),
        event(3, "e", node("n", "caps", json!({})), "side", json!("x")),
        event(4, "e", node("n", "fails", json!({})), "in", json!("x")),
        event(5, "e", node("n", "parses", json!({})), "in", json!("x")),
        event(6, "e", node("n", "strays", json!({})), "in", json!("x")),
        request(7, "call", json!({ "method": "node/start", "params": {} })),
        request(8, "nope", json!({})),
    ]
    .concat();
    let out = serve(module, input.as_bytes());
    assert_eq!(out.len(), 8, "one answer each, no emit: {out:?}");
    let expected = [
        (1, INVALID_PARAMS, "text@1 must be a string"),
        (2, INVALID_PARAMS, "no node type `ghost` here"),
        (3, INVALID_PARAMS, "`caps` takes nothing on `side`"),
        (4, FAILED, "no luck"),
        (5, FAILED, "invalid digit found in string"),
        (6, FAILED, "`strays` has no output `tool` (outputs: out)"),
        (7, METHOD_NOT_FOUND, "unknown method `node/start`"),
        (8, METHOD_NOT_FOUND, "unknown method `nope`"),
    ];
    for (id, code, message) in expected {
        assert_eq!(error_of(&out, id), (code, message), "{out:?}");
    }
}

#[test]
fn a_panic_fails_its_call_only_and_the_worker_goes_on() {
    let module = Module::new(MANIFEST)
        .node(
            "caps",
            |ctx: &Context, port: &str, value: Value| match value.as_str() {
                Some("boom") => panic!("asked to panic"),
                Some("again") => panic!("{} {}", "formatted", 2),
                _ => caps(ctx, port, value),
            },
        )
        .workers(1);
    let n = node("n", "caps", json!({}));
    let input = [
        event(1, "e", n.clone(), "in", json!("boom")),
        event(2, "e", n.clone(), "in", json!("again")),
        event(3, "e", n, "in", json!("fine")),
    ]
    .concat();
    let out = serve(module, input.as_bytes());
    assert_eq!(
        error_of(&out, 1),
        (FAILED, "the node panicked: asked to panic")
    );
    assert_eq!(
        error_of(&out, 2),
        (FAILED, "the node panicked: formatted 2")
    );
    let (at, ok) = answer(&out, &json!(3));
    assert_eq!(ok["result"], Value::Null);
    assert_eq!(
        out[at - 1]["params"]["value"],
        "FINE",
        "the one worker survived"
    );
}

#[test]
fn noise_notifications_answers_and_bad_utf8_get_no_answer_but_requests_do() {
    let mut input = b"\n   \nnot json\n[1, 2]\n".to_vec();
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"method\":\"emit\",\"params\":{}}\n");
    input.extend_from_slice(b"{\"jsonrpc\":\"2.0\",\"id\":5,\"result\":null}\n");
    let mut line = event(9, "e", node("n", "caps", json!({})), "in", json!("a~b")).into_bytes();
    let at = line.iter().position(|b| *b == b'~').unwrap();
    line[at] = 0xff; // not UTF-8
    input.extend_from_slice(&line);
    let out = serve(Module::new(MANIFEST).node("caps", caps), &input);
    assert_eq!(out.len(), 2, "{out:?}");
    assert_eq!(
        out[0]["params"]["value"], "A\u{fffd}B",
        "read as lossy UTF-8"
    );
    assert_eq!(out[1]["id"], 9);
}

#[test]
fn a_slow_event_does_not_hold_the_event_of_another_execution() {
    // `slow` waits until `release` has emitted: with one worker, in arrival order, it would
    // wait until its timeout and fail.
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    let release = Mutex::new(release);
    let module = Module::new(MANIFEST)
        .node("slow", move |ctx: &Context, _: &str, value: Value| {
            let waited = released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(20));
            waited.map_err(|_| Error::failed("never released"))?;
            ctx.emit("out", value)
        })
        .node("release", move |ctx: &Context, _: &str, value: Value| {
            ctx.emit("out", value)?;
            release.lock().unwrap().send(()).map_err(Error::from)
        });
    let input = [
        event(
            1,
            "slow-exec",
            node("s", "slow", json!({})),
            "in",
            json!("late"),
        ),
        event(
            2,
            "fast-exec",
            node("f", "release", json!({})),
            "in",
            json!("first"),
        ),
    ]
    .concat();
    let out = serve(module, input.as_bytes()); // the default workers
    assert_eq!(answer(&out, &json!(1)).1["result"], Value::Null, "{out:?}");
    assert_eq!(answer(&out, &json!(2)).1["result"], Value::Null, "{out:?}");
    let emitted = |value: &str| out.iter().position(|m| m["params"]["value"] == value);
    let order = (emitted("first"), emitted("late"));
    assert!(matches!(order, (Some(a), Some(b)) if a < b), "{out:?}");
}

#[test]
fn concurrent_emits_are_whole_lines_each_before_its_own_answer() {
    let big = "x".repeat(20_000);
    let module = Module::new(MANIFEST)
        .node("caps", move |ctx: &Context, _: &str, _: Value| {
            for part in 0..3 {
                ctx.emit("out", json!({ "part": part, "text": big }))?;
            }
            Ok(())
        })
        .workers(8);
    let input: String = (0..64)
        .map(|i| {
            event(
                i,
                &format!("e{i}"),
                node("n", "caps", json!({})),
                "in",
                json!(i),
            )
        })
        .collect();
    let out = serve(module, input.as_bytes()); // `serve` fails on any line that is not JSON
    assert_eq!(out.len(), 64 * 4);
    for i in 0..64 {
        let (at, ok) = answer(&out, &json!(i));
        assert_eq!(ok["result"], Value::Null);
        let execution = json!(format!("e{i}"));
        let emits: Vec<usize> = (0..out.len())
            .filter(|k| out[*k]["params"]["execution_id"] == execution)
            .collect();
        assert_eq!(emits.len(), 3);
        assert!(emits.iter().all(|k| *k < at), "{i}: {emits:?} after {at}");
    }
}

#[test]
fn a_line_longer_than_the_core_reads_is_never_written() {
    let module = Module::new(MANIFEST)
        .node("caps", |ctx: &Context, _: &str, value: Value| {
            let size = value.as_u64().unwrap_or_default() as usize;
            ctx.emit("out", "y".repeat(size))
        })
        .node("fails", |_: &Context, _: &str, _: Value| {
            Err(Error::failed("z".repeat(1_000)))
        })
        .max_line_bytes(300)
        .workers(1);
    let input = [
        event(1, "e", node("n", "caps", json!({})), "in", json!(10)),
        event(2, "e", node("n", "caps", json!({})), "in", json!(1_000)),
        event(3, "e", node("n", "fails", json!({})), "in", json!(0)),
    ]
    .concat();
    let mut output = Vec::new();
    module.serve(input.as_bytes(), &mut output);
    let lines: Vec<&[u8]> = output
        .split(|b| *b == b'\n')
        .filter(|l| !l.is_empty())
        .collect();
    assert!(lines.iter().all(|line| line.len() <= 300), "{lines:?}");
    let out: Vec<Value> = lines
        .iter()
        .map(|l| serde_json::from_slice(l).unwrap())
        .collect();
    assert_eq!(out.len(), 4, "one emit and three answers: {out:?}");
    assert_eq!(answer(&out, &json!(1)).1["result"], Value::Null);
    for id in [2, 3] {
        let (code, message) = error_of(&out, id);
        assert_eq!(code, FAILED);
        assert!(
            message.contains("the core reads lines of at most 300"),
            "{message}"
        );
    }
}

#[test]
fn every_node_and_execution_gets_its_own_context() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = Arc::clone(&seen);
    let module = Module::new(MANIFEST)
        .node("caps", move |ctx: &Context, port: &str, value: Value| {
            log.lock()
                .unwrap()
                .push((ctx.execution_id.clone(), ctx.node_id.clone()));
            caps(ctx, port, value)
        })
        .workers(1);
    // `b` comes without ports, as from an older core: any output goes then.
    let bare = json!({ "id": "b", "type": "caps", "config": { "prefix": "b:" } });
    let input = [
        event(
            1,
            "e1",
            node("a", "caps", json!({ "prefix": "a:" })),
            "in",
            json!("x"),
        ),
        event(2, "e1", bare, "in", json!("y")),
        event(3, "e2", node("a", "caps", json!({})), "in", json!("z")),
    ]
    .concat();
    let out = serve(module, input.as_bytes());
    let value_before = |id: i64| {
        let (at, _) = answer(&out, &json!(id));
        let emit = &out[at - 1]["params"];
        (
            emit["execution_id"].clone(),
            emit["node_id"].clone(),
            emit["value"].clone(),
        )
    };
    assert_eq!(value_before(1), (json!("e1"), json!("a"), json!("a:X")));
    assert_eq!(value_before(2), (json!("e1"), json!("b"), json!("b:Y")));
    assert_eq!(value_before(3), (json!("e2"), json!("a"), json!("Z")));
    assert_eq!(seen.lock().unwrap().len(), 3);
}

#[test]
#[should_panic(expected = "node type `caps` is registered twice")]
fn a_node_type_is_served_by_one_handler() {
    let _ = Module::new(MANIFEST).node("caps", caps).node("caps", caps);
}
