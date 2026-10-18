//! The desktop shell. It finds the core the home directory points at, or starts one, and
//! hands the webview where to connect; everything else goes over the core's socket.

use regulus_core::config::default_home;
use regulus_core::local::{self, Endpoint};
use regulus_core::Config;

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

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![core_endpoint, start_core])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
