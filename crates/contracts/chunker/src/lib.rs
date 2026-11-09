//! The `chunker@1` capability (docs/13-blocks.md §3.3, `documents`): a node that cuts a
//! text into the pieces a retrieval index holds. The `documents` module's `ingest` node
//! uses one wired into its `chunker` input instead of its built-in chunkers.
//!
//! The format belongs to the capability, not to any module. Adding an optional field
//! keeps `@1`; changing a shape means `chunker@2`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CAPABILITY: &str = "chunker@1";

/// Cuts one text: `NodeCall` with [`SplitArgs`] → [`Split`].
pub const SPLIT: &str = "chunker.split";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SplitArgs {
    pub text: String,
    /// What the source carries (title, kind, location and the user's metadata).
    #[serde(default)]
    pub metadata: Value,
}

/// One piece: `start` and `end` are byte offsets into the text, `end` excluded.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Piece {
    pub start: usize,
    pub end: usize,
    pub text: String,
    /// Kept with the chunk (a heading path, a page…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata: Option<Value>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Split {
    pub chunks: Vec<Piece>,
}
