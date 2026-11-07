//! Plain data capabilities: what travels over "then" wires between nodes of any module
//! (docs/11-composition.md §4). Their format is a JSON value with no methods behind it,
//! so they belong to no module and live here, in one place. A capability with methods
//! (`skill@1`, `tool@1`…) has its own contract crate instead.
//!
//! Adding an optional reading keeps `@1`; changing what a value is means `@2`.

/// A JSON string of text: a prompt, an answer, a document.
pub const TEXT: &str = "text@1";

/// Any JSON value: an object, an array, a number. Arrays travel as this too.
pub const JSON: &str = "json@1";

/// A JSON string with the absolute path of a file.
pub const PATH: &str = "path@1";
