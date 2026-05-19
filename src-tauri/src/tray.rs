use std::sync::Arc;

use tauri::{
    image::Image,
    menu::{Menu, MenuItem, PredefinedMenuItem},
    tray::{TrayIcon, TrayIconBuilder},
    AppHandle, Manager, Runtime,
};

use crate::process::ProcessManager;

pub const TRAY_ID: &str = "switchboard-tray";

/// Holds the mutable "Running: N" menu item so we can update its label
/// without re-querying the tray for its menu (the TrayIcon doesn't expose
/// `.menu()` to read the live menu back out).
pub struct TrayState<R: Runtime> {
    pub running_item: MenuItem<R>,
}

/// Build the menu-bar tray icon with Show Window / Running: N / Quit.
/// Stashes the "Running" MenuItem in Tauri-managed state for later updates.
pub fn build<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<TrayIcon<R>> {
    let show = MenuItem::with_id(app, "show", "Show Window", true, None::<&str>)?;
    let running = MenuItem::with_id(app, "running", "Running: 0", false, None::<&str>)?;
    let sep = PredefinedMenuItem::separator(app)?;
    let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &running, &sep, &quit])?;

    // Stash the running item so update_running_count() can mutate it.
    app.manage(TrayState {
        running_item: running.clone(),
    });

    // Use the bundled app icon as the tray image.
    let icon: Image<'static> =
        Image::from_bytes(include_bytes!("../icons/icon.png"))?.to_owned();

    let tray = TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .icon_as_template(true) // macOS: lets the system tint it for the menu bar
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => {
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.show();
                    let _ = w.set_focus();
                }
            }
            "quit" => {
                let app_clone = app.clone();
                tauri::async_runtime::spawn(async move {
                    if let Some(pm) = app_clone.try_state::<Arc<ProcessManager>>() {
                        let _ = pm.stop_all().await;
                    }
                    app_clone.exit(0);
                });
            }
            _ => {}
        })
        .build(app)?;
    Ok(tray)
}

/// Update the "Running: N" label. No-op if state isn't registered yet.
pub fn update_running_count<R: Runtime>(app: &AppHandle<R>, n: usize) {
    if let Some(state) = app.try_state::<TrayState<R>>() {
        let _ = state.running_item.set_text(format!("Running: {}", n));
    }
}
