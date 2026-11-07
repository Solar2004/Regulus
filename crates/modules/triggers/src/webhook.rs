//! Webhook listeners: one HTTP/1.1 listener per `bind`, shared by every Webhook node that
//! names it, each node on its own path. A small server of its own on `std::net`: each
//! connection gets a thread and a deadline for the whole request, so a client that stalls
//! holds up nobody else, and closing a listener never waits on a client.
//!
//! One request per connection (`connection: close`); bodies need `content-length`
//! (chunked bodies get 411).

use std::collections::HashMap;
use std::io::{ErrorKind, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_core::{ModuleCtx, NodeRef};
use serde_json::{json, Value};

/// The output that carries the body as text.
pub const BODY_PORT: &str = "body";
/// The output that carries the body as JSON, when it is.
pub const JSON_PORT: &str = "json";

/// Largest request line plus headers: an HTTP limit, not a setting of the node.
const MAX_HEAD: usize = 16 * 1024;
/// Most headers parsed per request.
const MAX_HEADERS: usize = 64;
/// How long the accept loop waits after an accept error before trying again.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(10);
/// After answering, how long unread bytes the client still sends are read and dropped,
/// so closing does not reset the connection before the client reads the answer.
const LINGER: Duration = Duration::from_millis(100);
/// The most unread bytes dropped that way.
const LINGER_BYTES: u64 = 64 * 1024;

/// One node's place on a listener.
#[derive(Clone)]
pub struct Route {
    pub node: NodeRef,
    pub token: Option<String>,
    pub max_body: usize,
    /// The whole request, head and body, must arrive within this.
    pub timeout: Duration,
    pub body_port: bool,
    pub json_port: bool,
}

type Routes = Arc<Mutex<HashMap<String, Route>>>;

struct Listener {
    routes: Routes,
    stopping: Arc<AtomicBool>,
    /// Says the accept loop is over, so its socket is closed.
    done: mpsc::Receiver<()>,
    addr: SocketAddr,
    /// How long the head of a request may take, before its route is known.
    timeout: Duration,
}

impl Listener {
    /// Stops accepting and waits at most `timeout` for the accept loop to close its
    /// socket. Connections already open finish by their own deadline.
    fn close(self) {
        self.stopping.store(true, Ordering::SeqCst);
        // Wake the blocked `accept` with a connection of our own.
        let wake = match self.addr.ip() {
            IpAddr::V4(ip) if ip.is_unspecified() => {
                SocketAddr::new(Ipv4Addr::LOCALHOST.into(), self.addr.port())
            }
            IpAddr::V6(ip) if ip.is_unspecified() => {
                SocketAddr::new(Ipv6Addr::LOCALHOST.into(), self.addr.port())
            }
            _ => self.addr,
        };
        let _ = TcpStream::connect_timeout(&wake, self.timeout);
        if self.done.recv_timeout(self.timeout).is_err() {
            eprintln!(
                "regulus: triggers: webhook {} did not close in time; left to close itself",
                self.addr
            );
        }
    }
}

/// Every listener, by the `bind` its nodes named.
#[derive(Default)]
pub struct Webhooks {
    listeners: Mutex<HashMap<String, Listener>>,
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn reason(status: u16) -> &'static str {
    match status {
        202 => "Accepted",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Service Unavailable",
    }
}

fn reply(stream: &mut TcpStream, status: u16, body: Value) {
    let body = body.to_string();
    let _ = write!(
        stream,
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
         connection: close\r\n\r\n{body}",
        reason(status),
        body.len()
    );
    let _ = stream.flush();
    let _ = stream.shutdown(std::net::Shutdown::Write);
    let _ = stream.set_read_timeout(Some(LINGER));
    let _ = std::io::copy(&mut (&*stream).take(LINGER_BYTES), &mut std::io::sink());
}

fn error(stream: &mut TcpStream, status: u16, message: impl Into<String>) {
    reply(stream, status, json!({ "error": message.into() }));
}

/// Reads into `buf` until `done` says enough or the deadline passes.
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

/// `Authorization: Bearer <token>`, compared without stopping at the first difference.
fn authorized(header: Option<&str>, token: &str) -> bool {
    let expected = format!("Bearer {token}");
    header.is_some_and(|got| {
        got.len() == expected.len()
            && got
                .bytes()
                .zip(expected.bytes())
                .fold(0, |diff, (a, b)| diff | (a ^ b))
                == 0
    })
}

/// The `Host` values a loopback listener on `addr` answers to.
fn own_host(host: &str, addr: SocketAddr) -> bool {
    let port = addr.port();
    host == addr.to_string()
        || host == format!("localhost:{port}")
        || host == format!("127.0.0.1:{port}")
        || host == format!("[::1]:{port}")
}

struct Head {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    /// Bytes after the head, the start of the body.
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
    Ok(Head {
        method: request.method.unwrap_or_default().to_string(),
        path: request
            .path
            .unwrap_or_default()
            .split('?')
            .next()
            .unwrap_or_default()
            .to_string(),
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

fn handle(
    ctx: &ModuleCtx,
    routes: &Routes,
    addr: SocketAddr,
    head_timeout: Duration,
    mut stream: TcpStream,
) {
    let started = Instant::now();
    let head = match read_head(&mut stream, started + head_timeout) {
        Ok(head) => head,
        Err(status) => return error(&mut stream, status, "unreadable request"),
    };
    let (route, guarded) = {
        let routes = lock(routes);
        let guarded = routes.values().any(|r| r.token.is_some());
        (routes.get(&head.path).cloned(), guarded)
    };
    // The token comes first: without it, nobody learns which paths or methods exist.
    let token = route.as_ref().and_then(|r| r.token.as_deref());
    match (&route, token) {
        (None, _) if guarded => return error(&mut stream, 401, "missing or wrong bearer token"),
        (None, _) => return error(&mut stream, 404, "no webhook here"),
        (Some(_), Some(token)) if !authorized(head.header("authorization"), token) => {
            return error(&mut stream, 401, "missing or wrong bearer token")
        }
        (Some(_), Some(_)) => {}
        // No token means loopback only, and only for callers that are not a web page: a
        // browser sends `Origin` on cross-site POSTs, and a rebound DNS name shows in `Host`.
        (Some(_), None) => {
            if head.header("origin").is_some() {
                return error(&mut stream, 403, "requests from web pages need a token");
            }
            if !head.header("host").is_some_and(|h| own_host(h, addr)) {
                return error(&mut stream, 403, "unexpected Host");
            }
        }
    }
    let route = route.expect("matched above");
    if !matches!(head.method.as_str(), "POST" | "PUT") {
        return error(&mut stream, 405, "POST or PUT");
    }
    if head
        .header("transfer-encoding")
        .is_some_and(|te| !te.eq_ignore_ascii_case("identity"))
    {
        return error(&mut stream, 411, "send a content-length");
    }
    let length = match head.header("content-length").map(str::parse::<usize>) {
        None => 0,
        Some(Ok(length)) => length,
        Some(Err(_)) => return error(&mut stream, 400, "bad content-length"),
    };
    if length > route.max_body {
        return error(
            &mut stream,
            413,
            format!("the body is over {} bytes", route.max_body),
        );
    }
    let mut body = head.rest;
    if let Err(status) = read_until(
        &mut stream,
        &mut body,
        started + route.timeout,
        |b| b.len() >= length,
        route.max_body,
    ) {
        return error(&mut stream, status, "the body did not arrive in time");
    }
    body.truncate(length);
    let Ok(text) = String::from_utf8(body) else {
        return error(&mut stream, 400, "the body is not UTF-8");
    };
    let parsed = serde_json::from_str::<Value>(&text).ok();
    if !route.body_port && route.json_port && parsed.is_none() {
        return error(&mut stream, 400, "the body is not JSON");
    }
    let mut emitted = Vec::new();
    if route.body_port {
        emitted.push(ctx.emit(&route.node, BODY_PORT, Value::String(text)));
    }
    if let (true, Some(json)) = (route.json_port, parsed) {
        emitted.push(ctx.emit(&route.node, JSON_PORT, json));
    }
    match emitted.into_iter().find_map(Result::err) {
        None => reply(&mut stream, 202, json!({ "accepted": true })),
        Some(e) => error(&mut stream, 503, e.to_string()),
    }
}

impl Webhooks {
    /// Puts `route` on `path` of the listener at `bind`, opening it if needed. Returns
    /// the address it listens on.
    pub fn add(
        &self,
        ctx: &ModuleCtx,
        bind: &str,
        path: &str,
        route: Route,
    ) -> Result<SocketAddr, String> {
        let mut listeners = lock(&self.listeners);
        if !listeners.contains_key(bind) {
            let socket =
                TcpListener::bind(bind).map_err(|e| format!("cannot listen on {bind}: {e}"))?;
            let addr = socket.local_addr().map_err(|e| e.to_string())?;
            let routes: Routes = Arc::default();
            let stopping = Arc::new(AtomicBool::new(false));
            let (done_tx, done) = mpsc::channel();
            let (ctx, thread_routes, thread_stopping) =
                (ctx.clone(), routes.clone(), stopping.clone());
            let timeout = route.timeout;
            std::thread::Builder::new()
                .name("regulus-webhook".into())
                .spawn(move || {
                    for stream in socket.incoming() {
                        if thread_stopping.load(Ordering::SeqCst) {
                            break;
                        }
                        let stream = match stream {
                            Ok(stream) => stream,
                            Err(error) => {
                                eprintln!("regulus: triggers: webhook {addr}: {error}");
                                std::thread::sleep(ACCEPT_BACKOFF);
                                continue;
                            }
                        };
                        let (ctx, routes) = (ctx.clone(), thread_routes.clone());
                        let spawned = std::thread::Builder::new()
                            .name("regulus-webhook-request".into())
                            .spawn(move || handle(&ctx, &routes, addr, timeout, stream));
                        if let Err(error) = spawned {
                            eprintln!("regulus: triggers: webhook {addr}: {error}");
                        }
                    }
                    drop(socket);
                    let _ = done_tx.send(());
                })
                .map_err(|e| format!("cannot start the webhook thread: {e}"))?;
            listeners.insert(
                bind.to_string(),
                Listener {
                    routes,
                    stopping,
                    done,
                    addr,
                    timeout,
                },
            );
        }
        let listener = listeners.get(bind).expect("inserted above");
        let mut routes = lock(&listener.routes);
        if let Some(taken) = routes.get(path) {
            return Err(format!(
                "{bind}{path} is taken by node `{}` of `{}`",
                taken.node.node_id, taken.node.execution_id
            ));
        }
        routes.insert(path.to_string(), route);
        Ok(listener.addr)
    }

    /// Takes `node` off every listener, and closes those left with no node. Closing
    /// happens outside the lock and waits for no client.
    pub fn remove(&self, node: &NodeRef) {
        let closing: Vec<Listener> = {
            let mut listeners = lock(&self.listeners);
            let empty: Vec<String> = listeners
                .iter()
                .filter_map(|(bind, listener)| {
                    let mut routes = lock(&listener.routes);
                    routes.retain(|_, route| route.node != *node);
                    routes.is_empty().then(|| bind.clone())
                })
                .collect();
            empty
                .iter()
                .filter_map(|bind| listeners.remove(bind))
                .collect()
        };
        for listener in closing {
            listener.close();
        }
    }

    /// Every node with its URL.
    pub fn list(&self) -> Vec<Value> {
        let listeners = lock(&self.listeners);
        let mut list: Vec<Value> = listeners
            .values()
            .flat_map(|listener| {
                lock(&listener.routes)
                    .iter()
                    .map(|(path, route)| {
                        json!({
                            "execution_id": route.node.execution_id,
                            "node_id": route.node.node_id,
                            "url": format!("http://{}{path}", listener.addr),
                            "token": route.token.is_some(),
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        list.sort_by_key(|v| v["url"].to_string());
        list
    }

    pub fn close_all(&self) {
        let closing: Vec<Listener> = lock(&self.listeners).drain().map(|(_, l)| l).collect();
        for listener in closing {
            listener.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hosts_and_tokens() {
        let addr: SocketAddr = "127.0.0.1:7790".parse().unwrap();
        assert!(own_host("127.0.0.1:7790", addr));
        assert!(own_host("localhost:7790", addr));
        assert!(!own_host("evil.example:7790", addr));
        assert!(!own_host("localhost:80", addr));
        assert!(authorized(Some("Bearer k"), "k"));
        assert!(!authorized(Some("Bearer x"), "k"));
        assert!(!authorized(None, "k"));
    }
}
