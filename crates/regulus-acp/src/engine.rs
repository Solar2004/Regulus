//! What every ACP engine module has in common (docs/12-agents.md §3). An engine only
//! says how to find its CLI, which protocols it speaks and how to launch it for a node;
//! [`EngineModule`] turns that into a module serving `agent.engine@1` over ACP, and
//! settles for it what every engine shares: the profile and the provider check.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use regulus_contract_agent_engine as contract;
use regulus_contract_agent_engine::LlmEndpoint;
use regulus_core::{Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, Node, NodeCall};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{Map, Value};

use crate::{Launch, Permissions, SessionOptions, Sessions, SetupRequest, State, StdioServer};

/// The tool protocol every ACP agent accepts (`session/new` takes MCP servers over stdio).
const MCP_STDIO: &str = "mcp-stdio";

/// The field every engine node has for how long its CLI may take to open a session.
pub const STARTUP_FIELD: &str = "startup_seconds";
/// `user` (the default): `config_dir`, or the user's own setup. `isolated`: a profile
/// that only has what is wired in (docs/12-agents.md §6.3).
pub const PROFILE_FIELD: &str = "profile";
pub const CONFIG_DIR_FIELD: &str = "config_dir";
const ISOLATED: &str = "isolated";
/// Inside the node's own directory, where an isolated profile lives by default.
const PROFILE_DIR: &str = "profile";
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// The part that differs between engines.
pub trait AcpEngine: Send + Sync + 'static {
    /// Installations of the CLI on this machine, best first.
    fn detect(&self) -> Vec<contract::Installation>;

    /// The `llm.endpoint` protocols this engine can hand to its CLI. A provider speaking
    /// any other is refused before anything is launched.
    fn protocols(&self) -> &'static [&'static str];

    /// The Agent box slots ([`contract::slot`]) this engine can hand to its CLI. Others
    /// are left out of the session with a warning.
    fn slots(&self) -> &'static [&'static str] {
        &[]
    }

    /// Where the CLI reads skills, relative to its profile directory. `None`: the engine
    /// takes no skills, and wired ones are left out with a warning.
    fn skills_dir(&self) -> Option<&'static str> {
        None
    }

    /// Writes `hooks` into the isolated profile at `dir`, replacing what an earlier run
    /// wrote, and returns a warning for each one left out. Called with an empty list too,
    /// so hooks unwired since are gone. `None`: the engine takes no hooks, and wired ones
    /// are left out with a warning.
    fn place_hooks(
        &self,
        _dir: &Path,
        _hooks: &[contract::HookMount],
    ) -> Option<Result<Vec<String>, String>> {
        None
    }

    /// How to launch the CLI's ACP side and open the session for this node.
    fn prepare(&self, start: &Start<'_>) -> Result<Prepared, String>;
}

/// Everything an engine needs to prepare one session.
pub struct Start<'a> {
    /// The node's fields with the manifest defaults filled in.
    pub settings: Settings<'a>,
    pub detected: &'a [contract::Installation],
    /// The profile directory to point the CLI at. `None`: the user's own setup.
    pub config_dir: Option<PathBuf>,
    /// Whether that directory is an isolated profile, which must not pick up anything
    /// the user did not wire in.
    pub isolated: bool,
    /// The provider wired into the box, already checked against [`AcpEngine::protocols`].
    pub provider: Option<&'a LlmEndpoint>,
    /// The slot providers, checked the same way and only those in [`AcpEngine::slots`].
    pub slots: &'a BTreeMap<String, LlmEndpoint>,
}

/// How to launch the CLI and what to tell it before and when opening the session.
#[derive(Debug, Default)]
pub struct Prepared {
    pub launch: Launch,
    /// Agent-specific `_meta` for `session/new`.
    pub meta: Option<Map<String, Value>>,
    pub setup: Vec<SetupRequest>,
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

/// Links each skill folder into `dir` under its name. Links left from an earlier run are
/// removed first, so a skill unwired since is gone; anything else in `dir` is kept.
pub fn place_skills(dir: &Path, skills: &[contract::Skill]) -> Result<(), String> {
    let cannot = |what: &str, path: &Path, e: std::io::Error| {
        format!("cannot {what} {}: {e}", path.display())
    };
    std::fs::create_dir_all(dir).map_err(|e| cannot("create", dir, e))?;
    let entries = std::fs::read_dir(dir).map_err(|e| cannot("read", dir, e))?;
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|t| t.is_symlink()) {
            let path = entry.path();
            std::fs::remove_file(&path).map_err(|e| cannot("remove", &path, e))?;
        }
    }
    for skill in skills {
        let link = dir.join(&skill.name);
        if link.exists() {
            return Err(format!(
                "two skills are named `{}`, or the profile has its own: rename one",
                skill.name
            ));
        }
        symlink(&skill.path, &link).map_err(|e| cannot("link", &link, e))?;
    }
    Ok(())
}

#[cfg(unix)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
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
    ctx: ModuleCtx,
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
            ctx: ModuleCtx::default(),
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
        let mut warnings = Vec::new();
        let mut slots = args.slots;
        slots.retain(|slot, _| {
            let known = self.engine.slots().contains(&slot.as_str());
            if !known {
                warnings.push(format!(
                    "{} has no `{slot}` slot: its provider was left out",
                    self.manifest.module.name
                ));
            }
            known
        });
        // The core does not keep what a node start answers yet: the server log does.
        for warning in &warnings {
            eprintln!("regulus: node `{}`: {warning}", call.node.id);
        }
        let speaks = self.engine.protocols();
        let wired = args.provider.iter().map(|p| ("provider", p));
        for (input, provider) in wired.chain(slots.iter().map(|(s, p)| (s.as_str(), p))) {
            if !speaks.contains(&provider.protocol.as_str()) {
                return Err(failed(format!(
                    "{} speaks {}; the `{input}` provider speaks `{}`: wire a translator between them",
                    self.manifest.module.name,
                    speaks.join(", "),
                    provider.protocol
                )));
            }
        }
        let isolated = settings.text(PROFILE_FIELD) == Some(ISOLATED);
        let config_dir = match settings.path(CONFIG_DIR_FIELD) {
            Some(dir) => Some(dir),
            None if isolated => {
                let me = NodeRef {
                    execution_id: call.execution_id.clone(),
                    node_id: call.node.id.clone(),
                };
                Some(self.ctx.node_dir(&me)?.join(PROFILE_DIR))
            }
            None => None,
        };
        if let Some(dir) = config_dir.as_ref().filter(|_| isolated) {
            std::fs::create_dir_all(dir)
                .map_err(|e| failed(format!("cannot create {}: {e}", dir.display())))?;
        }
        let left_out = match (self.engine.skills_dir(), config_dir.as_ref()) {
            _ if args.skills.is_empty() => None,
            (Some(sub), Some(dir)) if isolated => {
                place_skills(&dir.join(sub), &args.skills).map_err(failed)?;
                None
            }
            // The user's own profile is never written to (docs/12-agents.md §6.7).
            (Some(_), _) => Some("skills are placed only in an isolated profile".to_string()),
            (None, _) => Some(format!("{} takes no skills", self.manifest.module.name)),
        };
        if let Some(warning) = left_out {
            eprintln!("regulus: node `{}`: {warning}", call.node.id);
            warnings.push(warning);
        }
        warnings.extend(self.hooks(&call.node.id, &args.hooks, config_dir.as_deref(), isolated)?);
        let mut mcp_servers = Vec::new();
        for mount in args.tools {
            if mount.protocol == MCP_STDIO {
                mcp_servers.push(StdioServer {
                    name: mount.name,
                    command: mount.command,
                    args: mount.args,
                    env: mount.env,
                });
            } else {
                let warning = format!(
                    "{} cannot mount `{}` tools: it speaks {MCP_STDIO}",
                    self.manifest.module.name, mount.protocol
                );
                eprintln!("regulus: node `{}`: {warning}", call.node.id);
                warnings.push(warning);
            }
        }
        let detected = self.engine.detect();
        let prepared = self
            .engine
            .prepare(&Start {
                settings: Settings {
                    manifest: &self.manifest,
                    node: &call.node,
                },
                detected: &detected,
                config_dir,
                isolated,
                provider: args.provider.as_ref(),
                slots: &slots,
            })
            .map_err(failed)?;
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
            meta: prepared.meta,
            setup: prepared.setup,
            startup: Duration::from_secs_f64(startup.max(1.0)),
            mcp_servers,
        };
        let (session_id, agent) = self
            .sessions
            .start(prepared.launch, options)
            .map_err(failed)?;
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
            warnings,
        })
    }

    /// Places the hooks like the skills: only in an isolated profile, never in the user's.
    /// Returns the warnings, already logged.
    fn hooks(
        &self,
        node: &str,
        hooks: &[contract::HookMount],
        dir: Option<&Path>,
        isolated: bool,
    ) -> Result<Vec<String>, ModuleError> {
        let name = &self.manifest.module.name;
        let warnings = match dir.filter(|_| isolated) {
            // An isolated profile is rewritten even with no hooks: unwired ones go away.
            Some(dir) => match self.engine.place_hooks(dir, hooks) {
                Some(placed) => placed.map_err(failed)?,
                None if hooks.is_empty() => Vec::new(),
                None => vec![format!("{name} takes no hooks: {} left out", hooks.len())],
            },
            None if hooks.is_empty() => Vec::new(),
            // The user's own profile is never written to (docs/12-agents.md §6.7).
            None => vec!["hooks are placed only in an isolated profile".to_string()],
        };
        for warning in &warnings {
            eprintln!("regulus: node `{node}`: {warning}");
        }
        Ok(warnings)
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

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
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
            contract::SESSION_STATUS => {
                let call: NodeCall = parse(params)?;
                let args: contract::StatusArgs = parse(call.args)?;
                let state = match self.sessions.state(&args.session_id) {
                    State::Idle => contract::SessionState::Idle,
                    State::Busy => contract::SessionState::Busy,
                    State::Ended => contract::SessionState::Ended,
                };
                json(contract::Status { state })
            }
            contract::SESSION_CANCEL => {
                let call: NodeCall = parse(params)?;
                let args: contract::CancelArgs = parse(call.args)?;
                if self.sessions.cancel(&args.session_id) {
                    Ok(Value::Null)
                } else {
                    Err(failed(format!("no live session `{}`", args.session_id)))
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_are_linked_and_unwired_ones_go_away() {
        let root = std::env::temp_dir().join(format!("regulus-place-{}", std::process::id()));
        let (review, lint) = (root.join("src/review"), root.join("src/lint"));
        std::fs::create_dir_all(&review).unwrap();
        std::fs::create_dir_all(&lint).unwrap();
        let dir = root.join("profile/skills");
        let skill = |name: &str, path: &Path| contract::Skill {
            name: name.into(),
            path: path.to_path_buf(),
        };
        place_skills(&dir, &[skill("review", &review), skill("lint", &lint)]).unwrap();
        assert_eq!(std::fs::read_link(dir.join("review")).unwrap(), review);
        std::fs::create_dir_all(dir.join("own")).unwrap();
        place_skills(&dir, &[skill("lint", &lint)]).unwrap();
        assert!(!dir.join("review").exists());
        assert!(dir.join("lint").exists() && dir.join("own").is_dir());
        let error = place_skills(&dir, &[skill("own", &lint)]).unwrap_err();
        assert!(error.contains("`own`"), "{error}");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
