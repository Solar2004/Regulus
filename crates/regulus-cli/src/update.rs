//! `regulus update`: find a newer release, check it was signed by the release key, and
//! put it in place of the running program (docs/06-stack.md §5.4).
//!
//! Keys and signatures use minisign's format (Ed25519, pre-hashed with BLAKE2b-512), so
//! one key pair signs the manifest the CLI reads and the bundles the app's updater reads:
//! Tauri's updater verifies minisign signatures.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use blake2::{Blake2b512, Digest};
use ed25519_dalek::{Signer, SigningKey};
use regulus_core::config::UpdateConfig;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

/// The file every channel folder holds, and its signature next to it.
pub const MANIFEST_FILE: &str = "manifest.json";
pub const SIGNATURE_SUFFIX: &str = ".sig";
/// What a manifest entry for the command-line program says in `kind`.
pub const KIND_CLI: &str = "cli";
/// The environment variable `regulus update sign` reads the secret key from.
pub const KEY_ENV: &str = "REGULUS_SIGNING_KEY";
/// A manifest is a few hundred bytes; more than this is not a manifest.
const MANIFEST_MAX_BYTES: u64 = 1 << 20;
/// How long the downloaded program may take to answer `--version` before it is refused.
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
/// Algorithm tags of minisign: `Ed` for keys, `ED` for pre-hashed signatures.
const KEY_ALGORITHM: &[u8; 2] = b"Ed";
const SIGNATURE_ALGORITHM: &[u8; 2] = b"ED";
const KEY_ID_LEN: usize = 8;
const SEED_LEN: usize = 32;
const TRUSTED_PREFIX: &str = "trusted comment: ";
const UNTRUSTED_PREFIX: &str = "untrusted comment: ";

/// What `release.sh` writes and `regulus update` reads: the release of one channel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    #[serde(default)]
    pub date: String,
    #[serde(default)]
    pub channel: String,
    pub files: Vec<FileEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileEntry {
    pub name: String,
    pub sha256: String,
    pub size: u64,
    /// `<os>-<arch>` as Rust names them (`linux-x86_64`).
    pub platform: String,
    /// `cli` for the `regulus` program, `app` for a desktop bundle; absent means `cli`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Where to download it, absolute or relative to the manifest; absent means `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// The platform this program was built for, as manifests name it.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// The running program's version.
pub fn current_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).expect("the package version is semver")
}

/// Where updates come from, after `config.toml` and the command's flags.
#[derive(Debug, Clone)]
pub struct Source {
    pub url: String,
    pub channel: String,
    pub public_key: String,
    /// `None`: no limit (`timeout_ms = 0`).
    pub timeout: Option<Duration>,
    /// The most one release file may weigh.
    pub max_bytes: u64,
    /// Plain `http://` is taken only when `[update] allow_insecure` says so.
    pub allow_insecure: bool,
}

impl Source {
    /// `[update]` with the flags on top. Fails with what to set when updating is off.
    pub fn new(
        config: &UpdateConfig,
        config_path: &Path,
        url: Option<String>,
        channel: Option<String>,
    ) -> Result<Self> {
        let url = url.unwrap_or_else(|| config.url.clone());
        let url = url.trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            bail!(
                "updates are off: set `url` under [update] in {} (docs/06-stack.md §5.4)",
                config_path.display()
            );
        }
        config.url_allowed(&url).map_err(anyhow::Error::msg)?;
        let channel = channel.unwrap_or_else(|| config.channel.clone());
        if !channel_valid(&channel) {
            bail!("channel `{channel}`: use letters, digits, `.`, `-` or `_`");
        }
        Ok(Self {
            url,
            channel,
            public_key: config.public_key.trim().to_string(),
            timeout: config.timeout(),
            max_bytes: config.max_bytes,
            allow_insecure: config.allow_insecure,
        })
    }

    pub fn manifest_url(&self) -> String {
        format!("{}/{}/{MANIFEST_FILE}", self.url, self.channel)
    }

    /// Where an entry is downloaded from.
    pub fn file_url(&self, entry: &FileEntry) -> String {
        let target = entry.url.as_deref().unwrap_or(&entry.name);
        if target.contains("://") || target.starts_with('/') {
            target.to_string()
        } else {
            format!("{}/{}/{target}", self.url, self.channel)
        }
    }
}

/// A channel names a folder under the base URL: one path segment.
pub fn channel_valid(channel: &str) -> bool {
    !channel.is_empty()
        && channel != "."
        && channel != ".."
        && channel
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
}

/// What the channel offers, and whether its signature was checked.
#[derive(Debug, Clone)]
pub struct Offer {
    pub manifest: Manifest,
    pub version: semver::Version,
    pub signed: bool,
}

impl Offer {
    pub fn newer_than(&self, version: &semver::Version) -> bool {
        self.version > *version
    }

    /// The entry for this program on this platform.
    pub fn binary(&self, platform: &str) -> Option<&FileEntry> {
        self.manifest
            .files
            .iter()
            .find(|f| f.platform == platform && f.kind.as_deref().unwrap_or(KIND_CLI) == KIND_CLI)
    }
}

/// Reads the channel's manifest. With a public key, its signature must hold; without
/// one, `require_signature` decides whether an unsigned read is an error.
pub fn fetch_offer(source: &Source, require_signature: bool) -> Result<Offer> {
    let url = source.manifest_url();
    let bytes = fetch(source, &url, MANIFEST_MAX_BYTES)?;
    let signed = if source.public_key.is_empty() {
        if require_signature {
            bail!(
                "no release key: set `public_key` under [update] to the key `regulus update keygen` printed; nothing unsigned is installed"
            );
        }
        false
    } else {
        let sig_url = format!("{url}{SIGNATURE_SUFFIX}");
        let signature = fetch(source, &sig_url, MANIFEST_MAX_BYTES)?;
        let signature = String::from_utf8(signature).context("the signature is not text")?;
        verify(&source.public_key, &bytes, &signature)
            .with_context(|| format!("{url}: the signature does not match the release key"))?;
        true
    };
    let manifest: Manifest =
        serde_json::from_slice(&bytes).with_context(|| format!("{url}: not a manifest"))?;
    // A signed manifest must name its channel: otherwise one signed for `beta` could be
    // copied into `stable` and still verify.
    if signed && manifest.channel.is_empty() {
        bail!("{url}: the signed manifest names no channel; release.sh writes `channel`");
    }
    if !manifest.channel.is_empty() && manifest.channel != source.channel {
        bail!(
            "{url}: the manifest is for channel `{}`, not `{}`",
            manifest.channel,
            source.channel
        );
    }
    let version = semver::Version::parse(manifest.version.trim_start_matches('v'))
        .with_context(|| format!("{url}: version `{}` is not semver", manifest.version))?;
    Ok(Offer {
        manifest,
        version,
        signed,
    })
}

/// Downloads the entry and checks its size and SHA-256 against the (signed) manifest.
pub fn download(source: &Source, entry: &FileEntry) -> Result<Vec<u8>> {
    let url = source.file_url(entry);
    if url.starts_with("http://") && !source.allow_insecure {
        bail!("{url}: plain http is off; set `allow_insecure = true` under [update]");
    }
    if entry.size > source.max_bytes {
        bail!(
            "{url}: the manifest says {} bytes, more than `max_bytes` ({}) under [update]",
            entry.size,
            source.max_bytes
        );
    }
    let bytes = fetch(source, &url, entry.size)?;
    if bytes.len() as u64 != entry.size {
        bail!(
            "{url}: {} bytes, the manifest says {}",
            bytes.len(),
            entry.size
        );
    }
    let sum = sha256_hex(&bytes);
    if !sum.eq_ignore_ascii_case(entry.sha256.trim()) {
        bail!("{url}: sha256 {sum}, the manifest says {}", entry.sha256);
    }
    Ok(bytes)
}

/// `http(s)://` through the network, `file://` or a plain path from disk. At most
/// `max` bytes are read; a longer body is an error.
fn fetch(source: &Source, url: &str, max: u64) -> Result<Vec<u8>> {
    if url.starts_with("http://") || url.starts_with("https://") {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(source.timeout)
            .user_agent(concat!("regulus/", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        let mut response = agent
            .get(url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        let mut body = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(max.saturating_add(1))
            .read_to_end(&mut body)
            .with_context(|| format!("GET {url}"))?;
        if body.len() as u64 > max {
            bail!("{url}: longer than {max} bytes");
        }
        return Ok(body);
    }
    let path = url.strip_prefix("file://").unwrap_or(url);
    let mut body = Vec::new();
    std::fs::File::open(path)
        .with_context(|| format!("cannot read {path}"))?
        .take(max.saturating_add(1))
        .read_to_end(&mut body)
        .with_context(|| format!("cannot read {path}"))?;
    if body.len() as u64 > max {
        bail!("{path}: longer than {max} bytes");
    }
    Ok(body)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    <Sha256 as sha2::Digest>::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Puts `bytes` in place of `exe`: written next to it, checked to run and say `version`,
/// then renamed over it, so a failure at any step leaves the old program as it was.
pub fn install(exe: &Path, bytes: &[u8], version: &semver::Version) -> Result<()> {
    let dir = exe
        .parent()
        .with_context(|| format!("{} has no folder", exe.display()))?;
    let staged = Staged::create(dir, exe, bytes)?;
    probe_version(&staged.path, version)?;
    #[cfg(unix)]
    {
        // Staged as 0o700; it takes the old program's mode when it becomes the program.
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(exe)
            .map(|m| m.permissions().mode())
            .unwrap_or(0o755);
        std::fs::set_permissions(&staged.path, std::fs::Permissions::from_mode(mode))?;
    }
    if exe == std::env::current_exe().unwrap_or_default() {
        self_replace::self_replace(&staged.path)
    } else {
        std::fs::rename(&staged.path, exe)
    }
    .with_context(|| format!("cannot replace {}", exe.display()))
}

/// The downloaded program next to `exe`, under a name nobody can guess, created fresh
/// (never a file or symlink already there) and readable by its owner only. Removed when
/// dropped, unless it was renamed into place.
struct Staged {
    path: PathBuf,
}

impl Staged {
    fn create(dir: &Path, exe: &Path, bytes: &[u8]) -> Result<Self> {
        let mut nonce = [0u8; 8];
        getrandom::fill(&mut nonce).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
        let suffix: String = nonce.iter().map(|b| format!("{b:02x}")).collect();
        let path = dir.join(format!(".regulus-update-{suffix}"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o700);
        let mut file = options.open(&path).with_context(|| {
            format!(
                "cannot write next to {}: updating needs a folder you can write",
                exe.display()
            )
        })?;
        let staged = Self { path };
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .with_context(|| format!("cannot write {}", staged.path.display()))?;
        Ok(staged)
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Runs the downloaded program with `--version`: it has to start here and say `version`.
/// No stdin, and a program that hangs is killed and refused.
fn probe_version(program: &Path, version: &semver::Version) -> Result<()> {
    let mut child = std::process::Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .context("the new program does not start")?;
    let mut stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut said = String::new();
        if let Some(out) = stdout.as_mut() {
            let _ = out.take(4096).read_to_string(&mut said);
        }
        said
    });
    let deadline = Instant::now() + PROBE_TIMEOUT;
    let status = loop {
        if let Some(status) = child.try_wait().context("the new program does not start")? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            bail!(
                "the new program did not answer --version in {} s; nothing replaced",
                PROBE_TIMEOUT.as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let said = reader.join().unwrap_or_default();
    if !status.success() || !said.split_whitespace().any(|w| w == version.to_string()) {
        bail!(
            "the new program says `{}` to --version, not {version}; nothing replaced",
            said.trim()
        );
    }
    Ok(())
}

/// A new key pair: the secret key file's text and the public key (`RW…` line).
pub struct KeyPair {
    pub secret_file: String,
    pub public_key: String,
    pub key_id: String,
}

pub fn keygen() -> Result<KeyPair> {
    let mut id = [0u8; KEY_ID_LEN];
    let mut seed = [0u8; SEED_LEN];
    getrandom::fill(&mut id).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    getrandom::fill(&mut seed).map_err(|e| anyhow::anyhow!("no randomness: {e}"))?;
    let key = SecretKey { id, seed };
    let mut secret = KEY_ALGORITHM.to_vec();
    secret.extend_from_slice(&id);
    secret.extend_from_slice(&seed);
    let key_id = key.id_hex();
    Ok(KeyPair {
        secret_file: format!(
            "{UNTRUSTED_PREFIX}regulus update secret key {key_id}\n{}\n",
            BASE64.encode(secret)
        ),
        public_key: key.public_key(),
        key_id,
    })
}

/// A secret key as `keygen` writes it.
pub struct SecretKey {
    id: [u8; KEY_ID_LEN],
    seed: [u8; SEED_LEN],
}

impl SecretKey {
    /// Reads the key file's text (the comment line is optional).
    pub fn parse(text: &str) -> Result<Self> {
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with(UNTRUSTED_PREFIX))
            .context("the key file is empty")?;
        let bytes = BASE64
            .decode(line)
            .context("the key is not base64 (made by `regulus update keygen`?)")?;
        if bytes.len() != 2 + KEY_ID_LEN + SEED_LEN || &bytes[..2] != KEY_ALGORITHM {
            bail!("not a key made by `regulus update keygen`");
        }
        let mut id = [0u8; KEY_ID_LEN];
        let mut seed = [0u8; SEED_LEN];
        id.copy_from_slice(&bytes[2..2 + KEY_ID_LEN]);
        seed.copy_from_slice(&bytes[2 + KEY_ID_LEN..]);
        Ok(Self { id, seed })
    }

    fn id_hex(&self) -> String {
        // minisign prints the key id as a little-endian number.
        self.id.iter().rev().map(|b| format!("{b:02X}")).collect()
    }

    /// The minisign public key line (`RW…`): what `[update] public_key` holds.
    pub fn public_key(&self) -> String {
        let verifying = SigningKey::from_bytes(&self.seed).verifying_key();
        let mut bytes = KEY_ALGORITHM.to_vec();
        bytes.extend_from_slice(&self.id);
        bytes.extend_from_slice(&verifying.to_bytes());
        BASE64.encode(bytes)
    }

    /// A minisign signature of `data` (pre-hashed), with `comment` as its trusted
    /// comment: what a `.sig` file holds.
    pub fn sign(&self, data: &[u8], comment: &str) -> String {
        let key = SigningKey::from_bytes(&self.seed);
        let signature = key.sign(&Blake2b512::digest(data)).to_bytes();
        let mut global = signature.to_vec();
        global.extend_from_slice(comment.as_bytes());
        let global = key.sign(&global).to_bytes();
        let mut line = SIGNATURE_ALGORITHM.to_vec();
        line.extend_from_slice(&self.id);
        line.extend_from_slice(&signature);
        format!(
            "{UNTRUSTED_PREFIX}signature from regulus update secret key {}\n{}\n{TRUSTED_PREFIX}{comment}\n{}\n",
            self.id_hex(),
            BASE64.encode(line),
            BASE64.encode(global)
        )
    }
}

/// The trusted comment `sign` writes: Tauri's updater checks `version:` against the
/// version its `latest.json` announces.
pub fn trusted_comment(file: &str, version: Option<&str>) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let mut comment = format!("timestamp:{now}\tfile:{file}");
    if let Some(version) = version {
        comment.push_str(&format!("\tversion:{version}"));
    }
    comment
}

/// Checks a minisign signature of `data` against a public key line (`RW…`). Only
/// pre-hashed signatures are taken.
pub fn verify(public_key: &str, data: &[u8], signature: &str) -> Result<()> {
    let key = minisign_verify::PublicKey::from_base64(public_key.trim())
        .map_err(|e| anyhow::anyhow!("[update] public_key: {e}"))?;
    let signature = minisign_verify::Signature::decode(signature)
        .map_err(|e| anyhow::anyhow!("not a signature: {e}"))?;
    key.verify(data, &signature, false)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

/// Signs each file into `<file>.sig`; returns the paths written.
pub fn sign_files(
    key: &SecretKey,
    files: &[PathBuf],
    version: Option<&str>,
) -> Result<Vec<PathBuf>> {
    let mut written = Vec::new();
    for file in files {
        let data =
            std::fs::read(file).with_context(|| format!("cannot read {}", file.display()))?;
        let name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let signature = key.sign(&data, &trusted_comment(&name, version));
        let mut target = file.as_os_str().to_owned();
        target.push(SIGNATURE_SUFFIX);
        let target = PathBuf::from(target);
        std::fs::write(&target, signature)
            .with_context(|| format!("cannot write {}", target.display()))?;
        written.push(target);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> SecretKey {
        SecretKey::parse(&keygen().unwrap().secret_file).unwrap()
    }

    #[test]
    fn a_signature_holds_for_its_data_and_key_only() {
        let key = key();
        let signature = key.sign(
            b"manifest",
            &trusted_comment("manifest.json", Some("1.0.0")),
        );
        verify(&key.public_key(), b"manifest", &signature).unwrap();
        assert!(verify(&key.public_key(), b"manifest!", &signature).is_err());
        let other = self::key();
        assert!(verify(&other.public_key(), b"manifest", &signature).is_err());
    }

    #[test]
    fn a_tampered_trusted_comment_is_refused() {
        let key = key();
        let signature = key.sign(b"x", "version:1.0.0");
        let forged = signature.replace("version:1.0.0", "version:9.0.0");
        assert!(verify(&key.public_key(), b"x", &forged).is_err());
    }

    #[test]
    fn keys_read_back_and_print_as_minisign() {
        let pair = keygen().unwrap();
        let key = SecretKey::parse(&pair.secret_file).unwrap();
        assert_eq!(key.public_key(), pair.public_key);
        assert!(pair.public_key.starts_with("RW"), "{}", pair.public_key);
        assert!(SecretKey::parse("untrusted comment: x\nnot base64!").is_err());
        let tauri = String::from_utf8(
            BASE64
                .decode(
                    UpdateConfig {
                        public_key: pair.public_key.clone(),
                        ..UpdateConfig::default()
                    }
                    .tauri_public_key(),
                )
                .unwrap(),
        )
        .unwrap();
        minisign_verify::PublicKey::decode(&tauri).unwrap();
    }

    #[test]
    fn the_binary_is_the_cli_entry_for_this_platform() {
        let entry = |name: &str, platform: &str, kind: Option<&str>| FileEntry {
            name: name.into(),
            sha256: String::new(),
            size: 0,
            platform: platform.into(),
            kind: kind.map(Into::into),
            url: None,
        };
        let offer = Offer {
            manifest: Manifest {
                version: "1.0.0".into(),
                date: String::new(),
                channel: "stable".into(),
                files: vec![
                    entry("app.AppImage", "linux-x86_64", Some("app")),
                    entry("regulus-mac", "macos-aarch64", None),
                    entry("regulus", "linux-x86_64", Some("cli")),
                ],
            },
            version: semver::Version::new(1, 0, 0),
            signed: true,
        };
        assert_eq!(offer.binary("linux-x86_64").unwrap().name, "regulus");
        assert!(offer.binary("windows-x86_64").is_none());
        assert!(offer.newer_than(&semver::Version::new(0, 9, 0)));
        assert!(!offer.newer_than(&semver::Version::new(1, 0, 0)));
    }

    #[test]
    fn sources_need_a_url_and_a_plain_channel() {
        let config = UpdateConfig::default();
        let path = Path::new("config.toml");
        let off = Source::new(&config, path, None, None)
            .unwrap_err()
            .to_string();
        assert!(off.contains("updates are off"), "{off}");
        let source = Source::new(&config, path, Some("https://x/r/".into()), None).unwrap();
        assert_eq!(source.manifest_url(), "https://x/r/stable/manifest.json");
        assert_eq!(source.timeout, Some(Duration::from_millis(120_000)));
        let plain = Source::new(&config, path, Some("http://x".into()), None)
            .unwrap_err()
            .to_string();
        assert!(plain.contains("allow_insecure"), "{plain}");
        let lax = UpdateConfig {
            allow_insecure: true,
            timeout_ms: 0,
            ..UpdateConfig::default()
        };
        let source_lax = Source::new(&lax, path, Some("http://x".into()), None).unwrap();
        assert_eq!(source_lax.timeout, None, "timeout_ms = 0 is no limit");
        assert!(Source::new(
            &config,
            path,
            Some("https://x".into()),
            Some("../up".into())
        )
        .is_err());
        let mut entry = FileEntry {
            name: "regulus".into(),
            sha256: String::new(),
            size: 0,
            platform: platform(),
            kind: None,
            url: None,
        };
        assert_eq!(source.file_url(&entry), "https://x/r/stable/regulus");
        entry.url = Some("https://cdn/regulus".into());
        assert_eq!(source.file_url(&entry), "https://cdn/regulus");
    }

    fn write_site(dir: &Path, key: &SecretKey, manifest: &serde_json::Value) -> Source {
        std::fs::create_dir_all(dir.join("stable")).unwrap();
        let bytes = serde_json::to_vec(manifest).unwrap();
        std::fs::write(dir.join("stable/manifest.json"), &bytes).unwrap();
        std::fs::write(
            dir.join("stable/manifest.json.sig"),
            key.sign(&bytes, &trusted_comment("manifest.json", None)),
        )
        .unwrap();
        Source {
            url: format!("file://{}", dir.display()),
            channel: "stable".into(),
            public_key: key.public_key(),
            timeout: None,
            max_bytes: 1 << 30,
            allow_insecure: false,
        }
    }

    struct TempDir(PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("regulus-update-unit-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_signed_manifest_must_name_the_channel_it_is_read_from() {
        let key = key();
        let dir = TempDir::new("channel");
        let entry = serde_json::json!([]);
        let source = write_site(
            &dir.0,
            &key,
            &serde_json::json!({ "version": "1.0.0", "files": entry }),
        );
        let unnamed = fetch_offer(&source, true).unwrap_err().to_string();
        assert!(unnamed.contains("names no channel"), "{unnamed}");
        let source = write_site(
            &dir.0,
            &key,
            &serde_json::json!({ "version": "1.0.0", "channel": "beta", "files": entry }),
        );
        let other = fetch_offer(&source, true).unwrap_err().to_string();
        assert!(other.contains("for channel `beta`"), "{other}");
        let source = write_site(
            &dir.0,
            &key,
            &serde_json::json!({ "version": "1.0.0", "channel": "stable", "files": entry }),
        );
        assert!(fetch_offer(&source, true).unwrap().signed);
    }

    #[test]
    fn a_malformed_public_key_verifies_nothing() {
        let key = key();
        let signature = key.sign(b"x", "c");
        for bad in ["", "RW", "not base64!", &key.public_key()[..20]] {
            let error = verify(bad, b"x", &signature).unwrap_err().to_string();
            assert!(error.contains("public_key"), "{bad:?}: {error}");
        }
    }

    #[test]
    fn downloads_are_bounded_by_the_manifest_and_max_bytes() {
        let dir = TempDir::new("size");
        std::fs::write(dir.0.join("regulus-new"), b"program").unwrap();
        let mut source = Source {
            url: format!("file://{}", dir.0.display()),
            channel: ".".into(),
            public_key: String::new(),
            timeout: None,
            max_bytes: 1 << 30,
            allow_insecure: false,
        };
        let mut entry = FileEntry {
            name: "regulus-new".into(),
            sha256: sha256_hex(b"program"),
            size: 7,
            platform: platform(),
            kind: None,
            url: None,
        };
        assert_eq!(download(&source, &entry).unwrap(), b"program");
        entry.size = 6;
        assert!(download(&source, &entry)
            .unwrap_err()
            .to_string()
            .contains("longer than"));
        entry.size = 8;
        assert!(download(&source, &entry)
            .unwrap_err()
            .to_string()
            .contains("the manifest says 8"));
        entry.size = u64::MAX;
        let huge = download(&source, &entry).unwrap_err().to_string();
        assert!(huge.contains("max_bytes"), "{huge}");
        source.max_bytes = u64::MAX;
        let huge = download(&source, &entry).unwrap_err().to_string();
        assert!(huge.contains("the manifest says"), "{huge}");
        entry.size = 7;
        entry.url = Some("http://cdn/regulus".into());
        let plain = download(&source, &entry).unwrap_err().to_string();
        assert!(plain.contains("allow_insecure"), "{plain}");
    }

    #[test]
    fn a_staged_program_that_lies_is_refused_and_removed() {
        let dir = TempDir::new("stage");
        let exe = dir.0.join("regulus");
        std::fs::write(&exe, b"#!/bin/sh\necho old\n").unwrap();
        let liar = b"#!/bin/sh\necho \"regulus 1.0.0\"\n";
        let lie = install(&exe, liar, &semver::Version::new(2, 0, 0))
            .unwrap_err()
            .to_string();
        assert!(lie.contains("not 2.0.0"), "{lie}");
        assert_eq!(std::fs::read(&exe).unwrap(), b"#!/bin/sh\necho old\n");
        let left: Vec<_> = std::fs::read_dir(&dir.0)
            .unwrap()
            .flatten()
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with(".regulus-update-")
            })
            .collect();
        assert!(left.is_empty(), "{left:?}");
        install(&exe, liar, &semver::Version::new(1, 0, 0)).unwrap();
        assert_eq!(std::fs::read(&exe).unwrap(), &liar[..]);
    }
}
