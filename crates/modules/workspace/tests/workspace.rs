//! The workspace module: the example `examples/worktree.toml` against a real core with
//! `process`; the git engine on its own (worktree, `.worktreeinclude`, merge, conflict,
//! archive); and the folder engine with its quota, a rejecting gate and artifacts.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore, Storage};
use regulus_module_process::Process;
use regulus_module_workspace::engine::{
    check_branch, git, Engine, Folder, Git, Info, Merge, Options, Spec,
};
use regulus_module_workspace::{Workspace, ARTIFACTS};
use regulus_proto::{method, BusEventKind, Diagnostic, EventPage, Profile};
use serde_json::{json, Value};

const EXAMPLE: &str = include_str!("../examples/worktree.toml");

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-workspace-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn options() -> Options {
    Options {
        merge_mode: "merge".into(),
        message: "regulus: test".into(),
        author_name: "Regulus".into(),
        author_email: "regulus@localhost".into(),
        timeout_ms: 30_000,
    }
}

fn core(profile: Profile, data: &Path) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Process::new()), Box::new(Workspace::new())];
    let storage = Storage {
        data_dir: Some(data.to_path_buf()),
        ..Storage::default()
    };
    Core::build_with(modules, &[], profiles, storage)
}

fn emitted(core: &Core, id: &str, port: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let page: EventPage = serde_json::from_value(
            core.dispatch(method::EVENT_LIST, json!({ "execution_id": id }))
                .unwrap(),
        )
        .unwrap();
        let got: Vec<Value> = page
            .events
            .into_iter()
            .filter(|e| e.from.node_id == "self" && e.from.port_id == port)
            .filter_map(|e| match e.kind {
                BusEventKind::Emitted { value } => Some(value),
                _ => None,
            })
            .collect();
        if got.len() >= count {
            return got;
        }
        assert!(Instant::now() < deadline, "only {got:?} on {port}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_example_merges_the_worktree_once_the_gate_passes() {
    let data = temp("example");
    let profile: Profile = toml::from_str(EXAMPLE).unwrap();
    let core = core(profile, &data);
    let check: Vec<Diagnostic> = serde_json::from_value(
        core.dispatch(method::PROFILE_CHECK, json!({ "id": "worktree" }))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(check, []);
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "worktree" }))
        .unwrap();
    let id = started["id"].as_str().unwrap().to_string();
    let merged = emitted(&core, &id, "merged", 1).remove(0);
    assert_eq!(merged["merged"]["branch"], format!("regulus/{id}"));
    let repo = data.join("global/regulus/workspace/worktree/ws/demo-repo");
    let shown = Command::new("git")
        .arg("-C")
        .arg(&repo)
        .args(["show", "HEAD:hello.txt"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&shown.stdout),
        "hello from regulus\n"
    );
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = fs::remove_dir_all(&data);
}

#[test]
fn the_git_engine_copies_includes_merges_reports_conflicts_and_archives() {
    let root = temp("git");
    let repo = root.join("repo");
    let options = options();
    let spec = |id: &str| Spec {
        id: id.into(),
        path: root.join("trees").join(id),
        repo: Some(repo.clone()),
        branch: format!("regulus/{id}"),
        init: true,
        include_file: ".worktreeinclude".into(),
        options: options.clone(),
        ..Spec::default()
    };
    let first = Git.create(&spec("a")).unwrap();
    // The repo was made by `init`; now give it an ignored file the worktree should get.
    fs::write(repo.join(".gitignore"), ".env\n").unwrap();
    fs::write(
        repo.join(".worktreeinclude"),
        "# secrets\n.env\n../outside\n",
    )
    .unwrap();
    fs::write(repo.join(".env"), "KEY=1").unwrap();
    fs::write(root.join("outside"), "no").unwrap();
    git(&repo, &["add", ".gitignore", ".worktreeinclude"], &options).unwrap();
    git(&repo, &["commit", "-q", "-m", "ignore"], &options).unwrap();
    let second = Git.create(&spec("b")).unwrap();
    assert_eq!(
        fs::read_to_string(second.path.join(".env")).unwrap(),
        "KEY=1"
    );
    assert!(!root.join("trees/outside").exists() && !second.path.join("../outside").exists());
    assert!(
        !first.path.join(".env").exists(),
        "made before the file listed it"
    );
    // Both change the same file: the first merges, the second conflicts and the base
    // stays as the first left it.
    fs::write(first.path.join("same.txt"), "a").unwrap();
    fs::write(second.path.join("same.txt"), "b").unwrap();
    assert!(matches!(
        Git.merge(&first, &options).unwrap(),
        Merge::Done(_)
    ));
    let Merge::Conflict(conflict) = Git.merge(&second, &options).unwrap() else {
        panic!("expected a conflict");
    };
    assert_eq!(conflict["files"], json!(["same.txt"]));
    assert_eq!(fs::read_to_string(repo.join("same.txt")).unwrap(), "a");
    assert_eq!(
        git(&repo, &["status", "--porcelain"], &options).unwrap(),
        ""
    );
    // Archiving keeps the branch and what was not committed.
    fs::write(second.path.join("late.txt"), "kept").unwrap();
    Git.archive(&second, &options).unwrap();
    assert!(!second.path.exists());
    assert_eq!(
        git(&repo, &["show", "regulus/b:late.txt"], &options).unwrap(),
        "kept"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn the_folder_engine_needs_no_git() {
    let root = temp("folder");
    fs::create_dir_all(root.join("seed")).unwrap();
    fs::write(root.join("seed/a.txt"), "seed").unwrap();
    let spec = Spec {
        id: "x".into(),
        path: root.join("x"),
        seed: Some(root.join("seed")),
        target: Some(root.join("out")),
        archive_dir: root.join("archive"),
        ..Spec::default()
    };
    let info = Folder.create(&spec).unwrap();
    assert_eq!(fs::read_to_string(info.path.join("a.txt")).unwrap(), "seed");
    fs::write(info.path.join("b.txt"), "new").unwrap();
    assert!(matches!(
        Folder.merge(&info, &options()).unwrap(),
        Merge::Done(_)
    ));
    assert_eq!(fs::read_to_string(root.join("out/b.txt")).unwrap(), "new");
    let archived = Folder.archive(&info, &options()).unwrap();
    assert_eq!(archived["path"], json!(root.join("archive/x")));
    assert!(!info.path.exists());
    let _ = fs::remove_dir_all(&root);
}

fn port(id: &str, capability: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": "then" })
}

fn wire(from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": format!("{}.{}-{}.{}", from.0, from.1, to.0, to.1), "kind": "then",
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

#[test]
fn a_folder_workspace_keeps_its_quota_a_failed_gate_rejects_and_artifacts_are_kept() {
    let data = temp("quota");
    let module = "regulus/workspace@0.1.0";
    let profile: Profile = serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "ports": [port("write", "json@1", "input"), port("verdict", "text@1", "input"),
                      port("keep", "path@1", "input"),
                      port("written", "path@1", "output"), port("over", "json@1", "output"),
                      port("rejected", "json@1", "output"), port("kept", "path@1", "output"),
                      port("error", "text@1", "output")],
            "nodes": [
                { "id": "ws", "type": "workspace", "module_id": module,
                  "config": { "engine": "folder", "quota_bytes": 10, "check_ms": 0 },
                  "ports": [port("write", "json@1", "input"), port("info", "json@1", "output"),
                            port("written", "path@1", "output"),
                            port("quota_exceeded", "json@1", "output"),
                            port("error", "text@1", "output")] },
                { "id": "mq", "type": "merge_queue", "module_id": module, "config": {},
                  "ports": [port("in", "json@1", "input"), port("pass", "text@1", "input"),
                            port("fail", "text@1", "input"),
                            port("rejected", "json@1", "output")] },
                { "id": "art", "type": "artifacts", "module_id": module,
                  "config": { "allow_outside": "yes" },
                  "ports": [port("add", "path@1", "input"), port("out", "path@1", "output")] },
            ],
            "wires": [wire(("self", "write"), ("ws", "write")),
                      wire(("ws", "written"), ("self", "written")),
                      wire(("ws", "quota_exceeded"), ("self", "over")),
                      wire(("ws", "error"), ("self", "error")),
                      wire(("ws", "info"), ("mq", "in")),
                      wire(("self", "verdict"), ("mq", "fail")),
                      wire(("mq", "rejected"), ("self", "rejected")),
                      wire(("self", "keep"), ("art", "add")),
                      wire(("art", "out"), ("self", "kept"))] }]
    }))
    .unwrap();
    let core = core(profile, &data);
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    let id = started["id"].as_str().unwrap().to_string();
    let send = |port: &str, value: Value| {
        core.dispatch(
            method::WORKFLOW_SEND,
            json!({ "execution_id": id, "port": port, "value": value }),
        )
        .unwrap();
    };
    send("write", json!({ "path": "a.txt", "text": "12345" }));
    let written = emitted(&core, &id, "written", 1)[0]
        .as_str()
        .unwrap()
        .to_string();
    assert!(written.ends_with(&format!("{id}/a.txt")));
    send("write", json!({ "path": "../escape.txt", "text": "x" }));
    assert!(emitted(&core, &id, "error", 1)[0]
        .as_str()
        .unwrap()
        .contains("outside"));
    send("write", json!({ "path": "b.txt", "text": "123456" }));
    let over = emitted(&core, &id, "over", 1).remove(0);
    assert_eq!(
        (over["used"].as_u64(), over["quota"].as_u64()),
        (Some(5), Some(10))
    );
    assert!(emitted(&core, &id, "error", 2)[1]
        .as_str()
        .unwrap()
        .contains("refused"));
    send("verdict", json!("tests failed"));
    let rejected = emitted(&core, &id, "rejected", 1).remove(0);
    assert_eq!(
        (rejected["entry"].as_u64(), rejected["gate"].as_str()),
        (Some(1), Some("fail"))
    );
    send("keep", json!(written));
    let kept = emitted(&core, &id, "kept", 1)[0]
        .as_str()
        .unwrap()
        .to_string();
    assert!(kept.ends_with(&format!("{id}/1-a.txt")));
    assert_eq!(fs::read_to_string(&kept).unwrap(), "12345");
    let listed = core
        .dispatch(ARTIFACTS, json!({ "execution_id": id, "node_id": "art" }))
        .unwrap();
    assert_eq!(listed[0]["name"], "a.txt");
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = fs::remove_dir_all(&data);
}

#[test]
fn git_merges_refuse_option_like_names_and_a_dirty_checkout_and_undo_only_their_own() {
    let root = temp("guard");
    let repo = root.join("repo");
    let options = options();
    assert!(check_branch("-x", &options).is_err());
    assert!(check_branch("--upload-pack=touch", &options).is_err());
    assert!(check_branch("a..b", &options).is_err());
    assert!(check_branch("regulus/exec-1", &options).is_ok());
    let spec = |id: &str| Spec {
        id: id.into(),
        path: root.join("trees").join(id),
        repo: Some(repo.clone()),
        branch: format!("regulus/{id}"),
        init: true,
        options: options.clone(),
        ..Spec::default()
    };
    assert!(Git
        .create(&Spec {
            branch: "-b".into(),
            ..spec("x")
        })
        .is_err());
    let a = Git.create(&spec("a")).unwrap();
    // A forged branch never reaches git as an option.
    let forged = Info {
        branch: Some("--upload-pack=touch /tmp/pwn".into()),
        ..a.clone()
    };
    assert!(Git
        .merge(&forged, &options)
        .unwrap_err()
        .contains("not a branch name"));
    // The user's own staged change: no merge, and it stays staged.
    fs::write(repo.join("mine.txt"), "mine").unwrap();
    git(&repo, &["add", "mine.txt"], &options).unwrap();
    fs::write(a.path.join("theirs.txt"), "theirs").unwrap();
    assert!(Git.merge(&a, &options).unwrap_err().contains("uncommitted"));
    assert_eq!(
        git(&repo, &["status", "--porcelain"], &options).unwrap(),
        "A  mine.txt"
    );
    git(&repo, &["commit", "-q", "-m", "mine"], &options).unwrap();
    // `ff` with a base that moved: a conflict, and the checkout is left as it was.
    let ff = Options {
        merge_mode: "ff".into(),
        ..options.clone()
    };
    let Merge::Conflict(conflict) = Git.merge(&a, &ff).unwrap() else {
        panic!("a base that moved cannot fast-forward");
    };
    assert_eq!(conflict["files"], json!([]));
    assert_eq!(
        git(&repo, &["status", "--porcelain"], &options).unwrap(),
        ""
    );
    assert!(git(
        &repo,
        &["rev-parse", "-q", "--verify", "MERGE_HEAD"],
        &options
    )
    .is_err());
    let _ = fs::remove_dir_all(&root);
}

#[cfg(unix)]
#[test]
fn a_folder_merge_never_writes_through_a_link_in_its_target_and_a_seed_cannot_hold_it() {
    let root = temp("links");
    fs::create_dir_all(root.join("outside")).unwrap();
    fs::create_dir_all(root.join("target")).unwrap();
    std::os::unix::fs::symlink(root.join("outside"), root.join("target/evil")).unwrap();
    let spec = Spec {
        id: "x".into(),
        path: root.join("x"),
        target: Some(root.join("target")),
        archive_dir: root.join("archive"),
        ..Spec::default()
    };
    let info = Folder.create(&spec).unwrap();
    fs::create_dir_all(info.path.join("evil")).unwrap();
    fs::write(info.path.join("evil/payload"), "x").unwrap();
    assert!(Folder.merge(&info, &options()).is_err());
    assert!(!root.join("outside/payload").exists());
    // A link inside the workspace is left out of the merge.
    fs::remove_dir_all(info.path.join("evil")).unwrap();
    std::os::unix::fs::symlink("/etc", info.path.join("etc")).unwrap();
    fs::write(info.path.join("ok.txt"), "ok").unwrap();
    assert!(matches!(
        Folder.merge(&info, &options()).unwrap(),
        Merge::Done(_)
    ));
    assert!(root.join("target/ok.txt").exists());
    assert!(root.join("target/etc").symlink_metadata().is_err());
    // A folder inside its own seed would copy itself for ever.
    let looped = Spec {
        seed: Some(root.clone()),
        path: root.join("inner"),
        ..spec
    };
    assert!(Folder
        .create(&looped)
        .unwrap_err()
        .contains("inside `seed`"));
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_merge_queue_refuses_a_workspace_it_did_not_make() {
    let data = temp("forged");
    let module = "regulus/workspace@0.1.0";
    let profile: Profile = serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "ports": [port("in", "json@1", "input"), port("error", "text@1", "output")],
            "nodes": [
                { "id": "mq", "type": "merge_queue", "module_id": module, "config": {},
                  "ports": [port("in", "json@1", "input"), port("pass", "text@1", "input"),
                            port("error", "text@1", "output")] },
            ],
            "wires": [wire(("self", "in"), ("mq", "in")),
                      wire(("mq", "error"), ("self", "error"))] }]
    }))
    .unwrap();
    let core = core(profile, &data);
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    let id = started["id"].as_str().unwrap().to_string();
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": "in", "value": {
            "id": "x", "engine": "git", "path": "/home", "repo": "/home",
            "branch": "--upload-pack=touch /tmp/pwn", "base": "master" } }),
    )
    .unwrap();
    let error = emitted(&core, &id, "error", 1).remove(0);
    assert!(error.as_str().unwrap().contains("refused"), "{error}");
    // A bad merge mode does not start.
    let bad: Profile = serde_json::from_value(json!({
        "id": "q", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "ports": [], "wires": [],
            "nodes": [{ "id": "mq", "type": "merge_queue", "module_id": module,
                        "config": { "merge_mode": "rebase" },
                        "ports": [port("pass", "text@1", "input")] }] }]
    }))
    .unwrap();
    let mut profiles = ProfileStore::default();
    profiles.insert(bad).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Workspace::new())];
    let storage = Storage {
        data_dir: Some(data.clone()),
        ..Storage::default()
    };
    let other = Core::build_with(modules, &[], profiles, storage);
    let started = other
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "q" }))
        .unwrap();
    assert_eq!(started["state"]["status"], "failed", "{started}");
    core.dispatch(method::EXECUTION_STOP, json!({ "id": id }))
        .unwrap();
    let _ = fs::remove_dir_all(&data);
}
