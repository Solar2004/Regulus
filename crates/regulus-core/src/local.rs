//! Finding the core on this machine, or starting one, for clients that run beside it
//! (docs/04-client.md §4). A started core is detached: closing the client does not stop it.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use serde::Serialize;

use crate::Config;

/// The program a client starts when `client.core_command` is empty.
const CORE_PROGRAM: &str = "regulus";
const SERVE_COMMAND: &str = "serve";
const POLL: Duration = Duration::from_millis(50);

/// Where a core listens and the token it expects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Endpoint {
    pub url: String,
    pub token: String,
}

/// What the home directory says about the last core started there. It may be gone: a
/// client still has to say `hello` before trusting it.
pub fn endpoint(config: &Config) -> Option<Endpoint> {
    let port = fs::read_to_string(config.port_path()).ok()?;
    let token = fs::read_to_string(config.token_path()).ok()?;
    Some(Endpoint {
        url: format!("ws://{}:{}/", config.server.host, port.trim()),
        token: token.trim().to_string(),
    })
}

/// The command that starts a core for this home, as a person would type it.
pub fn start_command(config: &Config, program: &Path) -> String {
    format!(
        "{} --home {} {SERVE_COMMAND}",
        program.display(),
        config.home.display()
    )
}

/// The program that serves: the configured one, else `regulus` beside `beside` (the
/// client's own executable), else `regulus` on `PATH`.
pub fn core_program(config: &Config, beside: Option<PathBuf>) -> PathBuf {
    if !config.client.core_command.as_os_str().is_empty() {
        return config.client.core_command.clone();
    }
    let name = format!("{CORE_PROGRAM}{}", std::env::consts::EXE_SUFFIX);
    beside
        .and_then(|exe| Some(exe.parent()?.join(&name)))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Starts a detached core and waits until it has written a fresh port.
pub fn start(config: &Config, program: &Path) -> Result<Endpoint, String> {
    let command = start_command(config, program);
    if !config.client.autostart {
        return Err(format!("no core is running; start one with: {command}"));
    }
    fs::create_dir_all(&config.home)
        .map_err(|e| format!("cannot create {}: {e}", config.home.display()))?;
    let log_path = config.core_log_path();
    let log = fs::File::create(&log_path)
        .map_err(|e| format!("cannot write {}: {e}", log_path.display()))?;
    let since = SystemTime::now();
    let mut process = Command::new(program);
    process
        .arg("--home")
        .arg(&config.home)
        .arg(SERVE_COMMAND)
        .stdin(Stdio::null())
        .stdout(log.try_clone().map_err(|e| e.to_string())?)
        .stderr(log);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // Its own process group, so the client's terminal or window does not take it down.
        process.process_group(0);
    }
    let mut child = process
        .spawn()
        .map_err(|e| format!("cannot run `{command}`: {e}"))?;
    let deadline = Instant::now() + Duration::from_millis(config.client.start_timeout_ms);
    let fresh = || {
        fs::metadata(config.port_path())
            .and_then(|m| m.modified())
            .is_ok_and(|at| at >= since)
    };
    loop {
        if fresh() {
            if let Some(found) = endpoint(config) {
                // Nobody else waits for it: reap it when it ends.
                std::thread::spawn(move || child.wait());
                return Ok(found);
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(format!(
                "`{command}` exited ({status}); see {}",
                log_path.display()
            ));
        }
        if Instant::now() >= deadline {
            std::thread::spawn(move || child.wait());
            return Err(format!(
                "the core did not start in {} ms; see {}",
                config.client.start_timeout_ms,
                log_path.display()
            ));
        }
        std::thread::sleep(POLL);
    }
}
