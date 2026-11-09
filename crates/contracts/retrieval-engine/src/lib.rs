//! The `retrieval.engine@1` capability (docs/13-blocks.md §3.3, `documents`): the index
//! behind retrieval. The `documents` module keeps sources and chunks itself and has a
//! built-in lexical engine (BM25); a node serving this capability, wired into the
//! `engine` input of `ingest` and `retrieve`, replaces it (embeddings, vectors, hybrid)
//! without touching that module.
//!
//! The format belongs to the capability, not to any module. Adding an optional field
//! keeps `@1`; changing a shape means `retrieval.engine@2`.

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CAPABILITY: &str = "retrieval.engine@1";

/// Adds chunks to a collection: `NodeCall` with [`IndexArgs`] → anything.
pub const INDEX: &str = "retrieval.index";
/// Drops every chunk of a document: `NodeCall` with [`RemoveArgs`] → anything.
pub const REMOVE: &str = "retrieval.remove";
/// Finds chunks: `NodeCall` with [`QueryArgs`] → [`Hits`], best first.
pub const QUERY: &str = "retrieval.query";

/// One chunk to index, with its provenance: where it comes from and where in it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Chunk {
    /// `<doc_id>:<index>`.
    pub chunk_id: String,
    /// `<source_id>#<version>`: a new version of a source is a new document.
    pub doc_id: String,
    pub source_id: String,
    /// The source's file path, or `text:<title>`.
    #[serde(default)]
    pub location: String,
    /// Position among the document's chunks.
    #[serde(default)]
    pub index: usize,
    /// Byte offsets into the document's text, `end` excluded.
    #[serde(default)]
    pub start: usize,
    #[serde(default)]
    pub end: usize,
    pub text: String,
    #[serde(default)]
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IndexArgs {
    pub collection: String,
    pub chunks: Vec<Chunk>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveArgs {
    pub collection: String,
    pub doc_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryArgs {
    pub collection: String,
    pub query: String,
    pub k: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub chunk_id: String,
    pub score: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Hits {
    pub hits: Vec<Hit>,
}
