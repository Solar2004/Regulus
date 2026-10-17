//! A session against a scripted agent speaking the ACP wire format.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use regulus_acp::{Error, Launch, Permissions, Session, SessionOptions, SetupRequest};

fn launch() -> Launch {
    Launch {
        program: env!("CARGO_BIN_EXE_regulus-acp-fake-agent").into(),
        ..Launch::default()
    }
}

fn options(permissions: Permissions) -> SessionOptions {
    SessionOptions {
        cwd: PathBuf::from("/tmp/regulus-acp-test"),
        permissions,
        meta: None,
        setup: Vec::new(),
        startup: Duration::from_secs(10),
    }
}

#[test]
fn opens_a_session_and_keeps_context_across_prompts() {
    let session = Session::start(launch(), options(Permissions::Reject)).unwrap();
    assert_eq!(session.id(), "fake-session");
    assert_eq!(session.agent().name, "fake-agent");

    let turn = session.prompt("hello").unwrap();
    assert_eq!(turn.stop_reason, "end_turn");
    assert_eq!(turn.text, "echo: hello");
    assert_eq!(session.prompt("cwd").unwrap().text, "/tmp/regulus-acp-test");
}

#[test]
fn the_environment_can_be_added_to_and_removed_from() {
    let mut launch = launch();
    launch.env = BTreeMap::from([("REGULUS_ACP_ADDED".into(), "yes".into())]);
    launch.env_remove = vec!["HOME".into()];
    let session = Session::start(launch, options(Permissions::Reject)).unwrap();
    assert_eq!(session.prompt("env REGULUS_ACP_ADDED").unwrap().text, "yes");
    assert_eq!(session.prompt("env HOME").unwrap().text, "<unset>");
}

#[test]
fn permission_requests_follow_the_policy() {
    let allow = Session::start(launch(), options(Permissions::Allow)).unwrap();
    assert_eq!(allow.prompt("permission").unwrap().text, "yes");
    let reject = Session::start(launch(), options(Permissions::Reject)).unwrap();
    assert_eq!(reject.prompt("permission").unwrap().text, "no");
}

#[test]
fn an_agent_that_dies_mid_turn_closes_the_session() {
    let session = Session::start(launch(), options(Permissions::Reject)).unwrap();
    match session.prompt("exit") {
        Err(Error::Ended(reason)) => assert!(reason.contains("told to exit"), "{reason}"),
        other => panic!("expected the agent's last words, got {other:?}"),
    }
    assert!(session.prompt("hello").is_err());
}

#[test]
fn a_program_that_does_not_exist_fails_to_start() {
    let missing = Launch {
        program: "/nonexistent/agent".into(),
        ..Launch::default()
    };
    assert!(matches!(
        Session::start(missing, options(Permissions::Reject)),
        Err(Error::Protocol(_) | Error::Closed)
    ));
}

#[test]
fn a_program_that_never_answers_times_out() {
    let silent = Launch {
        program: "sleep".into(),
        args: vec!["30".into()],
        ..Launch::default()
    };
    let mut options = options(Permissions::Reject);
    options.startup = Duration::from_millis(300);
    let started = std::time::Instant::now();
    assert!(matches!(
        Session::start(silent, options),
        Err(Error::Timeout(_))
    ));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the dropped session must not wait for the agent"
    );
}

#[test]
fn setup_requests_reach_the_agent_before_the_session_opens() {
    let mut options = options(Permissions::Reject);
    options.setup = vec![SetupRequest {
        method: "providers/set".into(),
        params: serde_json::json!({ "providerId": "main", "baseUrl": "http://127.0.0.1:1" }),
    }];
    let mut meta = serde_json::Map::new();
    meta.insert(
        "claudeCode".into(),
        serde_json::json!({ "options": { "model": "m1" } }),
    );
    options.meta = Some(meta);
    let session = Session::start(launch(), options).unwrap();
    let provider: serde_json::Value =
        serde_json::from_str(&session.prompt("provider").unwrap().text).unwrap();
    assert_eq!(provider["baseUrl"], "http://127.0.0.1:1");
    let meta: serde_json::Value =
        serde_json::from_str(&session.prompt("meta").unwrap().text).unwrap();
    assert_eq!(meta["claudeCode"]["options"]["model"], "m1");
}

#[test]
fn a_setup_request_the_agent_refuses_fails_the_start() {
    let mut options = options(Permissions::Reject);
    options.setup = vec![SetupRequest {
        method: "nobody/knows".into(),
        params: serde_json::Value::Null,
    }];
    assert!(Session::start(launch(), options).is_err());
}
