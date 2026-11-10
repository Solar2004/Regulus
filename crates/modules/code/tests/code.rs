//! The code module against a real core: what an agent gets as tools, where those may
//! reach, how an edit that does not match is refused, how a command is bounded and
//! cleaned up, what git is kept from running, and what `code.inspect` answers.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use regulus_contract_tool as tool;
use regulus_core::config::StoreConfig;
use regulus_core::{Core, Module, ModuleCtx, ModuleError, ProfileStore, Storage, Store};
use regulus_module_code::{contract, Code};
use regulus_proto::{
    method, BusEventKind, EventPage, Manifest, ModuleInfo, Node, NodeCall, Profile,
};
use serde_json::{json, Value};

const MODULE: &str = "regulus/code@0.1.0";
const FAKE: &str = "t/remote@1.0.0";
const WAIT: Duration = Duration::from_secs(10);
/// JSON-RPC codes, as `regulus-proto` names them.
const INVALID_PARAMS: i32 = -32602;
const MODULE_FAILED: i32 = -32003;

const READS: [&str; 5] = [
    "code_list_files",
    "code_read_file",
    "code_search",
    "code_git_status",
    "code_git_diff",
];

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-code-it-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir.canonicalize().unwrap()
}

fn put(root: &Path, path: &str, content: impl AsRef<[u8]>) {
    let file = root.join(path);
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(file, content).unwrap();
}

fn port(id: &str, capability: &str, direction: &str, kind: &str) -> Value {
    json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
}

fn wire(id: &str, kind: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": kind,
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

/// The `workspace` input of a view or of the instructions.
fn workspace_input() -> Value {
    json!({ "id": "workspace", "capability": contract::CAPABILITY, "direction": "input",
            "kind": "uses", "required": true })
}

/// `ws` is the workspace; `ro`, `rw` and `all` are views of it; `docs` reads its
/// instructions. `self.root` moves it; what it changes and runs leaves on `self`.
fn profile(workspace: Value, docs: Value) -> Profile {
    let view = |id: &str, config: Value| {
        json!({ "id": id, "type": "tools", "module_id": MODULE, "config": config,
                "ports": [workspace_input(), port("tools", "tool@1", "output", "uses")] })
    };
    serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "ports": [port("root", "path@1", "input", "then"),
                      port("changed", "json@1", "output", "then"),
                      port("ran", "json@1", "output", "then")],
            "nodes": [
                { "id": "ws", "type": "workspace", "module_id": MODULE, "config": workspace,
                  "ports": [port("workspace", contract::CAPABILITY, "output", "uses"),
                            port("tools", "tool@1", "output", "uses"),
                            port("root", "path@1", "input", "then"),
                            port("changed", "json@1", "output", "then"),
                            port("ran", "json@1", "output", "then")] },
                view("ro", json!({})),
                view("rw", json!({ "access": "write" })),
                view("all", json!({ "access": "all",
                                    "tools": ["code_run_command", "code_read_file"] })),
                { "id": "docs", "type": "instructions", "module_id": MODULE, "config": docs,
                  "ports": [workspace_input(), port("hook", "hook@1", "output", "uses")] },
            ],
            "wires": [
                wire("root", "then", ("self", "root"), ("ws", "root")),
                wire("changed", "then", ("ws", "changed"), ("self", "changed")),
                wire("ran", "then", ("ws", "ran"), ("self", "ran")),
                wire("ro", "uses", ("ws", "workspace"), ("ro", "workspace")),
                wire("rw", "uses", ("ws", "workspace"), ("rw", "workspace")),
                wire("all", "uses", ("ws", "workspace"), ("all", "workspace")),
                wire("docs", "uses", ("ws", "workspace"), ("docs", "workspace")),
            ] }]
    }))
    .unwrap()
}

/// A core running one execution of a profile, over a folder of its own.
struct World {
    core: Arc<Core>,
    id: String,
    profile: Profile,
    home: PathBuf,
    root: PathBuf,
    /// The folder outlives this world: another one of the same test goes on in it.
    shared: bool,
}

impl Drop for World {
    fn drop(&mut self) {
        self.core.shutdown();
        if !self.shared {
            let _ = fs::remove_dir_all(&self.home);
        }
    }
}

impl World {
    /// A workspace over the empty folder `<home>/root`, with `config` over its defaults.
    fn start(name: &str, config: Value) -> Self {
        let home = temp(name);
        let root = home.join("root");
        fs::create_dir_all(&root).unwrap();
        Self::over(home, root, config, json!({}))
    }

    fn over(home: PathBuf, root: PathBuf, mut config: Value, docs: Value) -> Self {
        config["root"] = json!(root);
        let world = Self::build(
            home,
            root,
            profile(config, docs),
            vec![Box::new(Code::new())],
        );
        assert_eq!(world.state(), "running");
        world
    }

    fn build(
        home: PathBuf,
        root: PathBuf,
        profile: Profile,
        modules: Vec<Box<dyn Module>>,
    ) -> Self {
        let mut profiles = ProfileStore::default();
        profiles.insert(profile.clone()).unwrap();
        let core = Core::build(modules, &[], profiles);
        let started = core
            .dispatch(method::EXECUTION_START, json!({ "profile_id": profile.id }))
            .unwrap();
        Self {
            id: started["id"].as_str().unwrap().to_string(),
            core,
            profile,
            home,
            root,
            shared: false,
        }
    }

    /// Leaves its folder in place when it goes.
    fn sharing(mut self) -> Self {
        self.shared = true;
        self
    }

    fn execution(&self) -> Value {
        let listed = self
            .core
            .dispatch(method::EXECUTION_LIST, json!({}))
            .unwrap();
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["id"] == self.id.as_str())
            .unwrap()
            .clone()
    }

    fn state(&self) -> String {
        self.execution()["state"]["status"]
            .as_str()
            .unwrap()
            .to_string()
    }

    fn node(&self, id: &str) -> Node {
        self.profile.workflows[0]
            .nodes
            .iter()
            .find(|node| node.id == id)
            .unwrap_or_else(|| panic!("no node {id}"))
            .clone()
    }

    /// A call to a node itself, as an engine or a Tools box makes it over a wire.
    fn on_node(&self, node: &str, name: &str, args: Value) -> Result<Value, String> {
        let call = json!({ "execution_id": self.id, "node": self.node(node), "args": args });
        self.core
            .registry()
            .call_module(MODULE, name, call)
            .map_err(|e| e.to_string())
    }

    fn tools_of(&self, node: &str) -> Vec<String> {
        let specs: Vec<tool::Spec> =
            serde_json::from_value(self.on_node(node, tool::LIST, Value::Null).unwrap()).unwrap();
        specs.into_iter().map(|spec| spec.name).collect()
    }

    fn call_on(&self, node: &str, name: &str, args: Value) -> tool::Outcome {
        let call = json!({ "name": name, "args": args, "caller": { "name": "tester" } });
        serde_json::from_value(self.on_node(node, tool::CALL, call).unwrap()).unwrap()
    }

    /// An operation of the workspace that must work: what it answered.
    fn ok(&self, name: &str, args: Value) -> Value {
        let outcome = self.call_on("ws", name, args);
        assert!(!outcome.error, "{name}: {}", outcome.data);
        outcome.data
    }

    /// An operation of the workspace that must be refused: why.
    fn refused(&self, name: &str, args: Value) -> String {
        let outcome = self.call_on("ws", name, args.clone());
        assert!(
            outcome.error,
            "{name} {args} went through: {}",
            outcome.data
        );
        outcome
            .data
            .as_str()
            .unwrap_or_else(|| panic!("{name} {args}: {}", outcome.data))
            .to_string()
    }

    fn inspect(&self, node: &str, look: Value) -> Result<Value, (i32, String)> {
        let mut params = json!({ "execution_id": self.id, "node_id": node });
        params
            .as_object_mut()
            .unwrap()
            .extend(look.as_object().unwrap().clone());
        self.core
            .dispatch(contract::INSPECT, params)
            .map_err(|e| (e.code, e.message))
    }

    fn look(&self, look: Value) -> Value {
        self.inspect("ws", look.clone())
            .unwrap_or_else(|e| panic!("{look}: {e:?}"))
    }

    fn page(&self) -> EventPage {
        serde_json::from_value(
            self.core
                .dispatch(method::EVENT_LIST, json!({ "execution_id": self.id }))
                .unwrap(),
        )
        .unwrap()
    }

    /// The values that left on the execution's own output `port`, once the bus is idle.
    fn emitted(&self, port: &str, count: usize) -> Vec<Value> {
        let deadline = Instant::now() + WAIT;
        loop {
            let page = self.page();
            let got: Vec<Value> = page
                .events
                .iter()
                .filter(|e| e.from.node_id == "self" && e.from.port_id == port)
                .filter_map(|e| match &e.kind {
                    BusEventKind::Emitted { value } => Some(value.clone()),
                    _ => None,
                })
                .collect();
            if got.len() >= count && page.pending == 0 {
                return got;
            }
            assert!(Instant::now() < deadline, "self.{port} got {got:?}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn send(&self, port: &str, value: Value) {
        self.core
            .dispatch(
                method::WORKFLOW_SEND,
                json!({ "execution_id": self.id, "port": port, "value": value }),
            )
            .unwrap();
    }

    /// Why the bus could not hand a value over, if it could not.
    fn delivery_failures(&self) -> Vec<String> {
        let deadline = Instant::now() + WAIT;
        loop {
            let page = self.page();
            if page.pending == 0 {
                return page
                    .events
                    .into_iter()
                    .filter_map(|e| match e.kind {
                        BusEventKind::Failed { error, .. } => Some(error),
                        _ => None,
                    })
                    .collect();
            }
            assert!(Instant::now() < deadline, "the bus never went idle");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

/// Whether the process `pid` runs nothing any more: gone, or a zombie its parent has yet
/// to reap.
fn ended(pid: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let dead = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Err(_) => true,
            Ok(stat) => stat
                .rsplit(')')
                .next()
                .is_some_and(|rest| rest.trim_start().starts_with('Z')),
        };
        if dead || Instant::now() >= deadline {
            return dead;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn paths(entries: &Value) -> Vec<&str> {
    entries
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["path"].as_str().unwrap())
        .collect()
}

fn hits(found: &Value) -> Vec<(String, u64)> {
    found["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["path"].as_str().unwrap().to_string(),
                m["line"].as_u64().unwrap(),
            )
        })
        .collect()
}

const MAIN_RS: &str = "fn main() {\n    println!(\"hello\");\n}\n";
const LIB_RS: &str = "pub fn greet() -> &'static str {\n    \"Hello\"\n}\n";

/// A small tree: sources, a build folder a search passes by, and `.git`.
fn tree(root: &Path) {
    put(root, "README.md", "# Demo\nA demo project.\n");
    put(root, "src/main.rs", MAIN_RS);
    put(root, "src/lib.rs", LIB_RS);
    put(root, "src/deep/more.rs", "// TODO: more\n");
    put(root, "target/debug/out.txt", "hello from the build\n");
    put(
        root,
        ".git/config",
        "[core]\n\turl = https://token@example.invalid/x.git\n",
    );
    let numbered: String = (1..=100).map(|n| format!("line {n}\n")).collect();
    put(root, "big.txt", numbered);
    put(root, "bin.dat", [b'a', 0, b'b']);
}

#[test]
fn files_are_listed_read_and_searched_within_the_limits() {
    let world = World::start(
        "files",
        json!({ "max_read_bytes": 64, "max_search_matches": 3 }),
    );
    tree(&world.root);

    let top = world.ok("code_list_files", json!({}));
    assert_eq!(
        top,
        json!({ "path": "", "truncated": false, "entries": [
            { "path": "README.md", "kind": "file", "size": 23 },
            { "path": "big.txt", "kind": "file", "size": 792 },
            { "path": "bin.dat", "kind": "file", "size": 3 },
            { "path": "src", "kind": "directory" },
            { "path": "target", "kind": "directory" },
        ] }),
        "`.git` is never listed"
    );
    let deep = world.ok("code_list_files", json!({ "path": "src", "depth": "3" }));
    assert_eq!(
        paths(&deep["entries"]),
        ["src/deep", "src/deep/more.rs", "src/lib.rs", "src/main.rs"]
    );
    let all = world.ok("code_list_files", json!({ "depth": 5 }));
    assert!(paths(&all["entries"]).contains(&"target"));
    assert!(
        !paths(&all["entries"])
            .iter()
            .any(|p| p.starts_with("target/")),
        "a skipped folder is listed, not entered"
    );
    assert!(world
        .refused("code_list_files", json!({ "path": "README.md" }))
        .contains("not a folder"));

    let whole = world.ok("code_read_file", json!({ "path": "src/main.rs" }));
    assert_eq!(
        whole,
        json!({ "path": "src/main.rs", "text": MAIN_RS, "start_line": 1, "end_line": 3,
                "size": MAIN_RS.len(), "truncated": false })
    );
    // 64 bytes hold the nine lines `line 1` … `line 9`; the tenth is left for the next read.
    let first = world.ok("code_read_file", json!({ "path": "big.txt" }));
    assert_eq!(first["text"].as_str().unwrap().lines().count(), 9);
    assert_eq!(
        (first["start_line"].as_u64(), first["end_line"].as_u64()),
        (Some(1), Some(9))
    );
    assert_eq!(first["truncated"], true);
    let next = world.ok(
        "code_read_file",
        json!({ "path": "big.txt", "offset": 10, "limit": 2 }),
    );
    assert_eq!(next["text"], "line 10\nline 11\n");
    assert_eq!(
        (next["end_line"].as_u64(), next["truncated"].as_bool()),
        (Some(11), Some(true))
    );
    let last = world.ok(
        "code_read_file",
        json!({ "path": "big.txt", "offset": 100 }),
    );
    assert_eq!(
        (last["text"].as_str(), last["truncated"].as_bool()),
        (Some("line 100\n"), Some(false))
    );
    let past = world.ok(
        "code_read_file",
        json!({ "path": "big.txt", "offset": 101 }),
    );
    assert_eq!(
        (past["text"].as_str(), past["end_line"].as_u64()),
        (Some(""), Some(100))
    );
    for (path, why) in [
        ("bin.dat", "not a text file"),
        ("src", "is a folder"),
        ("nope.txt", "there is no `nope.txt`"),
    ] {
        let said = world.refused("code_read_file", json!({ "path": path }));
        assert!(said.contains(why), "{path}: {said}");
    }
    assert!(world
        .refused("code_read_file", json!({}))
        .contains("`path` is required"));

    // The text as written; the build folder is passed by.
    let exact = world.ok("code_search", json!({ "query": "hello" }));
    assert_eq!(hits(&exact), [("src/main.rs".to_string(), 2)]);
    assert_eq!(exact["matches"][0]["text"], "    println!(\"hello\");");
    let any_case = world.ok(
        "code_search",
        json!({ "query": "hello", "ignore_case": true }),
    );
    assert_eq!(
        hits(&any_case),
        [
            ("src/lib.rs".to_string(), 2),
            ("src/main.rs".to_string(), 2)
        ]
    );
    let by_regex = world.ok(
        "code_search",
        json!({ "query": r"fn \w+\(", "regex": true }),
    );
    assert_eq!(
        hits(&by_regex),
        [
            ("src/lib.rs".to_string(), 1),
            ("src/main.rs".to_string(), 1)
        ]
    );
    let literal = world.ok("code_search", json!({ "query": r"fn \w+\(" }));
    assert_eq!(
        literal["matches"],
        json!([]),
        "without `regex` it is the text itself"
    );
    let by_glob = world.ok(
        "code_search",
        json!({ "query": "demo", "ignore_case": true, "glob": "*.md" }),
    );
    assert_eq!(
        hits(&by_glob),
        [("README.md".to_string(), 1), ("README.md".to_string(), 2)]
    );
    let in_folder = world.ok(
        "code_search",
        json!({ "query": "TODO", "path": "src/deep" }),
    );
    assert_eq!(hits(&in_folder), [("src/deep/more.rs".to_string(), 1)]);
    let in_file = world.ok(
        "code_search",
        json!({ "query": "Demo", "path": "README.md" }),
    );
    assert_eq!(hits(&in_file), [("README.md".to_string(), 1)]);
    let asked_for = world.ok("code_search", json!({ "query": "hello", "path": "target" }));
    assert_eq!(hits(&asked_for), [("target/debug/out.txt".to_string(), 1)]);
    let many = world.ok("code_search", json!({ "query": "line" }));
    assert_eq!(many["matches"].as_array().unwrap().len(), 3);
    assert_eq!(many["truncated"], true);
    assert!(world
        .refused("code_search", json!({ "query": "(", "regex": true }))
        .contains("not a regular expression"));
    assert!(world
        .refused("code_search", json!({ "query": "" }))
        .contains("empty"));
}

#[test]
fn no_operation_leaves_the_root_or_enters_a_protected_name() {
    let world = World::start("contained", json!({}));
    let (root, home) = (world.root.clone(), world.home.clone());
    put(&home, "secret.txt", "outside\n");
    put(&root, "real.txt", "inside\n");
    put(&root, ".git/config", "[core]\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        symlink(home.join("secret.txt"), root.join("link_out")).unwrap();
        symlink(&home, root.join("dir_out")).unwrap();
        symlink(root.join("real.txt"), root.join("link_in")).unwrap();
        symlink(home.join("nowhere"), root.join("broken")).unwrap();
    }
    let outside = home.join("secret.txt").display().to_string();
    let leaving = [
        "../secret.txt",
        "sub/../../secret.txt",
        outside.as_str(),
        "/etc/passwd",
        "link_out",
        "dir_out/secret.txt",
    ];
    let said_no = |said: String, what: &str| {
        assert!(
            said.contains("outside the workspace") || said.contains("not a path inside"),
            "{what}: {said}"
        );
    };
    for path in leaving {
        said_no(
            world.refused("code_read_file", json!({ "path": path })),
            path,
        );
        said_no(
            world.refused("code_write_file", json!({ "path": path, "content": "x" })),
            path,
        );
        said_no(
            world.refused(
                "code_edit_file",
                json!({ "path": path, "old_text": "outside", "new_text": "x" }),
            ),
            path,
        );
        said_no(
            world.refused("code_search", json!({ "query": "o", "path": path })),
            path,
        );
        said_no(
            world.refused("code_git_diff", json!({ "path": path })),
            path,
        );
    }
    for folder in ["..", "dir_out", "/"] {
        said_no(
            world.refused("code_list_files", json!({ "path": folder })),
            folder,
        );
        said_no(
            world.refused(
                "code_run_command",
                json!({ "command": "pwd", "cwd": folder }),
            ),
            folder,
        );
    }
    said_no(
        world.refused(
            "code_write_file",
            json!({ "path": "dir_out/new/evil.txt", "content": "x" }),
        ),
        "a new file under a link out",
    );
    assert!(world
        .refused(
            "code_write_file",
            json!({ "path": "broken", "content": "x" })
        )
        .contains("leads nowhere"));
    assert!(world
        .refused(
            "code_write_file",
            json!({ "path": "broken/x.txt", "content": "x" })
        )
        .contains("leads nowhere"));
    for path in [
        ".git/config",
        ".git/hooks/pre-commit",
        ".GIT/config",
        "sub/.git/x",
    ] {
        for said in [
            world.refused("code_read_file", json!({ "path": path })),
            world.refused("code_write_file", json!({ "path": path, "content": "x" })),
        ] {
            assert!(said.contains("protected"), "{path}: {said}");
        }
    }
    assert!(world
        .refused("code_list_files", json!({ "path": ".git" }))
        .contains("protected"));
    assert!(world
        .refused(
            "code_run_command",
            json!({ "command": "pwd", "cwd": ".git" })
        )
        .contains("protected"));

    // Nothing was touched outside, and nothing appeared there.
    assert_eq!(
        fs::read_to_string(home.join("secret.txt")).unwrap(),
        "outside\n"
    );
    assert!(!home.join("evil.txt").exists() && !home.join("new").exists());
    assert!(
        !home.join("nowhere").exists(),
        "a broken link is not written through"
    );
    assert!(!root.join(".git/hooks").exists());

    // What is listed is what may be reached: the links out and `.git` are not there.
    let listed = world.ok("code_list_files", json!({}));
    assert_eq!(paths(&listed["entries"]), ["link_in", "real.txt"]);
    // A link that stays inside is the file it leads to.
    let through = world.ok("code_read_file", json!({ "path": "link_in" }));
    assert_eq!(
        (through["path"].as_str(), through["text"].as_str()),
        (Some("real.txt"), Some("inside\n"))
    );
    let found = world.ok("code_search", json!({ "query": "side" }));
    assert_eq!(
        hits(&found),
        [("real.txt".to_string(), 1)],
        "a search follows no link"
    );
}

#[test]
fn files_are_written_whole_and_edited_only_where_the_text_matches() {
    let world = World::start("edits", json!({ "max_write_bytes": 64 }));
    let root = world.root.clone();
    let text = |path: &str| fs::read_to_string(root.join(path)).unwrap();

    let made = world.ok(
        "code_write_file",
        json!({ "path": "notes/deep/a.txt", "content": "one\ntwo\none\n" }),
    );
    assert_eq!(
        made,
        json!({ "path": "notes/deep/a.txt", "bytes": 12, "created": true })
    );
    assert_eq!(text("notes/deep/a.txt"), "one\ntwo\none\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let file = root.join("notes/deep/a.txt");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o750)).unwrap();
        let again = world.ok(
            "code_write_file",
            json!({ "path": "notes/deep/a.txt", "content": "one\ntwo\none\n" }),
        );
        assert_eq!(again["created"], false);
        let mode = fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o750, "replacing a file keeps its permissions");
    }
    world.ok(
        "code_write_file",
        json!({ "path": "empty.txt", "content": "" }),
    );
    assert_eq!(text("empty.txt"), "");
    for (args, why) in [
        (
            json!({ "path": "big.txt", "content": "x".repeat(65) }),
            "max_write_bytes",
        ),
        (json!({ "path": "notes", "content": "x" }), "is a folder"),
        (json!({ "path": "", "content": "x" }), "`path` is empty"),
        (
            json!({ "path": ".", "content": "x" }),
            "the workspace folder itself",
        ),
        (json!({ "path": "a.txt" }), "`content` is required"),
    ] {
        let said = world.refused("code_write_file", args.clone());
        assert!(said.contains(why), "{args}: {said}");
    }
    assert!(!root.join("big.txt").exists());

    let edit = |old: &str, new: &str, all: bool| json!({ "path": "notes/deep/a.txt", "old_text": old, "new_text": new, "replace_all": all });
    let once = world.ok("code_edit_file", edit("two", "2", false));
    assert_eq!(
        once,
        json!({ "path": "notes/deep/a.txt", "replacements": 1, "bytes": 10 })
    );
    assert_eq!(text("notes/deep/a.txt"), "one\n2\none\n");

    // Text that is not there, or is there twice, changes nothing and says which.
    let stale = world.refused("code_edit_file", edit("two", "3", false));
    assert!(
        stale.starts_with("conflict: `old_text` is not in"),
        "{stale}"
    );
    let twice = world.refused("code_edit_file", edit("one", "1", false));
    assert!(
        twice.starts_with("conflict: `old_text` is in") && twice.contains("2 times"),
        "{twice}"
    );
    let spaced = world.refused("code_edit_file", edit("one \n2", "x", false));
    assert!(spaced.starts_with("conflict:"), "spaces count: {spaced}");
    assert_eq!(
        text("notes/deep/a.txt"),
        "one\n2\none\n",
        "a conflict leaves the file as it was"
    );

    let every = world.ok("code_edit_file", edit("one", "1", true));
    assert_eq!(every["replacements"], 2);
    assert_eq!(text("notes/deep/a.txt"), "1\n2\n1\n");
    for (args, why) in [
        (edit("", "x", false), "`old_text` is empty"),
        (edit("1", "1", false), "are the same"),
        (edit("2", &"y".repeat(70), false), "max_write_bytes"),
        (
            json!({ "path": "nope.txt", "old_text": "a", "new_text": "b" }),
            "there is no `nope.txt`",
        ),
        (
            json!({ "path": "notes", "old_text": "a", "new_text": "b" }),
            "is a folder",
        ),
    ] {
        let said = world.refused("code_edit_file", args.clone());
        assert!(said.contains(why), "{args}: {said}");
    }
    assert_eq!(text("notes/deep/a.txt"), "1\n2\n1\n");
    // A file without write permission stays as it is, though its folder would allow it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        put(&root, "frozen.txt", "keep me\n");
        fs::set_permissions(root.join("frozen.txt"), fs::Permissions::from_mode(0o444)).unwrap();
        for said in [
            world.refused(
                "code_write_file",
                json!({ "path": "frozen.txt", "content": "x" }),
            ),
            world.refused(
                "code_edit_file",
                json!({ "path": "frozen.txt", "old_text": "keep", "new_text": "lose" }),
            ),
        ] {
            assert!(said.contains("is read-only"), "{said}");
        }
        assert_eq!(text("frozen.txt"), "keep me\n");
    }
    let leftovers: Vec<String> = fs::read_dir(root.join("notes/deep"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(leftovers, ["a.txt"], "no temporary file is left behind");

    // Each change that landed left on `changed`, in the caller's name; a refused one did not.
    let changed = world.emitted("changed", 5);
    assert_eq!(changed.len(), 5, "{changed:?}");
    assert_eq!(
        changed[0],
        json!({ "tool": "code_write_file", "path": "notes/deep/a.txt", "bytes": 12,
                "created": true, "by": "tester" })
    );
    assert_eq!(
        changed[4],
        json!({ "tool": "code_edit_file", "path": "notes/deep/a.txt", "bytes": 6,
                "created": false, "by": "tester" })
    );
}

#[cfg(unix)]
#[test]
fn a_command_runs_in_the_root_bounded_and_leaves_nothing_running() {
    std::env::set_var("REGULUS_CODE_IT_SECRET", "leaked");
    let world = World::start(
        "commands",
        json!({ "max_output_bytes": 400, "command_timeout_ms": 400,
                "max_command_timeout_ms": 900, "kill_grace_ms": 100, "pipe_grace_ms": 200,
                "env": { "GREETING": "hola" }, "env_passthrough": ["PATH"] }),
    );
    let root = world.root.clone();
    fs::create_dir_all(root.join("sub")).unwrap();
    let run = |args: Value| world.call_on("ws", "code_run_command", args);

    let failed = run(json!({ "command": "echo out; echo err >&2; exit 3" }));
    assert!(failed.error, "a command that does not exit with 0 failed");
    assert_eq!(failed.data["code"], 3);
    assert_eq!(failed.data["signal"], Value::Null);
    assert_eq!(
        (
            failed.data["stdout"].as_str(),
            failed.data["stderr"].as_str()
        ),
        (Some("out\n"), Some("err\n"))
    );
    assert_eq!(
        (
            failed.data["timed_out"].as_bool(),
            failed.data["truncated"].as_bool()
        ),
        (Some(false), Some(false))
    );

    let here = run(json!({ "command": "pwd; echo \"$PWD\"" }));
    assert!(!here.error, "{}", here.data);
    assert_eq!(here.data["stdout"], format!("{0}\n{0}\n", root.display()));
    let there = run(json!({ "command": "pwd", "cwd": "sub" }));
    assert_eq!(
        there.data["stdout"],
        format!("{}\n", root.join("sub").display())
    );
    assert_eq!(there.data["cwd"], "sub");

    // Only what the workspace gives: not the core's environment, not even HOME here.
    let env = run(json!({ "command": "echo \"[$REGULUS_CODE_IT_SECRET][$GREETING][$HOME]\"" }));
    assert_eq!(env.data["stdout"], "[][hola][]\n");

    // A long output keeps its start and its end.
    let long = run(json!({
        "command": "i=0; while [ $i -lt 200 ]; do echo line$i; i=$((i+1)); done" }));
    let kept = long.data["stdout"].as_str().unwrap();
    assert!(kept.starts_with("line0\nline1\n"), "{kept}");
    assert!(kept.ends_with("line198\nline199\n"), "{kept}");
    assert!(kept.contains("bytes cut"), "{kept}");
    assert_eq!(long.data["truncated"], true);

    // Past its time the whole group goes, the background process included.
    let began = Instant::now();
    let late = run(json!({ "command": "sleep 31 & echo $!; sleep 30" }));
    assert!(late.error);
    assert_eq!(late.data["timed_out"], true);
    assert_eq!(late.data["code"], Value::Null);
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );
    let background = late.data["stdout"].as_str().unwrap().trim().to_string();
    assert!(
        ended(&background),
        "the background process {background} survived the timeout"
    );

    // A call cannot ask for more time than the workspace allows.
    let began = Instant::now();
    let capped = run(json!({ "command": "sleep 30", "timeout_ms": 60000 }));
    assert_eq!(capped.data["timed_out"], true);
    assert!(
        began.elapsed() < Duration::from_secs(5),
        "{:?}",
        began.elapsed()
    );

    // What a command leaves in the background ends with it, even when it ended well.
    let began = Instant::now();
    let left = run(json!({ "command": "sleep 32 & echo $!" }));
    assert!(!left.error, "{}", left.data);
    assert!(
        began.elapsed() < Duration::from_secs(3),
        "{:?}",
        began.elapsed()
    );
    let orphan = left.data["stdout"].as_str().unwrap().trim().to_string();
    assert!(
        ended(&orphan),
        "the background process {orphan} was left running"
    );

    for (args, why) in [
        (json!({}), "`command` is required"),
        (json!({ "command": "  " }), "`command` is empty"),
        (
            json!({ "command": "pwd", "cwd": "nope" }),
            "`cwd` is not a folder",
        ),
        (
            json!({ "command": "pwd", "timeout_ms": "soon" }),
            "not a whole number",
        ),
    ] {
        let said = world.refused("code_run_command", args.clone());
        assert!(said.contains(why), "{args}: {said}");
    }

    // Each command that ran left on `ran`, without what it printed.
    let ran = world.emitted("ran", 8);
    assert_eq!(ran.len(), 8, "{ran:?}");
    assert_eq!(ran[0]["command"], "echo out; echo err >&2; exit 3");
    assert_eq!(
        (ran[0]["code"].as_i64(), ran[0]["by"].as_str()),
        (Some(3), Some("tester"))
    );
    assert_eq!(ran[2]["cwd"], "sub");
    assert!(ran
        .iter()
        .all(|r| r.get("stdout").is_none() && r.get("stderr").is_none()));
}

#[cfg(unix)]
#[test]
fn a_workspace_runs_only_as_many_commands_as_it_may_and_says_when_its_shell_is_missing() {
    let world = World::start("running", json!({ "max_running": 1, "pipe_grace_ms": 100 }));
    std::thread::scope(|scope| {
        let first = scope.spawn(|| {
            world.call_on(
                "ws",
                "code_run_command",
                json!({ "command": "touch started; sleep 1" }),
            )
        });
        let deadline = Instant::now() + WAIT;
        while !world.root.join("started").exists() {
            assert!(Instant::now() < deadline, "the first command never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        let second = world.refused("code_run_command", json!({ "command": "true" }));
        assert!(second.contains("already runs 1"), "{second}");
        assert!(!first.join().unwrap().error);
    });
    world.ok("code_run_command", json!({ "command": "true" }));

    let lost = World::start(
        "no-shell",
        json!({ "shell": ["regulus-code-no-such-shell", "-c"] }),
    );
    let said = lost.refused("code_run_command", json!({ "command": "true" }));
    assert!(said.contains("could not be started"), "{said}");
}

#[cfg(unix)]
#[test]
fn stopping_the_execution_ends_a_command_in_flight() {
    let world = World::start("stop", json!({ "command_timeout_ms": 60000 }));
    std::thread::scope(|scope| {
        let running = scope.spawn(|| {
            world.call_on(
                "ws",
                "code_run_command",
                json!({ "command": "sleep 33 & echo $! > pid; sleep 30" }),
            )
        });
        let deadline = Instant::now() + WAIT;
        while fs::read_to_string(world.root.join("pid")).map_or(true, |pid| pid.trim().is_empty()) {
            assert!(Instant::now() < deadline, "the command never started");
            std::thread::sleep(Duration::from_millis(10));
        }
        let began = Instant::now();
        world
            .core
            .dispatch(method::EXECUTION_STOP, json!({ "id": world.id }))
            .unwrap();
        let outcome = running.join().unwrap();
        assert!(
            began.elapsed() < Duration::from_secs(5),
            "{:?}",
            began.elapsed()
        );
        assert!(outcome.error);
        assert!(
            outcome
                .data
                .as_str()
                .is_some_and(|said| said.contains("the workspace stopped")),
            "{}",
            outcome.data
        );
        let pid = fs::read_to_string(world.root.join("pid")).unwrap();
        assert!(
            ended(pid.trim()),
            "the background process outlived the execution"
        );
    });
    // A stopped workspace runs nothing more, and is no longer there to inspect.
    let after = world.call_on("ws", "code_run_command", json!({ "command": "true" }));
    assert!(after.error && after.data.as_str().unwrap().contains("is not running"));
    let (code, _) = world
        .inspect("ws", json!({ "action": "summary" }))
        .unwrap_err();
    assert_eq!(code, INVALID_PARAMS);
}

#[test]
fn a_search_that_does_not_look_everywhere_says_so() {
    let world = World::start(
        "search-bounds",
        json!({ "max_search_depth": 1, "max_folder_entries": 3 }),
    );
    put(&world.root, "top.txt", "a needle\n");
    put(&world.root, "a/one.txt", "a needle\n");
    put(&world.root, "a/b/two.txt", "a needle\n");
    // One level down and no further: what lies deeper is not looked at, and it is told.
    let tool = world.ok("code_search", json!({ "query": "needle" }));
    assert_eq!(
        hits(&tool),
        [("a/one.txt".to_string(), 1), ("top.txt".to_string(), 1)]
    );
    assert_eq!(tool["truncated"], true);
    let look = world.look(json!({ "action": "search", "query": "NEEDLE" }));
    assert_eq!(hits(&look), hits(&tool));
    assert_eq!(look["truncated"], true);
    // Asked for by path, the deeper folder is where the search starts.
    let deeper = world.ok("code_search", json!({ "query": "needle", "path": "a/b" }));
    assert_eq!(hits(&deeper), [("a/b/two.txt".to_string(), 1)]);
    assert_eq!(deeper["truncated"], false);

    // A folder with more entries than are looked at is listed in part, and says so.
    for n in 0..6 {
        put(&world.root, &format!("many/f{n}.txt"), "x\n");
    }
    let listed = world.ok("code_list_files", json!({ "path": "many" }));
    assert_eq!(listed["entries"].as_array().unwrap().len(), 3);
    assert_eq!(listed["truncated"], true);
    let looked = world.look(json!({ "action": "list", "path": "many" }));
    assert_eq!(looked["entries"].as_array().unwrap().len(), 3);
    assert_eq!(looked["truncated"], true);
}

#[test]
fn edits_at_the_same_time_all_land() {
    const LINES: usize = 12;
    const ROUNDS: usize = 25;
    let world = World::start("concurrent", json!({}));
    let file = world.root.join("shared.txt");
    for round in 0..ROUNDS {
        let before: String = (0..LINES).map(|n| format!("line {n}: old\n")).collect();
        fs::write(&file, before).unwrap();
        let gate = std::sync::Barrier::new(LINES);
        std::thread::scope(|scope| {
            for n in 0..LINES {
                let (world, gate) = (&world, &gate);
                scope.spawn(move || {
                    // Half through the workspace, half through a view of it: one folder.
                    let node = if n % 2 == 0 { "ws" } else { "rw" };
                    gate.wait();
                    let outcome = world.call_on(
                        node,
                        "code_edit_file",
                        json!({ "path": "shared.txt", "old_text": format!("line {n}: old"),
                                "new_text": format!("line {n}: new") }),
                    );
                    assert!(!outcome.error, "round {round}, line {n}: {}", outcome.data);
                });
            }
        });
        let after: String = (0..LINES).map(|n| format!("line {n}: new\n")).collect();
        assert_eq!(
            fs::read_to_string(&file).unwrap(),
            after,
            "round {round}: an edit was lost"
        );
    }
    // Every one of them left on `changed`.
    assert_eq!(
        world.emitted("changed", LINES * ROUNDS).len(),
        LINES * ROUNDS
    );
}

/// Runs the real git for a test's own setup, with no configuration but the repository's.
fn git(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .current_dir(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn has_git() -> bool {
    let found = Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success());
    if !found {
        eprintln!("skipped: git is not installed");
    }
    found
}

/// A repository on `main` with one commit, then: `a.txt` modified, `sub/old.txt` renamed,
/// `staged.txt` added and `sub/fresh.txt` untracked.
fn repository(home: &Path) -> PathBuf {
    let repo = home.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    put(&repo, "a.txt", "one\n");
    put(&repo, "sub/b.txt", "two\n");
    put(&repo, "sub/old.txt", "a file that will be renamed\n");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    put(&repo, "a.txt", "one\nmore\n");
    git(&repo, &["mv", "sub/old.txt", "sub/new.txt"]);
    put(&repo, "staged.txt", "staged\n");
    git(&repo, &["add", "staged.txt"]);
    put(&repo, "sub/fresh.txt", "untracked\n");
    repo
}

/// A workspace over `root`, a folder of `home`, whose git sees no configuration of the
/// user's (only `PATH` is inherited).
fn git_world(home: PathBuf, root: PathBuf, mut config: Value) -> World {
    config["env_passthrough"] = json!(["PATH"]);
    World::over(home, root, config, json!({}))
}

fn sorted_lines(status: &Value) -> Vec<String> {
    let mut lines: Vec<String> = status.as_str().unwrap().lines().map(String::from).collect();
    lines.sort();
    lines
}

#[test]
fn git_status_and_diff_are_those_of_the_root() {
    if !has_git() {
        return;
    }
    let home = temp("git");
    let repo = repository(&home);
    let world = git_world(home.clone(), repo.clone(), json!({}));

    let status = world.ok("code_git_status", json!({}));
    assert_eq!(status["repository"], true);
    assert_eq!(status["branch"], "main");
    assert_eq!(status["head"].as_str().unwrap().len(), 40);
    assert_eq!(
        sorted_lines(&status["status"]),
        [
            " M a.txt",
            "?? sub/fresh.txt",
            "A  staged.txt",
            "R  sub/old.txt -> sub/new.txt"
        ]
    );
    let diff = world.ok("code_git_diff", json!({}));
    let text = diff["text"].as_str().unwrap();
    assert!(
        text.contains("diff --git a/a.txt b/a.txt") && text.contains("+more"),
        "{text}"
    );
    assert!(
        text.contains("b/staged.txt") && text.contains("+staged"),
        "staged changes too"
    );
    assert!(text.contains("rename from sub/old.txt"), "{text}");
    assert!(
        !text.contains("fresh.txt"),
        "untracked files are in the status, not the diff"
    );
    let one = world.ok("code_git_diff", json!({ "path": "a.txt" }));
    let one = one["text"].as_str().unwrap();
    assert!(
        one.contains("+more") && !one.contains("staged.txt"),
        "{one}"
    );

    let summary = world.look(json!({ "action": "summary" }));
    assert_eq!(summary["root"], repo.display().to_string());
    assert_eq!(summary["git"]["branch"], "main");
    assert_eq!(summary["git"]["status"], status["status"]);
    assert_eq!(summary["git"]["truncated"], false);
    assert_eq!(summary["git"]["changes"].as_array().unwrap().len(), 4);
    assert!(summary["git"]["changes"]
        .as_array()
        .unwrap()
        .contains(&json!({ "path": "sub/new.txt", "code": "R ", "from": "sub/old.txt" })));
    assert!(summary.get("git_reason").is_none());
    assert_eq!(summary["tools"].as_array().unwrap().len(), 8);
    let whole = world.look(json!({ "action": "diff" }));
    assert_eq!(
        whole,
        json!({ "root": repo.display().to_string(), "text": text, "truncated": false })
    );
    let scoped = world.look(json!({ "action": "diff", "path": "sub" }));
    let scoped = scoped["text"].as_str().unwrap();
    assert!(
        scoped.contains("rename to sub/new.txt") && !scoped.contains("a.txt"),
        "{scoped}"
    );
    drop(world);

    // A root that is a folder of the repository: its own subtree, in its own paths.
    let home = temp("git-sub");
    let repo = repository(&home);
    let inner = git_world(home, repo.join("sub"), json!({}));
    let status = inner.ok("code_git_status", json!({}));
    assert_eq!(
        sorted_lines(&status["status"]),
        ["?? fresh.txt", "R  old.txt -> new.txt"]
    );
    let diff = inner.ok("code_git_diff", json!({}));
    let text = diff["text"].as_str().unwrap();
    assert!(
        text.contains("rename from old.txt") && !text.contains("a.txt"),
        "{text}"
    );
    drop(inner);

    // A diff past the limit is cut and says so.
    let home = temp("git-cut");
    let repo = repository(&home);
    let small = git_world(
        home,
        repo,
        json!({ "max_diff_bytes": 60, "inspect_max_bytes": 80 }),
    );
    let cut = small.ok("code_git_diff", json!({}));
    assert_eq!(
        (
            cut["text"].as_str().unwrap().len(),
            cut["truncated"].as_bool()
        ),
        (60, Some(true))
    );
    let cut = small.look(json!({ "action": "diff" }));
    assert_eq!(
        (
            cut["text"].as_str().unwrap().len(),
            cut["truncated"].as_bool()
        ),
        (80, Some(true))
    );
}

#[test]
fn a_new_a_detached_and_a_missing_repository_are_told_apart() {
    if !has_git() {
        return;
    }
    // Before the first commit: a branch, no commit, and everything added is new.
    let home = temp("git-unborn");
    let repo = home.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    put(&repo, "first.txt", "first\n");
    git(&repo, &["add", "first.txt"]);
    let unborn = git_world(home, repo, json!({}));
    let status = unborn.ok("code_git_status", json!({}));
    assert_eq!(
        (status["branch"].as_str(), status["head"].is_null()),
        (Some("main"), true)
    );
    assert_eq!(status["status"], "A  first.txt\n");
    let diff = unborn.ok("code_git_diff", json!({}));
    assert!(diff["text"].as_str().unwrap().contains("+first"), "{diff}");
    drop(unborn);

    // A detached HEAD has a commit and no branch.
    let home = temp("git-detached");
    let repo = repository(&home);
    git(&repo, &["checkout", "-q", "--detach"]);
    let detached = git_world(home, repo, json!({}));
    let summary = detached.look(json!({ "action": "summary" }));
    assert!(summary["git"]["branch"].is_null(), "{summary}");
    assert_eq!(summary["git"]["head"].as_str().unwrap().len(), 40);
    drop(detached);

    // A folder that is no repository: git is `null`, with why, and that is no error.
    let plain = World::start("git-none", json!({ "env_passthrough": ["PATH"] }));
    put(&plain.root, "a.txt", "one\n");
    let root = plain.root.display().to_string();
    assert_eq!(
        plain.ok("code_git_status", json!({})),
        json!({ "repository": false, "reason": "not a git repository" })
    );
    assert_eq!(
        plain.ok("code_git_diff", json!({})),
        json!({ "repository": false, "reason": "not a git repository" })
    );
    let summary = plain.look(json!({ "action": "summary" }));
    assert_eq!(summary["git"], Value::Null);
    assert_eq!(summary["git_reason"], "not a git repository");
    assert_eq!(
        plain.look(json!({ "action": "diff" })),
        json!({ "root": root, "text": "", "truncated": false,
                "git_reason": "not a git repository" })
    );
    drop(plain);

    // No git at all is the same answer, with its own why.
    let none = World::start("git-missing", json!({ "git": "regulus-code-no-such-git" }));
    let summary = none.look(json!({ "action": "summary" }));
    assert_eq!(summary["git"], Value::Null);
    assert!(
        summary["git_reason"]
            .as_str()
            .unwrap()
            .contains("is not installed"),
        "{summary}"
    );
}

#[cfg(unix)]
#[test]
fn git_runs_nothing_a_repository_configures() {
    if !has_git() {
        return;
    }
    let home = temp("git-quiet");
    let repo = home.join("repo");
    let marks = home.join("marks");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(&marks).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    put(&repo, "a.txt", "hello\n");
    put(&repo, ".gitattributes", "*.txt filter=evil diff=evil\n");
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "init"]);
    // Everything a configuration can make git run, each leaving a mark if it does.
    let mark = |name: &str| format!("touch {}/{name}", marks.display());
    for (key, value) in [
        ("core.fsmonitor", format!("{}; false", mark("fsmonitor"))),
        ("filter.evil.clean", format!("{}; cat", mark("clean"))),
        ("filter.evil.smudge", format!("{}; cat", mark("smudge"))),
        ("filter.evil.process", format!("{}; false", mark("process"))),
        ("diff.evil.textconv", format!("{}; cat", mark("textconv"))),
        (
            "diff.evil.command",
            format!("{}; false", mark("diffcommand")),
        ),
        ("diff.external", format!("{}; false", mark("external"))),
        ("core.pager", format!("{}; cat", mark("pager"))),
        ("pager.status", format!("{}; cat", mark("pagerstatus"))),
        ("pager.diff", format!("{}; cat", mark("pagerdiff"))),
    ] {
        git(&repo, &["config", key, &value]);
    }
    put(
        &repo,
        ".git/hooks/post-index-change",
        format!("#!/bin/sh\n{}\n", mark("hook")),
    );
    {
        use std::os::unix::fs::PermissionsExt;
        let hook = repo.join(".git/hooks/post-index-change");
        fs::set_permissions(hook, fs::Permissions::from_mode(0o755)).unwrap();
    }
    put(&repo, "a.txt", "changed\n");
    let marked = || -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&marks)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    };

    // The trap works: a plain git in this repository runs what it was told to.
    let _ = Command::new("git")
        .current_dir(&repo)
        .args(["status", "--porcelain"])
        .output();
    let _ = Command::new("git")
        .current_dir(&repo)
        .args(["diff"])
        .output();
    assert!(
        !marked().is_empty(),
        "the configured programs should run under a plain git"
    );
    let forget = || {
        for name in marked() {
            fs::remove_file(marks.join(name)).unwrap();
        }
    };
    forget();
    let index = fs::read(repo.join(".git/index")).unwrap();

    // The module's git reads the same repository and runs none of it.
    let world = git_world(home.clone(), repo.clone(), json!({})).sharing();
    let summary = world.look(json!({ "action": "summary" }));
    assert_eq!(summary["git"]["status"], " M a.txt\n", "{summary}");
    let diff = world.look(json!({ "action": "diff" }));
    assert!(
        diff["text"].as_str().unwrap().contains("+changed"),
        "{diff}"
    );
    assert_eq!(
        world.ok("code_git_status", json!({}))["status"],
        " M a.txt\n"
    );
    assert!(world.ok("code_git_diff", json!({}))["text"]
        .as_str()
        .unwrap()
        .contains("-hello"));
    assert_eq!(
        marked(),
        Vec::<String>::new(),
        "git ran something the repository configured"
    );
    assert_eq!(
        fs::read(repo.join(".git/index")).unwrap(),
        index,
        "the index was rewritten"
    );
    drop(world);

    // A repository that says its files are somewhere else is not read at all.
    let elsewhere = home.join("elsewhere");
    put(&elsewhere, "private.txt", "not yours\n");
    git(
        &repo,
        &["config", "core.worktree", elsewhere.to_str().unwrap()],
    );
    let moved = git_world(home.clone(), repo.clone(), json!({})).sharing();
    let summary = moved.look(json!({ "action": "summary" }));
    assert_eq!(summary["git"], Value::Null, "{summary}");
    assert!(
        summary["git_reason"]
            .as_str()
            .unwrap()
            .contains("work tree"),
        "{summary}"
    );
    let diff = moved.look(json!({ "action": "diff" }));
    assert_eq!(diff["text"], "");
    assert_eq!(marked(), Vec::<String>::new());
    drop(moved);
    git(&repo, &["config", "--unset", "core.worktree"]);

    // `git_dir = inside`: a repository kept outside the root (a linked worktree) is refused.
    let linked = home.join("linked");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            linked.to_str().unwrap(),
            "-b",
            "side",
        ],
    );
    // The test's own git ran what the repository configures: start counting again.
    forget();
    let open = git_world(home.clone(), linked.clone(), json!({})).sharing();
    assert_eq!(
        open.look(json!({ "action": "summary" }))["git"]["branch"],
        "side"
    );
    drop(open);
    let closed = git_world(home.clone(), linked, json!({ "git_dir": "inside" }));
    let summary = closed.look(json!({ "action": "summary" }));
    assert_eq!(summary["git"], Value::Null);
    assert!(
        summary["git_reason"]
            .as_str()
            .unwrap()
            .contains("outside the workspace"),
        "{summary}"
    );
    assert_eq!(marked(), Vec::<String>::new());
}

#[test]
fn inspect_answers_the_shapes_of_the_contract_and_says_what_is_wrong() {
    let world = World::start("inspect", json!({ "env_passthrough": ["PATH"] }));
    put(&world.root, "README.md", "# Demo\nA demo project.\n");
    put(&world.root, "src/main.rs", MAIN_RS);
    put(&world.root, "target/out.txt", "demo output\n");
    put(&world.root, ".git/config", "[core]\n");
    put(&world.root, "bin.dat", [0u8, 1, 2]);
    let root = world.root.display().to_string();

    let summary = world.look(json!({ "action": "summary" }));
    assert_eq!(summary["root"], root);
    assert_eq!(
        summary["tools"],
        json!([
            "code_list_files",
            "code_read_file",
            "code_search",
            "code_git_status",
            "code_git_diff",
            "code_write_file",
            "code_edit_file",
            "code_run_command"
        ])
    );
    assert_eq!(
        world.look(json!({ "action": "list" })),
        json!({ "root": root, "path": "", "truncated": false, "entries": [
            { "path": "README.md", "kind": "file", "size": 23 },
            { "path": "bin.dat", "kind": "file", "size": 3 },
            { "path": "src", "kind": "directory" },
            { "path": "target", "kind": "directory" },
        ] }),
        "direct children only, and never `.git`"
    );
    assert_eq!(
        world.look(json!({ "action": "list", "path": "src/" })),
        json!({ "root": root, "path": "src", "truncated": false, "entries": [
            { "path": "src/main.rs", "kind": "file", "size": MAIN_RS.len() } ] })
    );
    assert_eq!(
        world.look(json!({ "action": "read", "path": "src/main.rs" })),
        json!({ "root": root, "path": "src/main.rs", "text": MAIN_RS, "truncated": false,
                "size": MAIN_RS.len() })
    );
    // The text as typed, whatever its case; the build folder is passed by.
    assert_eq!(
        world.look(json!({ "action": "search", "query": "DEMO" })),
        json!({ "root": root, "truncated": false, "matches": [
            { "path": "README.md", "line": 1, "text": "# Demo" },
            { "path": "README.md", "line": 2, "text": "A demo project." } ] })
    );
    assert_eq!(
        world.look(json!({ "action": "search", "query": "demo", "path": "target" }))["matches"],
        json!([{ "path": "target/out.txt", "line": 1, "text": "demo output" }])
    );
    assert_eq!(
        world.look(json!({ "action": "search", "query": "fn main()", "path": "src" }))["matches"]
            [0]["line"],
        1,
        "the query is the text itself, not a pattern"
    );

    // A bad request is the caller's (-32602) and says what is wrong with it.
    let wrong = |node: &str, look: Value| {
        let (code, message) = world.inspect(node, look.clone()).unwrap_err();
        assert_eq!(code, INVALID_PARAMS, "{look}: {message}");
        message
    };
    assert!(wrong("ws", json!({ "action": "delete" })).contains("unknown action `delete`"));
    assert!(wrong("ws", json!({ "action": "read" })).contains("`path` is required"));
    assert!(wrong("ws", json!({ "action": "search" })).contains("`query` is required"));
    assert!(wrong("ws", json!({ "action": "search", "query": "" })).contains("`query`"));
    assert!(wrong("ws", json!({})).contains("action"));
    for (look, why) in [
        (
            json!({ "action": "read", "path": "../x" }),
            "outside the workspace",
        ),
        (
            json!({ "action": "read", "path": "/etc/passwd" }),
            "outside the workspace",
        ),
        (
            json!({ "action": "read", "path": ".git/config" }),
            "protected",
        ),
        (
            json!({ "action": "read", "path": "nope" }),
            "there is no `nope`",
        ),
        (json!({ "action": "read", "path": "src" }), "is a folder"),
        (
            json!({ "action": "read", "path": "bin.dat" }),
            "not a text file",
        ),
        (
            json!({ "action": "list", "path": "README.md" }),
            "not a folder",
        ),
        (
            json!({ "action": "list", "path": ".." }),
            "outside the workspace",
        ),
        (
            json!({ "action": "search", "query": "x", "path": "../.." }),
            "outside the workspace",
        ),
        (
            json!({ "action": "diff", "path": "../x" }),
            "outside the workspace",
        ),
    ] {
        let said = wrong("ws", look.clone());
        assert!(said.contains(why), "{look}: {said}");
    }
    // Only a live workspace node of that execution can be looked at.
    assert!(wrong("ro", json!({ "action": "summary" })).contains("no code workspace `ro`"));
    assert!(wrong("nope", json!({ "action": "summary" })).contains("no code workspace"));
    let other = world
        .core
        .dispatch(
            contract::INSPECT,
            json!({ "execution_id": "exec-999", "node_id": "ws", "action": "summary" }),
        )
        .unwrap_err();
    assert_eq!(other.code, INVALID_PARAMS);

    // The same look is what a wired box gets over `code.workspace@1`.
    let over_wire = world
        .on_node(
            "ws",
            contract::LOOK,
            json!({ "action": "read", "path": "README.md" }),
        )
        .unwrap();
    assert_eq!(over_wire["text"], "# Demo\nA demo project.\n");

    // A workspace that broke (its folder is gone) is the module's failure (-32003).
    fs::remove_dir_all(&world.root).unwrap();
    let (code, message) = world
        .inspect("ws", json!({ "action": "list" }))
        .unwrap_err();
    assert_eq!(code, MODULE_FAILED, "{message}");
    assert!(message.contains("cannot be opened"), "{message}");
}

#[test]
fn inspect_cuts_at_its_own_limits_not_at_the_tools() {
    let world = World::start(
        "inspect-limits",
        json!({ "max_read_bytes": 4, "max_list_entries": 1, "max_search_matches": 1,
                "inspect_max_bytes": 9, "inspect_max_entries": 2 }),
    );
    put(
        &world.root,
        "a.txt",
        "héllo wörld\nsecond héllo\nthird héllo\n",
    );
    put(&world.root, "b.txt", "b\n");
    put(&world.root, "c.txt", "c\n");

    let read = world.look(json!({ "action": "read", "path": "a.txt" }));
    // Nine bytes end inside `ö`: the text stops before it rather than break the character.
    assert_eq!(read["text"], "héllo w");
    assert_eq!(
        (read["truncated"].as_bool(), read["size"].as_u64()),
        (Some(true), Some(41))
    );
    let listed = world.look(json!({ "action": "list" }));
    assert_eq!(paths(&listed["entries"]), ["a.txt", "b.txt"]);
    assert_eq!(listed["truncated"], true);
    let found = world.look(json!({ "action": "search", "query": "héllo" }));
    assert_eq!(
        hits(&found),
        [("a.txt".to_string(), 1), ("a.txt".to_string(), 2)]
    );
    assert_eq!(found["truncated"], true);
    // The tools keep their own, smaller, limits.
    assert_eq!(
        world.ok("code_list_files", json!({}))["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        world.ok("code_search", json!({ "query": "héllo" }))["matches"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn a_view_offers_part_of_a_workspace_and_can_only_narrow_it() {
    let world = World::start("views", json!({}));
    put(&world.root, "a.txt", "one\n");
    let names = |node: &str| world.tools_of(node);
    assert_eq!(names("ws"), contract::tool_name::ALL);
    assert_eq!(names("ro"), READS);
    assert_eq!(
        names("rw"),
        [&READS[..], &["code_write_file", "code_edit_file"]].concat()
    );
    assert_eq!(
        names("all"),
        ["code_read_file", "code_run_command"],
        "only the named ones"
    );

    let read = world.call_on("ro", "code_read_file", json!({ "path": "a.txt" }));
    assert_eq!(
        (read.error, read.data["text"].as_str()),
        (false, Some("one\n"))
    );
    for (node, name, args) in [
        (
            "ro",
            "code_write_file",
            json!({ "path": "b.txt", "content": "x" }),
        ),
        (
            "ro",
            "code_edit_file",
            json!({ "path": "a.txt", "old_text": "one", "new_text": "1" }),
        ),
        (
            "ro",
            "code_run_command",
            json!({ "command": "touch c.txt" }),
        ),
        (
            "rw",
            "code_run_command",
            json!({ "command": "touch c.txt" }),
        ),
        (
            "all",
            "code_write_file",
            json!({ "path": "b.txt", "content": "x" }),
        ),
        ("ro", "code_fly", json!({})),
    ] {
        let outcome = world.call_on(node, name, args);
        assert!(outcome.error, "{node} ran {name}");
        assert_eq!(
            outcome.data,
            json!(format!("there is no tool `{name}` here"))
        );
    }
    assert_eq!(
        fs::read_to_string(world.root.join("a.txt")).unwrap(),
        "one\n"
    );
    assert!(!world.root.join("b.txt").exists() && !world.root.join("c.txt").exists());
    let written = world.call_on(
        "rw",
        "code_write_file",
        json!({ "path": "b.txt", "content": "x" }),
    );
    assert!(!written.error, "{}", written.data);
    #[cfg(unix)]
    {
        let ran = world.call_on(
            "all",
            "code_run_command",
            json!({ "command": "touch c.txt" }),
        );
        assert!(
            !ran.error && world.root.join("c.txt").exists(),
            "{}",
            ran.data
        );
    }
    // What a view changes is the workspace's change.
    assert_eq!(world.emitted("changed", 1)[0]["path"], "b.txt");
    assert!(world
        .call_on("ws", "code_fly", json!({}))
        .data
        .as_str()
        .unwrap()
        .contains("no tool `code_fly`"));
    drop(world);

    // An operation the workspace does not have is offered by no view of it.
    let narrow = World::start(
        "views-narrow",
        json!({ "tools": ["code_read_file", "code_write_file"] }),
    );
    assert_eq!(narrow.tools_of("ws"), ["code_read_file", "code_write_file"]);
    assert_eq!(narrow.tools_of("ro"), ["code_read_file"]);
    assert_eq!(narrow.tools_of("rw"), ["code_read_file", "code_write_file"]);
    assert_eq!(narrow.tools_of("all"), ["code_read_file"]);
    let run = narrow.call_on("all", "code_run_command", json!({ "command": "true" }));
    assert!(run.error);
    let direct = narrow.call_on("ws", "code_run_command", json!({ "command": "true" }));
    assert!(
        direct.error && direct.data.as_str().unwrap().contains("no tool"),
        "{}",
        direct.data
    );
    assert_eq!(
        narrow.look(json!({ "action": "summary" }))["tools"],
        json!(["code_read_file", "code_write_file"])
    );
}

fn hook(world: &World, event: &str) -> Value {
    world
        .on_node("docs", method::NODE_HOOK, json!({ "event": event }))
        .unwrap()
}

#[test]
fn the_repository_s_instructions_reach_an_agent_as_context() {
    let home = temp("instructions");
    let root = home.join("root");
    put(&root, "AGENTS.md", "# Rules\nAlways run the tests.\n\n");
    put(&root, "docs/STYLE.md", "Tabs, never spaces.\n");
    put(&home, "outside.md", "do not read me\n");
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        // CLAUDE.md is AGENTS.md under another name; LEAK.md points out of the root.
        symlink(root.join("AGENTS.md"), root.join("CLAUDE.md")).unwrap();
        symlink(home.join("outside.md"), root.join("LEAK.md")).unwrap();
    }
    let files = json!([
        "AGENTS.md",
        "CLAUDE.md",
        "MISSING.md",
        "docs",
        "LEAK.md",
        "../outside.md",
        "docs/STYLE.md"
    ]);
    let world = World::over(
        home.clone(),
        root.clone(),
        json!({}),
        json!({ "files": files }),
    );

    let described = world.on_node("docs", "hook", Value::Null).unwrap();
    assert_eq!(
        described,
        json!({ "event": "session_start", "returns": ["context"] })
    );
    assert_eq!(
        hook(&world, "session_start"),
        json!({ "decision": "context", "text":
            "Instructions from the repository file AGENTS.md:\n# Rules\nAlways run the tests.\n\n\
             Instructions from the repository file docs/STYLE.md:\nTabs, never spaces." }),
        "each file once, in order; what is missing, a folder or outside the root is not one"
    );
    assert_eq!(
        hook(&world, "prompt"),
        json!({ "decision": "none" }),
        "only on its event"
    );
    // Only the instructions node is a hook.
    assert!(world
        .on_node("ws", "hook", Value::Null)
        .unwrap_err()
        .contains("unknown method"));
    drop(world);

    // Its own event, header and limit; and nothing to say is nothing.
    let home = temp("instructions-cut");
    let root = home.join("root");
    put(&root, "AGENTS.md", "0123456789".repeat(10));
    let cut = World::over(
        home,
        root,
        json!({}),
        json!({ "event": "prompt", "header": "From {{path}}", "max_chars": 20 }),
    );
    assert_eq!(
        cut.on_node("docs", "hook", Value::Null).unwrap()["event"],
        "prompt"
    );
    assert_eq!(
        hook(&cut, "prompt"),
        json!({ "decision": "context", "text": "From AGENTS.md\n01234\n[cut]" })
    );
    assert_eq!(hook(&cut, "session_start"), json!({ "decision": "none" }));
    drop(cut);
    let empty = World::start("instructions-none", json!({}));
    assert_eq!(hook(&empty, "session_start"), json!({ "decision": "none" }));
}

#[test]
fn a_folder_on_root_moves_the_workspace() {
    let world = World::start("moves", json!({}));
    put(&world.root, "first.txt", "first\n");
    let other = world.home.join("other");
    put(&other, "second.txt", "second\n");
    let listed = || {
        let outcome = world.call_on("ws", "code_list_files", json!({}));
        (outcome.error, outcome.data)
    };
    assert_eq!(paths(&listed().1["entries"]), ["first.txt"]);

    // A path as text, or the `info` of a workspace of the workspace module.
    world.send("root", json!(other));
    assert_eq!(world.delivery_failures(), Vec::<String>::new());
    assert_eq!(paths(&listed().1["entries"]), ["second.txt"]);
    assert_eq!(
        world.look(json!({ "action": "summary" }))["root"],
        other.display().to_string()
    );
    world.send("root", json!(""));
    world.delivery_failures();
    assert_eq!(
        paths(&listed().1["entries"]),
        ["first.txt"],
        "empty: back to the field"
    );
    world.send("root", json!({ "id": "exec-1", "path": other }));
    world.delivery_failures();
    assert_eq!(paths(&listed().1["entries"]), ["second.txt"]);

    // A folder that is not there is refused on arrival and kept: what follows fails here
    // instead of going on in the folder before.
    world.send("root", json!(world.home.join("missing")));
    let failures = world.delivery_failures();
    assert!(
        failures.iter().any(|f| f.contains("cannot be opened")),
        "{failures:?}"
    );
    let (error, said) = listed();
    assert!(
        error && said.as_str().unwrap().contains("cannot be opened"),
        "{said}"
    );
    let (code, _) = world
        .inspect("ws", json!({ "action": "list" }))
        .unwrap_err();
    assert_eq!(code, MODULE_FAILED);
}

#[test]
fn a_moved_root_outlives_a_restart_of_the_core() {
    let home = temp("restart");
    let (root, other) = (home.join("root"), home.join("other"));
    put(&root, "first.txt", "first\n");
    put(&other, "second.txt", "second\n");
    let core = || {
        let mut profiles = ProfileStore::default();
        profiles
            .insert(profile(json!({ "root": root }), json!({})))
            .unwrap();
        let storage = Storage {
            store: Some(Arc::new(
                Store::open(&home.join("store"), &StoreConfig::default()).unwrap(),
            )),
            data_dir: Some(home.join("data")),
            ..Storage::default()
        };
        let modules: Vec<Box<dyn Module>> = vec![Box::new(Code::new())];
        Core::build_with(modules, &[], profiles, storage)
    };
    let listed = |core: &Core, id: &str| -> Vec<String> {
        let listing = core
            .dispatch(
                contract::INSPECT,
                json!({ "execution_id": id, "node_id": "ws", "action": "list" }),
            )
            .unwrap();
        paths(&listing["entries"])
            .into_iter()
            .map(String::from)
            .collect()
    };

    let first = core();
    let started = first
        .dispatch(method::EXECUTION_START, json!({ "profile_id": "p" }))
        .unwrap();
    let id = started["id"].as_str().unwrap().to_string();
    assert_eq!(listed(&first, &id), ["first.txt"]);
    first
        .dispatch(
            method::WORKFLOW_SEND,
            json!({ "execution_id": id, "port": "root", "value": other }),
        )
        .unwrap();
    let deadline = Instant::now() + WAIT;
    while listed(&first, &id) != ["second.txt"] {
        assert!(Instant::now() < deadline, "the root never moved");
        std::thread::sleep(Duration::from_millis(5));
    }
    first.shutdown();
    drop(first);

    // The next core resumes the execution where the root had moved to, not at the field.
    let second = core();
    assert_eq!(second.recover(), [format!("{id} resumed (1 checkpoints)")]);
    assert_eq!(listed(&second, &id), ["second.txt"]);
    second.shutdown();
    let _ = fs::remove_dir_all(&home);
}

#[test]
fn a_workspace_whose_root_is_not_there_does_not_start() {
    let home = temp("no-root");
    let world = World::build(
        home.clone(),
        home.join("missing"),
        profile(json!({ "root": home.join("missing") }), json!({})),
        vec![Box::new(Code::new())],
    );
    let execution = world.execution();
    assert_eq!(execution["state"]["status"], "failed", "{execution}");
    let reason = execution["state"]["reason"].as_str().unwrap();
    assert!(
        reason.contains("code workspace `ws`") && reason.contains("cannot be opened"),
        "{reason}"
    );
    drop(world);

    // Without a root at all it starts, and waits for one on `root`.
    let home = temp("late-root");
    let waiting = World::build(
        home.clone(),
        home.clone(),
        profile(json!({}), json!({})),
        vec![Box::new(Code::new())],
    );
    assert_eq!(waiting.state(), "running");
    let said = waiting.refused("code_list_files", json!({}));
    assert!(said.contains("has no root"), "{said}");
    put(&home, "late.txt", "late\n");
    waiting.send("root", json!(home));
    waiting.delivery_failures();
    assert_eq!(
        paths(&waiting.ok("code_list_files", json!({}))["entries"]),
        ["late.txt"]
    );
}

/// Another module that offers `code.workspace@1`: a workspace that is not a local folder.
struct Remote {
    manifest: Manifest,
    calls: Arc<Mutex<Vec<(String, Value)>>>,
}

impl Module for Remote {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn init(&mut self, _: &ModuleCtx) -> Result<(), ModuleError> {
        Ok(())
    }
    fn call(&self, name: &str, params: Value) -> Result<Value, ModuleError> {
        let call: NodeCall = serde_json::from_value(params)
            .map_err(|e| ModuleError::InvalidParams(e.to_string()))?;
        if name.starts_with("code.workspace.") {
            self.calls
                .lock()
                .unwrap()
                .push((name.into(), call.args.clone()));
        }
        let operation = |name: &str, access: &str| {
            json!({ "name": name, "description": name, "input": { "type": "object" },
                    "access": access })
        };
        match name {
            contract::DESCRIBE => Ok(json!({ "root": "/remote", "operations": [
                operation("remote_read", "read"), operation("remote_write", "write"),
                operation("remote_run", "execute")] })),
            contract::CALL => Ok(json!({ "data": { "ran": call.args["name"],
                                                   "by": call.args["caller"]["name"] } })),
            contract::LOOK if call.args["path"] == "AGENTS.md" => Ok(json!({
                "root": "/remote", "path": "AGENTS.md", "text": "Remote rules.\n",
                "truncated": false, "size": 14 })),
            contract::LOOK => Err(ModuleError::InvalidParams("no such file".into())),
            other => Err(ModuleError::UnknownMethod(other.into())),
        }
    }
    fn shutdown(&mut self) -> Result<(), ModuleError> {
        Ok(())
    }
}

#[test]
fn the_views_and_the_instructions_work_over_any_workspace_by_capability() {
    let calls: Arc<Mutex<Vec<(String, Value)>>> = Arc::default();
    let remote = Remote {
        manifest: Manifest {
            module: ModuleInfo {
                id: FAKE.into(),
                name: "remote".into(),
                requires_proto: None,
                methods: vec![],
                workflow_id: None,
                origin: None,
            },
            nodes: vec![],
        },
        calls: calls.clone(),
    };
    let profile: Profile = serde_json::from_value(json!({
        "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main",
            "nodes": [
                { "id": "far", "type": "box", "module_id": FAKE,
                  "ports": [port("workspace", contract::CAPABILITY, "output", "uses")] },
                { "id": "rw", "type": "tools", "module_id": MODULE,
                  "config": { "access": "write" },
                  "ports": [workspace_input(), port("tools", "tool@1", "output", "uses")] },
                { "id": "docs", "type": "instructions", "module_id": MODULE,
                  "ports": [workspace_input(), port("hook", "hook@1", "output", "uses")] },
            ],
            "wires": [
                wire("rw", "uses", ("far", "workspace"), ("rw", "workspace")),
                wire("docs", "uses", ("far", "workspace"), ("docs", "workspace")),
            ] }]
    }))
    .unwrap();
    let home = temp("remote");
    let world = World::build(
        home.clone(),
        home,
        profile,
        vec![Box::new(Code::new()), Box::new(remote)],
    );
    assert_eq!(world.state(), "running");

    // The view knows the operations by what they do, not by their names.
    assert_eq!(world.tools_of("rw"), ["remote_read", "remote_write"]);
    let written = world.call_on("rw", "remote_write", json!({ "x": 1 }));
    assert_eq!(
        written.data,
        json!({ "ran": "remote_write", "by": "tester" })
    );
    let run = world.call_on("rw", "remote_run", json!({}));
    assert!(
        run.error,
        "an operation outside the view's access never reaches the workspace"
    );
    assert_eq!(
        hook(&world, "session_start"),
        json!({ "decision": "context",
                "text": "Instructions from the repository file AGENTS.md:\nRemote rules." })
    );
    let seen: Vec<String> = calls
        .lock()
        .unwrap()
        .iter()
        .map(|(name, _)| name.clone())
        .collect();
    assert!(seen.contains(&contract::DESCRIBE.to_string()));
    assert_eq!(
        seen.iter().filter(|name| *name == contract::CALL).count(),
        1,
        "{seen:?}"
    );
    let looked: Vec<Value> = calls
        .lock()
        .unwrap()
        .iter()
        .filter(|(name, _)| name == contract::LOOK)
        .map(|(_, args)| args.clone())
        .collect();
    assert_eq!(
        looked,
        [
            json!({ "action": "read", "path": "AGENTS.md" }),
            json!({ "action": "read", "path": "CLAUDE.md" })
        ]
    );
    // `code.inspect` is this module's door to its own workspaces: not to another's.
    let (code, _) = world
        .inspect("far", json!({ "action": "summary" }))
        .unwrap_err();
    assert_eq!(code, INVALID_PARAMS);
}
