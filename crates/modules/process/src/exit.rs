//! The values this module puts on "then" wires. Their formats are fixed here, by the first
//! module that uses them (docs/03-modules.md §1); a second module that needs them moves
//! them to a contract crate. Adding an optional field keeps `@1`.

use serde::{Deserialize, Serialize};

/// How a run ended: [`Exit`].
pub const EXIT_CAPABILITY: &str = "exit@1";
/// A job that just started: [`Started`].
pub const JOB_CAPABILITY: &str = "job@1";

/// How a run ended. `code` is `null` when a signal killed it or it never started.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Exit {
    pub code: Option<i32>,
    /// The signal that killed it, if one did (Unix).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// Milliseconds from start to end.
    pub ms: u64,
    #[serde(default)]
    pub stdout: String,
    #[serde(default)]
    pub stderr: String,
    /// It ran past its `timeout_ms` and was killed.
    #[serde(default, skip_serializing_if = "is_false")]
    pub timed_out: bool,
    /// Why it never started (a program that does not exist, a directory it cannot enter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Jobs only: its number, its log and the last lines of it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl Exit {
    /// Exited 0 in time: the verdict a gate gives.
    pub fn ok(&self) -> bool {
        self.code == Some(0) && !self.timed_out && self.error.is_none()
    }

    /// A run that could not start.
    pub fn not_started(error: impl ToString) -> Self {
        Self {
            error: Some(error.to_string()),
            ..Self::default()
        }
    }
}

/// A job that is running: its number in the node, its process id and where it logs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Started {
    pub job: u64,
    pub pid: u32,
    pub log: String,
}
