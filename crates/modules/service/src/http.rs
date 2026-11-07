//! The HTTP engine: one listener per `http` node, serving `service.engine@1`. It turns each
//! request into a neutral one, queues it until an endpoint takes it (`service.engine.next`)
//! and holds the connection until that endpoint answers (`service.engine.reply`).
//!
//! The listener is the one of the `triggers` webhook, copied (modules do not share code):
//! `std::net`, a thread and a deadline per connection, one request per connection,
//! `content-length` bodies only (chunked gets 411), and closing never waits on a client.
//!
//! Routes: `POST /v1/workflows/<id>` (a JSON object, one key per input port; `?async=1` or
//! `Prefer: respond-async` answers at once with a job) and `GET /v1/jobs/<job>`.

use std::collections::{HashMap, VecDeque};
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{json, Value};

/// Largest request line plus headers: an HTTP limit, not a setting of the node.
const MAX_HEAD: usize = 16 * 1024;
const MAX_HEADERS: usize = 64;
const ACCEPT_BACKOFF: Duration = Duration::from_millis(10);
const LINGER: Duration = Duration::from_millis(100);
const LINGER_BYTES: u64 = 64 * 1024;
/// The most time writing one answer may take, however slowly the client reads.
const WRITE_TIME: Duration = Duration::from_secs(5);
/// The longest a `next` waits in one call: the module's lock is held meanwhile.
pub const MAX_PULL: Duration = Duration::from_secs(1);
const WORKFLOWS: &str = "/v1/workflows/";
const JOBS: &str = "/v1/jobs/";

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

/// What one `http` node is set to.
#[derive(Debug, Clone)]
pub struct Settings {
    pub bind: String,
    pub max_body: usize,
    /// The whole request must arrive within this.
    pub read_timeout: Duration,
    /// How long a request waits for an endpoint to take it; then 503.
    pub accept: Duration,
    /// How long a taken request waits for the endpoint's answer; then 504.
    pub max_wait: Duration,
    pub max_connections: usize,
}

/// An endpoint's answer to one request.
#[derive(Debug, Clone, Deserialize)]
pub struct Reply {
    pub id: String,
    pub status: u16,
    #[serde(default)]
    pub body: Value,
    #[serde(default)]
    pub retry_after_s: Option<u64>,
}

struct Queued {
    id: String,
    request: Value,
}

/// One live listener.
pub struct Engine {
    queue: Mutex<VecDeque<Queued>>,
    ready: Condvar,
    waiting: Mutex<HashMap<String, mpsc::Sender<Reply>>>,
    stopping: Arc<AtomicBool>,
    done: Mutex<Option<mpsc::Receiver<()>>>,
    pub addr: SocketAddr,
    settings: Settings,
    next_id: AtomicU64,
    open: AtomicUsize,
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Content Too Large",
        429 => "Too Many Requests",
        431 => "Request Header Fields Too Large",
        502 => "Bad Gateway",
        504 => "Gateway Timeout",
        _ => "Service Unavailable",
    }
}

fn write_reply(stream: &mut TcpStream, status: u16, body: &Value, retry_after: Option<u64>) {
    let body = body.to_string();
    let retry = retry_after
        .map(|s| format!("retry-after: {s}\r\n"))
        .unwrap_or_default();
    let head = format!(
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         {retry}connection: close\r\n\r\n",
        reason(status),
        body.len()
    );
    let deadline = Instant::now() + WRITE_TIME;
    if write_by(stream, head.as_bytes(), deadline) && write_by(stream, body.as_bytes(), deadline) {
        let _ = stream.flush();
    }
    let _ = stream.shutdown(std::net::Shutdown::Write);
    linger(stream);
}

/// Writes all of `bytes` unless `deadline` passes first: a client that reads slowly
/// cannot hold the connection past it.
fn write_by(stream: &mut TcpStream, mut bytes: &[u8], deadline: Instant) -> bool {
    while !bytes.is_empty() {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stream.set_write_timeout(Some(left)).is_err() {
            return false;
        }
        match stream.write(bytes) {
            Ok(0) => return false,
            Ok(n) => bytes = &bytes[n..],
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
    true
}

/// Reads and drops what the client still sends, for `LINGER` in all, so closing does not
/// reset the connection before it reads the answer. A client that trickles bytes gets no
/// more time than one that sends nothing.
fn linger(stream: &mut TcpStream) {
    let deadline = Instant::now() + LINGER;
    let mut chunk = [0u8; 4096];
    let mut dropped = 0u64;
    while dropped < LINGER_BYTES {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() || stream.set_read_timeout(Some(left)).is_err() {
            return;
        }
        match stream.read(&mut chunk) {
            Ok(0) => return,
            Ok(n) => dropped += n as u64,
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return,
        }
    }
}

/// Over the connection cap, on the accept thread: one non-blocking write of a 503 that
/// fits any socket buffer, then close. No waiting, no lingering.
fn refuse_fast(stream: TcpStream) {
    let body = json!({ "error": "too many connections" }).to_string();
    let reply = format!(
        "HTTP/1.1 503 {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        reason(503),
        body.len()
    );
    if stream.set_nonblocking(true).is_ok() {
        let _ = (&stream).write(reply.as_bytes());
    }
    let _ = stream.shutdown(std::net::Shutdown::Both);
}

fn error(stream: &mut TcpStream, status: u16, message: &str) {
    write_reply(stream, status, &json!({ "error": message }), None);
}

fn read_until(
    stream: &mut TcpStream,
    buf: &mut Vec<u8>,
    deadline: Instant,
    done: impl Fn(&[u8]) -> bool,
    cap: usize,
) -> Result<(), u16> {
    let mut chunk = [0u8; 4096];
    while !done(buf) {
        if buf.len() > cap {
            return Err(431);
        }
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(408);
        }
        stream.set_read_timeout(Some(left)).map_err(|_| 400u16)?;
        match stream.read(&mut chunk) {
            Ok(0) => return Err(400),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => {
                return Err(408)
            }
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(_) => return Err(400),
        }
    }
    Ok(())
}

fn head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|i| i + 4)
}

struct Head {
    method: String,
    path: String,
    query: String,
    headers: Vec<(String, String)>,
    rest: Vec<u8>,
}

impl Head {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

fn read_head(stream: &mut TcpStream, deadline: Instant) -> Result<Head, u16> {
    let mut buf = Vec::new();
    read_until(
        stream,
        &mut buf,
        deadline,
        |b| head_end(b).is_some(),
        MAX_HEAD,
    )?;
    let end = head_end(&buf).ok_or(400u16)?;
    let mut headers = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut request = httparse::Request::new(&mut headers);
    match request.parse(&buf[..end]) {
        Ok(httparse::Status::Complete(_)) => {}
        Err(httparse::Error::TooManyHeaders) => return Err(431),
        _ => return Err(400),
    }
    let target = request.path.unwrap_or_default();
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Ok(Head {
        method: request.method.unwrap_or_default().to_string(),
        path: path.to_string(),
        query: query.to_string(),
        headers: request
            .headers
            .iter()
            .map(|h| {
                let value = String::from_utf8_lossy(h.value).trim().to_string();
                (h.name.to_string(), value)
            })
            .collect(),
        rest: buf[end..].to_vec(),
    })
}

/// The bearer token, if the request carries one. Never logged.
fn bearer(head: &Head) -> Option<String> {
    let value = head.header("authorization")?;
    let (scheme, token) = value.split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then(|| token.trim().to_string())
}

fn wants_async(head: &Head) -> bool {
    head.query
        .split('&')
        .any(|pair| matches!(pair, "async=1" | "async=true" | "async"))
        || head
            .header("prefer")
            .is_some_and(|p| p.to_ascii_lowercase().contains("respond-async"))
}

impl Engine {
    /// Binds and starts accepting.
    pub fn open(settings: Settings) -> Result<Arc<Self>, String> {
        let socket = TcpListener::bind(&settings.bind)
            .map_err(|e| format!("cannot listen on {}: {e}", settings.bind))?;
        let addr = socket.local_addr().map_err(|e| e.to_string())?;
        let (done_tx, done) = mpsc::channel();
        let engine = Arc::new(Self {
            queue: Mutex::default(),
            ready: Condvar::new(),
            waiting: Mutex::default(),
            stopping: Arc::new(AtomicBool::new(false)),
            done: Mutex::new(Some(done)),
            addr,
            settings,
            next_id: AtomicU64::new(1),
            open: AtomicUsize::new(0),
        });
        let weak = Arc::downgrade(&engine);
        let stopping = engine.stopping.clone();
        std::thread::Builder::new()
            .name("regulus-service-http".into())
            .spawn(move || {
                for stream in socket.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    let stream = match stream {
                        Ok(stream) => stream,
                        Err(error) => {
                            eprintln!("regulus: service: http {addr}: {error}");
                            std::thread::sleep(ACCEPT_BACKOFF);
                            continue;
                        }
                    };
                    let Some(engine) = weak.upgrade() else {
                        break;
                    };
                    if engine.open.fetch_add(1, Ordering::SeqCst) >= engine.settings.max_connections
                    {
                        engine.open.fetch_sub(1, Ordering::SeqCst);
                        refuse_fast(stream);
                        continue;
                    }
                    let spawned = std::thread::Builder::new()
                        .name("regulus-service-request".into())
                        .spawn({
                            let engine = engine.clone();
                            move || {
                                engine.handle(stream);
                                engine.open.fetch_sub(1, Ordering::SeqCst);
                            }
                        });
                    if spawned.is_err() {
                        engine.open.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                drop(socket);
                let _ = done_tx.send(());
            })
            .map_err(|e| format!("cannot start the listener thread: {e}"))?;
        Ok(engine)
    }

    fn handle(&self, mut stream: TcpStream) {
        let started = Instant::now();
        let deadline = started + self.settings.read_timeout;
        let head = match read_head(&mut stream, deadline) {
            Ok(head) => head,
            Err(status) => return error(&mut stream, status, "unreadable request"),
        };
        let key = bearer(&head);
        let request = if let Some(job) = head.path.strip_prefix(JOBS) {
            if head.method != "GET" {
                return error(&mut stream, 405, "GET");
            }
            json!({ "op": "job", "key": key, "job": job })
        } else if let Some(workflow) = head.path.strip_prefix(WORKFLOWS) {
            if head.method != "POST" {
                return error(&mut stream, 405, "POST");
            }
            let input = match self.body(&mut stream, &head, deadline) {
                Ok(input) => input,
                Err((status, message)) => return error(&mut stream, status, &message),
            };
            json!({ "op": "call", "key": key, "workflow": workflow, "input": input,
                    "async": wants_async(&head) })
        } else {
            return error(
                &mut stream,
                404,
                "POST /v1/workflows/<id> or GET /v1/jobs/<job>",
            );
        };
        match self.ask(request) {
            Ok(reply) => write_reply(&mut stream, reply.status, &reply.body, reply.retry_after_s),
            Err((status, message)) => error(&mut stream, status, message),
        }
    }

    fn body(
        &self,
        stream: &mut TcpStream,
        head: &Head,
        deadline: Instant,
    ) -> Result<Value, (u16, String)> {
        if head
            .header("transfer-encoding")
            .is_some_and(|te| !te.eq_ignore_ascii_case("identity"))
        {
            return Err((411, "send a content-length".into()));
        }
        let length = match head.header("content-length").map(str::parse::<usize>) {
            None => 0,
            Some(Ok(length)) => length,
            Some(Err(_)) => return Err((400, "bad content-length".into())),
        };
        let max = self.settings.max_body;
        if length > max {
            return Err((413, format!("the body is over {max} bytes")));
        }
        let mut body = head.rest.clone();
        read_until(stream, &mut body, deadline, |b| b.len() >= length, max)
            .map_err(|status| (status, "the body did not arrive in time".to_string()))?;
        body.truncate(length);
        if body.iter().all(u8::is_ascii_whitespace) {
            return Ok(json!({}));
        }
        serde_json::from_slice(&body).map_err(|_| (400, "the body is not JSON".to_string()))
    }

    /// Queues `request` for an endpoint and waits for its answer.
    fn ask(&self, mut request: Value) -> Result<Reply, (u16, &'static str)> {
        let id = format!("r-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        request["id"] = json!(id);
        let (send, receive) = mpsc::channel();
        lock(&self.waiting).insert(id.clone(), send);
        lock(&self.queue).push_back(Queued {
            id: id.clone(),
            request,
        });
        self.ready.notify_one();
        let accept = self.settings.accept.min(self.settings.max_wait);
        let answer = match receive.recv_timeout(accept) {
            Ok(reply) => Ok(reply),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let mut queue = lock(&self.queue);
                if let Some(at) = queue.iter().position(|q| q.id == id) {
                    queue.remove(at);
                    drop(queue);
                    Err((503, "no endpoint took the request"))
                } else {
                    drop(queue);
                    receive
                        .recv_timeout(self.settings.max_wait.saturating_sub(accept))
                        .map_err(|_| (504, "the endpoint did not answer in time"))
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err((503, "the engine stopped")),
        };
        lock(&self.waiting).remove(&id);
        answer
    }

    /// The oldest queued request, waiting at most `wait` for one. `None` when stopping.
    pub fn next(&self, wait: Duration) -> Option<Value> {
        let mut queue = lock(&self.queue);
        let deadline = Instant::now() + wait.min(MAX_PULL);
        loop {
            if self.stopping.load(Ordering::SeqCst) {
                return None;
            }
            if let Some(queued) = queue.pop_front() {
                return Some(queued.request);
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            queue = self
                .ready
                .wait_timeout(queue, left)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }

    /// Hands an endpoint's answer to the connection waiting for it. False if none waits.
    pub fn reply(&self, reply: Reply) -> bool {
        match lock(&self.waiting).remove(&reply.id) {
            Some(send) => send.send(reply).is_ok(),
            None => false,
        }
    }

    /// Stops accepting and answers everything queued or waiting with 503. Waits at most
    /// the read timeout for the accept loop; never for a client.
    pub fn close(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        lock(&self.queue).clear();
        lock(&self.waiting).clear();
        self.ready.notify_all();
        let wake = match self.addr.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => {
                SocketAddr::new(Ipv4Addr::LOCALHOST.into(), self.addr.port())
            }
            IpAddr::V6(ip) if ip.is_unspecified() => {
                SocketAddr::new(Ipv6Addr::LOCALHOST.into(), self.addr.port())
            }
            _ => self.addr,
        };
        let timeout = self.settings.read_timeout;
        let _ = TcpStream::connect_timeout(&wake, timeout);
        if let Some(done) = lock(&self.done).take() {
            if done.recv_timeout(timeout).is_err() {
                eprintln!(
                    "regulus: service: http {} did not close in time; left to close itself",
                    self.addr
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn head(query: &str, headers: &[(&str, &str)]) -> Head {
        Head {
            method: "POST".into(),
            path: "/v1/workflows/w".into(),
            query: query.into(),
            headers: headers
                .iter()
                .map(|(n, v)| (n.to_string(), v.to_string()))
                .collect(),
            rest: Vec::new(),
        }
    }

    #[test]
    fn reads_the_bearer_and_async() {
        assert_eq!(
            bearer(&head("", &[("Authorization", "Bearer rgk_x")])).as_deref(),
            Some("rgk_x")
        );
        assert_eq!(bearer(&head("", &[("Authorization", "Basic x")])), None);
        assert!(wants_async(&head("async=1", &[])));
        assert!(wants_async(&head("", &[("Prefer", "respond-async")])));
        assert!(!wants_async(&head("asyncx=1", &[])));
    }

    fn engine(max_connections: usize, read_ms: u64) -> Arc<Engine> {
        Engine::open(Settings {
            bind: "127.0.0.1:0".into(),
            max_body: 100,
            read_timeout: Duration::from_millis(read_ms),
            accept: Duration::from_millis(100),
            max_wait: Duration::from_millis(100),
            max_connections,
        })
        .unwrap()
    }

    fn status_of(stream: &mut TcpStream) -> u16 {
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut text = String::new();
        let _ = stream.read_to_string(&mut text);
        text.get(9..12).and_then(|s| s.parse().ok()).unwrap_or(0)
    }

    fn wait_until(what: &str, done: impl Fn() -> bool) -> Duration {
        let started = Instant::now();
        while !done() {
            assert!(started.elapsed() < Duration::from_secs(5), "{what}");
            std::thread::sleep(Duration::from_millis(5));
        }
        started.elapsed()
    }

    #[test]
    fn a_large_body_gets_413_and_a_trickling_client_does_not_keep_the_slot() {
        let engine = engine(4, 5000);
        let mut stream = TcpStream::connect(engine.addr).unwrap();
        write!(
            stream,
            "POST /v1/workflows/w HTTP/1.1\r\ncontent-length: 1000000\r\n\r\n"
        )
        .unwrap();
        let mut writer = stream.try_clone().unwrap();
        let trickle = std::thread::spawn(move || {
            for _ in 0..60 {
                if writer.write_all(b"x").is_err() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
        });
        assert_eq!(status_of(&mut stream), 413);
        let freed = wait_until("the slot is freed", || {
            engine.open.load(Ordering::SeqCst) == 0
        });
        assert!(freed < Duration::from_millis(1000), "{freed:?}");
        drop(stream);
        let _ = trickle.join();
        engine.close();
    }

    #[test]
    fn a_slow_head_gets_408() {
        let engine = engine(4, 200);
        let mut stream = TcpStream::connect(engine.addr).unwrap();
        write!(stream, "POST /v1/workflows/w HTTP/1.1\r\n").unwrap();
        assert_eq!(status_of(&mut stream), 408);
        engine.close();
    }

    #[test]
    fn over_the_cap_gets_503_at_once_and_accept_goes_on() {
        let engine = engine(1, 3000);
        // Holds the only slot: a head that never ends.
        let mut stalled = TcpStream::connect(engine.addr).unwrap();
        write!(stalled, "POST /v1/").unwrap();
        wait_until("the first connection is open", || {
            engine.open.load(Ordering::SeqCst) == 1
        });
        // One that never reads must not freeze the accept loop for the next.
        let _silent = TcpStream::connect(engine.addr).unwrap();
        let started = Instant::now();
        let mut next = TcpStream::connect(engine.addr).unwrap();
        assert_eq!(status_of(&mut next), 503);
        assert!(started.elapsed() < Duration::from_millis(1000));
        drop(stalled);
        engine.close();
    }
}
