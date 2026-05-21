mod config;
mod process;
mod commands;
mod tray;

use std::path::PathBuf;
use std::sync::Arc;

use tauri::{Listener, Manager, WindowEvent};

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
            app.manage(pm.clone());

            // Background 2s sampler emits `app-stats` per running app so the
            // UI can render CPU/RAM next to the PID without polling.
            pm.start_stats_sampler(app.handle().clone());

            // Tray icon + menu. Built programmatically so we can mutate the
            // "Running: N" label without going through tauri.conf.json.
            tray::build(&app.handle())?;

            // Refresh "Running: N" whenever an app starts or exits.
            let h_started = app.handle().clone();
            let pm_started = pm.clone();
            app.listen("app-started", move |_event| {
                let n = pm_started.running_count();
                tray::update_running_count(&h_started, n);
            });
            let h_exit = app.handle().clone();
            let pm_exit = pm.clone();
            app.listen("app-exit", move |_event| {
                let n = pm_exit.running_count();
                tray::update_running_count(&h_exit, n);
            });

            // Window close → hide, not quit. The tray's Quit item is the only
            // way to actually exit (and it stops_all first).
            if let Some(window) = app.get_webview_window("main") {
                let win_clone = window.clone();
                window.on_window_event(move |event| {
                    if let WindowEvent::CloseRequested { api, .. } = event {
                        api.prevent_close();
                        let _ = win_clone.hide();
                    }
                });
            }

            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_apps,
            commands::add_app,
            commands::delete_app,
            commands::reorder_apps,
            commands::start_app,
            commands::stop_app,
            commands::start_all,
            commands::stop_all,
            commands::get_status,
            commands::attach_pty,
            commands::detach_pty,
            commands::write_pty,
            commands::resize_pty
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
