//! What the core answers, independent of the transport.

use regulus_proto::{code, method, Execution, IdParams, ProjectFilter};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::Value;

use crate::graph::check_profile;
use crate::module::ModuleError;
use crate::profiles::ProfileStore;
use crate::registry::{CallError, Registry};

/// JSON-RPC "method not found".
const METHOD_NOT_FOUND: i32 = -32601;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(code::INVALID_PARAMS, message)
    }
}

impl From<CallError> for RpcError {
    fn from(error: CallError) -> Self {
        let code = match &error {
            CallError::NotFound | CallError::Module(ModuleError::UnknownMethod(_)) => {
                METHOD_NOT_FOUND
            }
            CallError::Module(ModuleError::InvalidParams(_)) => code::INVALID_PARAMS,
            CallError::Unavailable { .. } => code::MODULE_UNAVAILABLE,
            CallError::Module(ModuleError::Failed(_)) | CallError::Panicked(_) => {
                code::MODULE_FAILED
            }
        };
        let message = match error {
            CallError::NotFound => "Method not found".to_string(),
            other => other.to_string(),
        };
        Self::new(code, message)
    }
}

/// The base: the module registry, the profiles and the executions. It has no functions of
/// its own; it only answers about the pieces and routes calls to the modules.
pub struct Core {
    registry: Registry,
    profiles: ProfileStore,
    executions: Vec<Execution>,
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params)
        .map_err(|e| RpcError::invalid_params(format!("Invalid params: {e}")))
}

fn json(value: impl Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(value).map_err(|e| RpcError::new(code::MODULE_FAILED, e.to_string()))
}

impl Core {
    pub fn new(registry: Registry, profiles: ProfileStore) -> Self {
        Self {
            registry,
            profiles,
            executions: Vec::new(),
        }
    }

    pub fn registry(&self) -> &Registry {
        &self.registry
    }

    /// Answers every method except `hello`, which belongs to the connection.
    pub fn dispatch(&self, name: &str, params: Value) -> Result<Value, RpcError> {
        match name {
            method::PROFILE_LIST => {
                let filter: ProjectFilter = parse(params)?;
                json(
                    self.profiles
                        .list(filter.project_id.as_deref())
                        .map(|p| p.summary())
                        .collect::<Vec<_>>(),
                )
            }
            method::PROFILE_GET => json(self.profile(parse(params)?)?),
            method::PROFILE_CHECK => {
                let profile = self.profile(parse(params)?)?;
                json(check_profile(profile, |id| self.registry.is_installed(id)))
            }
            method::MODULE_LIST => json(self.registry.summaries()),
            method::EXECUTION_LIST => {
                let filter: ProjectFilter = parse(params)?;
                let wanted = filter.project_id.as_deref();
                let matches =
                    |e: &&Execution| wanted.is_none_or(|w| e.project_id.as_deref() == Some(w));
                json(self.executions.iter().filter(matches).collect::<Vec<_>>())
            }
            method::HELLO => Err(RpcError::new(
                METHOD_NOT_FOUND,
                "hello is answered by the connection",
            )),
            other => Ok(self.registry.call(other, params)?),
        }
    }

    fn profile(&self, params: IdParams) -> Result<&regulus_proto::Profile, RpcError> {
        self.profiles
            .get(&params.id)
            .ok_or_else(|| RpcError::invalid_params("Unknown profile"))
    }
}
