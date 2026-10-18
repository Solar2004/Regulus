//! How a client beside the core finds it or starts one (docs/04-client.md §4), against the
//! real `regulus` program.

use std::path::{Path, PathBuf};

use jsonrpsee::core::client::ClientT;
use jsonrpsee::core::params::ObjectParams;
use jsonrpsee::ws_client::WsClientBuilder;
use regulus_core::local;
use regulus_core::Config;
use serde_json::Value;

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");

fn home(name: &str, config: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("regulus-local-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(home.join("config.toml"), config).unwrap();
    home
}

/// Stops every core serving `home`; the started core is detached, so nobody else will.
fn stop_cores(home: &Path) {
    let home = home.to_string_lossy().into_owned();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let Ok(cmdline) = std::fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<String> = cmdline
            .split(|b| *b == 0)
            .map(|a| String::from_utf8_lossy(a).into_owned())
            .collect();
        if args.contains(&home) && args.iter().any(|a| a == "serve") {
            let pid = entry.file_name().to_string_lossy().into_owned();
            let _ = std::process::Command::new("kill").arg(pid).status();
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_started_core_answers_hello_with_the_token_in_the_url() {
    let home = home("start", "[server]\nport = 0\n");
    let config = Config::load(&home).unwrap();
    assert_eq!(local::endpoint(&config), None);

    let started = local::start(&config, Path::new(PROGRAM));
    let result = async {
        let endpoint = started?;
        assert_eq!(local::endpoint(&config).as_ref(), Some(&endpoint));
        // The webview cannot set headers: it presents the token in the query.
        let url = format!("{}?token={}", endpoint.url, endpoint.token);
        let client = WsClientBuilder::default()
            .build(&url)
            .await
            .map_err(|e| e.to_string())?;
        let mut params = ObjectParams::new();
        params.insert("client", "test").map_err(|e| e.to_string())?;
        params.insert("proto", "1.0").map_err(|e| e.to_string())?;
        let hello: Value = client
            .request("hello", params)
            .await
            .map_err(|e| e.to_string())?;
        Ok::<Value, String>(hello)
    }
    .await;
    stop_cores(&home);
    let hello = result.unwrap();
    assert_eq!(hello["compatible"], true, "{hello}");
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn without_autostart_the_client_is_told_the_command() {
    let home = home("off", "[client]\nautostart = false\n");
    let config = Config::load(&home).unwrap();
    let error = local::start(&config, Path::new(PROGRAM)).unwrap_err();
    assert!(error.contains("start one with:"), "{error}");
    assert!(
        error.contains(&format!("--home {} serve", home.display())),
        "{error}"
    );
    let _ = std::fs::remove_dir_all(&home);
}

#[test]
fn a_core_that_exits_is_reported_with_its_log() {
    let home = home("broken", "[server]\nhost = \"10.0.0.1\"\n");
    // A non-loopback host fails validation here and in the program: build the config from
    // an empty home and point it at this one, so only the program fails.
    assert!(Config::load(&home).is_err());
    let mut config = Config::load(&home.join("empty")).unwrap();
    config.home = home.clone();
    config.client.start_timeout_ms = 10_000;
    let error = local::start(&config, Path::new(PROGRAM)).unwrap_err();
    assert!(error.contains("exited"), "{error}");
    assert!(error.contains("core.log"), "{error}");
    let _ = std::fs::remove_dir_all(&home);
}
