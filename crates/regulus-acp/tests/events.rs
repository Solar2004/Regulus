//! What a session reports as it goes, against the scripted agent: events behind a
//! cursor, permission requests a client answers, and sessions loaded by their native id.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_acp::{
    Launch, Permissions, Session, SessionEvent, SessionEventKind, SessionOptions, SessionState,
};

fn launch() -> Launch {
    Launch {
        program: env!("CARGO_BIN_EXE_regulus-acp-fake-agent").into(),
        ..Launch::default()
    }
}

fn options(permissions: Permissions) -> SessionOptions {
    SessionOptions {
        cwd: PathBuf::from("/tmp"),
        permissions,
        meta: None,
        setup: Vec::new(),
        startup: Duration::from_secs(10),
        mcp_servers: Vec::new(),
        events_keep: regulus_acp::DEFAULT_EVENTS_KEEP,
        permission_timeout: regulus_acp::DEFAULT_PERMISSION_TIMEOUT,
        resume: None,
    }
}

fn kinds(events: &[SessionEvent]) -> Vec<&'static str> {
    events
        .iter()
        .map(|e| match &e.kind {
            SessionEventKind::TurnStarted => "turn_started",
            SessionEventKind::Text { .. } => "text",
            SessionEventKind::Thought { .. } => "thought",
            SessionEventKind::ToolCall { .. } => "tool_call",
            SessionEventKind::ToolUpdate { .. } => "tool_update",
            SessionEventKind::Plan { .. } => "plan",
            SessionEventKind::PermissionRequest { .. } => "permission_request",
            SessionEventKind::PermissionResolved { .. } => "permission_resolved",
            SessionEventKind::TurnEnded { .. } => "turn_ended",
            SessionEventKind::State { .. } => "state",
        })
        .collect()
}

/// Waits for the permission request of the turn in flight and returns its id.
fn asked(session: &Session) -> (String, Vec<String>) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (events, _) = session.events(0);
        let found = events.iter().find_map(|e| match &e.kind {
            SessionEventKind::PermissionRequest { id, options, .. } => {
                Some((id.clone(), options.iter().map(|o| o.id.clone()).collect()))
            }
            _ => None,
        });
        if let Some(found) = found {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "no permission request: {events:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn events_come_in_order_behind_a_cursor() {
    let session = Session::start(launch(), options(Permissions::Reject)).unwrap();
    assert_eq!(session.prompt("tool").unwrap().text, "tool done");
    let (first, next) = session.events(0);
    assert_eq!(
        kinds(&first),
        [
            "state",
            "turn_started",
            "state",
            "tool_call",
            "tool_update",
            "text",
            "turn_ended",
            "state"
        ]
    );
    assert_eq!(next, first.last().unwrap().seq);
    assert!(first.windows(2).all(|w| w[1].seq == w[0].seq + 1));
    match &first[3].kind {
        SessionEventKind::ToolCall {
            id, title, kind, ..
        } => {
            assert_eq!((id.as_str(), title.as_str()), ("call-1", "read a file"));
            assert_eq!(kind.as_deref(), Some("read"));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(first[3].turn, Some(1));
    assert!(matches!(
        &first[4].kind,
        SessionEventKind::ToolUpdate { status, content: Some(_), .. } if status == "completed"
    ));

    assert_eq!(session.prompt("think").unwrap().text, "thought done");
    let (second, _) = session.events(next);
    assert_eq!(second[0].kind, SessionEventKind::TurnStarted);
    assert_eq!(second[0].turn, Some(2));
    assert!(second.iter().any(
        |e| matches!(&e.kind, SessionEventKind::Thought { text } if text == "thinking it over")
    ));
    assert!(session.events(second.last().unwrap().seq).0.is_empty());
}

#[test]
fn the_buffer_keeps_only_the_newest_events() {
    let mut options = options(Permissions::Reject);
    options.events_keep = 3;
    let session = Session::start(launch(), options).unwrap();
    session.prompt("tool").unwrap();
    let (kept, _) = session.events(0);
    assert_eq!(kinds(&kept), ["text", "turn_ended", "state"]);
    assert_eq!(
        kept[0].seq, 6,
        "seq counts the events that were dropped too"
    );
}

#[test]
fn an_asked_permission_waits_for_a_decision() {
    let session = Arc::new(Session::start(launch(), options(Permissions::Ask)).unwrap());
    let turn = {
        let session = session.clone();
        std::thread::spawn(move || session.prompt("ask"))
    };
    let (id, options) = asked(&session);
    assert_eq!(options, ["yes", "always", "no"]);
    assert!(!turn.is_finished(), "the agent waits for the answer");
    let wrong = session.decide(&id, Some("maybe".into())).unwrap_err();
    assert!(wrong.contains("no option `maybe`"), "{wrong}");
    session.decide(&id, Some("always".into())).unwrap();
    assert_eq!(turn.join().unwrap().unwrap().text, "chose always");
    let (events, _) = session.events(0);
    assert!(events.iter().any(|e| e.kind
        == SessionEventKind::PermissionResolved {
            id: id.clone(),
            option: Some("always".into())
        }));
    assert!(
        session.decide(&id, Some("yes".into())).is_err(),
        "answered once"
    );
}

#[test]
fn an_auto_policy_answers_without_asking() {
    let session = Session::start(launch(), options(Permissions::Reject)).unwrap();
    assert_eq!(session.prompt("ask").unwrap().text, "chose no");
    let (events, _) = session.events(0);
    assert!(!kinds(&events).contains(&"permission_request"));
    let session = Session::start(launch(), options(Permissions::Allow)).unwrap();
    assert_eq!(session.prompt("ask").unwrap().text, "chose yes");
}

#[test]
fn an_unanswered_ask_rejects_when_its_time_runs_out() {
    let mut options = options(Permissions::Ask);
    options.permission_timeout = Duration::from_millis(300);
    let session = Session::start(launch(), options).unwrap();
    assert_eq!(session.prompt("ask").unwrap().text, "chose no");
}

#[test]
fn a_cancel_cancels_the_request_it_waits_on() {
    let session = Arc::new(Session::start(launch(), options(Permissions::Ask)).unwrap());
    let turn = {
        let session = session.clone();
        std::thread::spawn(move || session.prompt("ask"))
    };
    let (id, _) = asked(&session);
    assert!(session.cancel());
    assert_eq!(turn.join().unwrap().unwrap().text, "chose cancelled");
    let (events, _) = session.events(0);
    assert!(events.iter().any(|e| e.kind
        == SessionEventKind::PermissionResolved {
            id: id.clone(),
            option: None
        }));
}

#[test]
fn a_session_is_loaded_by_its_native_id_without_its_replay_as_news() {
    let mut launch = launch();
    launch
        .env
        .insert("REGULUS_FAKE_LOAD_SESSION".into(), "1".into());
    let mut options = options(Permissions::Reject);
    options.resume = Some("old-session".into());
    let session = Session::start(launch, options).unwrap();
    assert!(session.load_session() && session.resumed());
    assert_eq!(session.id(), "old-session");
    assert!(session.warnings().is_empty());
    assert_eq!(session.prompt("session").unwrap().text, "old-session");
    let (events, _) = session.events(0);
    assert!(
        !events.iter().any(|e| matches!(&e.kind,
            SessionEventKind::Text { text } if text == "old answer")),
        "{events:?}"
    );
    assert_eq!(
        events[0].kind,
        SessionEventKind::State {
            state: SessionState::Idle
        }
    );
}

#[test]
fn an_agent_that_cannot_load_opens_a_new_session_and_says_so() {
    let mut options = options(Permissions::Reject);
    options.resume = Some("old-session".into());
    let session = Session::start(launch(), options).unwrap();
    assert!(!session.load_session() && !session.resumed());
    assert_eq!(session.id(), "fake-session");
    assert!(
        session.warnings()[0].contains("cannot load sessions"),
        "{:?}",
        session.warnings()
    );
}
