//! The repository's `templates/pxl.toml` (docs/13-blocks.md §5 row 1), applied through
//! the real `regulus` program and a real core in a temporary home: it writes a profile
//! that `profile.check` accepts and refuses to apply without `workdir`. Its renderer, the
//! python3 script embedded in the `render` gate, runs here as the gate runs it, on a
//! sprite that renders and on one that does not. No agent runs here.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use regulus_core::{local, Config};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const TEMPLATE: &str = "pxl";

fn templates_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../templates")
}

fn regulus(home: &Path, args: &[&str]) -> Output {
    Command::new(PROGRAM)
        .arg("--home")
        .arg(home)
        .args(args)
        .output()
        .unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
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
            let _ = Command::new("kill").arg(pid).status();
        }
    }
}

/// The wires of every workflow in `copy`, as `("node.port", "node.port")`, and the nodes
/// no wire touches, as `"workflow/node"`.
fn topology(copy: &str) -> (Vec<(String, String)>, Vec<String>) {
    let profile: toml::Value = toml::from_str(copy).unwrap();
    let end = |end: &toml::Value| {
        format!(
            "{}.{}",
            end["node_id"].as_str().unwrap(),
            end["port_id"].as_str().unwrap()
        )
    };
    let mut wires = Vec::new();
    let mut lonely = Vec::new();
    for workflow in profile["workflows"].as_array().unwrap() {
        let mine: Vec<(String, String)> = workflow["wires"]
            .as_array()
            .unwrap()
            .iter()
            .map(|wire| (end(&wire["from"]), end(&wire["to"])))
            .collect();
        for node in workflow["nodes"].as_array().unwrap() {
            let id = node["id"].as_str().unwrap();
            let prefix = format!("{id}.");
            if !mine
                .iter()
                .any(|(from, to)| from.starts_with(&prefix) || to.starts_with(&prefix))
            {
                lonely.push(format!("{}/{id}", workflow["id"].as_str().unwrap()));
            }
        }
        wires.extend(mine);
    }
    (wires, lonely)
}

fn assert_wired(copy: &str, expected: &[(&str, &str)]) {
    let (wires, lonely) = topology(copy);
    assert!(lonely.is_empty(), "nodes without a wire: {lonely:?}");
    for (from, to) in expected {
        assert!(
            wires.iter().any(|(f, t)| f == from && t == to),
            "no wire {from} → {to}"
        );
    }
}

/// The `render` node's config in the applied profile.
fn render_config(copy: &str) -> toml::Value {
    let profile: toml::Value = toml::from_str(copy).unwrap();
    let nodes = profile["workflows"][0]["nodes"].as_array().unwrap();
    let render = nodes
        .iter()
        .find(|n| n["id"].as_str() == Some("render"))
        .expect("a render node");
    render["config"].clone()
}

/// Runs the renderer as the gate does: its command and args, in `dir`.
fn render(config: &toml::Value, dir: &Path) -> Output {
    render_with(config, dir, &[])
}

/// The renderer with some of its arguments replaced: `(index in args, value)`. After the
/// script: sprite_file, output, scale, max_size, max_pixels.
fn render_with(config: &toml::Value, dir: &Path, changed: &[(usize, &str)]) -> Output {
    let mut args: Vec<String> = config["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().to_string())
        .collect();
    for (at, value) in changed {
        args[*at] = value.to_string();
    }
    Command::new(config["command"].as_str().unwrap())
        .args(&args)
        .current_dir(dir)
        .output()
        .unwrap()
}

/// Decodes a PNG with python3 (which the template needs anyway; no Rust decoder in the
/// workspace): every chunk's CRC checked, then the RGBA of pixel (x, y) for each `at`.
fn decode(png: &Path, at: &[(u32, u32)]) -> String {
    const DECODER: &str = r#"
import sys, zlib, struct
data = open(sys.argv[1], "rb").read()
pos, idat, width = 8, b"", 0
while pos < len(data):
    size, kind = struct.unpack(">I4s", data[pos:pos + 8])
    body = data[pos + 8:pos + 8 + size]
    crc = struct.unpack(">I", data[pos + 8 + size:pos + 12 + size])[0]
    assert zlib.crc32(kind + body) & 0xFFFFFFFF == crc, kind
    if kind == b"IHDR":
        width = struct.unpack(">I", body[:4])[0]
    if kind == b"IDAT":
        idat += body
    pos += 12 + size
raw = zlib.decompress(idat)
row = 1 + 4 * width
out = ["crc ok"]
for arg in sys.argv[2:]:
    x, y = map(int, arg.split(","))
    assert raw[y * row] == 0
    out.append(",".join(str(b) for b in raw[y * row + 1 + 4 * x:y * row + 5 + 4 * x]))
print(" ".join(out))
"#;
    let points: Vec<String> = at.iter().map(|(x, y)| format!("{x},{y}")).collect();
    let out = Command::new("python3")
        .arg("-c")
        .arg(DECODER)
        .arg(png)
        .args(&points)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", text(&out));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn steps(home: &Path) {
    let work = home.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let workdir = format!("workdir={}", work.display());

    let ls = regulus(home, &["template", "ls"]);
    assert!(ls.status.success(), "{}", text(&ls));
    assert!(text(&ls).contains("[needs: workdir]"), "{}", text(&ls));

    let out = regulus(home, &["template", "apply", TEMPLATE]);
    assert!(!out.status.success());
    assert!(text(&out).contains("`workdir`"), "{}", text(&out));
    assert!(!home.join("profiles/pxl.toml").exists());

    let applied = regulus(
        home,
        &[
            "template",
            "apply",
            TEMPLATE,
            "--set",
            &workdir,
            "--set",
            "scale=4",
            "--set",
            "turn_timeout_ms=60000",
        ],
    );
    assert!(applied.status.success(), "{}", text(&applied));
    let copy = std::fs::read_to_string(home.join("profiles/pxl.toml")).unwrap();
    assert!(
        copy.contains(r#"model = "opencode/glm-5.3-flash-free""#),
        "{copy}"
    );
    assert!(copy.contains("turn_timeout_ms = 60000"), "{copy}");
    assert!(
        copy.contains(r#"to = { node_id = "judge", port_id = "in" }"#),
        "{copy}"
    );
    assert!(!copy.contains("\n[template]"), "{copy}");

    assert_wired(
        &copy,
        &[
            ("self.go", "task.set"),
            ("first.out", "loop.in"),
            ("loop.body", "join.feedback"),
            ("compose.out", "agent.prompt"),
            ("engine.engine", "agent.engine"),
            ("agent.reply", "render.in"),
            ("render.exit", "rendered.in"),
            ("rendered.bad", "render_report.in"),
            ("render_report.out", "loop.back"),
            ("rendered.ok", "sprite.in"),
            ("preview.out", "judge.in"),
            ("judge_provider.endpoint", "judge.model"),
            ("verdict.out", "loop.back"),
            ("human.out", "notes.in"),
            ("human_fail.out", "loop.back"),
            ("human_rejected.out", "loop.back"),
            ("agent_report.out", "loop.back"),
            ("loop.done", "png_path.in"),
            ("png_path.out", "self.png"),
            ("viewer.started", "self.serving"),
        ],
    );

    let check = regulus(home, &["call", "profile.check", r#"{"id":"pxl"}"#]);
    assert!(check.status.success(), "{}", text(&check));
    assert_eq!(
        String::from_utf8_lossy(&check.stdout).trim(),
        "[]",
        "profile.check: {}",
        text(&check)
    );

    // A person instead of the judge: the preview goes to the review node.
    let human = regulus(
        home,
        &[
            "template",
            "apply",
            TEMPLATE,
            "--set",
            &workdir,
            "--set",
            "review=human",
            "--id",
            "pxl-human",
        ],
    );
    assert!(human.status.success(), "{}", text(&human));
    let copy_human = std::fs::read_to_string(home.join("profiles/pxl-human.toml")).unwrap();
    assert!(
        copy_human.contains(r#"to = { node_id = "human", port_id = "in" }"#),
        "{copy_human}"
    );
    assert_wired(
        &copy_human,
        &[
            ("preview.out", "human.in"),
            ("human.out", "notes.in"),
            ("notes.pass", "human_pass.in"),
            ("notes.fail", "human_fail.in"),
            ("human.rejected", "human_rejected.in"),
            ("human_pass.out", "loop.back"),
            ("human.pending", "self.pending"),
        ],
    );
    let check = regulus(home, &["call", "profile.check", r#"{"id":"pxl-human"}"#]);
    assert_eq!(
        String::from_utf8_lossy(&check.stdout).trim(),
        "[]",
        "profile.check: {}",
        text(&check)
    );

    // The renderer: a 4x4 sprite at scale 4 is a 16x16 PNG.
    let config = render_config(&copy);
    assert_eq!(config["cwd"].as_str(), Some(work.to_str().unwrap()));
    std::fs::write(
        work.join("sprite.px.txt"),
        "r=#e03030\nk=#401010\n\n.kk.\nkrrk\nkrrk\n.kk.\n",
    )
    .unwrap();
    let out = render(&config, &work);
    assert!(out.status.success(), "{}", text(&out));
    assert!(text(&out).contains("4x4 pixels"), "{}", text(&out));
    let png = std::fs::read(work.join("sprite.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert_eq!(&png[12..16], b"IHDR");
    let side = |at: usize| u32::from_be_bytes(png[at..at + 4].try_into().unwrap());
    assert_eq!((side(16), side(20)), (16, 16));
    // Every CRC holds; `.` is transparent, `k` is #401010, each pixel 4 x 4.
    assert_eq!(
        decode(
            &work.join("sprite.png"),
            &[(0, 0), (3, 3), (4, 0), (7, 3), (8, 4)]
        ),
        "crc ok 0,0,0,0 0,0,0,0 64,16,16,255 64,16,16,255 224,48,48,255"
    );

    // A ragged grid fails with a reason, the next turn's feedback.
    std::fs::write(work.join("sprite.px.txt"), "r=#e03030\n\nrr\nr\n").unwrap();
    let out = render(&config, &work);
    assert_eq!(out.status.code(), Some(1), "{}", text(&out));
    assert!(
        text(&out).contains("render failed: grid row 2 has 1 pixels"),
        "{}",
        text(&out)
    );
    // So does a letter with no color.
    std::fs::write(work.join("sprite.px.txt"), "r=#e03030\n\nrx\n").unwrap();
    let out = render(&config, &work);
    assert_eq!(out.status.code(), Some(1));
    assert!(text(&out).contains("'x' has no color"), "{}", text(&out));

    // Bounds: a side past `max_size`, a scale of 0, a PNG past `max_pixels` (at once,
    // not after building it), and paths that leave `workdir`.
    let wide = format!("r=#e03030\n\n{}\n", "r".repeat(65));
    std::fs::write(work.join("sprite.px.txt"), wide).unwrap();
    let refused = |changed: &[(usize, &str)], why: &str| {
        let began = std::time::Instant::now();
        let out = render_with(&config, &work, changed);
        assert_eq!(out.status.code(), Some(1), "{}", text(&out));
        assert!(text(&out).contains(why), "{why}: {}", text(&out));
        assert!(began.elapsed() < std::time::Duration::from_secs(10));
    };
    refused(&[], "the grid is 65x1, above max_size 64");
    std::fs::write(work.join("sprite.px.txt"), "r=#e03030\n\nrr\n").unwrap();
    refused(&[(4, "0")], "must be at least 1");
    refused(&[(4, "100000")], "above max_pixels 16000000");
    refused(&[(3, "../escape.png")], "leaves workdir");
    refused(
        &[(3, "/tmp/escape.png")],
        "must be a path relative to workdir",
    );
    refused(&[(2, "../../etc/passwd")], "leaves workdir");
    assert!(!home.join("escape.png").exists());
    assert_eq!(config["timeout_ms"].as_integer(), Some(60000));
}

#[test]
fn the_pxl_template_applies_checks_and_renders() {
    let home = std::env::temp_dir().join(format!("regulus-template-pxl-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let config = format!(
        "[server]\nport = 0\n\n[templates]\npaths = [{:?}]\n",
        templates_dir().display().to_string()
    );
    std::fs::write(home.join("config.toml"), config).unwrap();
    let config = Config::load(&home).unwrap();
    let started = local::start(&config, Path::new(PROGRAM));
    let result = std::panic::catch_unwind(|| {
        started.unwrap();
        steps(&home);
    });
    stop_cores(&home);
    let _ = std::fs::remove_dir_all(&home);
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}
