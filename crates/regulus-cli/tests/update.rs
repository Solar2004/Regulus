//! `regulus update` end to end (docs/06-stack.md §5.4): a release folder holds a signed
//! manifest with a newer version and a fake program; a copy of `regulus` replaces itself
//! with it over `file://`. A manifest changed after signing, a wrong key, a wrong size or
//! checksum, a missing platform and a program that says another version all leave the
//! program as it was. One case goes over HTTP (`python3 -m http.server`) and is skipped,
//! with a word, when there is no `python3`.

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

/// A folder next to the test binary (same filesystem, so copies of `regulus` can be hard
/// links instead of 300 MB copies), removed when the test ends however it ends.
struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Self {
        let dir = Path::new(PROGRAM)
            .parent()
            .unwrap()
            .join(format!("regulus-update-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
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
    regulus_cli::update::sha256_hex(&std::fs::read(path).unwrap())
}

/// A new release key; returns its path and the `RW…` public key `keygen` printed.
fn keygen(path: &Path) -> String {
    let made = run(
        Path::new(PROGRAM),
        &["update", "keygen", "--out", path.to_str().unwrap()],
    );
    assert!(made.status.success(), "{}", text(&made));
    text(&made)
        .lines()
        .find_map(|l| l.strip_prefix("public_key = \""))
        .and_then(|l| l.strip_suffix('"'))
        .expect("keygen prints the public key")
        .to_string()
}

/// A copy of `regulus` in its own folder, with a home whose `[update]` points at `url`.
/// `extra` goes under `[update]` too.
fn install_copy(
    root: &Path,
    name: &str,
    url: &str,
    public_key: &str,
    extra: &str,
) -> (PathBuf, PathBuf) {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join("home")).unwrap();
    let exe = dir.join("regulus");
    // A hard link: replacing it swaps the directory entry, the original stays as it is.
    if std::fs::hard_link(PROGRAM, &exe).is_err() {
        std::fs::copy(PROGRAM, &exe).unwrap();
    }
    std::fs::write(
        dir.join("home/config.toml"),
        format!("[update]\nurl = \"{url}\"\npublic_key = \"{public_key}\"\n{extra}"),
    )
    .unwrap();
    (exe, dir.join("home"))
}

fn update(exe: &Path, home: &Path, args: &[&str]) -> Output {
    let mut all = vec!["--home", home.to_str().unwrap(), "update"];
    all.extend_from_slice(args);
    run(exe, &all)
}

/// Nothing of the copy changed: same bytes, no staged file left next to it.
fn assert_untouched(exe: &Path) {
    let same_inode = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let (copy, original) = (
                std::fs::metadata(exe).unwrap(),
                std::fs::metadata(PROGRAM).unwrap(),
            );
            copy.ino() == original.ino() && copy.dev() == original.dev()
        }
        #[cfg(not(unix))]
        false
    };
    if !same_inode {
        static ORIGINAL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
        let original = ORIGINAL.get_or_init(|| sha256(Path::new(PROGRAM)));
        assert_eq!(&sha256(exe), original);
    }
    let leftovers: Vec<_> = std::fs::read_dir(exe.parent().unwrap())
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(leftovers.is_empty(), "staged files left: {leftovers:?}");
}

fn write_manifest(channel: &Path, version: &str, platform: &str, sha: &str, size: u64) {
    let manifest = serde_json::json!({
        "version": version,
        "date": "2026-11-08T00:00:00Z",
        "channel": "stable",
        "files": [
            { "name": "regulus-new", "sha256": sha, "size": size,
              "platform": platform, "kind": "cli" }
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

/// A "new release" that says `version` to `--version`; returns its size and sha256.
fn write_binary(channel: &Path, version: &str) -> (u64, String) {
    let binary = channel.join("regulus-new");
    std::fs::write(&binary, format!("#!/bin/sh\necho \"regulus {version}\"\n")).unwrap();
    (std::fs::metadata(&binary).unwrap().len(), sha256(&binary))
}

/// The release folder with its key: `(key path, public key, channel folder)`.
fn release(root: &Path) -> (PathBuf, String, PathBuf) {
    let channel = root.join("site/stable");
    std::fs::create_dir_all(&channel).unwrap();
    let key = root.join("release.key");
    let public_key = keygen(&key);
    let platform = regulus_cli::update::platform();
    let (size, sha) = write_binary(&channel, NEW_VERSION);
    write_manifest(&channel, NEW_VERSION, &platform, &sha, size);
    sign(&key, &channel);
    (key, public_key, channel)
}

#[test]
fn update_replaces_the_program_only_with_a_signed_release() {
    let root = Root::new("file");
    let root = &root.0;
    let (key, public_key, channel) = release(root);
    let platform = regulus_cli::update::platform();
    let url = format!("file://{}", root.join("site").display());
    let (size, sha) = (
        std::fs::metadata(channel.join("regulus-new"))
            .unwrap()
            .len(),
        sha256(&channel.join("regulus-new")),
    );

    // keygen keeps a key that exists, and --force rewrites it owner-only.
    let refused = run(
        Path::new(PROGRAM),
        &["update", "keygen", "--out", key.to_str().unwrap()],
    );
    assert!(!refused.status.success(), "keygen overwrote a key");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        let spare = root.join("spare.key");
        std::fs::copy(&key, &spare).unwrap();
        let forced = run(
            Path::new(PROGRAM),
            &[
                "update",
                "keygen",
                "--out",
                spare.to_str().unwrap(),
                "--force",
            ],
        );
        assert!(forced.status.success(), "{}", text(&forced));
        let mode = std::fs::metadata(&spare).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "keygen --force left {mode:o}");
    }

    // Off without a URL, with a message that says what to set.
    let (exe, home) = install_copy(root, "off", "", "", "");
    let off = update(&exe, &home, &["--yes"]);
    assert!(!off.status.success());
    assert!(text(&off).contains("updates are off"), "{}", text(&off));

    // --check says what is there and changes nothing.
    let (exe, home) = install_copy(root, "good", &url, &public_key, "");
    let checked = update(&exe, &home, &["--check"]);
    assert!(checked.status.success(), "{}", text(&checked));
    assert!(text(&checked).contains(NEW_VERSION), "{}", text(&checked));
    assert!(text(&checked).contains("signed"), "{}", text(&checked));
    assert_untouched(&exe);

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

    // Without a public key nothing is installed; --check works but says so.
    let (exe, home) = install_copy(root, "nokey", &url, "", "");
    let unsigned = update(&exe, &home, &["--yes"]);
    assert!(!unsigned.status.success());
    assert!(
        text(&unsigned).contains("no release key"),
        "{}",
        text(&unsigned)
    );
    let unverified = update(&exe, &home, &["--check"]);
    assert!(unverified.status.success(), "{}", text(&unverified));
    assert!(
        text(&unverified).contains("unverified"),
        "{}",
        text(&unverified)
    );
    assert_untouched(&exe);

    // A public key that is not one is refused before anything else.
    let (exe, home) = install_copy(root, "badkey", &url, "not-a-key", "");
    let bad_key = update(&exe, &home, &["--yes"]);
    assert!(!bad_key.status.success());
    assert!(text(&bad_key).contains("public_key"), "{}", text(&bad_key));
    assert_untouched(&exe);

    // Another key's signature is refused.
    let other_public = keygen(&root.join("other.key"));
    let (exe, home) = install_copy(root, "otherkey", &url, &other_public, "");
    let wrong_key = update(&exe, &home, &["--yes"]);
    assert!(!wrong_key.status.success());
    assert!(
        text(&wrong_key).contains("signature"),
        "{}",
        text(&wrong_key)
    );
    assert_untouched(&exe);

    // Plain http needs allow_insecure.
    let (exe, home) = install_copy(root, "plain", "http://127.0.0.1:9/r", &public_key, "");
    let plain = update(&exe, &home, &["--check"]);
    assert!(!plain.status.success());
    assert!(text(&plain).contains("allow_insecure"), "{}", text(&plain));

    // The same version, or an older one, is "up to date": no download is even tried
    // (the manifest points at a file that is not there).
    let (exe, home) = install_copy(root, "same", &url, &public_key, "");
    let current = regulus_cli::update::current_version().to_string();
    for version in [current.as_str(), "0.0.1"] {
        write_manifest(&channel, version, &platform, &sha, size);
        std::fs::rename(channel.join("regulus-new"), channel.join("regulus-away")).unwrap();
        sign(&key, &channel);
        let same = update(&exe, &home, &["--yes"]);
        std::fs::rename(channel.join("regulus-away"), channel.join("regulus-new")).unwrap();
        assert!(same.status.success(), "{version}: {}", text(&same));
        assert!(
            text(&same).contains("up to date"),
            "{version}: {}",
            text(&same)
        );
        assert_untouched(&exe);
    }

    // No program for this platform: said, nothing changed.
    write_manifest(&channel, NEW_VERSION, "plan9-mips", &sha, size);
    sign(&key, &channel);
    let (exe, home) = install_copy(root, "platform", &url, &public_key, "");
    let missing = update(&exe, &home, &["--yes"]);
    assert!(!missing.status.success());
    assert!(
        text(&missing).contains("no program for"),
        "{}",
        text(&missing)
    );
    assert_untouched(&exe);

    // A manifest changed after signing (another checksum, same signature) is refused.
    write_manifest(&channel, NEW_VERSION, &platform, &"0".repeat(64), size);
    let (exe, home) = install_copy(root, "tampered", &url, &public_key, "");
    let tampered = update(&exe, &home, &["--yes"]);
    assert!(!tampered.status.success());
    assert!(text(&tampered).contains("signature"), "{}", text(&tampered));
    assert_untouched(&exe);

    // Signed but with the wrong checksum: the download is refused.
    sign(&key, &channel);
    let bad_sum = update(&exe, &home, &["--yes"]);
    assert!(!bad_sum.status.success());
    assert!(text(&bad_sum).contains("sha256"), "{}", text(&bad_sum));
    assert_untouched(&exe);

    // Signed but with the wrong size: refused before the checksum.
    write_manifest(&channel, NEW_VERSION, &platform, &sha, size + 1);
    sign(&key, &channel);
    let bad_size = update(&exe, &home, &["--yes"]);
    assert!(!bad_size.status.success());
    assert!(
        text(&bad_size).contains("the manifest says"),
        "{}",
        text(&bad_size)
    );
    assert_untouched(&exe);

    // Over max_bytes: refused without downloading.
    write_manifest(&channel, NEW_VERSION, &platform, &sha, size);
    sign(&key, &channel);
    let (exe, home) = install_copy(root, "small", &url, &public_key, "max_bytes = 4\n");
    let too_big = update(&exe, &home, &["--yes"]);
    assert!(!too_big.status.success());
    assert!(text(&too_big).contains("max_bytes"), "{}", text(&too_big));
    assert_untouched(&exe);

    // A program that says another version to --version is not put in place.
    let (lie_size, lie_sha) = write_binary(&channel, "1.2.3");
    write_manifest(&channel, NEW_VERSION, &platform, &lie_sha, lie_size);
    sign(&key, &channel);
    let (exe, home) = install_copy(root, "liar", &url, &public_key, "");
    let liar = update(&exe, &home, &["--yes"]);
    assert!(!liar.status.success());
    assert!(text(&liar).contains("nothing replaced"), "{}", text(&liar));
    assert_untouched(&exe);
}

#[test]
fn update_works_over_http_when_allowed() {
    let root = Root::new("http");
    let root = &root.0;
    let (_key, public_key, _channel) = release(root);
    let Some((_server, url)) = serve(&root.join("site")) else {
        eprintln!("skipped: no python3 to serve the release over HTTP");
        return;
    };

    // Plain http is off by default, even for --check.
    let (exe, home) = install_copy(root, "plain", &url, &public_key, "");
    let plain = update(&exe, &home, &["--check"]);
    assert!(!plain.status.success());
    assert!(text(&plain).contains("allow_insecure"), "{}", text(&plain));
    assert_untouched(&exe);

    // Allowed, the same release installs over the network.
    let (exe, home) = install_copy(
        root,
        "allowed",
        &url,
        &public_key,
        "allow_insecure = true\n",
    );
    let updated = update(&exe, &home, &["--yes"]);
    assert!(updated.status.success(), "{}", text(&updated));
    let version = run(&exe, &["--version"]);
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("regulus {NEW_VERSION}")
    );
}
