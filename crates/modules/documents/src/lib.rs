//! Documents (docs/13-blocks.md §3.3): `ingest` keeps sources, documents and chunks in
//! the module's space of the store and indexes them; `retrieve` finds chunks for a
//! query; `context` turns hits into numbered evidence with refs; `cite_check` and
//! `recall` check, without a model, what came back.
//!
//! The index is the built-in lexical one ([`lexical`], BM25) unless a
//! `retrieval.engine@1` is wired into `engine`; the chunkers are the built-in ones
//! ([`chunk`]) unless a `chunker@1` is wired into `chunker`.

mod chunk;
mod cite;
mod lexical;
mod store;

use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use blake2::{Blake2b512, Digest};
use regulus_contract_chunker as chunker;
use regulus_contract_retrieval_engine as engine;
use regulus_contract_tool as tool;
use regulus_core::{parse_manifest, Endpoint, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Direction, Manifest, Node, NodeCall, NodeEvent, WireKind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub use store::{Chunk, Source};
use store::{Collection, Kv};

const MANIFEST: &str = include_str!("../regulus-module.toml");
/// `{ execution_id, node_id }` of a live `ingest` or `retrieve` → its collection's
/// sources.
pub const LIST: &str = "documents.list";
/// `{ execution_id, node_id, query, k? }` → hits, as `retrieve` sends them.
pub const QUERY: &str = "documents.query";
/// `{ execution_id, node_id, source_id }` → `{ deleted, chunks }`.
pub const DELETE: &str = "documents.delete";

const INGEST: &str = "ingest";
const RETRIEVE: &str = "retrieve";
const CONTEXT: &str = "context";
const CITE_CHECK: &str = "cite_check";
const RECALL: &str = "recall";

const INGESTED: &str = "ingested";
const HITS: &str = "hits";
const OUT: &str = "out";
const REFS: &str = "refs";
const PASS: &str = "pass";
const FAIL: &str = "fail";
const TEXT: &str = "text";
const SCORE: &str = "score";
const ERROR: &str = "error";
const ENGINE: &str = "engine";
const CHUNKER: &str = "chunker";
const YES: &str = "yes";

const KIND_FILE: &str = "file";
const KIND_TEXT: &str = "text";

const TOOL_SEARCH: &str = "search_documents";
const TOOL_READ: &str = "read_chunk";
const TOOL_INGEST: &str = "ingest_text";

/// Hex characters of a hash kept in a source id, a version and the version in a doc id.
const SOURCE_HEX: usize = 12;
const VERSION_HEX: usize = 16;
const DOC_VERSION_HEX: usize = 8;
/// Characters of a text's first line used as its title when it brings none.
const TITLE_CHARS: usize = 80;
/// Characters that make a path a glob.
const GLOB_CHARS: &[char] = &['*', '?', '['];

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn to_json(value: impl Serialize) -> Result<Value, ModuleError> {
    serde_json::to_value(value).map_err(|e| ModuleError::Failed(e.to_string()))
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

fn hex(bytes: &[u8]) -> String {
    Blake2b512::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// A source on its way in.
struct Incoming {
    kind: &'static str,
    location: String,
    title: String,
    text: String,
    metadata: Value,
}

pub struct Documents {
    manifest: Manifest,
    ctx: ModuleCtx,
    /// Collections when the core has no store.
    local: Arc<Mutex<BTreeMap<String, Vec<u8>>>>,
    /// Held while a collection changes: sources, chunks and index stay in step.
    writing: Mutex<()>,
    /// Live nodes, for the module's methods.
    live: Mutex<HashMap<(String, String), Node>>,
}

/// One node of one execution, with the settings the manifest defaults.
struct Here<'a> {
    docs: &'a Documents,
    node: Node,
    me: NodeRef,
}

impl Here<'_> {
    /// The node's setting, or the default a `retrieve` (then an `ingest`) has for it:
    /// the module's methods reach a collection through either kind of node.
    fn setting(&self, field: &str) -> Option<&Value> {
        let manifest = &self.docs.manifest;
        manifest.setting(&self.node, field).or_else(|| {
            [RETRIEVE, INGEST]
                .iter()
                .find_map(|kind| manifest.node_type(kind)?.field(field)?.default.as_ref())
        })
    }

    fn text(&self, field: &str) -> String {
        self.setting(field).map(text_of).unwrap_or_default()
    }

    fn number(&self, field: &str) -> f64 {
        match self.setting(field) {
            Some(Value::Number(n)) => n.as_f64().unwrap_or(0.0),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0.0),
            _ => 0.0,
        }
    }

    fn count(&self, field: &str) -> usize {
        self.number(field).max(0.0) as usize
    }

    fn has_output(&self, port: &str) -> bool {
        self.node
            .port(port)
            .is_some_and(|p| p.kind == WireKind::Then && p.direction == Direction::Output)
    }

    fn emit(&self, port: &str, value: Value) -> Result<(), ModuleError> {
        if !self.has_output(port) {
            return Ok(());
        }
        self.docs.ctx.emit(&self.me, port, value)
    }

    fn label(&self, message: impl std::fmt::Display) -> String {
        format!("{} `{}`: {message}", self.node.node_type, self.node.id)
    }

    /// `message` about this node, on `error` if it has one, and as the call's error.
    fn fail(&self, message: impl std::fmt::Display) -> ModuleError {
        let message = self.label(message);
        let _ = self.emit(ERROR, json!(message));
        ModuleError::Failed(message)
    }

    fn collection(&self) -> Result<Collection, String> {
        let kv = match self.docs.ctx.store(Some(&self.me)) {
            Ok(store) => Kv::Store(store),
            Err(_) => Kv::Local(self.docs.local.clone()),
        };
        Collection::new(kv, self.text("collection").trim())
    }

    fn wired(&self, port: &str) -> Option<Endpoint> {
        self.docs.ctx.resolve(&self.me, port).into_iter().next()
    }

    fn tokenizer(&self) -> lexical::Tokenizer {
        lexical::Tokenizer::new(&self.text("stopwords"), self.count("min_token_chars"))
    }

    /// The folder relative paths hang from: `dir` if absolute, else under the node's
    /// own directory.
    fn base(&self) -> Result<PathBuf, String> {
        let dir = self.text("dir");
        let dir = Path::new(dir.trim());
        if dir.is_absolute() {
            return Ok(dir.to_path_buf());
        }
        Ok(self
            .docs
            .ctx
            .node_dir(&self.me)
            .map_err(|e| e.to_string())?
            .join(dir))
    }
}

/// `path` with every symlink and `..` resolved; what does not exist yet must be plain
/// names under something that does.
fn real(path: &Path) -> Result<PathBuf, String> {
    let mut existing = path;
    let mut missing = Vec::new();
    loop {
        match existing.canonicalize() {
            Ok(mut found) => {
                found.extend(missing.iter().rev());
                return Ok(found);
            }
            Err(error) => {
                let name = match existing.components().next_back() {
                    Some(Component::Normal(name)) => name,
                    _ => return Err(format!("{}: {error}", path.display())),
                };
                missing.push(name);
                existing = existing
                    .parent()
                    .ok_or_else(|| format!("{}: {error}", path.display()))?;
            }
        }
    }
}

/// `path` resolved, if it lies under `base` resolved. The refusal says the same whether
/// the file exists or not.
fn confine(base: &Path, path: &Path) -> Result<PathBuf, String> {
    let base = base
        .canonicalize()
        .map_err(|e| format!("{}: {e}", base.display()))?;
    match real(path) {
        Ok(found) if found.starts_with(&base) => Ok(found),
        _ => Err(format!(
            "{} is outside {} and the node does not allow it (`allow_outside`)",
            path.display(),
            base.display()
        )),
    }
}

/// The bytes of a regular file, at most `max` of them.
fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let file = std::fs::File::open(path).map_err(|e| failed(&e))?;
    if !file.metadata().map_err(|e| failed(&e))?.is_file() {
        return Err(failed(&"not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(max + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| failed(&e))?;
    if bytes.len() as u64 > max {
        return Err(failed(&format!("larger than {max} bytes (`max_bytes`)")));
    }
    Ok(bytes)
}

impl Documents {
    pub fn new() -> Self {
        Self {
            manifest: parse_manifest(MANIFEST).expect("documents manifest is valid"),
            ctx: ModuleCtx::default(),
            local: Arc::default(),
            writing: Mutex::default(),
            live: Mutex::default(),
        }
    }

    fn here(&self, call: NodeCall) -> Here<'_> {
        Here {
            docs: self,
            me: NodeRef {
                execution_id: call.execution_id,
                node_id: call.node.id.clone(),
            },
            node: call.node,
        }
    }

    /// A live `ingest` or `retrieve` by id, for the module's methods.
    fn live(&self, params: &Value) -> Result<Here<'_>, ModuleError> {
        #[derive(Deserialize)]
        struct Target {
            execution_id: String,
            node_id: String,
        }
        let target: Target = parse(params.clone())?;
        let node = lock(&self.live)
            .get(&(target.execution_id.clone(), target.node_id.clone()))
            .cloned()
            .filter(|n| n.node_type == INGEST || n.node_type == RETRIEVE)
            .ok_or_else(|| {
                ModuleError::InvalidParams(format!(
                    "no `ingest` or `retrieve` node `{}` running in `{}`",
                    target.node_id, target.execution_id
                ))
            })?;
        Ok(Here {
            docs: self,
            me: NodeRef {
                execution_id: target.execution_id,
                node_id: target.node_id,
            },
            node,
        })
    }

    fn event(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        let here = self.here(call);
        match here.node.node_type.as_str() {
            INGEST => {
                let done = ingest_value(&here, &event.value).map_err(|e| here.fail(e))?;
                for ingested in &done {
                    here.emit(INGESTED, ingested.clone())?;
                }
                Ok(json!({ "ingested": done.len() }))
            }
            RETRIEVE => {
                let hits = retrieve_value(&here, &event.value).map_err(|e| here.fail(e))?;
                let count = hits["hits"].as_array().map_or(0, Vec::len);
                here.emit(HITS, hits)?;
                Ok(json!({ "hits": count }))
            }
            CONTEXT => {
                let made = cite::context(
                    &event.value,
                    &here.text("template"),
                    here.count("max_chars"),
                    &here.text("empty"),
                );
                here.emit(OUT, json!(made.text))?;
                here.emit(REFS, json!(made.refs))?;
                Ok(json!({ "refs": made.refs.len() }))
            }
            CITE_CHECK => {
                let (answer, refs) = cite::answer_and_refs(&event.value)
                    .map_err(|e| ModuleError::InvalidParams(here.label(e)))?;
                let rules = cite::CiteRules {
                    min_refs: here.count("min_refs"),
                    require_all: here.text("require_all") == YES,
                    sources_header: &here.text("sources_header"),
                };
                let (pass, verdict, text) = cite::cite_check(&answer, &refs, &rules);
                if pass {
                    here.emit(PASS, verdict)?;
                    here.emit(TEXT, json!(text))?;
                } else {
                    here.emit(FAIL, verdict)?;
                }
                Ok(json!({ "pass": pass }))
            }
            RECALL => {
                let score = cite::recall(&event.value, &here.text("expect_path"));
                let recall = score["recall"].as_f64().unwrap_or(0.0);
                // A recall of 0.8 computed as 0.7999… still meets a threshold of 0.8.
                let pass = score["total"].as_u64().unwrap_or(0) > 0
                    && recall >= here.number("threshold") - 1e-9;
                here.emit(SCORE, score.clone())?;
                here.emit(if pass { PASS } else { FAIL }, score)?;
                Ok(json!({ "recall": recall, "pass": pass }))
            }
            other => Err(ModuleError::InvalidParams(format!(
                "`{other}` takes nothing on `{}`",
                event.port
            ))),
        }
    }

    fn tool_call(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let request: tool::Call = parse(std::mem::take(&mut call.args))?;
        let here = self.here(call);
        // What went wrong is an answer for the agent to read, not a broken call.
        to_json(match run_tool(&here, &request) {
            Ok(data) => tool::Outcome { data, error: false },
            Err(e) => tool::Outcome {
                data: Value::String(e),
                error: true,
            },
        })
    }
}

/// What a value on `ingest`'s `in` names: one result per source.
fn ingest_value(here: &Here, value: &Value) -> Result<Vec<Value>, String> {
    let (path, text, title, metadata) = match value {
        Value::String(path) => (Some(path.clone()), None, None, Value::Null),
        Value::Object(o) => (
            o.get("path").and_then(Value::as_str).map(String::from),
            o.get("text").map(text_of),
            o.get("title").and_then(Value::as_str).map(String::from),
            o.get("metadata").cloned().unwrap_or(Value::Null),
        ),
        other => return Err(format!("takes a path or {{ path | text }}, not {other}")),
    };
    if let Some(text) = text {
        let incoming = text_source(text, title, metadata);
        return Ok(vec![add_source(here, incoming)?]);
    }
    let path = path
        .filter(|p| !p.trim().is_empty())
        .ok_or("the value has no `path` and no `text`")?;
    let files = files(here, &path)?;
    let single = files.len() == 1;
    files
        .into_iter()
        .map(|file| {
            let bytes = read_bounded(&file, here.number("max_bytes").max(0.0) as u64)?;
            let text = String::from_utf8(bytes)
                .map_err(|_| format!("{}: not UTF-8 text", file.display()))?;
            let name = file
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            add_source(
                here,
                Incoming {
                    kind: KIND_FILE,
                    location: file.display().to_string(),
                    title: title.clone().filter(|_| single).unwrap_or(name),
                    text,
                    metadata: metadata.clone(),
                },
            )
        })
        .collect()
}

fn text_source(text: String, title: Option<String>, metadata: Value) -> Incoming {
    let title = title.filter(|t| !t.trim().is_empty());
    let location = match &title {
        Some(title) => format!("text:{title}"),
        None => format!("text:{}", &hex(text.as_bytes())[..VERSION_HEX]),
    };
    let title = title.unwrap_or_else(|| {
        text.lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .chars()
            .take(TITLE_CHARS)
            .collect()
    });
    Incoming {
        kind: KIND_TEXT,
        location,
        title,
        text,
        metadata,
    }
}

/// The files `path` names, under the base unless `allow_outside`: the file, a folder's
/// files matching `glob`, or a glob's matches; sorted, at most `max_files`.
fn files(here: &Here, path: &str) -> Result<Vec<PathBuf>, String> {
    let base = here.base()?;
    let named = Path::new(path.trim());
    let named = if named.is_absolute() {
        named.to_path_buf()
    } else {
        base.join(named)
    };
    let pattern = if named.to_string_lossy().contains(GLOB_CHARS) {
        Some(named.to_string_lossy().into_owned())
    } else if named.is_dir() {
        Some(
            named
                .join(here.text("glob").trim())
                .to_string_lossy()
                .into_owned(),
        )
    } else {
        None
    };
    let outside = here.text("allow_outside") == YES;
    if !outside {
        // The literal part of the path first: what is refused says the same whether it
        // exists or not, and nothing outside is listed.
        let literal: PathBuf = named
            .components()
            .take_while(|c| !c.as_os_str().to_string_lossy().contains(GLOB_CHARS))
            .collect();
        confine(&base, &literal)?;
    }
    let max = here.count("max_files").max(1);
    let mut found = match pattern {
        None => vec![named],
        Some(pattern) => {
            let mut found: Vec<PathBuf> = glob::glob(&pattern)
                .map_err(|e| format!("{pattern}: {e}"))?
                .filter_map(Result::ok)
                .filter(|p| p.is_file())
                .take(max + 1)
                .collect();
            if found.is_empty() {
                return Err(format!("{pattern}: no file matches"));
            }
            found.sort();
            found
        }
    };
    if found.len() > max {
        return Err(format!("{path} names more than {max} files (`max_files`)"));
    }
    if !outside {
        found = found
            .iter()
            .map(|f| confine(&base, f))
            .collect::<Result<_, _>>()?;
    }
    Ok(found)
}

/// The pieces of a text: from the wired chunker, or the built-in one.
fn split(here: &Here, incoming: &Incoming) -> Result<Vec<chunker::Piece>, String> {
    if let Some(endpoint) = here.wired(CHUNKER) {
        let args = chunker::SplitArgs {
            text: incoming.text.clone(),
            metadata: json!({ "kind": incoming.kind, "location": incoming.location,
                              "title": incoming.title, "metadata": incoming.metadata }),
        };
        let answer = here
            .docs
            .ctx
            .call(&endpoint, chunker::SPLIT, json!(args))
            .map_err(|e| format!("its chunker: {e}"))?;
        let split: chunker::Split =
            serde_json::from_value(answer).map_err(|e| format!("its chunker: {e}"))?;
        return Ok(split.chunks);
    }
    let kind = chunk::Kind::parse(here.text(CHUNKER).trim())?;
    let size = chunk::Size {
        chunk_chars: here.count("chunk_chars").max(1),
        overlap_chars: here.count("overlap_chars"),
    };
    Ok(chunk::split(kind, &incoming.text, size))
}

/// A chunk's metadata: the source's object, with what the chunker found on top.
fn chunk_metadata(source: &Value, piece: Option<&Value>) -> Value {
    let mut merged = match source {
        Value::Object(o) => o.clone(),
        _ => serde_json::Map::new(),
    };
    if let Some(Value::Object(extra)) = piece {
        merged.extend(extra.clone());
    }
    Value::Object(merged)
}

/// Drops a document's chunks from the index and the store.
fn remove_doc(here: &Here, col: &Collection, doc_id: &str) -> Result<usize, String> {
    match here.wired(ENGINE) {
        Some(endpoint) => {
            let args = engine::RemoveArgs {
                collection: col.name.clone(),
                doc_id: doc_id.into(),
            };
            here.docs
                .ctx
                .call(&endpoint, engine::REMOVE, json!(args))
                .map_err(|e| format!("its engine: {e}"))?;
        }
        None => lexical::remove(col, doc_id).map_err(|e| e.to_string())?,
    }
    col.delete_chunks(doc_id).map_err(|e| e.to_string())
}

/// Keeps one source: nothing changes if its bytes did not; else its old document goes
/// and the new one is chunked and indexed.
fn add_source(here: &Here, incoming: Incoming) -> Result<Value, String> {
    let version = hex(incoming.text.as_bytes())[..VERSION_HEX].to_string();
    let identity = format!("{}\n{}", incoming.kind, incoming.location);
    let source_id = format!("s{}", &hex(identity.as_bytes())[..SOURCE_HEX]);
    let doc_id = format!("{source_id}#{}", &version[..DOC_VERSION_HEX]);
    let col = here.collection()?;
    let _writing = lock(&here.docs.writing);
    let old = col.source(&source_id).map_err(|e| e.to_string())?;
    let result = |chunks: usize, replaced: bool, unchanged: bool| {
        json!({ "source_id": source_id, "doc_id": doc_id, "chunks": chunks,
                "replaced": replaced, "unchanged": unchanged, "title": incoming.title,
                "location": incoming.location, "collection": col.name })
    };
    if let Some(old) = old.as_ref().filter(|o| o.version == version) {
        return Ok(result(old.chunks, false, true));
    }
    // Cut before anything is dropped: a chunker that fails leaves the old version whole.
    let pieces = split(here, &incoming)?;
    if let Some(old) = &old {
        remove_doc(here, &col, &old.doc_id)?;
    }
    let chunks: Vec<Chunk> = pieces
        .into_iter()
        .enumerate()
        .map(|(index, piece)| Chunk {
            chunk_id: format!("{doc_id}:{index}"),
            doc_id: doc_id.clone(),
            source_id: source_id.clone(),
            index,
            start: piece.start,
            end: piece.end,
            text: piece.text,
            metadata: chunk_metadata(&incoming.metadata, piece.metadata.as_ref()),
        })
        .collect();
    for chunk in &chunks {
        col.put_chunk(chunk).map_err(|e| e.to_string())?;
    }
    match here.wired(ENGINE) {
        Some(endpoint) => {
            let args = engine::IndexArgs {
                collection: col.name.clone(),
                chunks: chunks
                    .iter()
                    .map(|c| engine::Chunk {
                        chunk_id: c.chunk_id.clone(),
                        doc_id: c.doc_id.clone(),
                        source_id: c.source_id.clone(),
                        location: incoming.location.clone(),
                        index: c.index,
                        start: c.start,
                        end: c.end,
                        text: c.text.clone(),
                        metadata: c.metadata.clone(),
                    })
                    .collect(),
            };
            here.docs
                .ctx
                .call(&endpoint, engine::INDEX, json!(args))
                .map_err(|e| format!("its engine: {e}"))?;
        }
        None => {
            let texts: Vec<(String, String)> = chunks
                .iter()
                .map(|c| (c.chunk_id.clone(), c.text.clone()))
                .collect();
            lexical::add(&col, &here.tokenizer(), &doc_id, &texts).map_err(|e| e.to_string())?;
        }
    }
    let source = Source {
        source_id: source_id.clone(),
        kind: incoming.kind.into(),
        location: incoming.location.clone(),
        version,
        metadata: incoming.metadata.clone(),
        doc_id: doc_id.clone(),
        title: incoming.title.clone(),
        chars: incoming.text.chars().count(),
        chunks: chunks.len(),
        at: now_ms(),
    };
    col.put_source(&source).map_err(|e| e.to_string())?;
    Ok(result(chunks.len(), old.is_some(), false))
}

/// Deletes a source with its document, chunks and index entries.
fn delete_source(here: &Here, source_id: &str) -> Result<Value, String> {
    let col = here.collection()?;
    let _writing = lock(&here.docs.writing);
    let Some(source) = col.source(source_id).map_err(|e| e.to_string())? else {
        return Ok(json!({ "deleted": false, "chunks": 0 }));
    };
    let chunks = remove_doc(here, &col, &source.doc_id)?;
    col.delete_source(source_id).map_err(|e| e.to_string())?;
    Ok(json!({ "deleted": true, "chunks": chunks, "source_id": source_id }))
}

/// Whether `chunk` (of `source`) meets every condition of `filter`.
fn keeps(filter: &serde_json::Map<String, Value>, chunk: &Chunk) -> bool {
    filter.iter().all(|(key, wanted)| {
        let have = match key.as_str() {
            "source_id" => json!(chunk.source_id),
            "doc_id" => json!(chunk.doc_id),
            other => chunk.metadata.get(other).cloned().unwrap_or(Value::Null),
        };
        match wanted {
            Value::Array(any) => any.contains(&have),
            one => *one == have,
        }
    })
}

/// The best `k` chunks for `query`, as hits.
fn search(
    here: &Here,
    query: &str,
    k: usize,
    filter: Option<&Value>,
) -> Result<Vec<Value>, String> {
    let col = here.collection()?;
    let filter = filter.and_then(Value::as_object).filter(|f| !f.is_empty());
    let want = if filter.is_some() {
        k.max(here.count("candidates"))
    } else {
        k
    };
    let ranked: Vec<(String, f64)> = match here.wired(ENGINE) {
        Some(endpoint) => {
            let args = engine::QueryArgs {
                collection: col.name.clone(),
                query: query.into(),
                k: want,
            };
            let answer = here
                .docs
                .ctx
                .call(&endpoint, engine::QUERY, json!(args))
                .map_err(|e| format!("its engine: {e}"))?;
            let hits: engine::Hits =
                serde_json::from_value(answer).map_err(|e| format!("its engine: {e}"))?;
            hits.hits
                .into_iter()
                .map(|h| (h.chunk_id, h.score))
                .collect()
        }
        None => {
            let bm25 = lexical::Bm25 {
                k1: here.number("bm25_k1"),
                b: here.number("bm25_b"),
            };
            lexical::query(&col, &here.tokenizer(), bm25, query, want).map_err(|e| e.to_string())?
        }
    };
    let mut sources: HashMap<String, Option<Source>> = HashMap::new();
    let mut hits = Vec::new();
    for (chunk_id, score) in ranked {
        if hits.len() >= k {
            break;
        }
        // An engine may still know a chunk that is gone: it is skipped.
        let Some(chunk) = col.chunk(&chunk_id).map_err(|e| e.to_string())? else {
            continue;
        };
        if filter.is_some_and(|f| !keeps(f, &chunk)) {
            continue;
        }
        let source = match sources.get(&chunk.source_id) {
            Some(found) => found.clone(),
            None => {
                let found = col.source(&chunk.source_id).map_err(|e| e.to_string())?;
                sources.insert(chunk.source_id.clone(), found.clone());
                found
            }
        };
        let (location, title) = source.map(|s| (s.location, s.title)).unwrap_or_default();
        hits.push(json!({
            "chunk_id": chunk.chunk_id, "doc_id": chunk.doc_id, "source_id": chunk.source_id,
            "score": score, "text": chunk.text, "location": location, "start": chunk.start,
            "end": chunk.end, "title": title, "metadata": chunk.metadata,
        }));
    }
    Ok(hits)
}

/// What a value on `retrieve`'s `query` asks: text, or `{ query, k?, filter? }`.
fn retrieve_value(here: &Here, value: &Value) -> Result<Value, String> {
    let (query, k, filter, request) = match value {
        Value::Object(o) => (
            o.get("query").map(text_of).unwrap_or_default(),
            o.get("k").and_then(Value::as_u64).map(|k| k as usize),
            o.get("filter"),
            Some(value.clone()),
        ),
        other => (text_of(other), None, None, None),
    };
    if query.trim().is_empty() {
        return Err("an empty query finds nothing".into());
    }
    let k = k.unwrap_or_else(|| here.count("k")).max(1);
    let hits = search(here, &query, k, filter)?;
    let mut out = json!({ "query": query, "k": k, "hits": hits });
    if let Some(request) = request {
        out["request"] = request;
    }
    Ok(out)
}

fn string_arg(args: &Value, name: &str) -> Option<String> {
    args.get(name).and_then(Value::as_str).map(String::from)
}

fn run_tool(here: &Here, request: &tool::Call) -> Result<Value, String> {
    let kind = here.node.node_type.as_str();
    match (kind, request.name.as_str()) {
        (RETRIEVE, TOOL_SEARCH) => {
            let query = string_arg(&request.args, "query").ok_or("`query` is required")?;
            let k = request.args["k"]
                .as_u64()
                .map_or_else(|| here.count("k"), |k| k as usize)
                .max(1);
            let hits = search(here, &query, k, None)?;
            Ok(json!({ "query": query, "hits": hits }))
        }
        (RETRIEVE, TOOL_READ) => {
            let id = string_arg(&request.args, "chunk_id").ok_or("`chunk_id` is required")?;
            let col = here.collection()?;
            let chunk = col
                .chunk(&id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no chunk `{id}`"))?;
            let source = col.source(&chunk.source_id).map_err(|e| e.to_string())?;
            let mut found = json!(chunk);
            if let Some(source) = source {
                found["location"] = json!(source.location);
                found["title"] = json!(source.title);
            }
            Ok(found)
        }
        (INGEST, TOOL_INGEST) if here.text("agent_writes") == YES => {
            let title = string_arg(&request.args, "title").ok_or("`title` is required")?;
            let text = string_arg(&request.args, "text").ok_or("`text` is required")?;
            let mut metadata = match request.args.get("metadata") {
                Some(Value::Object(o)) => o.clone(),
                _ => serde_json::Map::new(),
            };
            metadata.insert("by".into(), json!(request.caller.name));
            let incoming = text_source(text, Some(title), Value::Object(metadata));
            let done = add_source(here, incoming)?;
            let _ = here.emit(INGESTED, done.clone());
            Ok(done)
        }
        (_, other) => Err(format!("no tool `{other}`")),
    }
}

fn tool_specs(here: &Here) -> Vec<tool::Spec> {
    let collection = here.text("collection");
    match here.node.node_type.as_str() {
        RETRIEVE => vec![
            tool::Spec {
                name: TOOL_SEARCH.into(),
                description: format!(
                    "Search the documents of the collection `{collection}` for passages \
                     about `query`, best first: each hit has its chunk_id, source, location \
                     and text. Cite what you use by its location."
                ),
                input: json!({ "type": "object", "properties": {
                    "query": { "type": "string", "description": "What to look for." },
                    "k": { "type": "integer", "description": "How many passages at most." } },
                    "required": ["query"] }),
            },
            tool::Spec {
                name: TOOL_READ.into(),
                description: format!(
                    "Read one passage of the collection `{collection}` by its chunk_id, as \
                     search_documents gave it."
                ),
                input: json!({ "type": "object", "properties": {
                    "chunk_id": { "type": "string" } }, "required": ["chunk_id"] }),
            },
        ],
        INGEST if here.text("agent_writes") == YES => vec![tool::Spec {
            name: TOOL_INGEST.into(),
            description: format!(
                "Add a text to the documents of the collection `{collection}`, so later \
                 searches find it. The same title again replaces it."
            ),
            input: json!({ "type": "object", "properties": {
                "title": { "type": "string" }, "text": { "type": "string" },
                "metadata": { "type": "object" } }, "required": ["title", "text"] }),
        }],
        _ => Vec::new(),
    }
}

impl Default for Documents {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Documents {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let failed = |e: String| ModuleError::Failed(e);
        match name {
            method::NODE_START => {
                let call: NodeCall = parse(params)?;
                let key = (call.execution_id.clone(), call.node.id.clone());
                lock(&self.live).insert(key, call.node);
                Ok(Value::Null)
            }
            method::NODE_STOP => {
                let call: NodeCall = parse(params)?;
                lock(&self.live).remove(&(call.execution_id, call.node.id));
                Ok(Value::Null)
            }
            method::NODE_EVENT => self.event(parse(params)?),
            tool::LIST => to_json(tool_specs(&self.here(parse(params)?))),
            tool::CALL => self.tool_call(parse(params)?),
            LIST => {
                let here = self.live(&params)?;
                let col = here.collection().map_err(failed)?;
                to_json(col.sources()?)
            }
            QUERY => {
                let here = self.live(&params)?;
                retrieve_value(&here, &params).map_err(failed)
            }
            DELETE => {
                let here = self.live(&params)?;
                let source_id = params["source_id"]
                    .as_str()
                    .ok_or_else(|| ModuleError::InvalidParams("`source_id` is required".into()))?;
                delete_source(&here, source_id).map_err(failed)
            }
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_declares_the_nodes_and_methods() {
        let docs = Documents::new();
        for kind in [INGEST, RETRIEVE, CONTEXT, CITE_CHECK, RECALL] {
            assert!(docs.manifest().node_type(kind).is_some(), "{kind}");
        }
        assert_eq!(docs.manifest().module.methods, [LIST, QUERY, DELETE]);
        let ingest = docs.manifest().node_type(INGEST).unwrap();
        let retrieve = docs.manifest().node_type(RETRIEVE).unwrap();
        // The tokenizer must be the same on both ends of the index.
        for field in ["stopwords", "min_token_chars", "collection"] {
            assert_eq!(
                ingest.field(field).unwrap().default,
                retrieve.field(field).unwrap().default,
                "{field}"
            );
        }
    }

    #[test]
    fn ids_are_stable_and_short() {
        let a = text_source("hello".into(), Some("Greeting".into()), Value::Null);
        assert_eq!(a.location, "text:Greeting");
        let b = text_source("\n  First line here\nmore".into(), None, Value::Null);
        assert_eq!(b.title, "First line here");
        assert!(b.location.starts_with("text:"));
        assert_eq!(hex(b"x"), hex(b"x"));
        assert_eq!(hex(b"x").len(), 128);
    }
}
