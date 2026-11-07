//! HTML to readable text or Markdown, by hand: scripts, styles and the head go; blocks
//! become lines; with Markdown, headings, list items, links, emphasis and code keep their
//! marks. Not a browser: what a page builds with JavaScript is not there.

/// Elements whose content is never text for a reader.
const SKIPPED: &[&str] = &[
    "script", "style", "noscript", "template", "svg", "head", "iframe", "object", "canvas",
];
/// Elements that start and end a block.
const BLOCKS: &[&str] = &[
    "p",
    "div",
    "section",
    "article",
    "header",
    "footer",
    "main",
    "nav",
    "aside",
    "ul",
    "ol",
    "table",
    "tr",
    "blockquote",
    "figure",
    "figcaption",
    "form",
    "dl",
    "dt",
    "dd",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "li",
    "pre",
    "hr",
    "br",
    "title",
    "body",
    "html",
];

fn decode(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some(' '),
        "mdash" => Some('—'),
        "ndash" => Some('–'),
        "hellip" => Some('…'),
        "copy" => Some('©'),
        _ => {
            let number = entity.strip_prefix('#')?;
            let code = match number.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok()?,
                None => number.parse().ok()?,
            };
            char::from_u32(code)
        }
    }
}

/// Text with its entities decoded.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let end = rest[1..].find(';').map(|e| e + 1).filter(|e| *e <= 10);
        match end.and_then(|e| decode(&rest[1..e]).map(|c| (e, c))) {
            Some((e, c)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// The value of `name` in a tag's attributes, if it has one.
fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let mut from = 0;
    while let Some(found) = lower[from..].find(name) {
        let at = from + found;
        from = at + name.len();
        let before = lower[..at].chars().last();
        if !before.is_some_and(char::is_whitespace) {
            continue;
        }
        let rest = tag[from..].trim_start();
        let Some(rest) = rest.strip_prefix('=') else {
            continue;
        };
        let rest = rest.trim_start();
        let value = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].split(q).next().unwrap_or_default(),
            _ => rest
                .split(|c: char| c.is_whitespace() || c == '>')
                .next()
                .unwrap_or_default(),
        };
        return Some(unescape(value));
    }
    None
}

/// The readable text of `html`; with `markdown`, as Markdown.
pub fn convert(html: &str, markdown: bool) -> String {
    let mut out = String::new();
    let mut skipping: Option<String> = None;
    let mut pre = 0usize;
    let mut links: Vec<Option<String>> = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let Some(open) = rest.find('<') else {
            push_text(&mut out, rest, pre > 0, skipping.is_some());
            break;
        };
        push_text(&mut out, &rest[..open], pre > 0, skipping.is_some());
        rest = &rest[open..];
        if let Some(comment) = rest.strip_prefix("<!--") {
            rest = comment.find("-->").map_or("", |e| &comment[e + 3..]);
            continue;
        }
        let Some(close) = rest.find('>') else {
            break;
        };
        let tag = &rest[1..close];
        rest = &rest[close + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        if let Some(skipped) = &skipping {
            if closing && *skipped == name {
                skipping = None;
            }
            continue;
        }
        if !closing && SKIPPED.contains(&name.as_str()) && !tag.ends_with('/') {
            skipping = Some(name);
            continue;
        }
        if BLOCKS.contains(&name.as_str()) {
            let tight = matches!(name.as_str(), "br" | "li" | "tr") || (closing && name == "pre");
            let blank = !tight;
            line_break(&mut out, blank);
        }
        if name == "pre" {
            pre = if closing {
                pre.saturating_sub(1)
            } else {
                pre + 1
            };
        }
        if !markdown {
            if name == "li" && !closing {
                out.push_str("- ");
            }
            if (name == "td" || name == "th") && closing {
                out.push(' ');
            }
            continue;
        }
        match (name.as_str(), closing) {
            ("h1" | "h2" | "h3" | "h4" | "h5" | "h6", false) => {
                let level = name[1..].parse().unwrap_or(1);
                out.push_str(&"#".repeat(level));
                out.push(' ');
            }
            ("li", false) => out.push_str("- "),
            ("pre", _) => {
                out.push_str("```");
                line_break(&mut out, closing);
            }
            ("code", _) if pre == 0 => out.push('`'),
            ("strong" | "b", _) => out.push_str("**"),
            ("em" | "i", _) => out.push('*'),
            ("hr", false) => {
                out.push_str("---");
                line_break(&mut out, true);
            }
            ("a", false) => {
                let href = attribute(tag, "href").filter(|h| !h.starts_with("javascript:"));
                if href.is_some() {
                    out.push('[');
                }
                links.push(href);
            }
            ("a", true) => {
                if let Some(Some(href)) = links.pop() {
                    out.push_str(&format!("]({href})"));
                }
            }
            ("td" | "th", true) => out.push_str(" | "),
            _ => {}
        }
    }
    tidy(&out)
}

fn push_text(out: &mut String, text: &str, pre: bool, skipping: bool) {
    if skipping || text.is_empty() {
        return;
    }
    let text = unescape(text);
    if pre {
        out.push_str(&text);
        return;
    }
    for (n, word) in text.split_whitespace().enumerate() {
        let starts_with_space = n > 0 || text.starts_with(char::is_whitespace);
        if starts_with_space && !out.ends_with([' ', '\n']) && !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    if text.ends_with(char::is_whitespace) && !out.ends_with([' ', '\n']) && !out.is_empty() {
        out.push(' ');
    }
}

fn line_break(out: &mut String, blank: bool) {
    while out.ends_with(' ') {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    if blank && !out.is_empty() && !out.ends_with("\n\n") {
        out.push('\n');
    }
}

/// Trailing spaces off every line, at most one blank line in a row, no blank ends.
fn tidy(text: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if line.is_empty() && lines.last().is_none_or(|l| l.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

/// Whether a body looks like HTML.
pub fn is_html(content_type: &str, body: &str) -> bool {
    if content_type.contains("html") {
        return true;
    }
    let start = body
        .trim_start()
        .get(..15)
        .unwrap_or_default()
        .to_ascii_lowercase();
    content_type.is_empty() && (start.starts_with("<!doctype html") || start.starts_with("<html"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PAGE: &str = "<!DOCTYPE html><html><head><title>T</title><style>p{}</style>\
        <script>alert(1)</script></head><body><h1>Hello &amp; welcome</h1>\
        <p>Some <b>bold</b> and <a href=\"/x?a=1&amp;b=2\">a link</a>.</p><!-- hidden -->\
        <ul><li>one</li><li>two</li></ul><pre>let x = 1;\n  y</pre></body></html>";

    #[test]
    fn markdown_keeps_marks_and_text_drops_them() {
        assert_eq!(
            convert(PAGE, true),
            "# Hello & welcome\n\nSome **bold** and [a link](/x?a=1&b=2).\n\n- one\n- two\n\n```\nlet x = 1;\n  y\n```"
        );
        assert_eq!(
            convert(PAGE, false),
            "Hello & welcome\n\nSome bold and a link.\n\n- one\n- two\n\nlet x = 1;\n  y"
        );
    }

    #[test]
    fn entities_and_detection() {
        assert_eq!(
            unescape("a &lt;b&gt; &#65;&#x42; &bogus; &"),
            "a <b> AB &bogus; &"
        );
        assert!(is_html("text/html; charset=utf-8", ""));
        assert!(is_html("", "  <!DOCTYPE html><p>"));
        assert!(!is_html("text/plain", "<html>"));
    }
}
