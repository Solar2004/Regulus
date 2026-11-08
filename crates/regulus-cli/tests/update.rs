//! `regulus update` end to end (docs/06-stack.md §5.4): a local HTTP server
//! (`python3 -m http.server`) serves a signed manifest with a newer version and a fake
//! program; a copy of `regulus` replaces itself with it. A manifest changed after signing,
//! a wrong key and a wrong checksum all leave the program as it was.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};

const PROGRAM: &str = env!("CARGO_BIN_EXE_regulus");
const NEW_VERSION: &str = "9.9.9";

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Serves `root` on a free port; `None` when there is no `python3`.
fn serve(root: &Path) -> Option<(Server, String)> {
    let mut child = Command::new("python3")
        .args(["-u", "-m", "http.server", "--bind", "127.0.0.1", "0"])
        .current_dir(root)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut line = String::new();
    BufReader::new(child.stdout.take()?)
        .read_line(&mut line)
        .ok()?;
    // "Serving HTTP on 127.0.0.1 port 41234 (http://127.0.0.1:41234/) ..."
    let port = line.split(" port ").nth(1)?.split_whitespace().next()?;
    Some((Server(child), format!("http://127.0.0.1:{port}")))
}

fn run(program: &Path, args: &[&str]) -> Output {
    Command::new(program).args(args).output().unwrap()
}

fn text(output: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

fn sha256(path: &Path) -> String {
    let out = Command::new("sha256sum").arg(path).output().unwrap();
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap()
        .to_string()
}

/// A copy of `regulus` in its own folder, with a home whose `[update]` points at `url`.
fn install_copy(root: &Path, name: &str, url: &str, public_key: &str) -> (PathBuf, PathBuf) {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let exe = dir.join("regulus");
    std::fs::copy(PROGRAM, &exe).unwrap();
    std::fs::write(
        dir.join("home/config.toml"),
        format!("[update]\nurl = \"{url}\"\npublic_key = \"{public_key}\"\n"),
    )
    .unwrap();
    (exe, dir.join("home"))
}

fn update(exe: &Path, home: &Path, args: &[&str]) -> Output {
    let mut all = vec!["--home", home.to_str().unwrap(), "update"];
    all.extend_from_slice(args);
    run(exe, &all)
}

fn write_manifest(channel: &Path, sha: &str, size: u64) {
    let manifest = serde_json::json!({
        "version": NEW_VERSION,
        "date": "2026-11-08T00:00:00Z",
        "channel": "stable",
        "files": [
            { "name": "regulus-new", "sha256": sha, "size": size,
              "platform": regulus_cli::update::platform(), "kind": "cli" }
        ]
    });
    std::fs::write(
        channel.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn sign(key: &Path, channel: &Path) {
    let manifest = channel.join("manifest.json");
    let signed = run(
        Path::new(PROGRAM),
        &[
            "update",
            "sign",
            "--key",
            key.to_str().unwrap(),
            "--release-version",
            NEW_VERSION,
            manifest.to_str().unwrap(),
        ],
    );
    assert!(signed.status.success(), "{}", text(&signed));
}

#[test]
fn update_replaces_the_program_only_with_a_signed_release() {
    let root = std::env::temp_dir().join(format!("regulus-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let site = root.join("site");
    let channel = site.join("stable");
    std::fs::create_dir_all(&channel).unwrap();

    // The release key, made the way a release machine makes it.
    let key = root.join("release.key");
    let made = run(
        Path::new(PROGRAM),
        &["update", "keygen", "--out", key.to_str().unwrap()],
    );
    assert!(made.status.success(), "{}", text(&made));
    let public_key = text(&made)
        .lines()
        .find_map(|l| l.strip_prefix("public_key = \""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("keygen prints the public key")
        .to_string();
    let refused = run(
        Path::new(PROGRAM),
        &["update", "keygen", "--out", key.to_str().unwrap()],
    );
    assert!(!refused.status.success(), "keygen overwrote a key");

    // The "new release": a program that says it is 9.9.9.
    let binary = channel.join("regulus-new");
    std::fs::write(
        &binary,
        format!("#!/bin/sh\necho \"regulus {NEW_VERSION}\"\n"),
    )
    .unwrap();
    let size = std::fs::metadata(&binary).unwrap().len();
    write_manifest(&channel, &sha256(&binary), size);
    sign(&key, &channel);

    let Some((_server, url)) = serve(&site) else {
        eprintln!("skipped: no python3 to serve the release");
        return;
    };

    // Off without a URL, with a message that says what to set.
    let (exe, home) = install_copy(&root, "off", "", "");
    let off = update(&exe, &home, &["--yes"]);
    assert!(!off.status.success());
    assert!(text(&off).contains("updates are off"), "{}", text(&off));

    // --check says what is there and changes nothing.
    let (exe, home) = install_copy(&root, "good", &url, &public_key);
    let checked = update(&exe, &home, &["--check"]);
    assert!(checked.status.success(), "{}", text(&checked));
    assert!(text(&checked).contains(NEW_VERSION), "{}", text(&checked));
    assert!(text(&checked).contains("signed"), "{}", text(&checked));
    assert_eq!(sha256(&exe), sha256(Path::new(PROGRAM)));

    // --yes installs it: the copy is now the new program.
    let updated = update(&exe, &home, &["--yes"]);
    assert!(updated.status.success(), "{}", text(&updated));
    let version = run(&exe, &["--version"]);
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("regulus {NEW_VERSION}")
    );
    let leftovers: Vec<_> = std::fs::read_dir(exe.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "staged files left: {leftovers:?}");

    // Without a public key nothing is installed.
    let (exe, home) = install_copy(&root, "nokey", &url, "");
    let unsigned = update(&exe, &home, &["--yes"]);
    assert!(!unsigned.status.success());
    assert!(
        text(&unsigned).contains("no release key"),
        "{}",
        text(&unsigned)
    );
    assert_eq!(sha256(&exe), sha256(Path::new(PROGRAM)));

    // Another key's signature is refused.
    let other = root.join("other.key");
    let made = run(
        Path::new(PROGRAM),
        &["update", "keygen", "--out", other.to_str().unwrap()],
    );
    let other_public = text(&made)
        .lines()
        .find_map(|l| l.strip_prefix("public_key = \""))
        .and_then(|l| l.strip_suffix('"'))
        .unwrap()
        .to_string();
    let (exe, home) = install_copy(&root, "otherkey", &url, &other_public);
    let wrong_key = update(&exe, &home, &["--yes"]);
    assert!(!wrong_key.status.success());
    assert!(
        text(&wrong_key).contains("signature"),
        "{}",
        text(&wrong_key)
    );
    assert_eq!(sha256(&exe), sha256(Path::new(PROGRAM)));

    // A manifest changed after signing (another checksum, same signature) is refused.
    write_manifest(&channel, &"0".repeat(64), size);
    let (exe, home) = install_copy(&root, "tampered", &url, &public_key);
    let tampered = update(&exe, &home, &["--yes"]);
    assert!(!tampered.status.success());
    assert!(text(&tampered).contains("signature"), "{}", text(&tampered));
    assert_eq!(sha256(&exe), sha256(Path::new(PROGRAM)));

    // Signed but with the wrong checksum: the download is refused.
    sign(&key, &channel);
    let bad_sum = update(&exe, &home, &["--yes"]);
    assert!(!bad_sum.status.success());
    assert!(text(&bad_sum).contains("sha256"), "{}", text(&bad_sum));
    assert_eq!(sha256(&exe), sha256(Path::new(PROGRAM)));

    let _ = std::fs::remove_dir_all(&root);
}
