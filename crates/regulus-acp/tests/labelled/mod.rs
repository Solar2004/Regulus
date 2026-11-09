//! A fake engine module whose agent knows its node's `label` (`env FAKE_LABEL`), so a test
//! can tell which engine node a session went through; and the boxes around it that the
//! tool tests share.

use std::sync::Arc;

use regulus_acp::engine::{AcpEngine, EngineModule, Prepared, Start};
use regulus_acp::Launch;
use regulus_contract_agent_engine::Installation;
use regulus_core::{parse_manifest, Core, Module, ProfileStore};
use regulus_proto::Profile;
use serde_json::{json, Value};

pub const ENGINE: &str = "test/engine-fake@0.1.0";
pub const LABEL_ENV: &str = "FAKE_LABEL";

const MANIFEST: &str = r#"
[module]
id      = "test/engine-fake@0.1.0"
name    = "Fake engine"
methods = ["fake.detect"]

[[node]]
type = "engine-fake"
name = "Fake"

  [[node.field]]
  id      = "label"
  type    = "text"
  default = ""
"#;

struct Fake;

impl AcpEngine for Fake {
    fn detect(&self) -> Vec<Installation> {
        vec![Installation {
            path: env!("CARGO_BIN_EXE_regulus-acp-fake-agent").into(),
            version: None,
        }]
    }
    fn protocols(&self) -> &'static [&'static str] {
        &[]
    }
    fn prepare(&self, start: &Start<'_>) -> Result<Prepared, String> {
        let mut launch = Launch {
            program: start.detected[0].path.clone(),
            ..Launch::default()
        };
        let label = start.settings.text("label").unwrap_or_default();
        launch.env.insert(LABEL_ENV.into(), label.into());
        Ok(Prepared {
            launch,
            ..Prepared::default()
        })
    }
}

pub fn engine_module() -> Box<dyn Module> {
    Box::new(EngineModule::new(parse_manifest(MANIFEST).unwrap(), Fake))
}

pub fn port(id: &str, capability: &str, direction: &str, kind: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
}

pub fn wire(id: &str, kind: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": kind,
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

pub fn engine_node(id: &str, label: &str) -> Value {
    json!({ "id": id, "type": "engine-fake", "module_id": ENGINE,
            "config": { "label": label },
            "ports": [port("engine", "agent.engine@1", "output", "uses")] })
}

/// An Agent box with an engine and a toolset input.
pub fn agent_node(id: &str) -> Value {
    json!({ "id": id, "type": "agent", "module_id": "regulus/agent@0.1.0",
            "config": { "name": id, "workdir": "/tmp", "permissions": "allow" },
            "ports": [
                { "id": "engine", "capability": "agent.engine@1", "direction": "input",
                  "kind": "uses", "required": true },
                { "id": "tools", "capability": "toolset@1", "direction": "input",
                  "kind": "uses", "cardinality": "many" },
                port("prompt", "text@1", "input", "then"),
                port("reply", "text@1", "output", "then"),
            ] })
}

/// The Tools box and its MCP engine: `tools.tools` takes `tool@1` nodes.
pub fn tools_nodes() -> Vec<Value> {
    vec![
        json!({ "id": "tools", "type": "tools", "module_id": "regulus/tools@0.1.0",
                "ports": [
                    { "id": "tools", "capability": "tool@1", "direction": "input",
                      "kind": "uses", "cardinality": "many" },
                    { "id": "engine", "capability": "tools.engine@1", "direction": "input",
                      "kind": "uses", "required": true },
                    port("toolset", "toolset@1", "output", "uses"),
                ] }),
        json!({ "id": "mcp", "type": "tools-mcp", "module_id": "regulus/tools-mcp@0.1.0",
                "ports": [port("engine", "tools.engine@1", "output", "uses")] }),
    ]
}

pub fn tools_wires(agent: &str) -> Vec<Value> {
    vec![
        wire("mcp-tools", "uses", ("mcp", "engine"), ("tools", "engine")),
        wire(
            "tools-agent",
            "uses",
            ("tools", "toolset"),
            (agent, "tools"),
        ),
    ]
}

/// A workflow with "uses" input `engine` and an Agent box on it: `prompt` in, `reply` out.
pub fn worker_workflow(id: &str) -> Value {
    json!({ "id": id, "kind": "extra",
            "ports": [port("prompt", "text@1", "input", "then"),
                      port("reply", "text@1", "output", "then"),
                      port("engine", "agent.engine@1", "input", "uses")],
            "nodes": [agent_node("worker")],
            "wires": [
                wire("p", "then", ("self", "prompt"), ("worker", "prompt")),
                wire("r", "then", ("worker", "reply"), ("self", "reply")),
                wire("e", "uses", ("self", "engine"), ("worker", "engine")),
            ] })
}

pub fn core(profile: Value, mut modules: Vec<Box<dyn Module>>) -> Arc<Core> {
    let profile: Profile = serde_json::from_value(profile).unwrap();
    modules.push(Box::new(regulus_module_agent::Agent::new()));
    modules.push(engine_module());
    modules.push(Box::new(regulus_module_tools::Tools::new()));
    modules.push(Box::new(regulus_module_tools_mcp::ToolsMcp::new(
        regulus_module_tools_mcp::Bridge {
            program: "/bin/true".into(),
            args: vec![],
        },
    )));
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    Core::build(modules, &[], profiles)
}

/// The binding the Tools box gave `agent`, read from the MCP servers its session got.
pub fn binding(core: &Core, execution: &str, agent: &str) -> String {
    let turn = core
        .dispatch(
            "agent.prompt",
            json!({ "execution_id": execution, "node_id": agent, "text": "mcp" }),
        )
        .unwrap();
    let servers: Value = serde_json::from_str(turn["text"].as_str().unwrap()).unwrap();
    fn find(value: &Value) -> Option<String> {
        match value {
            Value::Object(map) => {
                if map.get("name").and_then(Value::as_str)
                    == Some(regulus_module_tools_mcp::BINDING_ENV)
                {
                    return map.get("value").and_then(Value::as_str).map(String::from);
                }
                map.values().find_map(find)
            }
            Value::Array(items) => items.iter().find_map(find),
            _ => None,
        }
    }
    find(&servers).unwrap_or_else(|| panic!("no binding in {servers}"))
}

/// Calls a tool as the MCP bridge does; answers the text written for the agent.
pub fn call_tool(core: &Core, binding: &str, name: &str, args: Value) -> Value {
    core.dispatch(
        "tools.call",
        json!({ "binding": binding, "name": name, "args": args }),
    )
    .unwrap()
}
