//! A workflow offered as a tool (`flow` node `tool`), end to end: an Agent box mounts the
//! Tools box over MCP, and what the MCP bridge would do with its binding (`tools.list`,
//! `tools.call`) lists the workflow with its schema and answers what the workflow
//! returns. The second tool runs a workflow with an Agent box inside whose engine comes
//! from the tool node's own "uses" input: rewiring that input swaps the implementation
//! without touching the tool.

mod labelled;

use labelled::{agent_node, call_tool, engine_node, port, tools_nodes, tools_wires, wire};
use regulus_proto::method;
use serde_json::{json, Value};

/// `main`: agent `boss` with the tools `greet` (the hello greeter) and `ask` (an agent on
/// whatever engine `ask.engine` is wired to: `engine_of_ask`).
fn profile(engine_of_ask: &str) -> Value {
    let mut nodes = vec![
        agent_node("boss"),
        engine_node("engine_a", "label-a"),
        engine_node("engine_b", "label-b"),
        json!({ "id": "greet", "type": "tool", "module_id": "regulus/flow@0.1.0",
                "config": { "workflow": "greeter", "description": "Greets someone." },
                "ports": [port("tools", "tool@1", "output", "uses"),
                          port("ask", "text@1", "input", "then"),
                          port("reply", "text@1", "output", "then")] }),
        json!({ "id": "ask", "type": "tool", "module_id": "regulus/flow@0.1.0",
                "config": { "workflow": "asker", "input": "prompt" },
                "ports": [port("tools", "tool@1", "output", "uses"),
                          port("engine", "agent.engine@1", "input", "uses"),
                          port("prompt", "text@1", "input", "then"),
                          port("reply", "text@1", "output", "then")] }),
    ];
    nodes.extend(tools_nodes());
    let mut wires = vec![
        wire(
            "boss-engine",
            "uses",
            ("engine_a", "engine"),
            ("boss", "engine"),
        ),
        wire(
            "greet-tools",
            "uses",
            ("greet", "tools"),
            ("tools", "tools"),
        ),
        wire("ask-tools", "uses", ("ask", "tools"), ("tools", "tools")),
        wire(
            "ask-engine",
            "uses",
            (engine_of_ask, "engine"),
            ("ask", "engine"),
        ),
    ];
    wires.extend(tools_wires("boss"));
    let text = |id: &str, direction: &str| port(id, "text@1", direction, "then");
    json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [
            { "id": "main", "kind": "main", "nodes": nodes, "wires": wires },
            { "id": "greeter", "kind": "extra",
              "ports": [text("ask", "input"), text("reply", "output")],
              "nodes": [{ "id": "hello", "type": "greeter", "module_id": "regulus/hello@0.1.0",
                          "ports": [text("echo", "input"), text("reply", "output")] }],
              "wires": [wire("a", "then", ("self", "ask"), ("hello", "echo")),
                        wire("b", "then", ("hello", "reply"), ("self", "reply"))] },
            labelled::worker_workflow("asker"),
        ]
    })
}

fn start(engine_of_ask: &str) -> (std::sync::Arc<regulus_core::Core>, String, String) {
    let core = labelled::core(
        profile(engine_of_ask),
        vec![
            Box::new(regulus_module_flow::Flow::new()),
            Box::new(regulus_module_hello::Hello::new()),
        ],
    );
    let execution = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    assert!(execution.get("disabled").is_none(), "{execution}");
    let id = execution["id"].as_str().unwrap().to_string();
    let binding = labelled::binding(&core, &id, "boss");
    (core, id, binding)
}

#[test]
fn the_agent_lists_the_workflow_tool_and_gets_what_it_returns() {
    let (core, id, binding) = start("engine_a");
    let specs = core
        .dispatch("tools.list", json!({ "binding": binding }))
        .unwrap();
    let greet = specs
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "greet")
        .unwrap_or_else(|| panic!("{specs}"));
    assert_eq!(greet["description"], "Greets someone.");
    assert_eq!(
        greet["input"]["properties"],
        json!({ "ask": { "type": "string" } })
    );
    let ask = specs
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["name"] == "ask")
        .unwrap();
    assert_eq!(ask["input"]["required"], json!(["text"]));

    let answer = call_tool(&core, &binding, "greet", json!({ "ask": "Alexander" }));
    let written: Value = serde_json::from_str(answer["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        written,
        json!({ "port": "reply", "value": "Hello, Alexander!" })
    );
    assert!(answer.get("error").is_none(), "{answer}");

    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
}

#[test]
fn rewiring_the_tools_engine_swaps_what_runs_behind_it() {
    for (engine, label) in [("engine_a", "label-a"), ("engine_b", "label-b")] {
        let (core, id, binding) = start(engine);
        let answer = call_tool(
            &core,
            &binding,
            "ask",
            json!({ "text": format!("env {}", labelled::LABEL_ENV) }),
        );
        let written: Value = serde_json::from_str(answer["text"].as_str().unwrap())
            .unwrap_or_else(|_| panic!("{answer}"));
        assert_eq!(written["value"], label, "{answer}");
        core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
            .unwrap();
    }
}
