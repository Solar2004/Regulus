//! The `skill@1` capability (docs/12-agents.md §3): a skill node says where its folder
//! is, and the engine places it in the agent's profile. A skill is a folder with a
//! `SKILL.md`, the format Claude Code and opencode both read.
//!
//! The format belongs to the capability, not to any module. Adding an optional field
//! keeps `@1`; changing a shape means `skill@2`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "skill@1";

/// The description of the skill: `NodeCall` with no args → [`Skill`].
pub const DESCRIBE: &str = "skill";

/// The file that makes a folder a skill.
pub const ENTRY: &str = "SKILL.md";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skill {
    /// The folder name it gets in the profile, unique per agent.
    pub name: String,
    /// The absolute path of the folder holding [`ENTRY`].
    pub path: PathBuf,
}
