//! The `llm.endpoint@1` capability (docs/12-agents.md §6.1): a provider describes where
//! a model is and how to reach it, and whoever uses it talks to the model directly.
//! Regulus never sits in the traffic.
//!
//! The format belongs to the capability, not to any module. Adding an optional field
//! keeps `@1`; changing a shape means `llm.endpoint@2`.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const CAPABILITY: &str = "llm.endpoint@1";

/// The description of the endpoint: `NodeCall` with no args → [`LlmEndpoint`].
pub const DESCRIBE: &str = "llm.endpoint";

/// The protocols known today. `protocol` is plain data, so a new one does not change the
/// capability: each consumer says which ones it speaks.
pub mod protocol {
    /// Anthropic Messages. `base_url` as `ANTHROPIC_BASE_URL` takes it, without `/v1`.
    pub const ANTHROPIC: &str = "anthropic";
    /// OpenAI Chat Completions. `base_url` as `OPENAI_BASE_URL` takes it, with `/v1`.
    pub const OPENAI: &str = "openai";
    pub const BEDROCK: &str = "bedrock";
    pub const VERTEX: &str = "vertex";
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LlmEndpoint {
    pub protocol: String,
    pub base_url: String,
    /// Sent with every request, credentials included.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// Model ids offered; the first is the default. Empty: whatever the consumer picks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
}

impl LlmEndpoint {
    /// A header by name, ignoring case as HTTP does.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}
