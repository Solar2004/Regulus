//! The external boundary: JSON-RPC 2.0 over WebSocket on loopback (docs/06-stack.md §4).
//!
//! Two gates, in this order:
//! 1. The token from `<home>/token`, on the upgrade request: `Authorization: Bearer <t>`
//!    for clients that can set headers, `?token=<t>` for the webview, which cannot.
//! 2. `hello` first: until a compatible `hello` on the same connection, every other
//!    method answers `HELLO_REQUIRED`.

use std::fs;
use std::future::Future;
use std::io::{self, Write};
use std::net::SocketAddr;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::task::{Context, Poll};

use jsonrpsee::core::BoxError;
use jsonrpsee::server::middleware::rpc::{Batch, Notification, RpcServiceBuilder, RpcServiceT};
use jsonrpsee::server::{Extensions, HttpRequest, HttpResponse, Server, ServerHandle};
use jsonrpsee::types::{ErrorObjectOwned, Params, Request};
use jsonrpsee::RpcModule;
use regulus_proto::{code, compatible, method, HelloParams, HelloResult, PROTOCOL};
use serde_json::Value;

use crate::config::ServerConfig;
use crate::service::{Core, RpcError};

pub const CORE_NAME: &str = concat!("regulus/", env!("CARGO_PKG_VERSION"));
const TOKEN_BYTES: usize = 32;

impl From<RpcError> for ErrorObjectOwned {
    fn from(error: RpcError) -> Self {
        ErrorObjectOwned::owned(error.code, error.message, None::<()>)
    }
}

// --- token ----------------------------------------------------------------------------

/// Reads the token, or creates it (mode 600) on the first start.
pub fn load_or_create_token(path: &Path) -> io::Result<String> {
    match fs::read_to_string(path) {
        Ok(token) if !token.trim().is_empty() => return Ok(token.trim().to_string()),
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let mut bytes = [0u8; TOKEN_BYTES];
    getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(token.as_bytes())?;
    Ok(token)
}

fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn presented_token<B>(request: &HttpRequest<B>) -> Option<&str> {
    let header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok());
    if let Some(token) = header.and_then(|v| v.strip_prefix("Bearer ")) {
        return Some(token.trim());
    }
    request
        .uri()
        .query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix("token="))
}

#[derive(Clone)]
struct TokenLayer(Arc<str>);

impl<S> tower::Layer<S> for TokenLayer {
    type Service = TokenGate<S>;
    fn layer(&self, inner: S) -> Self::Service {
        TokenGate {
            inner,
            token: self.0.clone(),
        }
    }
}

#[derive(Clone)]
struct TokenGate<S> {
    inner: S,
    token: Arc<str>,
}

impl<S, B> tower::Service<HttpRequest<B>> for TokenGate<S>
where
    S: tower::Service<HttpRequest<B>, Response = HttpResponse>,
    S::Error: Into<BoxError> + 'static,
    S::Future: Send + 'static,
{
    type Response = HttpResponse;
    type Error = BoxError;
    type Future = Pin<Box<dyn Future<Output = Result<HttpResponse, BoxError>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }

    fn call(&mut self, request: HttpRequest<B>) -> Self::Future {
        if presented_token(&request).is_some_and(|t| same(t.as_bytes(), self.token.as_bytes())) {
            let future = self.inner.call(request);
            Box::pin(async move { future.await.map_err(Into::into) })
        } else {
            Box::pin(async { Ok(jsonrpsee::server::http::response::denied()) })
        }
    }
}

// --- hello gate -----------------------------------------------------------------------

/// Per-connection negotiation flag, handed to every method through its extensions.
#[derive(Clone)]
struct Negotiation(Arc<AtomicBool>);

/// Built once per connection by the RPC middleware.
#[derive(Clone)]
struct Session<S> {
    inner: S,
    negotiation: Negotiation,
}

impl<S: RpcServiceT + Send + Sync> RpcServiceT for Session<S> {
    type MethodResponse = S::MethodResponse;
    type NotificationResponse = S::NotificationResponse;
    type BatchResponse = S::BatchResponse;

    fn call<'a>(
        &self,
        mut request: Request<'a>,
    ) -> impl Future<Output = Self::MethodResponse> + Send + 'a {
        request.extensions_mut().insert(self.negotiation.clone());
        self.inner.call(request)
    }

    fn batch<'a>(
        &self,
        mut batch: Batch<'a>,
    ) -> impl Future<Output = Self::BatchResponse> + Send + 'a {
        for entry in batch.iter_mut().flatten() {
            entry.extensions_mut().insert(self.negotiation.clone());
        }
        self.inner.batch(batch)
    }

    fn notification<'a>(
        &self,
        mut n: Notification<'a>,
    ) -> impl Future<Output = Self::NotificationResponse> + Send + 'a {
        n.extensions_mut().insert(self.negotiation.clone());
        self.inner.notification(n)
    }
}

fn negotiated(extensions: &Extensions) -> Result<(), RpcError> {
    match extensions.get::<Negotiation>() {
        Some(n) if n.0.load(Ordering::Acquire) => Ok(()),
        _ => Err(RpcError::new(
            code::HELLO_REQUIRED,
            "Compatible hello required",
        )),
    }
}

/// Named parameters only: absent means `{}`, anything but an object is invalid.
fn object(params: Params) -> Result<Value, RpcError> {
    let Some(raw) = params.as_str() else {
        return Ok(Value::Object(Default::default()));
    };
    match serde_json::from_str::<Value>(raw) {
        Ok(value @ Value::Object(_)) => Ok(value),
        _ => Err(RpcError::invalid_params("Invalid params")),
    }
}

fn hello(params: Params, extensions: &Extensions) -> Result<HelloResult, RpcError> {
    let params: HelloParams = serde_json::from_value(object(params)?).unwrap_or_default();
    let ok = params
        .proto
        .as_deref()
        .is_some_and(|proto| compatible(PROTOCOL, proto));
    if let Some(negotiation) = extensions.get::<Negotiation>() {
        negotiation.0.store(ok, Ordering::Release);
    }
    Ok(HelloResult {
        core: CORE_NAME.into(),
        proto: PROTOCOL.into(),
        compatible: ok,
    })
}

fn methods(core: Arc<Core>) -> Result<RpcModule<Core>, jsonrpsee::server::RegisterMethodError> {
    let module_methods: Vec<&'static str> = core
        .registry()
        .methods()
        .map(|name| &*Box::leak(name.to_string().into_boxed_str()))
        .collect();
    let mut rpc = RpcModule::from_arc(core);
    rpc.register_method(method::HELLO, |params, _, extensions| {
        hello(params, extensions)
    })?;
    // Module code may block; whatever reaches a module runs on the blocking pool, never on
    // the reactor. That is every module method plus starting and stopping executions.
    let reaches_modules = [method::EXECUTION_START, method::EXECUTION_STOP];
    for name in method::CORE
        .iter()
        .copied()
        .filter(|name| *name != method::HELLO && !reaches_modules.contains(name))
    {
        rpc.register_method(name, move |params, core, extensions| {
            negotiated(extensions)?;
            core.dispatch(name, object(params)?)
        })?;
    }
    for name in module_methods.into_iter().chain(reaches_modules) {
        rpc.register_blocking_method(name, move |params, core, extensions| {
            negotiated(&extensions)?;
            core.dispatch(name, object(params)?)
        })?;
    }
    Ok(rpc)
}

// --- server ---------------------------------------------------------------------------

pub struct Running {
    pub addr: SocketAddr,
    handle: ServerHandle,
}

impl Running {
    pub async fn stop(self) {
        if self.handle.stop().is_ok() {
            self.handle.stopped().await;
        }
    }

    pub async fn stopped(self) {
        self.handle.stopped().await;
    }
}

/// Binds the socket and starts answering. `config.host` must be loopback.
pub async fn start(core: Arc<Core>, config: &ServerConfig, token: &str) -> io::Result<Running> {
    if !config.host.is_loopback() {
        return Err(io::Error::other(format!(
            "refusing to bind non-loopback address {}",
            config.host
        )));
    }
    let settings = jsonrpsee::server::ServerConfig::builder()
        .ws_only()
        .max_request_body_size(config.max_request_bytes)
        .max_connections(config.max_connections)
        .build();
    let rpc = RpcServiceBuilder::new().layer_fn(|inner| Session {
        inner,
        negotiation: Negotiation(Arc::default()),
    });
    let server = Server::builder()
        .set_config(settings)
        .set_http_middleware(tower::ServiceBuilder::new().layer(TokenLayer(token.into())))
        .set_rpc_middleware(rpc)
        .build((config.host, config.port))
        .await?;
    let addr = server.local_addr()?;
    let handle = server.start(methods(core).map_err(io::Error::other)?);
    Ok(Running { addr, handle })
}
