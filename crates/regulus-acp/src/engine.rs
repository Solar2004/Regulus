//! What every ACP engine module has in common (docs/12-agents.md §3). An engine only
//! says how to find its CLI and how to launch it for a node; [`EngineModule`] turns that
//! into a module serving `agent.engine@1` over ACP.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use regulus_contract_agent_engine as contract;
use regulus_core::{Module, ModuleCtx, ModuleError};
use regulus_proto::{method, Manifest, Node, NodeCall};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::{Launch, Permissions, SessionOptions, Sessions};

/// The field every engine node has for how long its CLI may take to open a session.
pub const STARTUP_FIELD: &str = "startup_seconds";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// The part that differs between engines.
pub trait AcpEngine: Send + Sync + 'static {
    /// Installations of the CLI on this machine, best first.
    fn detect(&self) -> Vec<contract::Installation>;

    /// How to launch the CLI's ACP side for this node. `settings` reads the node's fields
    /// with the manifest defaults filled in.
    fn launch(
        &self,
        settings: &Settings<'_>,
        detected: &[contract::Installation],
    ) -> Result<Launch, String>;

    /// Agent-specific `_meta` for `session/new`.
    fn session_meta(&self, _settings: &Settings<'_>) -> Option<Map<String, Value>> {
        None
    }
}

/// A node's settings: its own value, else the manifest default.
pub struct Settings<'a> {
    manifest: &'a Manifest,
    node: &'a Node,
}

impl Settings<'_> {
    pub fn value(&self, field: &str) -> Option<&Value> {
        self.manifest.setting(self.node, field)
    }

    /// A text field; empty counts as unset.
    pub fn text(&self, field: &str) -> Option<&str> {
        self.value(field)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    }

    /// A text field holding a path, with a leading `~` expanded.
    pub fn path(&self, field: &str) -> Option<PathBuf> {
        self.text(field).map(expand_home)
    }
}

pub fn expand_home(path: &str) -> PathBuf {
    match (path.strip_prefix("~/"), std::env::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => PathBuf::from(path),
    }
}

/// Executables on `PATH` whose file name matches, each real file once, in `PATH` order.
pub fn find_on_path(matches: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut seen = Vec::new();
    let Some(path) = std::env::var_os("PATH") else {
        return found;
    };
    for dir in std::env::split_paths(&path) {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        let mut names: Vec<_> = entries.flatten().map(|e| e.path()).collect();
        names.sort();
        for candidate in names {
            let name = candidate
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if !matches(name) || !is_executable(&candidate) {
                continue;
            }
            let real = std::fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone());
            if !seen.contains(&real) {
                seen.push(real);
                found.push(candidate);
            }
        }
    }
    found
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// The first line `<program> --version` prints, if it answers in time.
pub fn version(program: &Path) -> Option<String> {
    let mut child = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + VERSION_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    let output = child.wait_with_output().ok()?;
    let text = String::from_utf8_lossy(&output.stdout);
    text.lines()
        .next()
        .map(|line| line.trim().to_string())
        .filter(|line| !line.is_empty())
}

pub fn installations(paths: Vec<PathBuf>) -> Vec<contract::Installation> {
    paths
        .into_iter()
        .map(|path| contract::Installation {
            version: version(&path),
            path,
        })
        .collect()
}

/// A module serving `agent.engine@1` over ACP. Sessions belong to the engine node that
/// opened them and close when that node stops, whatever the box did.
pub struct EngineModule<E> {
    manifest: Manifest,
    engine: E,
    sessions: Sessions,
    owners: Mutex<HashMap<String, Vec<String>>>,
}

fn failed(error: impl ToString) -> ModuleError {
    ModuleError::Failed(error.to_string())
}

fn parse<T: DeserializeOwned>(value: Value) -> Result<T, ModuleError> {
    serde_json::from_value(value).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn json(value: impl Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(failed)
}

impl<E: AcpEngine> EngineModule<E> {
    pub fn new(manifest: Manifest, engine: E) -> Self {
        Self {
            manifest,
            engine,
            sessions: Sessions::default(),
            owners: Mutex::default(),
        }
    }

    fn owner(call: &NodeCall) -> String {
        format!("{}/{}", call.execution_id, call.node.id)
    }

    fn start(&self, call: NodeCall) -> Result<Value, ModuleError> {
        let args: contract::StartArgs = parse(call.args.clone())?;
        let settings = Settings {
            manifest: &self.manifest,
            node: &call.node,
        };
        let detected = self.engine.detect();
        let launch = self.engine.launch(&settings, &detected).map_err(failed)?;
        let startup = settings
            .value(STARTUP_FIELD)
            .and_then(Value::as_f64)
            .unwrap_or(60.0);
        let options = SessionOptions {
            cwd: args.cwd,
            permissions: match args.permissions {
                contract::Permissions::Allow => Permissions::Allow,
                contract::Permissions::Reject => Permissions::Reject,
            },
            meta: self.engine.session_meta(&settings),
            startup: Duration::from_secs_f64(startup.max(1.0)),
        };
        let (session_id, agent) = self.sessions.start(launch, options).map_err(failed)?;
        self.owners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(Self::owner(&call))
            .or_default()
            .push(session_id.clone());
        json(contract::Started {
            session_id,
            agent: contract::Agent {
                name: agent.name,
                version: agent.version,
            },
        })
    }

    fn stop_owned(&self, call: &NodeCall) {
        let owned = self
            .owners
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&Self::owner(call));
        for session in owned.into_iter().flatten() {
            self.sessions.stop(&session);
        }
    }
}

impl<E: AcpEngine> Module for EngineModule<E> {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, _ctx: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        // The module's own detect method (declared in `methods`) and the capability's
        // answer the same; the first one has no node behind it.
        if name == contract::DETECT || self.manifest.module.methods.iter().any(|m| m == name) {
            return json(contract::Detected {
                installations: self.engine.detect(),
            });
        }
        match name {
            contract::SESSION_START => self.start(parse(params)?),
            contract::SESSION_PROMPT => {
                let call: NodeCall = parse(params)?;
                let args: contract::PromptArgs = parse(call.args)?;
                let turn = self
                    .sessions
                    .prompt(&args.session_id, &args.text)
                    .map_err(failed)?;
                json(contract::Turn {
                    stop_reason: turn.stop_reason,
                    text: turn.text,
                })
            }
            contract::SESSION_STOP => {
                let call: NodeCall = parse(params)?;
                let args: contract::StopArgs = parse(call.args)?;
                self.sessions.stop(&args.session_id);
                Ok(Value::Null)
            }
            method::NODE_STOP => {
                self.stop_owned(&parse(params)?);
                Ok(Value::Null)
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.sessions.stop_all();
        Ok(())
    }
}
