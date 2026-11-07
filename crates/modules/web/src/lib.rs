//! Web (docs/13-blocks.md §3.3): `http` makes a request, `fetch` turns a page into
//! readable text or Markdown (in chunks if asked), `search` asks the engine wired into
//! it (`web.search@1`; `searxng` is the engine shipped), `pdf` extracts the text of a PDF
//! and `download` saves a URL to a file.
//!
//! Every value is handled on a thread of its own (`pool`), so the bus never waits for
//! the network; what a node emits from there starts a new chain (depth 0). Every request
//! goes through `net`: fixed User-Agent, no proxy, private destinations refused unless
//! `allow_private`, redirects followed by hand and checked again.

mod html;
pub mod net;
mod pdf;
mod place;
mod pool;

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use regulus_core::{parse_manifest, Module, ModuleCtx, ModuleError, NodeRef};
use regulus_proto::{method, Manifest, Node, NodeCall, NodeEvent};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use net::{Request, Rules};
use pool::{Pool, Taken};

const MANIFEST: &str = include_str!("../regulus-module.toml");

/// The capability of a search engine: a node serving [`SEARCH`].
pub const SEARCH_CAPABILITY: &str = "web.search@1";
/// `NodeCall` with [`SearchArgs`] → [`SearchAnswer`].
pub const SEARCH: &str = "web.search";

const OUT: &str = "out";
const TEXT: &str = "text";
const STATUS: &str = "status";
const ERROR: &str = "error";
const PATH: &str = "path";
const ENGINE: &str = "engine";
const YES: &str = "yes";

/// What a search engine is asked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchArgs {
    pub query: String,
    pub max_results: usize,
}

/// One result of a search.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Found {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub snippet: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchAnswer {
    pub results: Vec<Found>,
}

pub(crate) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn parse<T: serde::de::DeserializeOwned>(params: Value) -> Result<T, ModuleError> {
    serde_json::from_value(params).map_err(|e| ModuleError::InvalidParams(e.to_string()))
}

fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Percent-encodes all but the unreserved characters: a value put into a URL stays one
/// value.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// The input as JSON: itself, or what a text holds if it parses.
fn as_json(input: &Value) -> Value {
    match input {
        Value::String(text) => serde_json::from_str(text).unwrap_or_else(|_| input.clone()),
        other => other.clone(),
    }
}

/// A dotted path (`a.b.0`) into `value`.
fn lookup<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.')
        .filter(|s| !s.is_empty())
        .try_fold(value, |at, step| match at {
            Value::Array(items) => items.get(step.parse::<usize>().ok()?),
            other => other.get(step),
        })
}

/// `{{input}}`, `{{json}}` and `{{json.path}}` replaced by the input; `encode` each value
/// for a URL. A path that is not there is an error.
pub fn template(source: &str, input: &Value, url: bool) -> Result<String, String> {
    let mut out = String::new();
    let mut rest = source;
    while let Some(open) = rest.find("{{") {
        out.push_str(&rest[..open]);
        let after = &rest[open + 2..];
        let close = after.find("}}").ok_or("a `{{` without its `}}`")?;
        let name = after[..close].trim();
        let value = match name {
            "input" => as_text(input),
            "json" => as_json(input).to_string(),
            _ => {
                let path = name
                    .strip_prefix("json.")
                    .ok_or_else(|| format!("unknown placeholder `{{{{{name}}}}}`"))?;
                let json = as_json(input);
                as_text(lookup(&json, path).ok_or_else(|| format!("the input has no `{path}`"))?)
            }
        };
        out.push_str(&if url { encode(&value) } else { value });
        rest = &after[close + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Cuts `text` into chunks of at most `size` characters, at a line break past half of a
/// chunk when there is one.
pub fn chunks(text: &str, size: usize) -> Vec<String> {
    let size = size.max(1);
    let mut found = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let end = rest
            .char_indices()
            .nth(size)
            .map_or(rest.len(), |(at, _)| at);
        let mut cut = end;
        if end < rest.len() {
            let half = rest.char_indices().nth(size / 2).map_or(0, |(at, _)| at);
            if let Some(newline) = rest[..end].rfind('\n').filter(|n| *n >= half) {
                cut = newline + 1;
            }
        }
        found.push(rest[..cut].to_string());
        rest = &rest[cut..];
    }
    found
}

/// One node of one execution, with the settings the manifest defaults.
pub(crate) struct Job {
    manifest: Arc<Manifest>,
    ctx: ModuleCtx,
    node: Node,
    me: NodeRef,
}

/// Values to emit, in order.
type Emits = Vec<(String, Value)>;

impl Job {
    fn setting(&self, field: &str) -> Option<&Value> {
        self.manifest.setting(&self.node, field)
    }

    fn text(&self, field: &str) -> String {
        match self.setting(field) {
            Some(Value::String(s)) => s.trim().to_string(),
            Some(Value::Null) | None => String::new(),
            Some(other) => other.to_string(),
        }
    }

    fn number(&self, field: &str) -> u64 {
        match self.setting(field) {
            Some(Value::Number(n)) => n.as_f64().map_or(0, |n| n.max(0.0) as u64),
            Some(Value::String(s)) => s.trim().parse().unwrap_or(0),
            _ => 0,
        }
    }

    fn yes(&self, field: &str) -> bool {
        self.text(field) == YES
    }

    fn has(&self, port: &str) -> bool {
        self.node.port(port).is_some()
    }

    fn rules(&self) -> Rules {
        Rules {
            allow_private: self.yes("allow_private"),
            https_only: self.setting("allow_http").is_some() && !self.yes("allow_http"),
            max_redirects: self.number("max_redirects") as u32,
            timeout: Duration::from_millis(self.number("timeout_ms").max(1)),
            max_bytes: self.number("max_bytes").max(1),
        }
    }

    /// The URL to use: the `url` field with the input in its placeholders, or the input
    /// itself when the field is empty.
    fn url(&self, input: &Value) -> Result<String, String> {
        let field = self.text("url");
        if field.is_empty() {
            let url = as_text(input).trim().to_string();
            if url.is_empty() {
                return Err("no `url` set and the value is empty".into());
            }
            return Ok(url);
        }
        template(&field, input, true)
    }

    /// `dir` if absolute, else `dir` under the node's own directory.
    fn base(&self) -> Result<PathBuf, String> {
        let dir = self.text("dir");
        let dir = Path::new(&dir);
        if dir.is_absolute() {
            return Ok(dir.to_path_buf());
        }
        Ok(self
            .ctx
            .node_dir(&self.me)
            .map_err(|e| e.to_string())?
            .join(dir))
    }

    fn error(&self, message: &str) -> String {
        format!("{} `{}`: {message}", self.node.node_type, self.node.id)
    }

    fn emit(&self, port: &str, value: Value) {
        if !self.has(port) {
            return;
        }
        if let Err(error) = self.ctx.emit(&self.me, port, value) {
            eprintln!("regulus: web: {}", self.error(&error.to_string()));
        }
    }
}

/// `http`: one request per value.
fn http(job: &Job, input: &Value) -> Result<Emits, String> {
    let rules = job.rules();
    let method = match job.text("method") {
        m if m.is_empty() => "GET".to_string(),
        m => m.to_uppercase(),
    };
    let mut headers: Vec<(String, String)> = match job.setting("headers") {
        Some(Value::Object(map)) => map.iter().map(|(k, v)| (k.clone(), as_text(v))).collect(),
        _ => Vec::new(),
    };
    let has_header = |headers: &[(String, String)], name: &str| {
        headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name))
    };
    let body_field = job.setting("body").map(as_text).unwrap_or_default();
    let body = if !body_field.trim().is_empty() {
        Some(template(&body_field, input, false)?.into_bytes())
    } else if matches!(method.as_str(), "POST" | "PUT" | "PATCH") {
        let (bytes, kind) = match input {
            Value::String(text) => (text.clone().into_bytes(), "text/plain; charset=utf-8"),
            other => (other.to_string().into_bytes(), "application/json"),
        };
        if !has_header(&headers, "content-type") {
            headers.push(("content-type".into(), kind.into()));
        }
        Some(bytes)
    } else {
        None
    };
    let request = Request {
        method,
        url: job.url(input)?,
        headers,
        body,
    };
    let fetched = net::fetch(&net::agent(&rules), &rules, request)?;
    // The status goes out first, also when it is an error.
    job.emit(
        STATUS,
        json!({ "status": fetched.status, "url": net::shown(&fetched.url) }),
    );
    let mut emits = Vec::new();
    if fetched.status >= 400 && !job.text("fail_on_status").eq("no") {
        return Err(format!(
            "{} answered {}",
            net::shown(&fetched.url),
            fetched.status
        ));
    }
    let text = String::from_utf8_lossy(&fetched.body).into_owned();
    let json_wanted = match job.text("parse").as_str() {
        "json" => true,
        "text" => false,
        _ => fetched.content_type.contains("json"),
    };
    if json_wanted {
        let parsed: Value = serde_json::from_str(&text)
            .map_err(|e| format!("{} sent no valid JSON: {e}", net::shown(&fetched.url)))?;
        emits.push((OUT.into(), parsed));
    } else {
        emits.push((TEXT.into(), json!(text)));
    }
    Ok(emits)
}

/// `fetch`: the page as readable text, whole or in chunks.
fn fetch(job: &Job, input: &Value) -> Result<Emits, String> {
    let rules = job.rules();
    let request = Request {
        method: "GET".into(),
        url: job.url(input)?,
        ..Request::default()
    };
    let fetched = net::fetch(&net::agent(&rules), &rules, request)?;
    if fetched.status >= 400 {
        return Err(format!(
            "{} answered {}",
            net::shown(&fetched.url),
            fetched.status
        ));
    }
    if fetched.content_type.contains("pdf") || fetched.body.starts_with(b"%PDF-") {
        return Err(format!(
            "{} is a PDF: use a `pdf` node",
            net::shown(&fetched.url)
        ));
    }
    let body = String::from_utf8_lossy(&fetched.body);
    let text = match job.text("format").as_str() {
        "raw" => body.into_owned(),
        format if html::is_html(&fetched.content_type, &body) => {
            html::convert(&body, format != "text")
        }
        _ => body.into_owned(),
    };
    if !job.yes("paginate") {
        return Ok(vec![(OUT.into(), json!(text))]);
    }
    let size = job.number("chunk_chars").max(1) as usize;
    Ok(chunks(&text, size)
        .into_iter()
        .map(|chunk| (OUT.to_string(), json!(chunk)))
        .collect())
}

/// `search`: the query to the engine wired into `engine`.
fn search(job: &Job, input: &Value) -> Result<Emits, String> {
    let query = match job.text("query") {
        q if q.is_empty() => as_text(input),
        q => template(&q, input, false)?,
    };
    if query.trim().is_empty() {
        return Err("an empty query".into());
    }
    let engine = job
        .ctx
        .resolve(&job.me, ENGINE)
        .into_iter()
        .next()
        .ok_or("no engine: wire a `web.search@1` node (e.g. `searxng`) into `engine`")?;
    let args = SearchArgs {
        query,
        max_results: job.number("max_results").max(1) as usize,
    };
    let args = serde_json::to_value(args).map_err(|e| e.to_string())?;
    let answer = job
        .ctx
        .call(&engine, SEARCH, args)
        .map_err(|e| format!("the engine: {e}"))?;
    let mut answer: SearchAnswer =
        serde_json::from_value(answer).map_err(|e| format!("the engine answered: {e}"))?;
    answer
        .results
        .truncate(job.number("max_results").max(1) as usize);
    let listed: Vec<String> = answer
        .results
        .iter()
        .enumerate()
        .map(|(n, r)| format!("{}. {}\n   {}\n   {}", n + 1, r.title, r.url, r.snippet))
        .collect();
    Ok(vec![
        (OUT.into(), json!(answer.results)),
        (TEXT.into(), json!(listed.join("\n"))),
    ])
}

/// `searxng`: the engine. A SearXNG-style JSON endpoint (`format=json`).
fn searxng(job: &Job, args: SearchArgs) -> Result<SearchAnswer, String> {
    let rules = job.rules();
    let mut url = url::Url::parse(&job.text("url")).map_err(|e| format!("`url`: {e}"))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("q", &args.query)
            .append_pair("format", "json");
        for field in ["categories", "language"] {
            let value = job.text(field);
            if !value.is_empty() {
                query.append_pair(field, &value);
            }
        }
    }
    let request = Request {
        method: "GET".into(),
        url: url.to_string(),
        headers: vec![("accept".into(), "application/json".into())],
        body: None,
    };
    let fetched = net::fetch(&net::agent(&rules), &rules, request)?;
    if fetched.status >= 400 {
        return Err(format!(
            "{} answered {}",
            net::shown(&fetched.url),
            fetched.status
        ));
    }
    let body: Value = serde_json::from_slice(&fetched.body)
        .map_err(|e| format!("{} sent no valid JSON: {e}", net::shown(&fetched.url)))?;
    let results = body["results"]
        .as_array()
        .ok_or("the answer has no `results` list")?
        .iter()
        .filter_map(|r| {
            Some(Found {
                title: r["title"].as_str().unwrap_or_default().to_string(),
                url: r["url"].as_str()?.to_string(),
                snippet: r["content"].as_str().unwrap_or_default().to_string(),
            })
        })
        .take(args.max_results)
        .collect();
    Ok(SearchAnswer { results })
}

/// `pdf`: the text of a PDF at a URL or a path.
fn pdf_text(job: &Job, input: &Value) -> Result<Emits, String> {
    let field = job.text("path");
    // A path the value shaped, whole or through a placeholder, may come from anything
    // upstream: it stays under the base folder.
    let shaped = field.is_empty() || field.contains("{{");
    let source = if field.is_empty() {
        as_text(input).trim().to_string()
    } else {
        template(&field, input, false)?
    };
    if source.is_empty() {
        return Err("no `path` set and the value is empty".into());
    }
    let max = job.number("max_bytes").max(1);
    let bytes = if source.starts_with("http://") || source.starts_with("https://") {
        let rules = job.rules();
        let request = Request {
            method: "GET".into(),
            url: source,
            ..Request::default()
        };
        let fetched = net::fetch(&net::agent(&rules), &rules, request)?;
        if fetched.status >= 400 {
            return Err(format!(
                "{} answered {}",
                net::shown(&fetched.url),
                fetched.status
            ));
        }
        fetched.body
    } else {
        let base = job.base()?;
        let mut path = Path::new(&source).to_path_buf();
        if !path.is_absolute() {
            path = base.join(path);
        }
        if shaped && !job.yes("allow_outside") {
            fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
            path = confined(&base, &path)?;
        }
        read_bounded(&path, max)?
    };
    let text = pdf::extract(
        &bytes,
        job.number("max_pages").max(1) as usize,
        job.number("max_chars").max(1) as usize,
    )?;
    Ok(vec![(OUT.into(), json!(text))])
}

/// `path` resolved under `base`, or one refusal that says the same whether or not
/// something exists outside, so a value cannot probe the disk.
fn confined(base: &Path, path: &Path) -> Result<PathBuf, String> {
    place::confine(base, path).map_err(|_| {
        format!(
            "{} is not a file under the node's folder (`allow_outside`)",
            path.display()
        )
    })
}

fn read_bounded(path: &Path, max: u64) -> Result<Vec<u8>, String> {
    let failed = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    // Checked before opening: opening a FIFO with no writer would hold the thread.
    if !fs::metadata(path).map_err(|e| failed(&e))?.is_file() {
        return Err(failed(&"not a regular file"));
    }
    let file = fs::File::open(path).map_err(|e| failed(&e))?;
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

/// A file name made of the last segment of `url`: letters, digits, `.`, `-` and `_`.
fn file_name(url: &str) -> String {
    let last = url::Url::parse(url)
        .ok()
        .and_then(|u| u.path_segments()?.next_back().map(String::from))
        .unwrap_or_default();
    let clean: String = last
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let clean = clean.trim_start_matches('.').to_string();
    if clean.is_empty() {
        "download".into()
    } else {
        clean
    }
}

/// `download`: the URL into a file under the base folder.
fn download(job: &Job, input: &Value, stop: &AtomicBool) -> Result<Emits, String> {
    let rules = job.rules();
    let url = job.url(input)?;
    let base = job.base()?;
    fs::create_dir_all(&base).map_err(|e| format!("{}: {e}", base.display()))?;
    let max = rules.max_bytes;
    let request = Request {
        method: "GET".into(),
        url,
        ..Request::default()
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = net::send(
        &net::agent(&rules),
        &rules,
        request,
        |status, url, _, reader| {
            if status >= 400 {
                return Err(format!("{} answered {status}", net::shown(url)));
            }
            let name = match job.text("file") {
                f if f.is_empty() => file_name(url),
                f => template(&f, input, false)?,
            };
            let target = base.join(name);
            let target = if job.yes("allow_outside") {
                target
            } else {
                confined(&base, &target)?
            };
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
            }
            let temp = target.with_file_name(format!(
                ".{}.{}-{}.part",
                target.file_name().unwrap_or_default().to_string_lossy(),
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            let copied = copy_bounded(reader, &temp, max, stop)
                .and_then(|()| fs::rename(&temp, &target).map_err(|e| e.to_string()));
            if let Err(error) = copied {
                let _ = fs::remove_file(&temp);
                return Err(format!("{}: {error}", target.display()));
            }
            Ok(target)
        },
    )?;
    Ok(vec![(PATH.into(), json!(path.display().to_string()))])
}

fn copy_bounded(
    reader: &mut dyn Read,
    temp: &Path,
    max: u64,
    stop: &AtomicBool,
) -> Result<(), String> {
    let mut file = fs::File::create(temp).map_err(|e| e.to_string())?;
    let mut buffer = vec![0u8; 64 * 1024];
    let mut total = 0u64;
    loop {
        if stop.load(Ordering::SeqCst) {
            return Err("the node stopped".into());
        }
        let n = reader.read(&mut buffer).map_err(|e| e.to_string())?;
        if n == 0 {
            return file.flush().map_err(|e| e.to_string());
        }
        total += n as u64;
        if total > max {
            return Err(format!("more than {max} bytes (`max_bytes`)"));
        }
        file.write_all(&buffer[..n]).map_err(|e| e.to_string())?;
    }
}

pub struct Web {
    manifest: Arc<Manifest>,
    ctx: ModuleCtx,
    pool: Arc<Pool>,
}

impl Web {
    pub fn new() -> Self {
        Self {
            manifest: Arc::new(parse_manifest(MANIFEST).expect("web manifest is valid")),
            ctx: ModuleCtx::default(),
            pool: Arc::default(),
        }
    }

    fn job(&self, call: NodeCall) -> Job {
        Job {
            manifest: self.manifest.clone(),
            ctx: self.ctx.clone(),
            me: NodeRef {
                execution_id: call.execution_id,
                node_id: call.node.id.clone(),
            },
            node: call.node,
        }
    }

    fn event(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let event: NodeEvent = parse(std::mem::take(&mut call.args))?;
        let run: fn(&Job, &Value, &AtomicBool) -> Result<Emits, String> =
            match call.node.node_type.as_str() {
                "http" => |job, input, _| http(job, input),
                "fetch" => |job, input, _| fetch(job, input),
                "search" => |job, input, _| search(job, input),
                "pdf" => |job, input, _| pdf_text(job, input),
                "download" => download,
                other => {
                    return Err(ModuleError::InvalidParams(format!(
                        "`{other}` takes no values"
                    )))
                }
            };
        let job = Arc::new(self.job(call));
        let key = (job.me.execution_id.clone(), job.me.node_id.clone());
        let max_in_flight = job.number("max_in_flight") as usize;
        let max_queue = job.number("max_queue") as usize;
        let worker = job.clone();
        let taken = self.pool.take(
            key,
            event.value,
            max_in_flight,
            max_queue,
            move |value, stop| {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run(&worker, &value, stop)
                }))
                .unwrap_or_else(|_| Err("failed unexpectedly on this value (a panic)".into()));
                if stop.load(Ordering::SeqCst) {
                    return;
                }
                match outcome {
                    Ok(emits) => {
                        for (port, value) in emits {
                            if stop.load(Ordering::SeqCst) {
                                return;
                            }
                            worker.emit(&port, value);
                        }
                    }
                    Err(error) => {
                        let error = worker.error(&error);
                        eprintln!("regulus: web: {error}");
                        worker.emit(ERROR, json!(error));
                    }
                }
            },
        );
        match taken {
            Taken::Running => Ok(json!({ "running": true })),
            Taken::Queued(n) => Ok(json!({ "queued": n })),
            Taken::Refused(reason) => {
                let error = job.error(&reason);
                job.emit(ERROR, json!(error));
                Err(ModuleError::Failed(error))
            }
        }
    }

    /// The `searxng` engine answering a search box, on the caller's thread.
    fn engine(&self, mut call: NodeCall) -> Result<Value, ModuleError> {
        let args: SearchArgs = parse(std::mem::take(&mut call.args))?;
        let job = self.job(call);
        if job.node.node_type != "searxng" {
            return Err(ModuleError::InvalidParams(format!(
                "`{}` is not a search engine",
                job.node.node_type
            )));
        }
        let answer = searxng(&job, args).map_err(|e| ModuleError::Failed(job.error(&e)))?;
        serde_json::to_value(answer).map_err(|e| ModuleError::Failed(e.to_string()))
    }
}

impl Default for Web {
    fn default() -> Self {
        Self::new()
    }
}

impl Module for Web {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    fn init(&mut self, ctx: &ModuleCtx) -> Result<(), ModuleError> {
        self.ctx = ctx.clone();
        Ok(())
    }

    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        match name {
            method::NODE_EVENT => self.event(parse(params)?),
            method::NODE_STOP => {
                let call: NodeCall = parse(params)?;
                self.pool.stop(&(call.execution_id, call.node.id));
                Ok(Value::Null)
            }
            SEARCH => self.engine(parse(params)?),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }

    fn shutdown(&mut self) -> Result<(), ModuleError> {
        self.pool.stop_all();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_fill_input_and_json_paths() {
        let input = json!({ "q": "a b&c", "items": [{ "id": 7 }] });
        assert_eq!(
            template(
                "https://x/s?q={{json.q}}&id={{ json.items.0.id }}",
                &input,
                true
            )
            .unwrap(),
            "https://x/s?q=a%20b%26c&id=7"
        );
        assert_eq!(template("{{input}}!", &json!("hi"), false).unwrap(), "hi!");
        assert_eq!(
            template("{{json.q}}", &json!("{\"q\":1}"), false).unwrap(),
            "1"
        );
        assert!(template("{{json.nope}}", &input, false).is_err());
        assert!(template("{{other}}", &input, false).is_err());
    }

    #[test]
    fn chunks_cut_at_line_breaks_when_they_can() {
        assert_eq!(chunks("aaaa\nbbbb\ncc", 6), ["aaaa\n", "bbbb\n", "cc"]);
        assert_eq!(chunks("abcdefgh", 3), ["abc", "def", "gh"]);
        assert!(chunks("", 3).is_empty());
    }

    #[test]
    fn only_regular_files_are_read() {
        let dir = std::env::temp_dir().join(format!("regulus-web-read-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        assert!(read_bounded(&dir, 10).is_err(), "a folder");
        #[cfg(unix)]
        {
            let fifo = dir.join("fifo");
            let made = std::process::Command::new("mkfifo").arg(&fifo).status();
            if made.is_ok_and(|s| s.success()) {
                // Opening it with no writer would block for ever.
                assert!(read_bounded(&fifo, 10).is_err());
            }
        }
        fs::write(dir.join("f"), "12345").unwrap();
        assert!(read_bounded(&dir.join("f"), 4).is_err());
        assert_eq!(read_bounded(&dir.join("f"), 5).unwrap(), b"12345");
    }

    #[test]
    fn download_names_stay_plain() {
        assert_eq!(file_name("https://h/data/set.csv?x=1"), "set.csv");
        assert_eq!(file_name("https://h/"), "download");
        assert_eq!(file_name("https://h/..%2F..%2Fetc"), "_2F.._2Fetc");
    }

    #[test]
    fn manifest_declares_the_nodes() {
        let web = Web::new();
        for kind in ["http", "fetch", "search", "searxng", "pdf", "download"] {
            assert!(web.manifest().node_type(kind).is_some(), "{kind}");
        }
        let engine = web.manifest().node_type("search").unwrap();
        let port = engine.ports.iter().find(|p| p.id == ENGINE).unwrap();
        assert_eq!(port.capability, SEARCH_CAPABILITY);
    }
}
