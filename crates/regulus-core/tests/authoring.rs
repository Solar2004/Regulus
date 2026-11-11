//! Writing nodes end to end against a real core and real Python processes (protocol 1.17):
//! `node.init` → `source.get` → a save with a syntax error (checked, never run) → a stale
//! save refused → a reload that keeps the running module → the fix → a reload refused
//! while an execution runs, then done with `stop_executions` → stale ports → `profile.sync`
//! → a run whose value goes out on a repeated port. And what an untrusted project gets.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::projects::Projects;
use regulus_core::service::ProjectHome;
use regulus_core::{Core, LocalModulesConfig, ProfileStore, RpcError};
use regulus_proto::{code, method};
use serde_json::{json, Value};

struct Setup {
    dir: PathBuf,
    core: Arc<Core>,
}

impl Drop for Setup {
    fn drop(&mut self) {
        self.core.shutdown();
        let _ = fs::remove_dir_all(&self.dir);
    }
}

impl Setup {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "regulus-core-authoring-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("home")).unwrap();
        let core = Core::build(Vec::new(), &[], ProfileStore::default());
        core.set_local_modules(LocalModulesConfig::default());
        core.set_projects(
            Projects::default(),
            ProjectHome {
                config: dir.join("home/config.toml"),
                home: dir.join("home"),
            },
        );
        Self { dir, core }
    }

    fn project(&self, name: &str, trust: bool) -> PathBuf {
        let path = self.dir.join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(
            path.join("regulus.toml"),
            format!("[project]\nid = \"{name}\"\n"),
        )
        .unwrap();
        self.ok(
            method::PROJECT_OPEN,
            json!({ "path": path, "trust": trust }),
        );
        path
    }

    fn call(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        self.core.dispatch(method, params)
    }

    fn ok(&self, method: &str, params: Value) -> Value {
        self.call(method, params.clone())
            .unwrap_or_else(|e| panic!("{method} {params}: {e:?}"))
    }

    fn err(&self, method: &str, params: Value) -> RpcError {
        match self.call(method, params.clone()) {
            Ok(value) => panic!("{method} {params} should fail, gave {value}"),
            Err(error) => error,
        }
    }

    /// The first `emitted` event from `node.port` of `execution`, waiting for it.
    fn emitted(&self, execution: &str, node: &str, port: &str) -> Value {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let page = self.ok(method::EVENT_LIST, json!({ "execution_id": execution }));
            let found = page["events"].as_array().unwrap().iter().find(|e| {
                e["kind"] == "emitted"
                    && e["from"]["node_id"] == node
                    && e["from"]["port_id"] == port
            });
            if let Some(event) = found {
                return event["value"].clone();
            }
            assert!(
                Instant::now() < deadline,
                "nothing emitted on {node}.{port}: {page:#}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

const BROKEN: &str = "import regulus as rg\n\ndef handle(ctx, port, value)\n    pass\n";

const FIXED: &str = r#"import regulus as rg


@rg.node("caps", name="Caps",
         inputs=[rg.then_in("in", "text@1")],
         outputs=[rg.then_out("case_{item}", "text@1", repeat="cases")],
         fields=[rg.field("cases", "list", default=["a", "b"])])
def handle(ctx, port, value):
    ctx.emit("case_" + value, value.upper())


if __name__ == "__main__":
    rg.run(version="0.2.0")
"#;

fn port(id: &str, direction: &str) -> Value {
    json!({ "id": id, "capability": "text@1", "direction": direction, "kind": "then" })
}

fn wire(id: &str, from: (&str, &str), to: (&str, &str)) -> Value {
    json!({ "id": id, "kind": "then",
            "from": { "node_id": from.0, "port_id": from.1 },
            "to": { "node_id": to.0, "port_id": to.1 } })
}

fn codes(check: &Value) -> Vec<String> {
    check["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn a_python_node_is_written_checked_reloaded_synced_and_run() {
    let setup = Setup::new("journey");
    let root = setup.project("demo", true);
    let started = Instant::now();

    // Create: the files, the entry and a running module.
    let made = setup.ok(
        method::NODE_INIT,
        json!({ "project_id": "demo", "id": "caps" }),
    );
    assert_eq!(made["module_id"], "demo/caps");
    assert_eq!(made["source"], "tools/caps/run.py");
    assert_eq!(made["module"]["outcome"], "started", "{made:#}");
    assert_eq!(made["module"]["state"]["status"], "idle", "{made:#}");
    assert_eq!(made["module"]["id"], "demo/caps@0.1.0");
    let details = setup.ok(method::PROJECT_GET, json!({ "id": "demo" }));
    assert_eq!(
        details["sources"],
        json!([{ "module": "caps", "path": "tools/caps/run.py", "error": null }])
    );
    let again = setup.err(
        method::NODE_INIT,
        json!({ "project_id": "demo", "id": "caps" }),
    );
    assert_eq!(again.code, code::SOURCE_CONFLICT, "{again:?}");

    // Open: the text and its revision; the shape of its node.
    let source = setup.ok(
        method::SOURCE_GET,
        json!({ "project_id": "demo", "module": "caps" }),
    );
    let first = source["revision"].as_str().unwrap().to_string();
    assert!(source["text"].as_str().unwrap().contains("@rg.node("));
    let shape = setup.ok(
        method::MODULE_SHAPE,
        json!({ "module_id": "demo/caps", "type": "caps", "config": {} }),
    );
    let old_ports = shape["ports"].clone();
    assert_eq!(
        old_ports,
        json!([
            { "id": "in", "name": "in", "capability": "text@1", "direction": "input", "kind": "then",
              "required": false, "cardinality": "many" },
            { "id": "out", "name": "out", "capability": "text@1", "direction": "output", "kind": "then",
              "required": false },
        ])
    );

    // A profile that uses it, with the shape it has now.
    let profile = json!({
        "id": "flow", "project_id": "demo", "main_workflow_id": "main",
        "workflows": [{
            "id": "main", "kind": "main",
            "ports": [port("in", "input"), port("a", "output")],
            "nodes": [{ "id": "c", "type": "caps", "module_id": "demo/caps", "ports": old_ports, "config": {} }],
            "wires": [wire("w1", ("self", "in"), ("c", "in")), wire("w2", ("c", "out"), ("self", "a"))],
        }],
    });
    setup.ok(method::PROFILE_SAVE, json!({ "profile": profile }));

    // A syntax error: saved and reported, never run.
    let saved = setup.ok(
        method::SOURCE_SAVE,
        json!({ "project_id": "demo", "module": "caps", "text": BROKEN, "revision": first }),
    );
    let second = saved["revision"].as_str().unwrap().to_string();
    assert_ne!(first, second);
    let error = &saved["check"]["diagnostics"][0];
    assert_eq!(
        (error["code"].as_str(), error["line"].as_u64()),
        (Some("python.syntax"), Some(3)),
        "{saved:#}"
    );
    assert_eq!(saved["check"]["checked"], true);
    assert_eq!(
        fs::read_to_string(root.join("tools/caps/run.py")).unwrap(),
        BROKEN
    );

    // A save from the old revision is refused and says the current one.
    let stale = setup.err(
        method::SOURCE_SAVE,
        json!({ "project_id": "demo", "module": "caps", "text": "x = 1\n", "revision": first }),
    );
    assert_eq!(stale.code, code::SOURCE_CONFLICT);
    assert_eq!(stale.data.as_ref().unwrap()["revision"], json!(second));
    assert_eq!(
        fs::read_to_string(root.join("tools/caps/run.py")).unwrap(),
        BROKEN
    );

    // Reloading the broken file keeps the module that runs.
    let kept = setup.ok(
        method::PROJECT_RELOAD,
        json!({ "id": "demo", "module": "caps" }),
    );
    let module = &kept["modules"][0];
    assert_eq!(module["outcome"], "kept", "{kept:#}");
    assert!(
        module["error"].as_str().unwrap().contains("SyntaxError"),
        "{kept:#}"
    );
    assert_eq!(module["state"]["status"], "idle");
    let listed = setup.ok(method::MODULE_LIST, json!({}));
    assert_eq!(listed[0]["id"], "demo/caps@0.1.0");
    assert_eq!(listed[0]["state"]["status"], "idle");

    // The fix: checked complete, and not what the running module declares yet.
    let fixed = setup.ok(
        method::SOURCE_SAVE,
        json!({ "project_id": "demo", "module": "caps", "text": FIXED, "revision": second }),
    );
    assert_eq!(codes(&fixed["check"]), Vec::<String>::new(), "{fixed:#}");
    assert_eq!(fixed["check"]["complete"], true);
    assert_eq!(fixed["check"]["matches_loaded"], false);
    assert_eq!(
        fixed["check"]["nodes"][0]["dynamic_port"][0]["id"],
        "case_{item}"
    );

    // A live execution of the module: the reload refuses, and says which.
    let execution = setup.ok(method::EXECUTION_START, json!({ "profile_id": "flow" }));
    let id = execution["id"].as_str().unwrap().to_string();
    let busy = setup.err(
        method::PROJECT_RELOAD,
        json!({ "id": "demo", "module": "caps" }),
    );
    assert_eq!(busy.code, code::MODULE_IN_USE);
    let data = busy.data.unwrap();
    assert_eq!(data["executions"][0]["id"], json!(id));
    assert_eq!(data["executions"][0]["nodes"], json!(["c"]));
    assert_eq!(
        setup.ok(method::MODULE_LIST, json!({}))[0]["id"],
        "demo/caps@0.1.0"
    );

    // Asked to stop it: the execution stops, the new module takes the place of the old.
    let reloaded = setup.ok(
        method::PROJECT_RELOAD,
        json!({ "id": "demo", "module": "demo/caps", "stop_executions": true }),
    );
    let module = &reloaded["modules"][0];
    assert_eq!(module["outcome"], "replaced", "{reloaded:#}");
    assert_eq!(
        (module["previous"].as_str(), module["id"].as_str()),
        (Some("demo/caps@0.1.0"), Some("demo/caps@0.2.0"))
    );
    assert_eq!(module["node_types"]["changed"], json!(["caps"]));
    assert_eq!(reloaded["stopped_executions"], json!([id]));
    assert_eq!(
        reloaded["stale"],
        json!([{ "profile_id": "flow", "workflow_id": "main", "node_id": "c" }])
    );
    let stopped = setup.ok(method::EXECUTION_LIST, json!({}));
    assert_eq!(stopped[0]["state"]["status"], "stopped");
    let check = setup.ok(
        method::SOURCE_CHECK,
        json!({ "project_id": "demo", "module": "caps" }),
    );
    assert_eq!(check["matches_loaded"], true, "{check:#}");

    // The node's ports are stale; the sync previews what it drops.
    let found = setup.ok(method::PROFILE_CHECK, json!({ "id": "flow" }));
    let stale: Vec<&Value> = found
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["code"] == "node.ports_stale")
        .collect();
    assert_eq!(stale.len(), 1, "{found:#}");
    assert!(
        stale[0]["message"]
            .as_str()
            .unwrap()
            .contains("missing `case_a`, `case_b`; extra `out`"),
        "{found:#}"
    );
    let synced = setup.ok(method::PROFILE_SYNC, json!({ "profile_id": "flow" }));
    assert_eq!(synced["drops_wires"], true);
    let node = &synced["nodes"][0];
    assert_eq!(node["wires_removed"][0]["wire"]["id"], "w2");
    assert!(node["wires_removed"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("`out`"));
    // Nothing written by the preview.
    let loaded = setup.ok(method::PROFILE_GET, json!({ "id": "flow" }));
    assert_eq!(loaded["workflows"][0]["wires"].as_array().unwrap().len(), 2);

    // Applied by the client: the synced profile, a wire on a repeated port, saved.
    let mut profile = synced["profile"].clone();
    profile["workflows"][0]["wires"]
        .as_array_mut()
        .unwrap()
        .push(wire("w3", ("c", "case_a"), ("self", "a")));
    setup.ok(
        method::PROFILE_SAVE,
        json!({ "profile": profile, "force": true }),
    );
    let found = setup.ok(method::PROFILE_CHECK, json!({ "id": "flow" }));
    assert!(
        found
            .as_array()
            .unwrap()
            .iter()
            .all(|d| d["code"] != "node.ports_stale"),
        "{found:#}"
    );

    // Real data through the new port.
    let run = setup.ok(method::EXECUTION_START, json!({ "profile_id": "flow" }));
    let run = run["id"].as_str().unwrap().to_string();
    setup.ok(
        method::WORKFLOW_SEND,
        json!({ "execution_id": run, "port": "in", "value": "a" }),
    );
    assert_eq!(setup.emitted(&run, "c", "case_a"), json!("A"));
    setup.ok(method::EXECUTION_STOP, json!({ "id": run }));
    eprintln!("journey took {:?}", started.elapsed());

    // Read methods against the 5 ms budget (docs/00-principles.md §3): the median of 50.
    let median = |method: &str, params: Value| {
        let mut took: Vec<Duration> = (0..50)
            .map(|_| {
                let at = Instant::now();
                setup.ok(method, params.clone());
                at.elapsed()
            })
            .collect();
        took.sort();
        eprintln!("{method}: median {:?}, max {:?}", took[25], took[49]);
        took[25]
    };
    let config = json!({ "cases": ["a", "b", "c", "d"] });
    median(
        method::MODULE_SHAPE,
        json!({ "module_id": "demo/caps", "type": "caps", "config": config }),
    );
    median(method::PROFILE_SYNC, json!({ "profile_id": "flow" }));
    median(method::PROFILE_CHECK, json!({ "id": "flow" }));
    median(
        method::SOURCE_GET,
        json!({ "project_id": "demo", "module": "caps" }),
    );
    median(method::PROJECT_GET, json!({ "id": "demo" }));

    // Closing the project ends its process.
    setup.ok(method::PROJECT_CLOSE, json!({ "id": "demo" }));
    assert_eq!(setup.ok(method::MODULE_LIST, json!({})), json!([]));
}

#[test]
fn an_untrusted_project_is_read_and_checked_but_never_run() {
    let setup = Setup::new("untrusted");
    let root = setup.project("open", false);
    let made = setup.ok(
        method::NODE_INIT,
        json!({ "project_id": "open", "id": "probe" }),
    );
    assert_eq!(made["module"]["outcome"], "blocked", "{made:#}");
    assert_eq!(made["module"]["state"]["status"], "blocked");

    // A source whose import would leave a mark: checking it leaves none.
    let marker = root.join("ran");
    let text = format!(
        "import pathlib\npathlib.Path({:?}).write_text('ran')\nimport regulus as rg\n\n\
         @rg.node('probe', inputs=[rg.then_in('in', 'text@1')])\ndef probe(ctx, port, value):\n    pass\n\n\
         rg.run()\n",
        marker.display().to_string()
    );
    let source = setup.ok(
        method::SOURCE_GET,
        json!({ "project_id": "open", "module": "probe" }),
    );
    let saved = setup.ok(
        method::SOURCE_SAVE,
        json!({ "project_id": "open", "module": "probe", "text": text, "revision": source["revision"] }),
    );
    assert_eq!(saved["check"]["complete"], true, "{saved:#}");
    assert_eq!(saved["check"]["nodes"][0]["type"], "probe");
    assert!(!codes(&saved["check"]).contains(&"sdk.outdated".to_string()));
    // An edited SDK copy that still says this core's version: said as such.
    let copy = root.join("tools/probe/regulus.py");
    let edited = fs::read_to_string(&copy).unwrap() + "# edited by hand\n";
    fs::write(&copy, edited).unwrap();
    let checked = setup.ok(
        method::SOURCE_CHECK,
        json!({ "project_id": "open", "module": "probe" }),
    );
    let note = checked["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "sdk.outdated")
        .unwrap_or_else(|| panic!("no sdk.outdated: {checked:#}"));
    let message = note["message"].as_str().unwrap();
    assert!(
        message.contains("differs from this core's SDK, though both say"),
        "{message}"
    );
    let reloaded = setup.ok(method::PROJECT_RELOAD, json!({ "id": "open" }));
    assert_eq!(reloaded["modules"][0]["outcome"], "blocked", "{reloaded:#}");
    assert!(
        !marker.exists(),
        "nothing of the project may run while it is not trusted"
    );

    // Trusting it runs it; the mark shows that it was the module, not the checker.
    setup.ok(method::PROJECT_TRUST, json!({ "path": root }));
    let listed = setup.ok(method::MODULE_LIST, json!({}));
    assert_eq!(listed[0]["state"]["status"], "idle", "{listed:#}");
    assert!(marker.exists());
}

#[test]
fn sources_stay_inside_the_project_and_whole_project_reloads_follow_the_file() {
    let setup = Setup::new("bounds");
    let root = setup.project("box", true);
    let outside = setup.dir.join("outside.py");
    fs::write(&outside, "x = 1\n").unwrap();
    fs::write(
        root.join("regulus.toml"),
        "[project]\nid = \"box\"\n\
         [[modules.local]]\nid = \"far\"\ncommand = [\"python3\", \"../outside.py\"]\n\
         [[modules.local]]\nid = \"bin\"\ncommand = [\"sh\", \"-c\", \"exit 1\"]\n",
    )
    .unwrap();
    for module in ["far", "bin", "ghost"] {
        let refused = setup.err(
            method::SOURCE_GET,
            json!({ "project_id": "box", "module": module }),
        );
        assert_eq!(
            refused.code,
            code::SOURCE_UNAVAILABLE,
            "{module}: {refused:?}"
        );
        let refused = setup.err(
            method::SOURCE_SAVE,
            json!({ "project_id": "box", "module": module, "text": "y = 2\n", "force": true }),
        );
        assert_eq!(
            refused.code,
            code::SOURCE_UNAVAILABLE,
            "{module}: {refused:?}"
        );
    }
    assert_eq!(fs::read_to_string(&outside).unwrap(), "x = 1\n");

    // A whole-project reload picks up entries added and drops entries removed.
    let reloaded = setup.ok(method::PROJECT_RELOAD, json!({ "id": "box" }));
    let outcomes: Vec<(&str, &str)> = reloaded["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["local_id"].as_str().unwrap(),
                m["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        outcomes,
        [("far", "failed"), ("bin", "failed")],
        "{reloaded:#}"
    );
    fs::write(root.join("regulus.toml"), "[project]\nid = \"box\"\n").unwrap();
    // The project file is read again when its time changes; make sure it does.
    let later = std::time::SystemTime::now() + Duration::from_secs(2);
    fs::File::options()
        .write(true)
        .open(root.join("regulus.toml"))
        .unwrap()
        .set_modified(later)
        .unwrap();
    let reloaded = setup.ok(method::PROJECT_RELOAD, json!({ "id": "box" }));
    let outcomes: Vec<&str> = reloaded["modules"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["outcome"].as_str().unwrap())
        .collect();
    assert_eq!(outcomes, ["removed", "removed"], "{reloaded:#}");
    assert_eq!(setup.ok(method::MODULE_LIST, json!({})), json!([]));
    let unknown = setup.err(
        method::PROJECT_RELOAD,
        json!({ "id": "box", "module": "far" }),
    );
    assert_eq!(unknown.code, code::INVALID_PARAMS);
}

/// A local module that answers the protocol by hand, waiting first as many seconds as its
/// `delay` file says: a slow start to reload against.
const SLOW: &str = r#"import json, pathlib, sys, time
delay = pathlib.Path("delay")
time.sleep(float(delay.read_text()) if delay.exists() else 0)
port = {"id": "in", "capability": "text@1", "direction": "input", "kind": "then"}
manifest = {"module": {"name": "slow"}, "node": [{"type": "slow", "name": "Slow", "port": [port]}]}
for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "manifest":
        answer = {"result": manifest}
    elif method == "call":
        answer = {"error": {"code": -32601, "message": "no lifecycle"}}
    else:
        answer = {"result": None}
    answer.update({"jsonrpc": "2.0", "id": message["id"]})
    print(json.dumps(answer), flush=True)
    if method == "stop":
        break
"#;

#[test]
fn a_slow_reload_blocks_nothing_else_and_refuses_a_run_that_started_meanwhile() {
    let setup = Arc::new(Setup::new("concurrent"));
    let root = setup.dir.join("conc");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("slow.py"), SLOW).unwrap();
    fs::write(
        root.join("regulus.toml"),
        "[project]\nid = \"conc\"\n[[modules.local]]\nid = \"slow\"\ncommand = [\"python3\", \"slow.py\"]\n",
    )
    .unwrap();
    setup.ok(method::PROJECT_OPEN, json!({ "path": root, "trust": true }));
    let node = json!({ "id": "s", "type": "slow", "module_id": "conc/slow",
                       "ports": [port("in", "input")], "config": {} });
    let profile = |id: &str| {
        json!({ "id": id, "project_id": "conc", "main_workflow_id": "main",
                                     "workflows": [{ "id": "main", "kind": "main", "nodes": [node] }] })
    };
    setup.ok(method::PROFILE_SAVE, json!({ "profile": profile("p") }));

    fs::write(root.join("delay"), "1.5").unwrap();
    let reloading = {
        let setup = setup.clone();
        std::thread::spawn(move || setup.call(method::PROJECT_RELOAD, json!({ "id": "conc" })))
    };
    std::thread::sleep(Duration::from_millis(300));
    // While the candidate starts: reads, a profile write, a check and a run go on.
    let quick = |what: &str, call: &dyn Fn()| {
        let at = Instant::now();
        call();
        let took = at.elapsed();
        eprintln!("{what} during the reload took {took:?}");
        assert!(
            took < Duration::from_millis(750),
            "{what} waited for the reload: {took:?}"
        );
    };
    quick("module.list", &|| {
        let listed = setup.ok(method::MODULE_LIST, json!({}));
        assert_eq!(listed[0]["state"]["status"], "idle");
    });
    quick("project.get", &|| {
        setup.ok(method::PROJECT_GET, json!({ "id": "conc" }));
    });
    quick("profile.save", &|| {
        setup.ok(method::PROFILE_SAVE, json!({ "profile": profile("q") }));
    });
    quick("source.check", &|| {
        let check = setup.ok(
            method::SOURCE_CHECK,
            json!({ "project_id": "conc", "module": "slow" }),
        );
        assert_eq!(check["checked"], true, "{check:#}");
    });
    let run = setup.ok(method::EXECUTION_START, json!({ "profile_id": "p" }));
    assert!(
        !reloading.is_finished(),
        "the reload was meant to still be starting"
    );

    // The run started while the candidate started: the swap refuses, and says which.
    let refused = reloading.join().unwrap().unwrap_err();
    assert_eq!(refused.code, code::MODULE_IN_USE, "{refused:?}");
    assert_eq!(refused.data.unwrap()["executions"][0]["id"], run["id"]);
    let listed = setup.ok(method::MODULE_LIST, json!({}));
    // The old module kept running, and the run used it.
    assert_eq!(
        (
            listed.as_array().unwrap().len(),
            &listed[0]["state"]["status"]
        ),
        (1, &json!("active"))
    );

    // Stopped by hand, the same reload goes through.
    setup.ok(method::EXECUTION_STOP, json!({ "id": run["id"] }));
    fs::write(root.join("delay"), "0").unwrap();
    let done = setup.ok(method::PROJECT_RELOAD, json!({ "id": "conc" }));
    assert_eq!(done["modules"][0]["outcome"], "replaced", "{done:#}");
}

/// A module that starts after `delay` seconds and, once the file `stubborn` exists, neither
/// answers `stop` nor ends on SIGTERM: stopping it takes the whole grace twice.
const STUBBORN: &str = r#"import json, pathlib, signal, sys, time
delay = pathlib.Path("delay")
time.sleep(float(delay.read_text()) if delay.exists() else 0)
port = {"id": "in", "capability": "text@1", "direction": "input", "kind": "then"}
manifest = {"module": {"name": "lazy"}, "node": [{"type": "lazy", "name": "Lazy", "port": [port]}]}
for line in sys.stdin:
    message = json.loads(line)
    if "id" not in message:
        continue
    method = message.get("method")
    if method == "stop" and pathlib.Path("stubborn").exists():
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        time.sleep(60)
    answer = {"result": manifest} if method == "manifest" else {"result": None}
    answer.update({"jsonrpc": "2.0", "id": message["id"]})
    print(json.dumps(answer), flush=True)
    if method == "stop":
        break
"#;

#[test]
fn a_refused_reload_stops_its_candidate_without_holding_the_core() {
    let setup = Arc::new(Setup::new("refused"));
    let grace = 800;
    setup.core.set_local_modules(LocalModulesConfig {
        stop_grace_ms: grace,
        ..LocalModulesConfig::default()
    });
    let root = setup.dir.join("hold");
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("lazy.py"), STUBBORN).unwrap();
    fs::write(
        root.join("regulus.toml"),
        "[project]\nid = \"hold\"\n[[modules.local]]\nid = \"lazy\"\ncommand = [\"python3\", \"lazy.py\"]\n",
    )
    .unwrap();
    setup.ok(method::PROJECT_OPEN, json!({ "path": root, "trust": true }));
    // Another project, where profiles are written while the reload ends.
    setup.project("other", false);

    // The candidate starts slowly; meanwhile the project closes, so the swap is refused and
    // the candidate, which will not stop quickly, has to be stopped.
    fs::write(root.join("delay"), "1.0").unwrap();
    let reloading = {
        let setup = setup.clone();
        std::thread::spawn(move || {
            let at = Instant::now();
            let result = setup.call(method::PROJECT_RELOAD, json!({ "id": "hold" }));
            (result, at.elapsed())
        })
    };
    std::thread::sleep(Duration::from_millis(300));
    setup.ok(method::PROJECT_CLOSE, json!({ "id": "hold" }));
    fs::write(root.join("stubborn"), "").unwrap();

    // Profile writes go on while the refused candidate is being stopped.
    let profile = json!({ "id": "free", "project_id": "other", "main_workflow_id": "main",
                          "workflows": [{ "id": "main", "kind": "main", "nodes": [] }] });
    let mut slowest = Duration::ZERO;
    while !reloading.is_finished() {
        let at = Instant::now();
        setup.ok(
            method::PROFILE_SAVE,
            json!({ "profile": profile, "force": true }),
        );
        slowest = slowest.max(at.elapsed());
        std::thread::sleep(Duration::from_millis(25));
    }
    let (result, took) = reloading.join().unwrap();
    let refused = result.unwrap_err();
    assert_eq!(refused.code, code::MODULE_FAILED, "{refused:?}");
    // The stop really was slow (twice the grace), so the probes ran while it happened.
    assert!(
        took >= Duration::from_millis(1_000 + 2 * grace - 200),
        "the candidate was not stopped the slow way: {took:?}"
    );
    eprintln!("refused reload took {took:?}; slowest profile.save meanwhile {slowest:?}");
    assert!(
        slowest < Duration::from_millis(500),
        "profile.save waited for the candidate's stop: {slowest:?}"
    );
}

#[test]
fn a_slow_or_missing_checker_says_so_and_is_killed_in_time() {
    let setup = Setup::new("checker");
    setup.project("chk", true);
    setup.ok(method::NODE_INIT, json!({ "project_id": "chk", "id": "n" }));
    let fake = setup.dir.join("slow-python");
    fs::write(&fake, "#!/bin/sh\nsleep 30\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    }
    setup.core.set_local_modules(LocalModulesConfig {
        python: fake.display().to_string(),
        check_timeout_ms: 300,
        ..LocalModulesConfig::default()
    });
    let at = Instant::now();
    let check = setup.ok(
        method::SOURCE_CHECK,
        json!({ "project_id": "chk", "module": "n" }),
    );
    assert!(at.elapsed() < Duration::from_secs(5), "{:?}", at.elapsed());
    assert_eq!(check["checked"], false);
    assert_eq!(check["diagnostics"][0]["code"], "check.unavailable");
    assert!(
        check["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("300 ms"),
        "{check:#}"
    );

    setup.core.set_local_modules(LocalModulesConfig {
        python: "regulus-no-such-python".into(),
        ..LocalModulesConfig::default()
    });
    let check = setup.ok(
        method::SOURCE_CHECK,
        json!({ "project_id": "chk", "module": "n" }),
    );
    assert_eq!(
        (
            check["checked"].as_bool(),
            check["nodes"].as_array().unwrap().len()
        ),
        (Some(false), 0)
    );
    assert!(check["diagnostics"][0]["message"]
        .as_str()
        .unwrap()
        .contains("regulus-no-such-python"));
}
