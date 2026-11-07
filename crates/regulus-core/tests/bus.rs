//! "Then" wires carry data over the bus (docs/11-composition.md §4): delivery in order to
//! every matching input, capability checks, and the depth cut that stops loops.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_core::graph::check_profile;
use regulus_core::{
    BusConfig, Core, Module, ModuleCtx, ModuleError, NodeRef, ProfileStore, Storage,
};
use regulus_proto::{
    method, BusEventKind, EventPage, Manifest, ModuleInfo, NodeCall, NodeEvent, Profile,
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
    let log = Log::default();
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let bus = BusConfig {
        max_depth,
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
