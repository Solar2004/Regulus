//! Delegation end to end (docs/13-blocks.md §3.3): an agent's tools reach a `delegate`
//! node, which keeps two workers (each a workflow with an Agent box) on two different
//! engines; their results come back per worker, closing one leaves the other, a follow-up
//! to the other gives a new result, and stopping the parent ends what is left.

mod labelled;

use labelled::{agent_node, call_tool, engine_node, port, tools_nodes, tools_wires, wire};
use regulus_core::Core;
use regulus_proto::{method, ExecutionTree};
use serde_json::{json, Value};

fn profile() -> Value {
    let mut nodes = vec![
        agent_node("boss"),
        engine_node("engine_a", "label-a"),
        engine_node("engine_b", "label-b"),
        json!({ "id": "team", "type": "delegate", "module_id": "regulus/delegation@0.1.0",
                "config": { "workers": { "echo": "worker" } },
                "ports": [port("tools", "tool@1", "output", "uses"),
                          { "id": "engine", "capability": "agent.engine@1", "direction": "input",
                            "kind": "uses", "cardinality": "many" }] }),
    ];
    nodes.extend(tools_nodes());
    let mut wires = vec![
        wire(
            "boss-engine",
            "uses",
            ("engine_a", "engine"),
            ("boss", "engine"),
        ),
        wire("team-a", "uses", ("engine_a", "engine"), ("team", "engine")),
        wire("team-b", "uses", ("engine_b", "engine"), ("team", "engine")),
        wire("team-tools", "uses", ("team", "tools"), ("tools", "tools")),
    ];
    wires.extend(tools_wires("boss"));
    json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main", "nodes": nodes, "wires": wires },
            labelled::worker_workflow("worker"),
        ]
    })
}

/// A tool's answer, as the agent would read it, parsed back.
fn data(core: &Core, binding: &str, name: &str, args: Value) -> Value {
    let answer = call_tool(core, binding, name, args);
    assert!(answer.get("error").is_none(), "{name}: {answer}");
    serde_json::from_str(answer["text"].as_str().unwrap()).unwrap()
}

fn live_children(core: &Core, id: &str) -> Vec<String> {
    let tree: ExecutionTree = serde_json::from_value(
        core.dispatch(method::EXECUTION_TREE, json!({ "execution_id": id }))
            .unwrap(),
    )
    .unwrap();
    tree.children
        .into_iter()
        .filter(|c| c.execution.state.is_live())
        .map(|c| c.execution.tag.unwrap_or_default())
        .collect()
}

#[test]
fn an_agent_keeps_two_workers_on_two_engines() {
    let core = labelled::core(
        profile(),
        vec![Box::new(regulus_module_delegation::Delegation::new())],
    );
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    let id = execution["id"].as_str().unwrap().to_string();
    let binding = labelled::binding(&core, &id, "boss");
    let ask = format!("env {}", labelled::LABEL_ENV);

    let first = data(
        &core,
        &binding,
        "worker_create",
        json!({ "brief": ask, "engine": "engine_a" }),
    );
    let second = data(
        &core,
        &binding,
        "worker_create",
        json!({ "brief": ask, "engine": "engine_b" }),
    );
    assert_eq!(
        (&first["worker"], &second["worker"]),
        (&json!("w-1"), &json!("w-2"))
    );
    assert_eq!(
        (&first["engine"], &second["engine"]),
        (&json!("engine_a"), &json!("engine_b"))
    );
    assert_ne!(first["execution"], second["execution"]);
    for worker in ["w-1", "w-2"] {
        let status = data(
            &core,
            &binding,
            "worker_status",
            json!({ "worker": worker }),
        );
        assert_eq!(status["state"], "running", "{status}");
    }
    let mut tags = live_children(&core, &id);
    tags.sort();
    assert_eq!(tags, ["w-1", "w-2"]);

    for (worker, label) in [("w-1", "label-a"), ("w-2", "label-b")] {
        let result = data(
            &core,
            &binding,
            "worker_result",
            json!({ "worker": worker, "wait_ms": 10000 }),
        );
        assert_eq!(result["value"], label, "{result}");
    }

    let closed = data(&core, &binding, "worker_close", json!({ "worker": "w-1" }));
    assert_eq!(closed["state"], "closed");
    assert_eq!(live_children(&core, &id), ["w-2"]);

    data(
        &core,
        &binding,
        "worker_send",
        json!({ "worker": "w-2", "text": "follow up" }),
    );
    let again = data(
        &core,
        &binding,
        "worker_result",
        json!({ "worker": "w-2", "wait_ms": 10000 }),
    );
    assert_eq!(again["value"], "echo: follow up", "{again}");

    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    assert!(live_children(&core, &id).is_empty());
}
