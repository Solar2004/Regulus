//! "Then" wires carry data over the bus (docs/11-composition.md §4): delivery in order to
//! every matching input, capability checks, and the depth cut that stops loops.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_core::graph::check_profile;
use regulus_core::{
    BusConfig, Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore, Storage,
};
use regulus_proto::{
    method, BusEvent, BusEventKind, EventPage, Manifest, ModuleInfo, NodeCall, NodeEvent, Profile,
};
use serde_json::{json, Value};

type Log = Arc<Mutex<Vec<String>>>;

/// Emits `config.say` on `out` (or `config.port`) when it starts; logs what arrives and, with
/// `config.relay`, sends it on through `out`.
struct Relay {
    manifest: Manifest,
    log: Log,
    ctx: ModuleCtx,
}

impl Module for Relay {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }
    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params)
            .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        match method {
            method::NODE_START => {
                if let Some(say) = call.node.config.get("say") {
                    let port = call.node.config.get("port").and_then(Value::as_str);
                    self.ctx.emit(&me, port.unwrap_or("out"), say.clone())?;
                }
                Ok(Value::Null)
            }
            method::NODE_EVENT => {
                let event: NodeEvent = serde_json::from_value(call.args)
                    .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
                self.log.lock().unwrap().push(format!(
                    "{}.{} <- {}.{} {}",
                    call.node.id, event.port, event.from.node_id, event.from.port_id, event.value
                ));
                if call.node.config.get("relay") == Some(&json!(true)) {
                    self.ctx.emit(&me, "out", event.value)?;
                }
                Ok(Value::Null)
            }
            method::NODE_STOP => Ok(Value::Null),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

const MODULE: &str = "t/relay@1.0.0";

fn relay(log: &Log) -> Box<dyn Module> {
    Box::new(Relay {
        manifest: Manifest {
            module: ModuleInfo {
                id: MODULE.into(),
                name: MODULE.into(),
                requires_proto: None,
                methods: vec![],
                workflow_id: None,
                origin: None,
            },
            nodes: vec![],
        },
        log: log.clone(),
        ctx: ModuleCtx::default(),
    })
}

fn node(id: &str, config: Value, input: &str, output: &str) -> Value {
    json!({ "id": id, "type": "relay", "module_id": MODULE, "config": config, "ports": [
        { "id": "in", "capability": input, "direction": "input", "kind": "then", "cardinality": "many" },
        { "id": "out", "capability": output, "direction": "output", "kind": "then" },
    ]})
}

fn then(id: &str, from: &str, to: &str) -> Value {
    json!({ "id": id, "kind": "then",
            "from": { "node_id": from, "port_id": "out" },
            "to": { "node_id": to, "port_id": "in" } })
}

fn profile(nodes: Vec<Value>, wires: Vec<Value>) -> Profile {
    serde_json::from_value(json!({
        "id": "p", "project_id": "proj", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "nodes": nodes, "wires": wires }]
    }))
    .unwrap()
}

fn core(profile: Profile, max_depth: u32) -> (Arc<Core>, Log) {
    core_with(profile, max_depth, BusConfig::default().max_queue)
}

fn core_with(profile: Profile, max_depth: u32, max_queue: usize) -> (Arc<Core>, Log) {
    let log = Log::default();
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let bus = BusConfig {
        max_depth,
        max_queue,
        ..BusConfig::default()
    };
    let core = Core::build_with_bus(vec![relay(&log)], &[], profiles, Storage::default(), bus);
    (core, log)
}

/// Starts the profile and waits until its bus is idle.
fn run(core: &Core) -> (String, EventPage) {
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert_eq!(execution["state"]["status"], "running");
    let id = execution["id"].as_str().unwrap().to_string();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        if page.pending == 0 {
            return (id, page);
        }
        assert!(Instant::now() < deadline, "the bus never went idle");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn kinds(page: &EventPage) -> Vec<&'static str> {
    page.events
        .iter()
        .map(|e| match e.kind {
            BusEventKind::Emitted { .. } => "emitted",
            BusEventKind::Delivered { .. } => "delivered",
            BusEventKind::Failed { .. } => "failed",
            BusEventKind::Cut { .. } => "cut",
        })
        .collect()
}

#[test]
fn a_value_reaches_every_matching_input_once_all_nodes_run() {
    // `a` emits while starting, before `b` and `c` have started: they still get it.
    let (core, log) = core(
        profile(
            vec![
                node("a", json!({ "say": "hi" }), "text@1", "text@1"),
                node("b", json!({}), "text@1", "text@1"),
                node("c", json!({}), "text@1", "text@1"),
            ],
            vec![then("ab", "a", "b"), then("ac", "a", "c")],
        ),
        64,
    );
    let (id, page) = run(&core);
    let mut got = log.lock().unwrap().clone();
    got.sort();
    assert_eq!(got, [r#"b.in <- a.out "hi""#, r#"c.in <- a.out "hi""#]);
    assert_eq!(kinds(&page), ["emitted", "delivered", "delivered"]);
    let emitted = &page.events[0];
    assert_eq!(emitted.execution_id, id);
    assert_eq!(emitted.project_id.as_deref(), Some("proj"));
    assert_eq!(emitted.capability, "text@1");
    assert_eq!(emitted.depth, 0);
    assert!(page.events.iter().all(|e| e.event == emitted.seq));

    // The cursor: nothing new after the last event.
    let after: EventPage = serde_json::from_value(
        core.dispatch(method::EVENT_LIST, json!({ "since": page.next }))
            .unwrap(),
    )
    .unwrap();
    assert!(after.events.is_empty());
    assert_eq!(after.next, page.next);
}

#[test]
fn a_capability_mismatch_is_rejected_by_the_check_and_never_delivered() {
    let profile = profile(
        vec![
            node("a", json!({ "say": "hi" }), "text@1", "text@1"),
            node("b", json!({}), "image@1", "text@1"),
        ],
        vec![then("ab", "a", "b")],
    );
    let codes: Vec<String> = check_profile(&profile, |_| true)
        .into_iter()
        .map(|d| d.code)
        .collect();
    assert_eq!(codes, ["wire.capability_mismatch"]);

    let (core, log) = core(profile, 64);
    let (_, page) = run(&core);
    assert!(log.lock().unwrap().is_empty());
    assert_eq!(kinds(&page), ["emitted", "failed"]);
    assert!(matches!(&page.events[1].kind,
        BusEventKind::Failed { error, .. } if error.contains("does not fit")));
}

#[test]
fn a_loop_is_cut_past_the_configured_depth() {
    let (core, log) = core(
        profile(
            vec![
                node(
                    "a",
                    json!({ "say": "ping", "relay": true }),
                    "text@1",
                    "text@1",
                ),
                node("b", json!({ "relay": true }), "text@1", "text@1"),
            ],
            vec![then("ab", "a", "b"), then("ba", "b", "a")],
        ),
        3,
    );
    let (_, page) = run(&core);
    // Depths 0..=3 are delivered; the emission at depth 4 is cut.
    assert_eq!(log.lock().unwrap().len(), 4);
    let depths: Vec<u32> = page
        .events
        .iter()
        .filter(|e| matches!(e.kind, BusEventKind::Emitted { .. }))
        .map(|e| e.depth)
        .collect();
    assert_eq!(depths, [0, 1, 2, 3]);
    // Reported while `a` handles the value at depth 3, so before that delivery is.
    let cut = page
        .events
        .iter()
        .find(|e| matches!(e.kind, BusEventKind::Cut { .. }))
        .unwrap();
    assert_eq!(cut.kind, BusEventKind::Cut { max_depth: 3 });
    assert_eq!(cut.depth, 4);
}

#[test]
fn a_full_queue_reports_the_extra_deliveries_as_failed() {
    // One emission fans out to three inputs, but the queue holds two.
    let (core, log) = core_with(
        profile(
            vec![
                node("a", json!({ "say": "ping" }), "text@1", "text@1"),
                node("b", json!({}), "text@1", "text@1"),
                node("c", json!({}), "text@1", "text@1"),
                node("d", json!({}), "text@1", "text@1"),
            ],
            vec![
                then("ab", "a", "b"),
                then("ac", "a", "c"),
                then("ad", "a", "d"),
            ],
        ),
        64,
        2,
    );
    let (_, page) = run(&core);
    assert_eq!(log.lock().unwrap().len(), 2);
    let failed: Vec<&BusEvent> = page
        .events
        .iter()
        .filter(|e| matches!(e.kind, BusEventKind::Failed { .. }))
        .collect();
    assert_eq!(failed.len(), 1);
    match &failed[0].kind {
        BusEventKind::Failed { to, error } => {
            assert_eq!(to.node_id, "d");
            assert!(error.contains("queue is full"), "{error}");
        }
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_node_emits_only_on_its_then_outputs() {
    let (core, _) = core(
        profile(
            vec![node(
                "a",
                json!({ "say": "x", "port": "in" }),
                "text@1",
                "text@1",
            )],
            vec![],
        ),
        64,
    );
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    let reason = execution["state"]["reason"].as_str().unwrap();
    assert!(
        reason.contains(r#"`in` is not a "then" output"#),
        "{reason}"
    );
    let page: EventPage =
        serde_json::from_value(core.dispatch(method::EVENT_LIST, json!({})).unwrap()).unwrap();
    assert!(page.events.is_empty());

    // A context with no core behind it cannot emit at all.
    let a = NodeRef {
        execution_id: "exec-1".into(),
        node_id: "a".into(),
    };
    assert!(ModuleCtx::default().emit(&a, "out", json!("x")).is_err());
}

// --- across a workflow's boundary --------------------------------------------------------

fn text_ports() -> Value {
    json!([
        { "id": "ask", "capability": "text@1", "direction": "input", "kind": "then" },
        { "id": "reply", "capability": "text@1", "direction": "output", "kind": "then" },
    ])
}

fn wire(id: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": "then",
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

fn a_box(id: &str, workflow: &str) -> Value {
    json!({ "id": id, "type": "workflow", "internal_workflow_id": workflow })
}

/// `main`: `a` says hi into box `outer` (workflow `mid`), whose reply reaches `b`. `mid`
/// passes it through box `inner` (workflow `leaf`), where `x` relays it back out.
fn nested() -> Profile {
    serde_json::from_value(json!({
        "id": "p", "project_id": "proj", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main",
              "nodes": [node("a", json!({ "say": "hi" }), "text@1", "text@1"),
                        a_box("outer", "mid"),
                        node("b", json!({}), "text@1", "text@1")],
              "wires": [wire("w1", ("a", "out"), ("outer", "ask")),
                        wire("w2", ("outer", "reply"), ("b", "in"))] },
            { "id": "mid", "kind": "extra", "ports": text_ports(),
              "nodes": [a_box("inner", "leaf")],
              "wires": [wire("w1", ("self", "ask"), ("inner", "ask")),
                        wire("w2", ("inner", "reply"), ("self", "reply"))] },
            { "id": "leaf", "kind": "extra", "ports": text_ports(),
              "nodes": [node("x", json!({ "relay": true }), "text@1", "text@1")],
              "wires": [wire("w1", ("self", "ask"), ("x", "in")),
                        wire("w2", ("x", "out"), ("self", "reply"))] },
        ]
    }))
    .unwrap()
}

fn emitted_from(page: &EventPage) -> Vec<(String, u32)> {
    page.events
        .iter()
        .filter(|e| matches!(e.kind, BusEventKind::Emitted { .. }))
        .map(|e| (format!("{}.{}", e.from.node_id, e.from.port_id), e.depth))
        .collect()
}

#[test]
fn a_value_crosses_two_levels_of_boxes_in_and_back_out() {
    let profile = nested();
    assert_eq!(check_profile(&profile, |_| true), []);
    let (core, log) = core(profile, 64);
    let (_, page) = run(&core);
    assert_eq!(
        log.lock().unwrap().as_slice(),
        [
            r#"outer/inner/x.in <- outer/inner.ask "hi""#,
            r#"b.in <- outer.reply "hi""#
        ]
    );
    // Every crossing is a delivery to the box port and an emission from it.
    assert_eq!(
        emitted_from(&page),
        [
            ("a.out".into(), 0),
            ("outer.ask".into(), 1),
            ("outer/inner.ask".into(), 2),
            ("outer/inner/x.out".into(), 3),
            ("outer/inner.reply".into(), 4),
            ("outer.reply".into(), 5),
        ]
    );
    assert!(!kinds(&page).contains(&"failed"), "{page:#?}");
}

#[test]
fn the_depth_cut_counts_deliveries_across_levels() {
    let (core, log) = core(nested(), 3);
    let (_, page) = run(&core);
    // `x` gets it at depth 2 and emits at 3; crossing back out at 4 is cut.
    assert_eq!(log.lock().unwrap().len(), 1);
    let cut = page
        .events
        .iter()
        .find(|e| matches!(e.kind, BusEventKind::Cut { .. }))
        .unwrap();
    assert_eq!(cut.depth, 4);
    assert_eq!(cut.from.node_id, "outer/inner");
    assert_eq!(cut.from.port_id, "reply");
}

#[test]
fn the_queue_bound_holds_across_the_boundary() {
    // `a` fans out to `b` and to the box; the queue holds one, so the box gets nothing.
    let mut profile = nested();
    let main = &mut profile.workflows[0];
    main.wires.insert(
        0,
        serde_json::from_value(wire("w0", ("a", "out"), ("b", "in"))).unwrap(),
    );
    let (core, log) = core_with(profile, 64, 1);
    let (_, page) = run(&core);
    assert_eq!(log.lock().unwrap().as_slice(), [r#"b.in <- a.out "hi""#]);
    assert!(page.events.iter().any(|e| matches!(&e.kind,
        BusEventKind::Failed { to, error } if to.node_id == "outer" && error.contains("queue is full"))));
}

/// Waits until the bus of `id` is idle and returns its events.
fn idle(core: &Core, id: &str) -> EventPage {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        if page.pending == 0 {
            return page;
        }
        assert!(Instant::now() < deadline, "the bus never went idle");
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn send_talks_to_a_running_workflow_and_its_reply_comes_from_self() {
    // `main` itself has ports: what comes in on `ask` goes through box `outer` and back.
    let mut profile = nested();
    let main = &mut profile.workflows[0];
    main.ports = serde_json::from_value(text_ports()).unwrap();
    main.nodes.retain(|n| n.id == "outer");
    main.wires = serde_json::from_value(json!([
        wire("w1", ("self", "ask"), ("outer", "ask")),
        wire("w2", ("outer", "reply"), ("self", "reply")),
    ]))
    .unwrap();
    assert_eq!(check_profile(&profile, |_| true), []);
    let (core, log) = core(profile, 64);
    let (id, page) = run(&core);
    assert!(page.events.is_empty());

    let sent = core
        .dispatch(
            method::WORKFLOW_SEND,
            json!({ "execution_id": id, "port": "ask", "value": "hello" }),
        )
        .unwrap();
    let page = idle(&core, &id);
    assert_eq!(sent["event"], page.events[0].seq);
    assert_eq!(
        log.lock().unwrap().as_slice(),
        [r#"outer/inner/x.in <- outer/inner.ask "hello""#]
    );
    let reply = page.events.last().unwrap();
    assert_eq!(
        (reply.from.node_id.as_str(), reply.from.port_id.as_str()),
        ("self", "reply")
    );
    assert_eq!(
        reply.kind,
        BusEventKind::Emitted {
            value: json!("hello")
        }
    );
    assert_eq!(reply.capability, "text@1");

    let send = |params: Value| {
        core.dispatch(method::WORKFLOW_SEND, params)
            .unwrap_err()
            .message
    };
    let output = send(json!({ "execution_id": id, "port": "reply", "value": "x" }));
    assert!(output.contains(r#"is not a "then" input"#), "{output}");
    let unknown = send(json!({ "execution_id": "exec-9", "port": "ask", "value": "x" }));
    assert!(unknown.contains("Unknown execution"), "{unknown}");
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let stopped = send(json!({ "execution_id": id, "port": "ask", "value": "x" }));
    assert!(stopped.contains("not running"), "{stopped}");
}

#[test]
fn a_start_refuses_ids_that_would_be_ambiguous_in_the_running_copy() {
    let start = |profile: Profile| -> String {
        let (core, log) = core(profile, 64);
        let error = core
            .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
            .unwrap_err()
            .message;
        assert!(log.lock().unwrap().is_empty(), "nothing started");
        error
    };
    // A node literally named `outer/inner` next to the box `outer` holding `inner`.
    let mut clash = nested();
    clash.workflows[0]
        .nodes
        .push(serde_json::from_value(node("outer/inner", json!({}), "text@1", "text@1")).unwrap());
    assert!(start(clash).contains("`outer/inner` is reserved"));
    // A node named `self` would have its wires taken by a workflow port.
    let mut named_self = nested();
    named_self.workflows[2].nodes[0].id = "self".into();
    assert!(start(named_self).contains("`self` is reserved"));
    // Plain duplicates, of nodes and of wires once inlined.
    let mut twice = nested();
    twice.workflows[0].nodes[2].id = "a".into();
    assert!(start(twice).contains("node id `a` is used more than once"));
    let mut wire_twice = nested();
    wire_twice.workflows[0].wires[1].id = "w1".into();
    assert!(start(wire_twice).contains("wire id `w1` is used more than once"));
    let mut slash = nested();
    slash.workflows[0].wires[1].id = "outer/w1".into();
    assert!(start(slash).contains("wire id `outer/w1` is reserved"));
    let codes: Vec<String> = check_profile(
        &{
            let mut p = nested();
            p.workflows[0].wires[1].id = "outer/w1".into();
            p
        },
        |_| true,
    )
    .into_iter()
    .map(|d| d.code)
    .collect();
    assert_eq!(codes, ["wire.reserved_id"]);
}

#[test]
fn a_wire_into_the_wrong_side_of_a_boundary_is_never_delivered() {
    let mut profile = nested();
    let main = &mut profile.workflows[0];
    main.ports = serde_json::from_value(text_ports()).unwrap();
    main.nodes.retain(|n| n.id == "outer");
    // `outer.reply → self.ask` feeds the workflow's own input from inside, and
    // `x.out → self.ask` in `leaf` feeds the box's input from inside: both would loop.
    main.wires = serde_json::from_value(json!([
        wire("w1", ("self", "ask"), ("outer", "ask")),
        wire("w2", ("outer", "reply"), ("self", "reply")),
        wire("back", ("outer", "reply"), ("self", "ask")),
    ]))
    .unwrap();
    profile.workflows[2]
        .wires
        .push(serde_json::from_value(wire("back", ("x", "out"), ("self", "ask"))).unwrap());
    let codes: Vec<String> = check_profile(&profile, |_| true)
        .into_iter()
        .map(|d| d.code)
        .collect();
    assert_eq!(codes, ["wire.direction", "wire.direction"]);

    let (core, log) = core(profile, 64);
    let (id, _) = run(&core);
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": "ask", "value": "once" }),
    )
    .unwrap();
    let page = idle(&core, &id);
    assert_eq!(log.lock().unwrap().len(), 1, "no loop through the boundary");
    let failed: Vec<String> = page
        .events
        .iter()
        .filter_map(|e| match &e.kind {
            BusEventKind::Failed { to, error } => {
                assert!(error.contains(r#"no "then" input"#), "{error}");
                Some(format!("{}.{}", to.node_id, to.port_id))
            }
            _ => None,
        })
        .collect();
    assert_eq!(failed, ["outer/inner.ask", "self.ask"]);
    assert!(!kinds(&page).contains(&"cut"));
    assert_eq!(emitted_from(&page).last().unwrap().0, "self.reply");
}
