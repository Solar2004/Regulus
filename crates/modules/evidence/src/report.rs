//! The report check: a text cites entries by marker; it passes when every marker names an
//! entry that validates now. Pure functions over a [`Book`], so the verdict can be
//! recomputed from an export.

use regex::Regex;
use serde::Serialize;
use serde_json::{json, Value};

use crate::book::Book;
use crate::model::{cut, EntryCheck, Status, CHECKED, ENTAILMENT};

/// How a report is read and judged.
pub(crate) struct Settings {
    pub marker: Regex,
    pub min_citations: usize,
    pub paragraphs: bool,
    pub min_paragraph_chars: usize,
    pub max_excerpt_shown: usize,
    pub append_references: bool,
    pub header: String,
}

impl Settings {
    pub fn marker(pattern: &str) -> Result<Regex, String> {
        Regex::new(pattern).map_err(|e| format!("`marker` is not a regular expression: {e}"))
    }
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct Problem {
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub message: String,
}

pub(crate) struct Verdict {
    /// Distinct entry ids in the order they first appear.
    pub cited: Vec<String>,
    pub citations: Vec<EntryCheck>,
    pub problems: Vec<Problem>,
}

impl Verdict {
    pub fn valid(&self) -> bool {
        self.problems.is_empty()
    }

    pub fn to_json(&self) -> Value {
        json!({
            "valid": self.valid(),
            "check": CHECKED,
            "entailment": ENTAILMENT,
            "cited": self.cited,
            "citations": self.citations,
            "problems": self.problems,
        })
    }

    /// One problem per line, for `error` and for the agent.
    pub fn problem_text(&self) -> String {
        self.problems
            .iter()
            .map(|p| p.message.clone())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// The entry ids `text` cites, in order of first appearance.
fn markers(text: &str, marker: &Regex) -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for found in marker.captures_iter(text) {
        let id = found
            .get(1)
            .or_else(|| found.get(0))
            .map_or("", |m| m.as_str())
            .to_string();
        if !id.is_empty() && !ids.contains(&id) {
            ids.push(id);
        }
    }
    ids
}

/// Long paragraphs that cite nothing, as their first words. A heading line is exempt, but
/// only the heading line: the lines that follow it in the same block still need a citation.
fn uncited_paragraphs(text: &str, settings: &Settings) -> Vec<String> {
    let blank = Regex::new(r"\r?\n[ \t]*\r?\n").expect("the paragraph pattern is valid");
    blank
        .split(text)
        .map(|block| {
            let block = block.trim();
            if block.starts_with('#') {
                block.split_once('\n').map_or("", |(_, rest)| rest).trim()
            } else {
                block
            }
        })
        .filter(|p| !p.is_empty())
        .filter(|p| p.chars().count() >= settings.min_paragraph_chars)
        .filter(|p| !settings.marker.is_match(p))
        .map(|p| cut(&p.replace('\n', " "), 60))
        .collect()
}

/// Checks `text` against the book.
pub(crate) fn verify(text: &str, book: &Book, settings: &Settings) -> Result<Verdict, String> {
    let cited = markers(text, &settings.marker);
    let mut citations = Vec::new();
    let mut problems = Vec::new();
    for id in &cited {
        match book.check(id).map_err(|e| e.to_string())? {
            None => problems.push(Problem {
                kind: "unknown_entry",
                id: Some(id.clone()),
                message: format!("[{id}] cites an entry that does not exist"),
            }),
            Some(check) => {
                if check.status != Status::Valid {
                    let why = check
                        .supports
                        .iter()
                        .find(|s| s.status != Status::Valid)
                        .map(|s| s.detail.clone())
                        .unwrap_or_default();
                    problems.push(Problem {
                        kind: "invalid_entry",
                        id: Some(id.clone()),
                        message: format!(
                            "[{id}] does not validate ({}): {why}",
                            serde_json::to_value(check.status)
                                .ok()
                                .and_then(|v| v.as_str().map(String::from))
                                .unwrap_or_default()
                        ),
                    });
                }
                citations.push(check);
            }
        }
    }
    let valid = citations
        .iter()
        .filter(|c| c.status == Status::Valid)
        .count();
    if valid < settings.min_citations {
        problems.push(Problem {
            kind: "too_few_citations",
            id: None,
            message: format!(
                "the report cites {valid} valid entries; at least {} are needed (`min_citations`)",
                settings.min_citations
            ),
        });
    }
    if settings.paragraphs {
        for preview in uncited_paragraphs(text, settings) {
            problems.push(Problem {
                kind: "uncited_paragraph",
                id: None,
                message: format!("a paragraph cites nothing: \"{preview}\""),
            });
        }
    }
    Ok(Verdict {
        cited,
        citations,
        problems,
    })
}

/// The references of a passing report: each cited entry with its statement, the source's
/// location and version, and the exact excerpt.
pub(crate) fn references(verdict: &Verdict, settings: &Settings) -> String {
    let mut out = String::new();
    for check in &verdict.citations {
        out.push_str(&format!(
            "\n- [{}] {}",
            check.id,
            one_line(&check.statement)
        ));
        for support in &check.supports {
            let location = support.location.clone().unwrap_or_default();
            let link = location_markdown(&location);
            out.push_str(&format!(
                "\n  - {link} (source {}, version {}, sha256 {}, characters {}–{})\n    > {}",
                support.source,
                support.pinned_version,
                &support.pinned_sha256[..support.pinned_sha256.len().min(12)],
                support.start,
                support.end,
                cut(&one_line(&support.excerpt), settings.max_excerpt_shown),
            ));
        }
    }
    format!("{}\n{}", settings.header, out)
}

/// A location as Markdown that cannot add links of its own: a plain http(s) URL with none of
/// the characters that end or open a link is a link; anything else is a code span.
fn location_markdown(location: &str) -> String {
    let plain = (location.starts_with("http://") || location.starts_with("https://"))
        && !location
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || "()[]<>`\\\"'*_!|".contains(c));
    if plain {
        return format!("[{location}]({location})");
    }
    // A fence longer than any run of backticks inside it.
    let longest = location
        .split(|c| c != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let fence = "`".repeat(longest + 1);
    let flat = location.split_whitespace().collect::<Vec<_>>().join(" ");
    format!("{fence} {flat} {fence}")
}

fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The report with its references, if the settings ask for them.
pub(crate) fn rendered(text: &str, verdict: &Verdict, settings: &Settings) -> String {
    if !settings.append_references || verdict.citations.is_empty() {
        return text.to_string();
    }
    format!("{}\n\n{}\n", text.trim_end(), references(verdict, settings))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Settings {
        Settings {
            marker: Settings::marker(r"\[(E[0-9]+)\]").unwrap(),
            min_citations: 0,
            paragraphs: true,
            min_paragraph_chars: 20,
            max_excerpt_shown: 100,
            append_references: true,
            header: "## References".into(),
        }
    }

    #[test]
    fn only_the_heading_line_is_exempt_from_citing() {
        let s = settings();
        assert!(
            uncited_paragraphs("# Title\n\nCited claim that is long enough [E1].", &s).is_empty()
        );
        // A heading with text glued under it: the text still needs a citation.
        let found = uncited_paragraphs(
            "# Title\nA long uncited claim sits right under the heading",
            &s,
        );
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("A long uncited claim"));
        // A heading alone, and short blocks, need nothing.
        assert!(uncited_paragraphs("# Just a heading\n\nshort", &s).is_empty());
        assert_eq!(
            uncited_paragraphs("First long paragraph without any marker.", &s).len(),
            1
        );
    }

    #[test]
    fn a_location_cannot_inject_links_into_the_references() {
        assert_eq!(
            location_markdown("https://example.org/a?b=1"),
            "[https://example.org/a?b=1](https://example.org/a?b=1)"
        );
        for evil in [
            "https://x/) [click](https://evil.test)",
            "https://x/<script>",
            "https://x/a b",
            "https://x/[y]",
            "file:///tmp/a.md",
            "back`tick``s",
        ] {
            let md = location_markdown(evil);
            assert!(md.starts_with('`') && md.ends_with('`'), "{evil} -> {md}");
            // Inside a code span a link is text: nothing is a link unless it starts as one.
            assert!(!md.starts_with('['), "{evil} -> {md}");
        }
        assert_eq!(location_markdown("a``b"), "``` a``b ```");
    }
}
