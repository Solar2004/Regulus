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
