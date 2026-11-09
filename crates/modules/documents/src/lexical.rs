//! The built-in index: BM25 over lowercase word tokens, kept in the store as an inverted
//! index per collection. `lex/tok/<token>` holds the postings of a token (chunk id →
//! term frequency and chunk length), `lex/doc/<doc_id>` the tokens of a document (to
//! remove it), `lex/stats` the chunk count and total length.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use regulus_core::ModuleError;
use serde::{Deserialize, Serialize};

use crate::store::Collection;

/// How text becomes tokens: the same at ingest and at query time.
#[derive(Debug, Clone)]
pub struct Tokenizer {
    pub stopwords: HashSet<String>,
    pub min_chars: usize,
}

impl Tokenizer {
    /// `stopwords` comma separated.
    pub fn new(stopwords: &str, min_chars: usize) -> Self {
        Self {
            stopwords: stopwords
                .split(',')
                .map(|w| w.trim().to_lowercase())
                .filter(|w| !w.is_empty())
                .collect(),
            min_chars,
        }
    }

    pub fn tokens(&self, text: &str) -> Vec<String> {
        text.split(|c: char| !c.is_alphanumeric())
            .filter(|w| w.chars().count() >= self.min_chars.max(1))
            .map(str::to_lowercase)
            .filter(|w| !self.stopwords.contains(w))
            .collect()
    }
}

/// BM25 parameters.
#[derive(Debug, Clone, Copy)]
pub struct Bm25 {
    pub k1: f64,
    pub b: f64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
struct Stats {
    chunks: u64,
    tokens: u64,
}

/// Postings of one token: chunk id → (term frequency, chunk length in tokens).
type Postings = BTreeMap<String, (u32, u32)>;

fn token_key(col: &Collection, token: &str) -> String {
    col.key(&format!("lex/tok/{token}"))
}

fn doc_key(col: &Collection, doc_id: &str) -> String {
    col.key(&format!("lex/doc/{doc_id}"))
}

fn stats(col: &Collection) -> Result<Stats, ModuleError> {
    Ok(col.kv.get(&col.key("lex/stats"))?.unwrap_or_default())
}

/// Indexes the chunks of one document: `(chunk_id, text)`.
pub fn add(
    col: &Collection,
    tokenizer: &Tokenizer,
    doc_id: &str,
    chunks: &[(String, String)],
) -> Result<(), ModuleError> {
    let mut by_token: BTreeMap<String, Vec<(String, u32, u32)>> = BTreeMap::new();
    let mut total = 0u64;
    for (chunk_id, text) in chunks {
        let tokens = tokenizer.tokens(text);
        let length = u32::try_from(tokens.len()).unwrap_or(u32::MAX);
        total += u64::from(length);
        let mut counts: HashMap<String, u32> = HashMap::new();
        for token in tokens {
            *counts.entry(token).or_default() += 1;
        }
        for (token, tf) in counts {
            by_token
                .entry(token)
                .or_default()
                .push((chunk_id.clone(), tf, length));
        }
    }
    for (token, entries) in &by_token {
        let key = token_key(col, token);
        let mut postings: Postings = col.kv.get(&key)?.unwrap_or_default();
        for (chunk_id, tf, length) in entries {
            postings.insert(chunk_id.clone(), (*tf, *length));
        }
        col.kv.set(&key, &postings)?;
    }
    let tokens: Vec<&String> = by_token.keys().collect();
    col.kv.set(&doc_key(col, doc_id), &tokens)?;
    let mut stats = stats(col)?;
    stats.chunks += chunks.len() as u64;
    stats.tokens += total;
    col.kv.set(&col.key("lex/stats"), &stats)
}

/// Drops every chunk of `doc_id` from the index.
pub fn remove(col: &Collection, doc_id: &str) -> Result<(), ModuleError> {
    let key = doc_key(col, doc_id);
    let Some(tokens) = col.kv.get::<Vec<String>>(&key)? else {
        return Ok(());
    };
    let prefix = format!("{doc_id}:");
    let mut lengths: BTreeMap<String, u32> = BTreeMap::new();
    for token in tokens {
        let key = token_key(col, &token);
        let mut postings: Postings = col.kv.get(&key)?.unwrap_or_default();
        postings.retain(|chunk_id, (_, length)| {
            if chunk_id.starts_with(&prefix) {
                lengths.insert(chunk_id.clone(), *length);
                return false;
            }
            true
        });
        if postings.is_empty() {
            col.kv.delete(&key)?;
        } else {
            col.kv.set(&key, &postings)?;
        }
    }
    col.kv.delete(&key)?;
    // Chunks without a single token are in no postings: the stats drop what was seen.
    let mut stats = stats(col)?;
    stats.chunks = stats.chunks.saturating_sub(lengths.len() as u64);
    stats.tokens = stats
        .tokens
        .saturating_sub(lengths.values().map(|l| u64::from(*l)).sum());
    col.kv.set(&col.key("lex/stats"), &stats)
}

/// The best `k` chunks for `query`, best first; ties by chunk id.
pub fn query(
    col: &Collection,
    tokenizer: &Tokenizer,
    bm25: Bm25,
    query: &str,
    k: usize,
) -> Result<Vec<(String, f64)>, ModuleError> {
    let stats = stats(col)?;
    if stats.chunks == 0 {
        return Ok(Vec::new());
    }
    let n = stats.chunks as f64;
    let average = (stats.tokens as f64 / n).max(1.0);
    let wanted: BTreeSet<String> = tokenizer.tokens(query).into_iter().collect();
    let mut scores: HashMap<String, f64> = HashMap::new();
    for token in wanted {
        let Some(postings) = col.kv.get::<Postings>(&token_key(col, &token))? else {
            continue;
        };
        let df = postings.len() as f64;
        let idf = (1.0 + (n - df + 0.5) / (df + 0.5)).ln();
        for (chunk_id, (tf, length)) in postings {
            let tf = f64::from(tf);
            let norm = 1.0 - bm25.b + bm25.b * f64::from(length) / average;
            *scores.entry(chunk_id).or_default() +=
                idf * tf * (bm25.k1 + 1.0) / (tf + bm25.k1 * norm);
        }
    }
    let mut ranked: Vec<(String, f64)> = scores.into_iter().collect();
    ranked.sort_by(|(a, x), (b, y)| y.total_cmp(x).then_with(|| a.cmp(b)));
    ranked.truncate(k);
    Ok(ranked)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::store::Kv;

    const BM25: Bm25 = Bm25 { k1: 1.2, b: 0.75 };

    fn chunks(doc: &str, texts: &[&str]) -> Vec<(String, String)> {
        texts
            .iter()
            .enumerate()
            .map(|(i, t)| (format!("{doc}:{i}"), t.to_string()))
            .collect()
    }

    #[test]
    fn tokens_drop_stopwords_and_short_words() {
        let tokenizer = Tokenizer::new("the, of", 2);
        assert_eq!(
            tokenizer.tokens("The Rust book: a guide of Ünïcode"),
            ["rust", "book", "guide", "ünïcode"]
        );
    }

    #[test]
    fn bm25_ranks_rarer_words_higher_and_removal_cleans_up() {
        let col = Collection::new(Kv::Local(Arc::default()), "c").unwrap();
        let tokenizer = Tokenizer::new("the", 2);
        add(
            &col,
            &tokenizer,
            "a#1",
            &chunks("a#1", &["rust ownership borrow", "rust cargo crates"]),
        )
        .unwrap();
        add(
            &col,
            &tokenizer,
            "b#1",
            &chunks("b#1", &["python packages pip", "rust everywhere"]),
        )
        .unwrap();
        let found = query(&col, &tokenizer, BM25, "the cargo rust", 10).unwrap();
        assert_eq!(found[0].0, "a#1:1", "{found:?}");
        assert_eq!(found.len(), 3, "{found:?}");
        assert!(query(&col, &tokenizer, BM25, "the", 10).unwrap().is_empty());
        let top = query(&col, &tokenizer, BM25, "rust", 1).unwrap();
        assert_eq!(top.len(), 1);

        remove(&col, "a#1").unwrap();
        let found = query(&col, &tokenizer, BM25, "cargo rust", 10).unwrap();
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].0, "b#1:1");
        assert!(col
            .kv
            .get::<Postings>(&token_key(&col, "cargo"))
            .unwrap()
            .is_none());
        let stats = stats(&col).unwrap();
        assert_eq!((stats.chunks, stats.tokens), (2, 5));
        remove(&col, "b#1").unwrap();
        let stats = super::stats(&col).unwrap();
        assert_eq!((stats.chunks, stats.tokens), (0, 0));
        assert!(col
            .kv
            .list::<serde_json::Value>(&col.key("lex/tok/"))
            .unwrap()
            .is_empty());
    }
}
