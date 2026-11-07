//! Webhook listeners: one HTTP server per `bind`, shared by every Webhook node that names
//! it, each node on its own path. The server runs on a thread of its own and closes when
//! its last node stops.

use std::collections::HashMap;
use std::io::Read;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use regulus_core::{ModuleCtx, NodeRef};
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server};

/// The output that carries the body as text.
pub const BODY_PORT: &str = "body";
/// The output that carries the body as JSON, when it is.
pub const JSON_PORT: &str = "json";

/// One node's place on a listener.
#[derive(Clone)]
pub struct Route {
    pub node: NodeRef,
    pub token: Option<String>,
    pub max_body: usize,
    pub body_port: bool,
    pub json_port: bool,
}

type Routes = Arc<Mutex<HashMap<String, Route>>>;

struct Listener {
    server: Arc<Server>,
    routes: Routes,
    stopping: Arc<std::sync::atomic::AtomicBool>,
    handle: Option<JoinHandle<()>>,
    addr: SocketAddr,
}

impl Listener {
    fn close(mut self) {
        self.stopping
            .store(true, std::sync::atomic::Ordering::SeqCst);
        self.server.unblock();
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
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

fn reply(request: Request, status: u16, body: Value) {
    let header = Header::from_bytes("content-type", "application/json").expect("valid header");
    let response = Response::from_string(body.to_string())
        .with_status_code(status)
        .with_header(header);
    let _ = request.respond(response);
}

/// `Authorization: Bearer <token>`, compared without stopping at the first difference.
fn authorized(request: &Request, token: &str) -> bool {
    let expected = format!("Bearer {token}");
    request
        .headers()
        .iter()
        .find(|h| h.field.equiv("authorization"))
        .is_some_and(|h| {
            let got = h.value.as_str().as_bytes();
            got.len() == expected.len()
                && got
                    .iter()
                    .zip(expected.as_bytes())
                    .fold(0, |diff, (a, b)| diff | (a ^ b))
                    == 0
        })
}

fn handle(ctx: &ModuleCtx, routes: &Routes, mut request: Request) {
    let path = request.url().split('?').next().unwrap_or("").to_string();
    let Some(route) = lock(routes).get(&path).cloned() else {
        return reply(
            request,
            404,
            json!({ "error": format!("no webhook at {path}") }),
        );
    };
    if !matches!(request.method(), Method::Post | Method::Put) {
        return reply(request, 405, json!({ "error": "POST or PUT" }));
    }
    if let Some(token) = &route.token {
        if !authorized(&request, token) {
            return reply(
                request,
                401,
                json!({ "error": "missing or wrong bearer token" }),
            );
        }
    }
    let mut body = Vec::new();
    let limit = route.max_body as u64 + 1;
    if let Err(error) = request.as_reader().take(limit).read_to_end(&mut body) {
        return reply(request, 400, json!({ "error": error.to_string() }));
    }
    if body.len() > route.max_body {
        let error = format!("the body is over {} bytes", route.max_body);
        return reply(request, 413, json!({ "error": error }));
    }
    let Ok(text) = String::from_utf8(body) else {
        return reply(request, 400, json!({ "error": "the body is not UTF-8" }));
    };
    let parsed = serde_json::from_str::<Value>(&text).ok();
    if !route.body_port && route.json_port && parsed.is_none() {
        return reply(request, 400, json!({ "error": "the body is not JSON" }));
    }
    let mut emitted = Vec::new();
    if route.body_port {
        emitted.push(ctx.emit(&route.node, BODY_PORT, Value::String(text)));
    }
    if let (true, Some(json)) = (route.json_port, parsed) {
        emitted.push(ctx.emit(&route.node, JSON_PORT, json));
    }
    match emitted.into_iter().find_map(Result::err) {
        None => reply(request, 202, json!({ "accepted": true })),
        Some(error) => reply(request, 503, json!({ "error": error.to_string() })),
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
            let server = Server::http(bind).map_err(|e| format!("cannot listen on {bind}: {e}"))?;
            let addr = server
                .server_addr()
                .to_ip()
                .ok_or_else(|| format!("{bind} is not an IP address"))?;
            let server = Arc::new(server);
            let routes: Routes = Arc::default();
            let stopping = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let (thread_server, thread_routes, thread_stopping) =
                (server.clone(), routes.clone(), stopping.clone());
            let ctx = ctx.clone();
            let handle = std::thread::Builder::new()
                .name("regulus-webhook".into())
                .spawn(move || loop {
                    match thread_server.recv() {
                        Ok(request) => handle(&ctx, &thread_routes, request),
                        Err(_) if thread_stopping.load(std::sync::atomic::Ordering::SeqCst) => {
                            return
                        }
                        Err(error) => eprintln!("regulus: triggers: webhook {addr}: {error}"),
                    }
                })
                .map_err(|e| format!("cannot start the webhook thread: {e}"))?;
            listeners.insert(
                bind.to_string(),
                Listener {
                    server,
                    routes,
                    stopping,
                    handle: Some(handle),
                    addr,
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

    /// Takes `node` off every listener, and closes those left with no node.
    pub fn remove(&self, node: &NodeRef) {
        let mut listeners = lock(&self.listeners);
        let empty: Vec<String> = listeners
            .iter()
            .filter_map(|(bind, listener)| {
                let mut routes = lock(&listener.routes);
                routes.retain(|_, route| route.node != *node);
                routes.is_empty().then(|| bind.clone())
            })
            .collect();
        for bind in empty {
            if let Some(listener) = listeners.remove(&bind) {
                listener.close();
            }
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
        let mut listeners = lock(&self.listeners);
        for (_, listener) in listeners.drain() {
            listener.close();
        }
    }
}
