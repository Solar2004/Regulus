//! The built-in chunkers: `fixed` windows, `paragraph` packing and `markdown` sections.
//! Offsets are byte offsets into the text, `end` excluded; sizes are counted in
//! characters.

use regulus_contract_chunker::Piece;
use serde_json::json;

/// What cuts a text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Fixed,
    Paragraph,
    Markdown,
}

impl Kind {
    pub fn parse(text: &str) -> Result<Self, String> {
        match text {
            "fixed" => Ok(Self::Fixed),
            "paragraph" => Ok(Self::Paragraph),
            "markdown" => Ok(Self::Markdown),
            other => Err(format!(
                "`chunker` is fixed, paragraph or markdown, not `{other}`"
            )),
        }
    }
}

/// How large chunks are.
#[derive(Debug, Clone, Copy)]
pub struct Size {
    pub chunk_chars: usize,
    pub overlap_chars: usize,
}

type Range = (usize, usize);

fn chars(text: &str, (start, end): Range) -> usize {
    text[start..end].chars().count()
}

/// Windows of `chunk_chars` over `range`, each sharing `overlap_chars` with the one
/// before. Always moves forward.
fn windows(text: &str, (start, end): Range, size: Size) -> Vec<Range> {
    let chunk = size.chunk_chars.max(1);
    let step = chunk - size.overlap_chars.min(chunk - 1);
    // Byte offset of every character of the range, and the end.
    let mut bounds: Vec<usize> = text[start..end]
        .char_indices()
        .map(|(i, _)| start + i)
        .collect();
    bounds.push(end);
    let count = bounds.len() - 1;
    let mut found = Vec::new();
    let mut at = 0;
    while at < count {
        let stop = (at + chunk).min(count);
        found.push((bounds[at], bounds[stop]));
        if stop == count {
            break;
        }
        at += step;
    }
    found
}

/// The paragraphs of `range`: runs of non-blank lines, without the line break that ends
/// the last one.
fn paragraphs(text: &str, (start, end): Range) -> Vec<Range> {
    let mut found = Vec::new();
    let mut open: Option<usize> = None;
    let mut last_end = start;
    let mut at = start;
    for line in text[start..end].split_inclusive('\n') {
        let line_start = at;
        at += line.len();
        let content = line.trim_end_matches(['\n', '\r']);
        if content.trim().is_empty() {
            if let Some(from) = open.take() {
                found.push((from, last_end));
            }
            continue;
        }
        open.get_or_insert(line_start);
        last_end = line_start + content.len();
    }
    if let Some(from) = open {
        found.push((from, last_end));
    }
    found
}

/// Paragraphs packed into chunks of at most `chunk_chars`; a longer paragraph is cut
/// into windows. The trailing paragraphs of a chunk that fit in `overlap_chars` start
/// the next one.
fn pack(text: &str, paragraphs: &[Range], size: Size) -> Vec<Range> {
    let mut found = Vec::new();
    let mut group: Vec<Range> = Vec::new();
    for &paragraph in paragraphs {
        if chars(text, paragraph) > size.chunk_chars {
            if let (Some(first), Some(last)) = (group.first(), group.last()) {
                found.push((first.0, last.1));
            }
            group.clear();
            found.extend(windows(text, paragraph, size));
            continue;
        }
        if let Some(first) = group.first() {
            if chars(text, (first.0, paragraph.1)) > size.chunk_chars {
                found.push((first.0, group.last().expect("not empty").1));
                let mut kept = Vec::new();
                let mut used = 0;
                for &previous in group.iter().rev() {
                    used += chars(text, previous);
                    if used > size.overlap_chars
                        || chars(text, (previous.0, paragraph.1)) > size.chunk_chars
                    {
                        break;
                    }
                    kept.insert(0, previous);
                }
                group = kept;
            }
        }
        group.push(paragraph);
    }
    if let (Some(first), Some(last)) = (group.first(), group.last()) {
        found.push((first.0, last.1));
    }
    found
}

/// A heading line: its level and title.
fn heading(line: &str) -> Option<(usize, String)> {
    let indent = line.len() - line.trim_start_matches(' ').len();
    if indent > 3 {
        return None;
    }
    let rest = &line[indent..];
    let level = rest.len() - rest.trim_start_matches('#').len();
    if !(1..=6).contains(&level) {
        return None;
    }
    let after = &rest[level..];
    if !after.is_empty() && !after.starts_with([' ', '\t']) {
        return None;
    }
    let title = after.trim().trim_end_matches('#').trim().to_string();
    Some((level, title))
}

/// Sections by heading, outside code fences: each with its range and heading path.
fn sections(text: &str) -> Vec<(Range, Vec<String>)> {
    let mut found = Vec::new();
    let mut path: Vec<(usize, String)> = Vec::new();
    let mut from = 0;
    let mut fence: Option<&str> = None;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let line_start = at;
        at += line.len();
        let content = line.trim_end_matches(['\n', '\r']);
        let trimmed = content.trim_start();
        if let Some(open) = fence {
            if trimmed.starts_with(open) {
                fence = None;
            }
            continue;
        }
        if trimmed.starts_with("```") {
            fence = Some("```");
            continue;
        }
        if trimmed.starts_with("~~~") {
            fence = Some("~~~");
            continue;
        }
        let Some((level, title)) = heading(content) else {
            continue;
        };
        if line_start > from {
            found.push(((from, line_start), titles(&path)));
        }
        while path.last().is_some_and(|(l, _)| *l >= level) {
            path.pop();
        }
        path.push((level, title));
        from = line_start;
    }
    if text.len() > from {
        found.push(((from, text.len()), titles(&path)));
    }
    found
}

fn titles(path: &[(usize, String)]) -> Vec<String> {
    path.iter().map(|(_, t)| t.clone()).collect()
}

fn piece(text: &str, (start, end): Range, headings: &[String]) -> Piece {
    Piece {
        start,
        end,
        text: text[start..end].to_string(),
        metadata: (!headings.is_empty()).then(|| json!({ "headings": headings })),
    }
}

/// `text` cut by `kind`; blank text gives no chunk.
pub fn split(kind: Kind, text: &str, size: Size) -> Vec<Piece> {
    let whole = (0, text.len());
    match kind {
        Kind::Fixed => {
            if text.trim().is_empty() {
                return Vec::new();
            }
            windows(text, whole, size)
                .into_iter()
                .map(|r| piece(text, r, &[]))
                .collect()
        }
        Kind::Paragraph => pack(text, &paragraphs(text, whole), size)
            .into_iter()
            .map(|r| piece(text, r, &[]))
            .collect(),
        Kind::Markdown => sections(text)
            .into_iter()
            .flat_map(|(range, headings)| {
                pack(text, &paragraphs(text, range), size)
                    .into_iter()
                    .map(move |r| (r, headings.clone()))
            })
            .map(|(r, headings)| piece(text, r, &headings))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIZE: Size = Size {
        chunk_chars: 10,
        overlap_chars: 3,
    };

    fn texts(pieces: &[Piece]) -> Vec<&str> {
        pieces.iter().map(|p| p.text.as_str()).collect()
    }

    #[test]
    fn fixed_windows_overlap_and_respect_characters() {
        let text = "ñandú-0123456789";
        let pieces = split(Kind::Fixed, text, SIZE);
        assert_eq!(texts(&pieces), ["ñandú-0123", "123456789"]);
        for p in &pieces {
            assert_eq!(&text[p.start..p.end], p.text);
        }
        assert!(split(Kind::Fixed, "  \n", SIZE).is_empty());
        // An overlap as large as the chunk still moves forward.
        let big = Size {
            chunk_chars: 4,
            overlap_chars: 9,
        };
        assert_eq!(
            texts(&split(Kind::Fixed, "abcdef", big)),
            ["abcd", "bcde", "cdef"]
        );
    }

    #[test]
    fn paragraphs_pack_with_whole_paragraph_overlap() {
        let text = "aaa\n\nbbb\n\nccc\n\ndddddddddddddd\n";
        let pieces = split(Kind::Paragraph, text, SIZE);
        assert_eq!(
            texts(&pieces),
            ["aaa\n\nbbb", "bbb\n\nccc", "dddddddddd", "ddddddd"]
        );
        for p in &pieces {
            assert_eq!(&text[p.start..p.end], p.text);
        }
        let none = Size {
            chunk_chars: 100,
            overlap_chars: 0,
        };
        assert_eq!(
            texts(&split(Kind::Paragraph, text, none)),
            ["aaa\n\nbbb\n\nccc\n\ndddddddddddddd"]
        );
    }

    #[test]
    fn markdown_keeps_the_heading_path_and_ignores_fenced_hashes() {
        let text = "intro\n\n# Guide\n\nhello\n\n## Install\n\n```sh\n# not a heading\n```\n\n# Other\nend";
        let size = Size {
            chunk_chars: 200,
            overlap_chars: 0,
        };
        let pieces = split(Kind::Markdown, text, size);
        let found: Vec<(String, Option<serde_json::Value>)> = pieces
            .iter()
            .map(|p| (p.text.clone(), p.metadata.clone()))
            .collect();
        assert_eq!(
            found,
            [
                ("intro".to_string(), None),
                (
                    "# Guide\n\nhello".into(),
                    Some(json!({ "headings": ["Guide"] }))
                ),
                (
                    "## Install\n\n```sh\n# not a heading\n```".into(),
                    Some(json!({ "headings": ["Guide", "Install"] }))
                ),
                (
                    "# Other\nend".into(),
                    Some(json!({ "headings": ["Other"] }))
                ),
            ]
        );
        assert_eq!(heading("#hash"), None);
        assert_eq!(heading("### Title ###"), Some((3, "Title".into())));
        assert!(Kind::parse("semantic").is_err());
    }
}
