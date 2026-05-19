mod config;
mod process;
mod commands;
mod tray;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::Manager;

use crate::process::ProcessManager;

/// Mirror config.rs's debug/release split for the log directory.
/// Debug: `<project-root>/logs`. Release: `<config_dir>/switchboard/logs`.
fn resolve_log_dir(debug: bool, manifest_dir: &str, config_dir: &std::path::Path) -> PathBuf {
    if debug {
        let manifest = PathBuf::from(manifest_dir);
        let project_root = manifest
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or(manifest);
        project_root.join("logs")
    } else {
        config_dir.join("switchboard").join("logs")
    }
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let config_dir = app
                .path()
                .config_dir()
                .expect("resolving OS user config dir");
            let log_dir = resolve_log_dir(
                cfg!(debug_assertions),
                env!("CARGO_MANIFEST_DIR"),
                &config_dir,
            );
            if let Err(e) = std::fs::create_dir_all(&log_dir) {
                eprintln!("warning: could not create log dir {}: {e}", log_dir.display());
            }
            let pm = Arc::new(ProcessManager::new(log_dir));
            app.manage(pm);
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_apps,
            commands::add_app,
            commands::delete_app,
            commands::start_app,
            commands::stop_app,
            commands::get_status
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
