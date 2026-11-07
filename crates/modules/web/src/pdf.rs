//! The text of a PDF, with `lopdf`: pages in order, at most `max_pages`, the result cut
//! at `max_chars`. Decompressed content per page is bounded too, against bombs.

/// Bytes one page's content may decompress to.
const MAX_PAGE_CONTENT: usize = 64 * 1024 * 1024;

pub(crate) fn extract(bytes: &[u8], max_pages: usize, max_chars: usize) -> Result<String, String> {
    let document =
        lopdf::Document::load_mem(bytes).map_err(|e| format!("not a readable PDF: {e}"))?;
    if document.is_encrypted() {
        return Err("the PDF is encrypted".into());
    }
    let pages: Vec<u32> = document
        .get_pages()
        .keys()
        .copied()
        .take(max_pages)
        .collect();
    let mut text = String::new();
    for page in pages {
        let found = document
            .extract_text_with_limit(&[page], MAX_PAGE_CONTENT)
            .map_err(|e| format!("page {page}: {e}"))?;
        text.push_str(found.trim_end());
        text.push_str("\n\n");
        if text.chars().count() > max_chars {
            break;
        }
    }
    let text = text.trim_end().to_string();
    Ok(match text.char_indices().nth(max_chars) {
        Some((at, _)) => text[..at].to_string(),
        None => text,
    })
}
