//! The Agent box (docs/12-agents.md §2). Regulus has no agents of its own: this node
//! holds what is wired into it and hands the work to its engine, reached only through
//! the `agent.engine@1` capability. It never knows which CLI is behind it.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use regulus_contract_agent_engine as engine;
use regulus_contract_llm_endpoint as llm;
use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, NodeCall};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

const MANIFEST: &str = include_str!("../regulus-module.toml");
const PROMPT: &str = "agent.prompt";
const ENGINE_PORT: &str = "engine";
const PROVIDER_PORT: &str = "provider";

/// A started agent node: the engine behind it and the session it opened there.
struct Running {
    engine: Endpoint,
    session_id: String,
}

pub struct Agent {
    manifest: Manifest,
    ctx: ModuleCtx,
    running: Mutex<HashMap<NodeRef, Running>>,
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
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("agent manifest is valid"),
            ctx: ModuleCtx::default(),
            running: Mutex::default(),
        }
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
        let provider = match self.ctx.resolve(&me, PROVIDER_PORT).first() {
            None => None,
            Some(endpoint) => Some(parse::<llm::LlmEndpoint>(self.ctx.call(
                endpoint,
                llm::DESCRIBE,
                Value::Null,
            )?)?),
        };
        let args = engine::StartArgs {
            cwd: workdir(self.manifest.setting(&call.node, "workdir"))?,
            permissions,
            provider,
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
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}
