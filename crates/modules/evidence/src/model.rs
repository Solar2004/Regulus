//! Sources, entries and the deterministic checks over them. No I/O and no clock here:
//! everything is a function of the records, so every verdict can be reproduced from an
//! export.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// What a check established, in every verdict, so nobody reads it as more.
pub const CHECKED: &str = "excerpt_inclusion";
/// What a check did not establish.
pub const ENTAILMENT: &str = "not_checked";

/// One version of a source that is not the current one: no text, just what identifies it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Past {
    pub version: u32,
    pub sha256: String,
    pub bytes: usize,
    pub location: String,
    pub at: u64,
}

/// A location and the text that was supplied for it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub id: String,
    /// Where the text came from: a URL, a path, a label. Never fetched or verified by
    /// this module: it says where to look, not that the text is what is there.
    pub location: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,
    /// 1 for the first text; one more each time the text changes.
    pub version: u32,
    /// SHA-256 of the text's UTF-8 bytes, lowercase hex: what an entry pins.
    pub sha256: String,
    pub bytes: usize,
    /// Milliseconds since the Unix epoch of the last change of the text.
    pub at: u64,
    /// Earlier versions, oldest first.
    #[serde(default)]
    pub history: Vec<Past>,
    /// Order of arrival, for listings.
    #[serde(default)]
    pub seq: u64,
    pub text: String,
}

impl Source {
    /// The source without its text, as listings and outputs show it.
    pub fn summary(&self) -> Value {
        let mut value = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(object) = value.as_object_mut() {
            object.remove("text");
            object.remove("history");
            object.remove("seq");
        }
        value
    }
}

/// One excerpt that backs a statement, pinned to the version of the source it was found in.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Support {
    pub source: String,
    /// The version and hash of the source when the entry was registered.
    pub version: u32,
    pub sha256: String,
    /// The source's location when it was cited; a source that comes back at another
    /// location does not validate the citation. Empty in records from before this field.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub location: String,
    /// The exact slice of the source's text, whatever form the writer quoted it in.
    pub excerpt: String,
    /// What the writer quoted, when it differs from `excerpt` (whitespace matching).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub cited: String,
    /// Character offsets of `excerpt` in the text (end exclusive).
    pub start: usize,
    pub end: usize,
}

/// A statement and the excerpts that back it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub statement: String,
    pub supports: Vec<Support>,
    pub at: u64,
    /// Who registered it: an agent's `@name` through the tools, or `node:<id>`.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub by: String,
    #[serde(default)]
    pub seq: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Valid,
    /// The excerpt is not (or no longer) at the pinned offsets of the pinned text.
    ExcerptNotFound,
    /// The source has another text now: the citation was made against a text that is gone.
    Stale,
    /// There is no such source.
    MissingSource,
}

/// A support checked against the sources as they are now.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SupportCheck {
    pub source: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    pub pinned_version: u32,
    pub pinned_sha256: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_sha256: Option<String>,
    pub excerpt: String,
    pub start: usize,
    pub end: usize,
    /// `stale` only: whether the excerpt still occurs in the new text. Informational: the
    /// citation stays stale until it is made again against the new version.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub still_in_current: Option<bool>,
    pub detail: String,
}

/// An entry with the verdict on each of its supports.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EntryCheck {
    pub id: String,
    pub statement: String,
    /// Valid only if every support is; otherwise the worst one.
    pub status: Status,
    pub supports: Vec<SupportCheck>,
    pub at: u64,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub by: String,
    /// What was established: that the excerpts are in the sources, as cited.
    pub checked: &'static str,
    /// What was not: that the statement is true or follows from the excerpts.
    pub entailment: &'static str,
}

/// Checks one entry against the sources by id.
pub fn check_entry(entry: &Entry, source: impl Fn(&str) -> Option<Source>) -> EntryCheck {
    let supports: Vec<SupportCheck> = entry
        .supports
        .iter()
        .map(|support| check_support(support, source(&support.source).as_ref()))
        .collect();
    let status = supports
        .iter()
        .map(|s| s.status)
        .max()
        .unwrap_or(Status::MissingSource);
    EntryCheck {
        id: entry.id.clone(),
        statement: entry.statement.clone(),
        status,
        supports,
        at: entry.at,
        by: entry.by.clone(),
        checked: CHECKED,
        entailment: ENTAILMENT,
    }
}

fn check_support(support: &Support, source: Option<&Source>) -> SupportCheck {
    let mut check = SupportCheck {
        source: support.source.clone(),
        status: Status::Valid,
        location: None,
        pinned_version: support.version,
        pinned_sha256: support.sha256.clone(),
        current_version: None,
        current_sha256: None,
        excerpt: support.excerpt.clone(),
        start: support.start,
        end: support.end,
        still_in_current: None,
        detail: String::new(),
    };
    let Some(source) = source else {
        check.status = Status::MissingSource;
        check.detail = format!("there is no source `{}`", support.source);
        return check;
    };
    check.location = Some(source.location.clone());
    check.current_version = Some(source.version);
    check.current_sha256 = Some(source.sha256.clone());
    if !support.location.is_empty() && source.location != support.location {
        check.status = Status::Stale;
        check.detail = format!(
            "cited when `{}` was at {}, which is now at {}: cite it again",
            support.source, support.location, source.location
        );
        return check;
    }
    if source.sha256 != support.sha256 {
        check.status = Status::Stale;
        check.still_in_current = Some(source.text.contains(&support.excerpt));
        check.detail = format!(
            "cited against version {} of `{}`, which is now version {}: cite it again",
            support.version, support.source, source.version
        );
        return check;
    }
    // Same text as when it was cited. The slice is checked anyway: a record that was
    // altered after the fact must not validate.
    if char_slice(&source.text, support.start, support.end) != support.excerpt {
        check.status = Status::ExcerptNotFound;
        check.detail = format!(
            "the excerpt is not at characters {}..{} of `{}`",
            support.start, support.end, support.source
        );
    }
    check
}

/// How an excerpt is looked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Match {
    Exact,
    /// Runs of whitespace are one space.
    Whitespace,
}

impl Match {
    pub fn parse(name: &str) -> Self {
        if name == "whitespace" {
            Self::Whitespace
        } else {
            Self::Exact
        }
    }
}

/// Byte offset of the `chars`-th character (or the end).
fn byte_at(text: &str, chars: usize) -> usize {
    text.char_indices()
        .nth(chars)
        .map_or(text.len(), |(b, _)| b)
}

/// The characters `start..end` of `text` (clamped to its end).
pub fn char_slice(text: &str, start: usize, end: usize) -> &str {
    let from = byte_at(text, start);
    let to = from + byte_at(&text[from..], end.saturating_sub(start));
    &text[from..to]
}

/// `text` with every run of whitespace as one space, and for each character of the result
/// the byte range of the original it stands for.
fn collapse(text: &str) -> (String, Vec<(usize, usize)>) {
    let mut out = String::with_capacity(text.len());
    let mut spans: Vec<(usize, usize)> = Vec::with_capacity(text.len());
    let mut in_space = false;
    for (at, c) in text.char_indices() {
        let end = at + c.len_utf8();
        if c.is_whitespace() {
            if in_space {
                if let Some(last) = spans.last_mut() {
                    last.1 = end;
                }
            } else {
                out.push(' ');
                spans.push((at, end));
            }
            in_space = true;
        } else {
            out.push(c);
            spans.push((at, end));
            in_space = false;
        }
    }
    (out, spans)
}

/// Where `excerpt` is in `text`: its first occurrence, as character offsets (end
/// exclusive). With `Whitespace`, the excerpt is trimmed and its whitespace runs match any
/// whitespace run of the text.
pub fn locate(text: &str, excerpt: &str, mode: Match) -> Option<(usize, usize)> {
    match mode {
        Match::Exact => {
            if excerpt.is_empty() {
                return None;
            }
            let at = text.find(excerpt)?;
            let start = text[..at].chars().count();
            Some((start, start + excerpt.chars().count()))
        }
        Match::Whitespace => {
            let (needle, _) = collapse(excerpt.trim());
            if needle.is_empty() {
                return None;
            }
            let (hay, spans) = collapse(text);
            let at = hay.find(&needle)?;
            let first = hay[..at].chars().count();
            let last = first + needle.chars().count() - 1;
            let from = spans[first].0;
            let to = spans[last].1;
            Some((text[..from].chars().count(), text[..to].chars().count()))
        }
    }
}

/// SHA-256 of `text`, lowercase hex.
pub fn sha256(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// An id that is safe in a key, a marker and a URL: 1 to 64 of `A-Za-z0-9._-`.
pub fn check_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return Err(format!(
            "`{id}` is not an id: use 1 to 64 of letters, digits, `.`, `_` and `-`"
        ));
    }
    Ok(())
}

/// `text` cut at `max_chars` characters, with an ellipsis when it was.
pub fn cut(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((at, _)) => format!("{}…", &text[..at]),
        None => text.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locating_exact_and_across_whitespace() {
        let text = "Intro.\nThe borrow checker\n  enforces ownership   rules.\nEnd é.";
        assert_eq!(locate(text, "borrow checker", Match::Exact), Some((11, 25)));
        // A line break and indentation in the source are not in the quote.
        assert_eq!(
            locate(
                text,
                "borrow checker enforces ownership rules",
                Match::Exact
            ),
            None
        );
        let (start, end) = locate(
            text,
            "borrow checker enforces ownership rules",
            Match::Whitespace,
        )
        .unwrap();
        assert_eq!(
            char_slice(text, start, end),
            "borrow checker\n  enforces ownership   rules"
        );
        // Offsets are characters, not bytes: `é` is two bytes.
        let (s, e) = locate(text, "End é", Match::Exact).unwrap();
        assert_eq!(char_slice(text, s, e), "End é");
        assert_eq!(locate(text, "", Match::Exact), None);
        assert_eq!(locate(text, "   ", Match::Whitespace), None);
        assert_eq!(locate(text, "absent", Match::Whitespace), None);
        assert_eq!(char_slice("héllo", 1, 3), "él");
        assert_eq!(char_slice("abc", 2, 99), "c");
    }

    #[test]
    fn ids_are_plain() {
        assert!(check_id("S1").is_ok());
        assert!(check_id("a.b_c-9").is_ok());
        assert!(check_id("").is_err());
        assert!(check_id("a/b").is_err());
        assert!(check_id("a b").is_err());
        assert!(check_id(&"x".repeat(65)).is_err());
    }

    fn source(text: &str) -> Source {
        Source {
            id: "S1".into(),
            location: "http://x/y".into(),
            title: String::new(),
            version: 1,
            sha256: sha256(text),
            bytes: text.len(),
            at: 1,
            history: vec![],
            seq: 1,
            text: text.into(),
        }
    }

    fn entry_for(source: &Source, excerpt: &str) -> Entry {
        let (start, end) = locate(&source.text, excerpt, Match::Exact).unwrap();
        Entry {
            id: "E1".into(),
            statement: "s".into(),
            supports: vec![Support {
                source: source.id.clone(),
                version: source.version,
                sha256: source.sha256.clone(),
                location: source.location.clone(),
                excerpt: excerpt.into(),
                cited: String::new(),
                start,
                end,
            }],
            at: 2,
            by: String::new(),
            seq: 1,
        }
    }

    #[test]
    fn a_changed_missing_or_altered_source_never_validates() {
        let original = source("The borrow checker enforces ownership.");
        let entry = entry_for(&original, "borrow checker enforces");
        let found = |s: &Source| {
            let s = s.clone();
            move |id: &str| (id == "S1").then(|| s.clone())
        };
        let ok = check_entry(&entry, found(&original));
        assert_eq!(ok.status, Status::Valid);
        assert_eq!(ok.entailment, "not_checked");

        // The text changes and still contains the quote: stale all the same.
        let mut changed = source("Notes: the borrow checker enforces ownership, mostly.");
        changed.version = 2;
        let stale = check_entry(&entry, found(&changed));
        assert_eq!(stale.status, Status::Stale);
        assert_eq!(stale.supports[0].still_in_current, Some(true));
        assert_eq!(stale.supports[0].current_version, Some(2));

        let missing = check_entry(&entry, |_| None);
        assert_eq!(missing.status, Status::MissingSource);

        // A record altered in place (same hash, other offsets) does not validate.
        let mut moved = entry.clone();
        moved.supports[0].start += 1;
        moved.supports[0].end += 1;
        assert_eq!(
            check_entry(&moved, found(&original)).status,
            Status::ExcerptNotFound
        );

        // The worst support decides.
        let mut two = entry.clone();
        two.supports.push(Support {
            source: "S9".into(),
            ..entry.supports[0].clone()
        });
        assert_eq!(
            check_entry(&two, found(&original)).status,
            Status::MissingSource
        );
    }
}
