//! The Agent box (docs/12-agents.md §2). Regulus has no agents of its own: this node
//! holds what is wired into it and hands the work to its engine, reached only through
//! the `agent.engine@1` capability. It never knows which CLI is behind it.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use regulus_contract_agent_engine as engine;
use regulus_contract_llm_endpoint as llm;
use regulus_contract_participant as participant;
use regulus_contract_skill as skill;
use regulus_contract_toolset as toolset;
use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Direction, Manifest, NodeCall};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

const MANIFEST: &str = include_str!("../regulus-module.toml");
const PROMPT: &str = "agent.prompt";
const ENGINE_PORT: &str = "engine";
const PROVIDER_PORT: &str = "provider";
const SKILLS_PORT: &str = "skills";
const TOOLS_PORT: &str = "tools";
const HOOKS_PORT: &str = "hooks";
/// The flags of the hook bridge (`regulus hook`) the box fills in for each hook.
const HOOK_FLAGS: [&str; 7] = [
    "--execution",
    "--node",
    "--hook",
    "--event",
    "--timeout-ms",
    "--returns",
    "--fail",
];
const NAME_FIELD: &str = "name";
/// What the box says it is to channels.
const KIND: &str = "agent";

/// A started agent node: the engine behind it and the session it opened there.
struct Running {
    engine: Endpoint,
    session_id: String,
}

/// How to start the hook bridge: the host's own program and the arguments that reach the
/// running core and name the bridge. Given by the host, the only one that knows them.
#[derive(Debug, Clone, Default)]
pub struct HookBridge {
    pub program: PathBuf,
    pub args: Vec<String>,
}

pub struct Agent {
    manifest: Manifest,
    ctx: ModuleCtx,
    running: Mutex<HashMap<NodeRef, Running>>,
    bridge: HookBridge,
}

#[derive(Deserialize)]
struct PromptParams {
    execution_id: String,
    node_id: String,
    text: String,
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, ModuleError> {
    serde_json::from_value(value).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn json(value: impl serde::Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(|e| ModuleError::Failed(e.to_string()))
}

fn workdir(setting: Option<&Value>) -> Result<PathBuf, ModuleError> {
    let home = std::env::home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let dir = match setting
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        None => home,
        Some(path) => match path.strip_prefix("~/") {
            Some(rest) => home.join(rest),
            None => PathBuf::from(path),
        },
    };
    if !dir.is_absolute() || !dir.is_dir() {
        return Err(ModuleError::Failed(format!(
            "workdir `{}` is not an existing absolute directory",
            dir.display()
        )));
    }
    Ok(dir)
}

impl Agent {
    /// A box on a host that cannot start the hook bridge: wiring a hook fails its start.
    pub fn new() -> Self {
        Self::with_bridge(HookBridge::default())
    }

    pub fn with_bridge(bridge: HookBridge) -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("agent manifest is valid"),
            ctx: ModuleCtx::default(),
            running: Mutex::default(),
            bridge,
        }
    }

    /// Each hook wired in, as it describes itself, with the bridge command that asks it
    /// on behalf of this box.
    fn hooks(&self, me: &NodeRef) -> Result<Vec<engine::HookMount>, ModuleError> {
        let wired = self.ctx.resolve(me, HOOKS_PORT);
        if wired.is_empty() {
            return Ok(Vec::new());
        }
        if self.bridge.program.as_os_str().is_empty() {
            return Err(ModuleError::Failed(
                "this host cannot start the hook bridge".into(),
            ));
        }
        wired
            .iter()
            .map(|endpoint| {
                let hook: engine::Hook = parse(self.ctx.call(
                    endpoint,
                    regulus_proto::HOOK_DESCRIBE,
                    Value::Null,
                )?)?;
                let node = endpoint.node().node_id.clone();
                let [execution, at, only, event, timeout, returns, fail] = HOOK_FLAGS;
                let mut args = self.bridge.args.clone();
                for (flag, value) in [
                    (execution, me.execution_id.as_str()),
                    (at, me.node_id.as_str()),
                    (only, node.as_str()),
                    (event, hook.event.as_str()),
                ] {
                    args.extend([flag.to_string(), value.to_string()]);
                }
                if let Some(ms) = hook.timeout_ms {
                    args.extend([timeout.to_string(), ms.to_string()]);
                }
                if !hook.returns.is_empty() {
                    let names: Vec<&str> = hook.returns.iter().map(|e| e.name()).collect();
                    args.extend([returns.to_string(), names.join(",")]);
                }
                if !hook.fail.is_open() {
                    args.extend([fail.to_string(), "closed".to_string()]);
                }
                Ok(engine::HookMount {
                    node,
                    hook,
                    command: self.bridge.program.clone(),
                    args,
                })
            })
            .collect()
    }

    fn slot_ports<'a>(&'a self, node_type: &str) -> impl Iterator<Item = &'a str> {
        self.manifest
            .node_type(node_type)
            .into_iter()
            .flat_map(|t| t.ports.iter())
            .filter(|p| {
                p.direction == Direction::Input
                    && p.capability == llm::CAPABILITY
                    && p.id != PROVIDER_PORT
            })
            .map(|p| p.id.as_str())
    }

    fn running(&self) -> std::sync::MutexGuard<'_, HashMap<NodeRef, Running>> {
        self.running.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: call.execution_id.clone(),
            node_id: call.node.id.clone(),
        };
        let engine = self
            .ctx
            .resolve(&me, ENGINE_PORT)
            .into_iter()
            .next()
            .ok_or_else(|| {
                ModuleError::Failed(format!(
                    "no engine wired into `{ENGINE_PORT}`: connect Claude Code, opencode…"
                ))
            })?;
        let permissions = match self
            .manifest
            .setting(&call.node, "permissions")
            .and_then(Value::as_str)
        {
            Some("allow") => engine::Permissions::Allow,
            _ => engine::Permissions::Reject,
        };
        let describe = |port: &str| -> Result<Option<llm::LlmEndpoint>, ModuleError> {
            match self.ctx.resolve(&me, port).first() {
                None => Ok(None),
                Some(endpoint) => {
                    parse(self.ctx.call(endpoint, llm::DESCRIBE, Value::Null)?).map(Some)
                }
            }
        };
        let provider = describe(PROVIDER_PORT)?;
        // Every other `llm.endpoint` input of the box is a slot named after its port, so a
        // new slot is a manifest change (docs/12-agents.md §6.5).
        let mut slots = BTreeMap::new();
        for port in self.slot_ports(&call.node.node_type) {
            if let Some(endpoint) = describe(port)? {
                slots.insert(port.to_string(), endpoint);
            }
        }
        let skills = self
            .ctx
            .resolve(&me, SKILLS_PORT)
            .iter()
            .map(|endpoint| parse(self.ctx.call(endpoint, skill::DESCRIBE, Value::Null)?))
            .collect::<Result<Vec<skill::Skill>, _>>()?;
        // Tools are mounted in the box's own name: the agent cannot pick another one.
        let caller = self.name(&call.node);
        let tools = self
            .ctx
            .resolve(&me, TOOLS_PORT)
            .iter()
            .map(|endpoint| {
                let args = json(toolset::MountArgs {
                    caller: caller.clone(),
                })?;
                parse(self.ctx.call(endpoint, toolset::MOUNT, args)?)
            })
            .collect::<Result<Vec<toolset::Mount>, _>>()?;
        let hooks = self.hooks(&me)?;
        let args = engine::StartArgs {
            cwd: workdir(self.manifest.setting(&call.node, "workdir"))?,
            permissions,
            provider,
            slots,
            skills,
            tools,
            hooks,
        };
        let started: engine::Started =
            parse(self.ctx.call(&engine, engine::SESSION_START, json(args)?)?)?;
        let answer = json(&started)?;
        self.running().insert(
            me,
            Running {
                engine,
                session_id: started.session_id,
            },
        );
        Ok(answer)
    }

    fn stop(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        if let Some(running) = self.running().remove(&me) {
            let args = engine::StopArgs {
                session_id: running.session_id,
            };
            self.ctx
                .call(&running.engine, engine::SESSION_STOP, json(args)?)?;
        }
        Ok(Value::Null)
    }

    /// The name channels and tools know the agent by: its `name` field, or the node id.
    fn name(&self, node: &regulus_proto::Node) -> String {
        self.manifest
            .setting(node, NAME_FIELD)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty())
            .unwrap_or(&node.id)
            .to_string()
    }

    /// The agent as a participant: its name and where its session stands, asked to the
    /// engine (ACP knows when a turn is running).
    fn describe(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let name = self.name(&call.node);
        let me = NodeRef {
            execution_id: call.execution_id,
            node_id: call.node.id,
        };
        let running = self
            .running()
            .get(&me)
            .map(|r| (r.engine.clone(), r.session_id.clone()));
        let state = match running {
            None => participant::State::Gone,
            Some((engine, session_id)) => {
                let args = json(engine::StatusArgs { session_id })?;
                match self.ctx.call(&engine, engine::SESSION_STATUS, args) {
                    Ok(answer) => match parse::<engine::Status>(answer)?.state {
                        engine::SessionState::Idle => participant::State::Idle,
                        engine::SessionState::Busy => participant::State::Busy,
                        engine::SessionState::Ended => participant::State::Gone,
                    },
                    Err(ModuleError::UnknownMethod(_)) => participant::State::Unknown,
                    Err(e) => return Err(e),
                }
            }
        };
        json(participant::Identity {
            name,
            kind: KIND.into(),
            state,
        })
    }

    /// A message from a channel, prompted into the session as the user would type it.
    fn deliver(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let delivery: participant::Delivery = parse(call.args)?;
        let text = format!(
            "[message from @{} in channel `{}`]\n{}",
            delivery.from, delivery.channel, delivery.text
        );
        let turn: engine::Turn = parse(self.prompt(PromptParams {
            execution_id: call.execution_id,
            node_id: call.node.id,
            text,
        })?)?;
        json(participant::Reply { text: turn.text })
    }

    fn prompt(&self, params: PromptParams) -> Result<Value, ModuleError> {
        let me = NodeRef {
            execution_id: params.execution_id,
            node_id: params.node_id,
        };
        let (engine, session_id) = {
            let running = self.running();
            let found = running.get(&me).ok_or_else(|| {
                ModuleError::InvalidParams(format!(
                    "no running agent `{}` in execution `{}`",
                    me.node_id, me.execution_id
                ))
            })?;
            (found.engine.clone(), found.session_id.clone())
        };
        let args = engine::PromptArgs {
            session_id,
            text: params.text,
        };
        self.ctx.call(&engine, engine::SESSION_PROMPT, json(args)?)
    }
}

impl Default for Agent {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Agent {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            method::NODE_START => self.start(parse(params)?),
            method::NODE_STOP => self.stop(parse(params)?),
            PROMPT => self.prompt(parse(params)?),
            participant::DESCRIBE => self.describe(parse(params)?),
            participant::DELIVER => self.deliver(parse(params)?),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}
