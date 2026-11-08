//! The desktop shell. It finds the core the home directory points at, or starts one, and
//! hands the webview where to connect; everything else goes over the core's socket.
//! It also updates the app itself, from `[update]` of the same `config.toml`
//! (docs/06-stack.md §5.4).

use std::time::Duration;

use regulus_core::config::{default_home, UpdateConfig};
use regulus_core::local::{self, Endpoint};
use regulus_core::Config;
use serde::Serialize;
use tauri_plugin_updater::{Update, UpdaterExt};

/// The file the app's updater reads in each channel folder.
const LATEST_FILE: &str = "latest.json";

fn config() -> Result<Config, String> {
    Config::load(&default_home()).map_err(|e| e.to_string())
}

/// Where the last core started in this home listens, if it left a port behind.
#[tauri::command]
fn core_endpoint() -> Result<Option<Endpoint>, String> {
    Ok(local::endpoint(&config()?))
}

/// Starts a detached core (`client.autostart`), for when none answers `hello`.
#[tauri::command]
async fn start_core() -> Result<Endpoint, String> {
    let config = config()?;
    let program = local::core_program(&config, std::env::current_exe().ok());
    tauri::async_runtime::spawn_blocking(move || local::start(&config, &program))
        .await
        .map_err(|e| e.to_string())?
}

/// What Settings shows about updates before asking the network anything.
#[derive(Serialize)]
struct UpdateSettings {
    current: String,
    /// Empty: updates are off.
    url: String,
    channel: String,
    check_on_start: bool,
    has_key: bool,
    /// Where to set `[update]`.
    config_path: String,
}

/// What a check found.
#[derive(Serialize)]
struct UpdateFound {
    current: String,
    /// The newer version on the channel; none when up to date.
    available: Option<String>,
    notes: Option<String>,
    date: Option<String>,
}

#[tauri::command]
fn update_settings(app: tauri::AppHandle) -> Result<UpdateSettings, String> {
    let config = config()?;
    Ok(UpdateSettings {
        current: app.package_info().version.to_string(),
        url: config.update.url.trim().to_string(),
        channel: config.update.channel.clone(),
        check_on_start: config.update.check_on_start,
        has_key: !config.update.public_key.trim().is_empty(),
        config_path: config.config_path().display().to_string(),
    })
}

/// The channel's `latest.json`, checked against the release key of `[update]`.
async fn find_update(app: &tauri::AppHandle) -> Result<Option<Update>, String> {
    let config = config()?;
    let update: &UpdateConfig = &config.update;
    let base = update.url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(format!(
            "updates are off: set `url` under [update] in {}",
            config.config_path().display()
        ));
    }
    if update.public_key.trim().is_empty() {
        return Err(format!(
            "no release key: set `public_key` under [update] in {}",
            config.config_path().display()
        ));
    }
    let endpoint = format!("{base}/{}/{LATEST_FILE}", update.channel);
    let endpoint = endpoint.parse().map_err(|e| format!("{endpoint}: {e}"))?;
    app.updater_builder()
        .endpoints(vec![endpoint])
        .map_err(|e| e.to_string())?
        .pubkey(update.tauri_public_key())
        .timeout(Duration::from_millis(update.timeout_ms))
        .build()
        .map_err(|e| e.to_string())?
        .check()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn update_check(app: tauri::AppHandle) -> Result<UpdateFound, String> {
    let found = find_update(&app).await?;
    Ok(UpdateFound {
        current: app.package_info().version.to_string(),
        available: found.as_ref().map(|u| u.version.clone()),
        notes: found.as_ref().and_then(|u| u.body.clone()),
        date: found.and_then(|u| u.date).map(|d| d.to_string()),
    })
}

/// Downloads, verifies and installs the newer release, then restarts into it.
#[tauri::command]
async fn update_install(app: tauri::AppHandle) -> Result<(), String> {
    let Some(update) = find_update(&app).await? else {
        return Err("already up to date".into());
    };
    update
        .download_and_install(|_, _| {}, || {})
        .await
        .map_err(|e| e.to_string())?;
    app.restart()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_updater::Builder::new().build())
        .invoke_handler(tauri::generate_handler![
            core_endpoint,
            start_core,
            update_settings,
            update_check,
            update_install
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
