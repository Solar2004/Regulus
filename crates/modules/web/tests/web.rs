//! The web nodes against a real core and a local HTTP server: no internet needed.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use regulus_core::{Core, Module, ProfileStore, Storage};
use regulus_module_web::Web;
use regulus_proto::{method, BusEventKind, EventPage, Profile};
use serde_json::{json, Value};

const WAIT: Duration = Duration::from_secs(10);

fn pdf() -> Vec<u8> {
    use lopdf::content::{Content, Operation};
    use lopdf::{dictionary, Document, Object, Stream};
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(
        dictionary! { "Type" => "Font", "Subtype" => "Type1", "BaseFont" => "Courier" },
    );
    let resources_id = doc.add_object(dictionary! { "Font" => dictionary! { "F1" => font_id } });
    let content = Content {
        operations: vec![
            Operation::new("BT", vec![]),
            Operation::new("Tf", vec!["F1".into(), 24.into()]),
            Operation::new("Td", vec![100.into(), 600.into()]),
            Operation::new("Tj", vec![Object::string_literal("Hello from a PDF")]),
            Operation::new("ET", vec![]),
        ],
    };
    let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
    let page_id = doc.add_object(
        dictionary! { "Type" => "Page", "Parent" => pages_id, "Contents" => content_id },
    );
    let pages = dictionary! {
        "Type" => "Pages", "Kids" => vec![page_id.into()], "Count" => 1,
        "Resources" => resources_id,
        "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages));
    let catalog_id = doc.add_object(dictionary! { "Type" => "Catalog", "Pages" => pages_id });
    doc.trailer.set("Root", catalog_id);
    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).unwrap();
    bytes
}

/// A tiny server: the example site, plus routes for redirects, limits, echo, search and
/// a PDF. Returns its address.
fn serve() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let pdf = Arc::new(pdf());
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let pdf = pdf.clone();
            std::thread::spawn(move || answer(stream, &pdf));
        }
    });
    addr
}

fn answer(mut stream: TcpStream, pdf: &[u8]) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut first = String::new();
    reader.read_line(&mut first).unwrap();
    let mut headers = HashMap::new();
    loop {
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        if line.trim().is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    let length: usize = headers
        .get("content-length")
        .and_then(|l| l.parse().ok())
        .unwrap_or(0);
    let mut body = vec![0; length];
    reader.read_exact(&mut body).unwrap();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let (path, query) = target.split_once('?').unwrap_or((&target, ""));
    let site = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("examples/site");
    let (status, kind, content, location): (u16, &str, Vec<u8>, Option<&str>) = match path {
        "/data.json" => (
            200,
            "application/json",
            std::fs::read(site.join("data.json")).unwrap(),
            None,
        ),
        "/page.html" => (
            200,
            "text/html; charset=utf-8",
            std::fs::read(site.join("page.html")).unwrap(),
            None,
        ),
        "/redirect" => (302, "text/plain", vec![], Some("/page.html")),
        "/loop" => (302, "text/plain", vec![], Some("/loop")),
        "/big" => (200, "text/plain", vec![b'x'; 5000], None),
        "/doc.pdf" => (200, "application/pdf", pdf.to_vec(), None),
        "/missing" => (404, "text/plain", b"no".to_vec(), None),
        "/echo" => {
            let echo = json!({ "method": method, "body": String::from_utf8_lossy(&body),
                "ua": headers.get("user-agent"), "auth": headers.get("authorization"),
                "type": headers.get("content-type") });
            (200, "application/json", echo.to_string().into_bytes(), None)
        }
        "/search" => {
            let q = query
                .split('&')
                .find_map(|p| p.strip_prefix("q="))
                .unwrap_or_default();
            let results = json!({ "results": [
                { "title": format!("About {q}"), "url": "https://example.com/a", "content": "first" },
                { "title": "Second", "url": "https://example.com/b", "content": "second" },
                { "title": "Third", "url": "https://example.com/c", "content": "third" } ] });
            (
                200,
                "application/json",
                results.to_string().into_bytes(),
                None,
            )
        }
        _ => (404, "text/plain", vec![], None),
    };
    let mut head = format!(
        "HTTP/1.1 {status} X\r\ncontent-type: {kind}\r\ncontent-length: {}\r\nconnection: close\r\n",
        content.len()
    );
    if let Some(location) = location {
        head.push_str(&format!("location: {location}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(&content);
}

fn temp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("regulus-web-it-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn core(profile: Profile, home: &std::path::Path) -> Arc<Core> {
    let mut profiles = ProfileStore::default();
    profiles.insert(profile).unwrap();
    let modules: Vec<Box<dyn Module>> = vec![Box::new(Web::new())];
    let storage = Storage {
        data_dir: Some(home.join("data")),
        ..Storage::default()
    };
    Core::build_with(modules, &[], profiles, storage)
}

fn start(core: &Core, profile: &str) -> String {
    let started = core
        .dispatch(method::EXECUTION_START, json!({ "profile_id": profile }))
        .unwrap();
    assert_eq!(started["state"]["status"], "running", "{started}");
    started["id"].as_str().unwrap().to_string()
}

fn send(core: &Core, id: &str, port: &str, value: Value) {
    core.dispatch(
        method::WORKFLOW_SEND,
        json!({ "execution_id": id, "port": port, "value": value }),
    )
    .unwrap();
}

fn emitted(core: &Core, id: &str, node: &str, port: &str) -> Vec<Value> {
    let page: EventPage = serde_json::from_value(
        core.dispatch(
            method::EVENT_LIST,
            json!({ "execution_id": id, "limit": 10000 }),
        )
        .unwrap(),
    )
    .unwrap();
    page.events
        .into_iter()
        .filter(|e| e.from.node_id == node && e.from.port_id == port)
        .filter_map(|e| match e.kind {
            BusEventKind::Emitted { value } => Some(value),
            _ => None,
        })
        .collect()
}

fn until(core: &Core, id: &str, node: &str, port: &str, count: usize) -> Vec<Value> {
    let deadline = Instant::now() + WAIT;
    loop {
        let got = emitted(core, id, node, port);
        if got.len() >= count {
            return got;
        }
        assert!(Instant::now() < deadline, "{node}.{port} got {got:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn the_example_gets_json_reads_a_page_saves_a_file_and_refuses_loopback() {
    let addr = serve();
    let source = include_str!("../examples/web.toml").replace("127.0.0.1:8765", &addr);
    let profile: Profile = toml::from_str(&source).unwrap();
    let home = temp("example");
    let core = core(profile, &home);
    let id = start(&core, "web");

    send(&core, &id, "get", json!("data.json"));
    let got = until(&core, &id, "self", "json", 1);
    assert_eq!(got[0]["items"][1]["title"], "second");
    let status = until(&core, &id, "api", "status", 1);
    assert_eq!(
        status[0],
        json!({ "status": 200, "url": format!("http://{addr}/data.json") })
    );

    send(
        &core,
        &id,
        "page",
        json!(format!("http://{addr}/page.html")),
    );
    let chunks = until(&core, &id, "self", "chunks", 2);
    let whole: String = chunks.iter().map(|c| c.as_str().unwrap()).collect();
    assert!(
        whole.starts_with(
            "# Release notes\n\nVersion **0.1** adds the [web module](https://example.com/web)."
        ),
        "{whole}"
    );
    assert!(
        whole.contains("- HTTP requests\n- Readable fetch"),
        "{whole}"
    );
    assert!(
        !whole.contains("track()") && !whole.contains("color"),
        "{whole}"
    );
    assert!(chunks
        .iter()
        .all(|c| c.as_str().unwrap().chars().count() <= 100));

    send(
        &core,
        &id,
        "save",
        json!(format!("http://{addr}/data.json")),
    );
    let saved = until(&core, &id, "self", "saved", 1);
    let saved = PathBuf::from(saved[0].as_str().unwrap());
    assert!(saved.ends_with("data.json"));
    assert!(std::fs::read_to_string(&saved).unwrap().contains("regulus"));

    send(&core, &id, "guarded", json!("data.json"));
    let blocked = until(&core, &id, "self", "blocked", 1);
    assert!(
        blocked[0].as_str().unwrap().contains("private address"),
        "{blocked:?}"
    );
    assert!(emitted(&core, &id, "self", "errors").is_empty());
}

/// A profile of one workflow: each node gets `in` from `self.<node id>`.
fn profile(nodes: Vec<Value>, extra_wires: Vec<Value>) -> Profile {
    let mut wires = extra_wires;
    for node in &nodes {
        let id = node["id"].as_str().unwrap();
        if node["ports"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["id"] == "in")
        {
            wires.push(json!({ "id": format!("to-{id}"), "kind": "then",
                "from": { "node_id": "self", "port_id": id },
                "to": { "node_id": id, "port_id": "in" } }));
        }
    }
    let ports: Vec<Value> = nodes
        .iter()
        .filter(|n| n["ports"].as_array().unwrap().iter().any(|p| p["id"] == "in"))
        .map(|n| json!({ "id": n["id"], "capability": "text@1", "direction": "input", "kind": "then" }))
        .collect();
    serde_json::from_value(json!({ "id": "p", "main_workflow_id": "main",
        "workflows": [{ "id": "main", "kind": "main", "ports": ports, "nodes": nodes, "wires": wires }] }))
    .unwrap()
}

fn node(id: &str, kind: &str, config: Value, ports: &[(&str, &str, &str)]) -> Value {
    let ports: Vec<Value> = ports
        .iter()
        .map(|(id, capability, direction)| {
            let kind = if capability.starts_with("web.") {
                "uses"
            } else {
                "then"
            };
            json!({ "id": id, "capability": capability, "direction": direction, "kind": kind })
        })
        .collect();
    json!({ "id": id, "type": kind, "module_id": "regulus/web@0.1.0", "config": config, "ports": ports })
}

const IN: (&str, &str, &str) = ("in", "text@1", "input");
const ERR: (&str, &str, &str) = ("error", "text@1", "output");

#[test]
fn redirects_limits_posts_search_and_pdf() {
    let addr = serve();
    let open = json!({ "allow_private": "yes" });
    let with = |extra: Value| {
        let mut config = open.clone();
        config
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        config
    };
    let nodes = vec![
        node(
            "follow",
            "fetch",
            with(json!({ "format": "text" })),
            &[IN, ("out", "text@1", "output"), ERR],
        ),
        node(
            "looping",
            "fetch",
            with(json!({ "max_redirects": 3 })),
            &[IN, ("out", "text@1", "output"), ERR],
        ),
        node(
            "small",
            "fetch",
            with(json!({ "max_bytes": 100 })),
            &[IN, ("out", "text@1", "output"), ERR],
        ),
        node(
            "post",
            "http",
            with(
                json!({ "method": "POST", "url": format!("http://{addr}/echo"),
            "headers": { "authorization": "Bearer s3cret" } }),
            ),
            &[IN, ("out", "json@1", "output"), ERR],
        ),
        node(
            "missing",
            "http",
            with(json!({ "url": format!("http://{addr}/missing") })),
            &[IN, ("status", "json@1", "output"), ERR],
        ),
        node(
            "find",
            "search",
            json!({ "max_results": 2 }),
            &[
                ("engine", "web.search@1", "input"),
                IN,
                ("out", "json@1", "output"),
                ("text", "text@1", "output"),
                ERR,
            ],
        ),
        node(
            "searx",
            "searxng",
            with(json!({ "url": format!("http://{addr}/search") })),
            &[("engine", "web.search@1", "output")],
        ),
        node(
            "doc",
            "pdf",
            open.clone(),
            &[IN, ("out", "text@1", "output"), ERR],
        ),
        node(
            "plain",
            "download",
            with(json!({})),
            &[IN, ("path", "path@1", "output"), ERR],
        ),
    ];
    let engine = json!({ "id": "engine", "kind": "uses",
        "from": { "node_id": "searx", "port_id": "engine" },
        "to": { "node_id": "find", "port_id": "engine" } });
    let home = temp("nodes");
    let core = core(profile(nodes, vec![engine]), &home);
    let id = start(&core, "p");
    let url = |path: &str| json!(format!("http://{addr}{path}"));

    send(&core, &id, "follow", url("/redirect"));
    let text = until(&core, &id, "follow", "out", 1);
    assert!(
        text[0]
            .as_str()
            .unwrap()
            .starts_with("Release notes\n\nVersion 0.1"),
        "{text:?}"
    );

    send(&core, &id, "looping", url("/loop"));
    let error = until(&core, &id, "looping", "error", 1);
    assert!(
        error[0].as_str().unwrap().contains("more than 3 times"),
        "{error:?}"
    );

    send(&core, &id, "small", url("/big"));
    let error = until(&core, &id, "small", "error", 1);
    assert!(
        error[0].as_str().unwrap().contains("more than 100 bytes"),
        "{error:?}"
    );

    send(&core, &id, "post", json!({ "hello": "world" }));
    let echo = until(&core, &id, "post", "out", 1);
    assert_eq!(echo[0]["method"], "POST");
    assert_eq!(echo[0]["body"], "{\"hello\":\"world\"}");
    assert_eq!(echo[0]["type"], "application/json");
    assert_eq!(echo[0]["auth"], "Bearer s3cret");
    assert_eq!(
        echo[0]["ua"],
        format!("regulus/{}", env!("CARGO_PKG_VERSION"))
    );

    send(&core, &id, "missing", json!("x"));
    let error = until(&core, &id, "missing", "error", 1);
    assert!(
        error[0].as_str().unwrap().contains("answered 404"),
        "{error:?}"
    );
    assert_eq!(until(&core, &id, "missing", "status", 1)[0]["status"], 404);

    send(&core, &id, "find", json!("rust pdf"));
    let found = until(&core, &id, "find", "out", 1);
    assert_eq!(found[0].as_array().unwrap().len(), 2);
    assert_eq!(found[0][0]["title"], "About rust+pdf");
    let listed = until(&core, &id, "find", "text", 1);
    assert!(listed[0]
        .as_str()
        .unwrap()
        .starts_with("1. About rust+pdf\n   https://example.com/a"));

    send(&core, &id, "doc", url("/doc.pdf"));
    let text = until(&core, &id, "doc", "out", 1);
    assert!(
        text[0].as_str().unwrap().contains("Hello from a PDF"),
        "{text:?}"
    );
    // A path from the value stays under the node's folder.
    send(&core, &id, "doc", json!("/etc/hostname"));
    let error = until(&core, &id, "doc", "error", 1);
    assert!(
        error[0].as_str().unwrap().contains("allow_outside"),
        "{error:?}"
    );

    // Download defaults to https only.
    send(&core, &id, "plain", url("/data.json"));
    let error = until(&core, &id, "plain", "error", 1);
    assert!(
        error[0].as_str().unwrap().contains("allow_http"),
        "{error:?}"
    );
}
