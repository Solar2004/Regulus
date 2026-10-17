//! The socket as a client sees it: the token gate, the `hello` gate, the mock methods and
//! routing to modules (docs/06-stack.md §4, docs/composition-preview.md).

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use jsonrpsee::core::client::{ClientT, Error};
use jsonrpsee::core::params::ObjectParams;
use jsonrpsee::rpc_params;
use jsonrpsee::ws_client::{HeaderMap, WsClient, WsClientBuilder};
use regulus_core::server::Running;
use regulus_core::{assemble, serve, Config, Module, ModuleCtx, ModuleError};
use regulus_proto::{code, Manifest, ModuleInfo, ModuleState, ModuleSummary, Profile, PROTOCOL};
use serde_json::{json, Value};

const FIXTURE: &str = include_str!("../../regulus-tauri/frontend/mock/profile.json");
const METHOD_NOT_FOUND: i32 = -32601;

struct Echo {
    manifest: Manifest,
    panics: bool,
}

fn echo(id: &str, method: &str, panics: bool) -> Box<dyn Module> {
    let module = ModuleInfo {
        id: id.into(),
        name: id.into(),
        requires_proto: None,
        methods: vec![method.into()],
        workflow_id: None,
    };
    Box::new(Echo {
        manifest: Manifest {
            module,
            nodes: vec![],
        },
        panics,
    })
}

impl Module for Echo {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, method: &str, params: Value) -> Result<Value, ModuleError> {
        if self.panics {
            panic!("echo exploded");
        }
        Ok(json!({ "method": method, "params": params }))
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

struct Harness {
    home: PathBuf,
    running: Option<Running>,
    url: String,
    token: String,
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn temp_home() -> PathBuf {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "regulus-server-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("profiles")).unwrap();
    dir
}

async fn start() -> Harness {
    let home = temp_home();
    std::fs::write(home.join("profiles/mock.json"), FIXTURE).unwrap();
    let mut config = Config::load(&home).unwrap();
    config.server.port = 0;
    let modules = vec![
        echo("t/echo@1.0.0", "echo.ping", false),
        echo("t/bomb@1.0.0", "bomb.ping", true),
    ];
    let (core, warnings) = assemble(&config, modules);
    assert!(warnings.is_empty(), "{warnings:?}");
    let running = serve(&config, Arc::new(core)).await.unwrap();
    let port = std::fs::read_to_string(config.port_path()).unwrap();
    assert_eq!(port, running.addr.port().to_string());
    let token = std::fs::read_to_string(config.token_path()).unwrap();
    Harness {
        home,
        url: format!("ws://{}/", running.addr),
        running: Some(running),
        token,
    }
}

impl Harness {
    async fn connect(&self) -> WsClient {
        WsClientBuilder::default()
            .build(format!("{}?token={}", self.url, self.token))
            .await
            .unwrap()
    }

    async fn connect_with_bearer(&self) -> WsClient {
        let mut headers = HeaderMap::new();
        headers.insert(
            "Authorization",
            format!("Bearer {}", self.token).parse().unwrap(),
        );
        WsClientBuilder::default()
            .set_headers(headers)
            .build(&self.url)
            .await
            .unwrap()
    }

    /// A connection that already said a compatible `hello`.
    async fn ready(&self) -> WsClient {
        let client = self.connect().await;
        let hello: Value = client
            .request(
                "hello",
                params(json!({ "client": "test", "proto": PROTOCOL })),
            )
            .await
            .unwrap();
        assert_eq!(hello["compatible"], true);
        client
    }

    async fn stop(mut self) {
        if let Some(running) = self.running.take() {
            running.stop().await;
        }
    }
}

fn params(value: Value) -> ObjectParams {
    let mut params = ObjectParams::new();
    for (key, value) in value.as_object().expect("named params") {
        params.insert(key, value).unwrap();
    }
    params
}

fn error_code(result: Result<Value, Error>) -> i32 {
    match result {
        Err(Error::Call(error)) => error.code(),
        other => panic!("expected a call error, got {other:?}"),
    }
}

#[tokio::test]
async fn without_the_token_the_upgrade_is_refused() {
    let harness = start().await;
    assert!(WsClientBuilder::default()
        .build(&harness.url)
        .await
        .is_err());
    assert!(WsClientBuilder::default()
        .build(format!("{}?token=wrong", harness.url))
        .await
        .is_err());
    harness.connect_with_bearer().await;
    harness.stop().await;
}

#[tokio::test]
async fn every_method_needs_a_compatible_hello_first() {
    let harness = start().await;
    let client = harness.connect().await;
    assert_eq!(
        error_code(client.request("module.list", rpc_params![]).await),
        code::HELLO_REQUIRED
    );

    let hello: Value = client
        .request("hello", params(json!({ "client": "test", "proto": "2.0" })))
        .await
        .unwrap();
    assert_eq!(hello["compatible"], false);
    assert_eq!(hello["proto"], PROTOCOL);
    assert_eq!(
        error_code(client.request("module.list", rpc_params![]).await),
        code::HELLO_REQUIRED
    );

    let hello: Value = client
        .request("hello", params(json!({ "client": "test", "proto": "1.0" })))
        .await
        .unwrap();
    assert_eq!(hello["compatible"], true);
    let _: Value = client.request("module.list", rpc_params![]).await.unwrap();

    // The negotiation belongs to the connection, not to the server.
    let other = harness.connect().await;
    assert_eq!(
        error_code(other.request("module.list", rpc_params![]).await),
        code::HELLO_REQUIRED
    );
    harness.stop().await;
}

#[tokio::test]
async fn profile_get_returns_the_profile_as_loaded() {
    let harness = start().await;
    let client = harness.ready().await;
    let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
    let expected: Profile = serde_json::from_value(fixture["profiles"][0].clone()).unwrap();

    let listed: Vec<Value> = client.request("profile.list", rpc_params![]).await.unwrap();
    assert!(listed.iter().any(|p| p["id"] == expected.id));
    let profile: Profile = client
        .request("profile.get", params(json!({ "id": expected.id })))
        .await
        .unwrap();
    assert_eq!(profile, expected);
    harness.stop().await;
}

#[tokio::test]
async fn bad_params_and_unknown_methods_use_the_standard_codes() {
    let harness = start().await;
    let client = harness.ready().await;
    assert_eq!(
        error_code(client.request("profile.get", rpc_params![]).await),
        code::INVALID_PARAMS
    );
    assert_eq!(
        error_code(
            client
                .request("profile.get", rpc_params!["positional"])
                .await
        ),
        code::INVALID_PARAMS
    );
    assert_eq!(
        error_code(
            client
                .request("profile.get", params(json!({ "id": "nope" })))
                .await
        ),
        code::INVALID_PARAMS
    );
    assert_eq!(
        error_code(client.request("no.such.method", rpc_params![]).await),
        METHOD_NOT_FOUND
    );
    harness.stop().await;
}

#[tokio::test]
async fn module_methods_are_routed_by_manifest() {
    let harness = start().await;
    let client = harness.connect_with_bearer().await;
    let _: Value = client
        .request("hello", params(json!({ "proto": PROTOCOL })))
        .await
        .unwrap();
    let answer: Value = client
        .request("echo.ping", params(json!({ "x": 1 })))
        .await
        .unwrap();
    assert_eq!(
        answer,
        json!({ "method": "echo.ping", "params": { "x": 1 } })
    );
    harness.stop().await;
}

#[tokio::test]
async fn a_panicking_module_breaks_alone_and_the_connection_survives() {
    let harness = start().await;
    let client = harness.ready().await;
    assert_eq!(
        error_code(client.request("bomb.ping", rpc_params![]).await),
        code::MODULE_FAILED
    );
    assert_eq!(
        error_code(client.request("bomb.ping", rpc_params![]).await),
        code::MODULE_UNAVAILABLE
    );

    let modules: Vec<ModuleSummary> = client.request("module.list", rpc_params![]).await.unwrap();
    let bomb = modules.iter().find(|m| m.id == "t/bomb@1.0.0").unwrap();
    assert!(
        matches!(&bomb.state, ModuleState::Broken { reason } if reason.contains("echo exploded"))
    );
    let _: Value = client.request("echo.ping", rpc_params![]).await.unwrap();
    harness.stop().await;
}
