//! The external boundary: JSON-RPC 2.0 methods, parameters and versioning.
//!
//! Versioning rules (docs/06-stack.md §5.1): same major = compatible; a new method bumps
//! the minor; removing or changing a method bumps the major.

use serde::{Deserialize, Serialize};

/// Protocol spoken by this core. 1.0 = the mock methods; 1.1 adds `profile.check`.
pub const PROTOCOL: &str = "1.1";

pub mod method {
    pub const HELLO: &str = "hello";
    pub const PROFILE_LIST: &str = "profile.list";
    pub const PROFILE_GET: &str = "profile.get";
    pub const PROFILE_CHECK: &str = "profile.check";
    pub const MODULE_LIST: &str = "module.list";
    pub const EXECUTION_LIST: &str = "execution.list";

    /// Methods served by the core itself. A module may not claim any of them.
    pub const CORE: &[&str] = &[
        HELLO,
        PROFILE_LIST,
        PROFILE_GET,
        PROFILE_CHECK,
        MODULE_LIST,
        EXECUTION_LIST,
    ];
}

/// Error codes beyond the JSON-RPC 2.0 standard ones.
pub mod code {
    pub const INVALID_PARAMS: i32 = -32602;
    /// Any method other than `hello` before a compatible `hello` on this connection.
    pub const HELLO_REQUIRED: i32 = -32001;
    /// The module serving the method is broken, disabled or incompatible.
    pub const MODULE_UNAVAILABLE: i32 = -32002;
    /// The module returned an error or panicked.
    pub const MODULE_FAILED: i32 = -32003;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub proto: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    pub core: String,
    pub proto: String,
    pub compatible: bool,
}

/// Parameters of read methods: no `project_id` means every project.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectFilter {
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdParams {
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}

/// A problem found when checking a profile. `code` is stable and machine readable;
/// `message` is for humans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub severity: Severity,
    pub code: String,
    pub message: String,
    pub workflow_id: Option<String>,
    pub node_id: Option<String>,
    pub wire_id: Option<String>,
}

fn parts(version: &str) -> Option<(u64, Option<u64>)> {
    let mut parts = version.trim().split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = match parts.next() {
        Some(minor) => Some(minor.parse().ok()?),
        None => None,
    };
    Some((major, minor))
}

/// Same major = compatible.
pub fn compatible(core: &str, client: &str) -> bool {
    match (parts(core), parts(client)) {
        (Some((a, _)), Some((b, _))) => a == b,
        _ => false,
    }
}

/// Whether `have` satisfies a requirement such as `^1`, `^1.1` or `1`: same major and,
/// if the requirement names a minor, at least that minor.
pub fn satisfies(requirement: &str, have: &str) -> bool {
    let requirement = requirement.trim().trim_start_matches('^');
    match (parts(requirement), parts(have)) {
        (Some((major, minor)), Some((have_major, have_minor))) => {
            major == have_major && minor.is_none_or(|minor| have_minor.unwrap_or(0) >= minor)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_major_is_compatible() {
        assert!(compatible("1.1", "1.0"));
        assert!(compatible("1.0", "1.9"));
        assert!(!compatible("1.1", "2.0"));
        assert!(!compatible("1.1", "garbage"));
        assert!(!compatible("1.1", ""));
    }

    #[test]
    fn requirements_check_major_and_minimum_minor() {
        assert!(satisfies("^1", "1.1"));
        assert!(satisfies("^1.1", "1.1"));
        assert!(satisfies("1", "1.0"));
        assert!(!satisfies("^1.2", "1.1"));
        assert!(!satisfies("^2", "1.1"));
        assert!(!satisfies("latest", "1.1"));
    }
}
