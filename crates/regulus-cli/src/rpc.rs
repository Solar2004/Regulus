//! Talking to a core over its socket: the connection every command and the terminal UI
//! share. Nothing here reaches into the core; it is a client like the app.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::params::ObjectParams;
use jsonrpsee::ws_client::{HeaderMap, WsClient, WsClientBuilder};
use regulus_core::local::Endpoint;
use regulus_proto::{method, HelloParams, HelloResult, PROTOCOL};
use serde::de::DeserializeOwned;
use serde_json::Value;

/// Connects to `endpoint` and says `hello` as `client`. Fails unless the core speaks a
/// compatible protocol.
pub async fn connect(
    endpoint: &Endpoint,
    client: &str,
    timeout: Duration,
) -> Result<(WsClient, HelloResult)> {
    let mut headers = HeaderMap::new();
    headers.insert(
        "Authorization",
        format!("Bearer {}", endpoint.token).parse()?,
    );
    let ws = WsClientBuilder::default()
        .set_headers(headers)
        .request_timeout(timeout)
        .build(&endpoint.url)
        .await
        .with_context(|| format!("cannot connect to {}", endpoint.url))?;
    let hello = HelloParams {
        client: client.into(),
        proto: Some(PROTOCOL.into()),
    };
    let answer: HelloResult = ws
        .request(method::HELLO, object(serde_json::to_value(hello)?)?)
        .await?;
    if !answer.compatible {
        bail!(
            "core {} speaks protocol {}, this client speaks {PROTOCOL}",
            answer.core,
            answer.proto
        );
    }
    Ok((ws, answer))
}

/// Calls `name` with named parameters. A call error from the core is shown as its
/// message alone.
pub async fn call<T: DeserializeOwned>(ws: &WsClient, name: &str, params: Value) -> Result<T> {
    match ws.request(name, object(params)?).await {
        Ok(value) => Ok(value),
        Err(jsonrpsee::core::client::Error::Call(error)) => bail!("{}", error.message()),
        Err(error) => Err(error.into()),
    }
}

/// A failed call as a client decides on it: the core's error code, when it gave one, and
/// its message. Panels tell "not installed" (`-32601`) from "unavailable" (`-32002`) by the
/// code, never by the module's name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallError {
    pub code: Option<i32>,
    pub message: String,
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

/// Like [`call`], keeping the error code.
pub async fn call_coded<T: DeserializeOwned>(
    ws: &WsClient,
    name: &str,
    params: Value,
) -> std::result::Result<T, CallError> {
    let params = object(params).map_err(|e| CallError {
        code: None,
        message: format!("{e:#}"),
    })?;
    match ws.request(name, params).await {
        Ok(value) => Ok(value),
        Err(jsonrpsee::core::client::Error::Call(error)) => Err(CallError {
            code: Some(error.code()),
            message: error.message().to_string(),
        }),
        Err(error) => Err(CallError {
            code: None,
            message: error.to_string(),
        }),
    }
}

/// The protocol takes named parameters only; `null` means none.
pub fn object(params: Value) -> Result<ObjectParams> {
    let mut out = ObjectParams::new();
    match params {
        Value::Null => {}
        Value::Object(map) => {
            for (key, value) in map {
                out.insert(&key, value)?;
            }
        }
        other => bail!("params must be a JSON object, got {other}"),
    }
    Ok(out)
}
