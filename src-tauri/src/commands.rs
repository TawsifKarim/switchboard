use std::path::Path;
use std::sync::Arc;

use tauri::AppHandle;
use ulid::Ulid;

use crate::config::{self, AppEntry};
use crate::process::{ProcessManager, StatusSnapshot};

const DEFAULT_TAG: &str = "#64748b"; // slate-500

#[tauri::command]
pub async fn list_apps(app: AppHandle) -> Result<Vec<AppEntry>, String> {
    let path = config::config_path(&app).map_err(|e| e.to_string())?;
    config::load(&path).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn add_app(
    app: AppHandle,
    name: String,
    directory: String,
    command: String,
    tag: String,
) -> Result<AppEntry, String> {
    let name = name.trim().to_string();
    let directory = directory.trim().to_string();
    let command = command.trim().to_string();
    let tag = tag.trim();

    if name.is_empty() {
        return Err("name must not be empty".into());
    }
    if directory.is_empty() {
        return Err("directory must not be empty".into());
    }
    if command.is_empty() {
        return Err("command must not be empty".into());
    }
    if !Path::new(&directory).is_dir() {
        return Err(format!("directory does not exist: {directory}"));
    }

    let tag = if tag.is_empty() { DEFAULT_TAG.to_string() } else { tag.to_string() };

    let entry = AppEntry {
        id: Ulid::new().to_string(),
        name,
        directory,
        command,
        tag,
    };

    let path = config::config_path(&app).map_err(|e| e.to_string())?;
    config::add(&path, entry.clone()).map_err(|e| e.to_string())?;
    Ok(entry)
}

#[tauri::command]
pub async fn delete_app(app: AppHandle, id: String) -> Result<(), String> {
    let path = config::config_path(&app).map_err(|e| e.to_string())?;
    config::delete(&path, &id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn start_app(
    app: AppHandle,
    pm: tauri::State<'_, Arc<ProcessManager>>,
    id: String,
) -> Result<u32, String> {
    let path = config::config_path(&app).map_err(|e| e.to_string())?;
    let apps = config::load(&path).map_err(|e| e.to_string())?;
    let entry = apps
        .into_iter()
        .find(|a| a.id == id)
        .ok_or_else(|| format!("app not found: {id}"))?;
    pm.start(app.clone(), entry).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn stop_app(
    pm: tauri::State<'_, Arc<ProcessManager>>,
    id: String,
) -> Result<(), String> {
    pm.stop(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_status(
    pm: tauri::State<'_, Arc<ProcessManager>>,
    id: String,
) -> Result<StatusSnapshot, String> {
    Ok(pm.status(&id).await)
}

#[tauri::command]
pub async fn attach_pty(
    app: AppHandle,
    pm: tauri::State<'_, Arc<ProcessManager>>,
    id: String,
) -> Result<(), String> {
    pm.attach(&id, app.clone()).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn detach_pty(
    pm: tauri::State<'_, Arc<ProcessManager>>,
    id: String,
) -> Result<(), String> {
    pm.detach(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn write_pty(
    pm: tauri::State<'_, Arc<ProcessManager>>,
    id: String,
    data: String,
) -> Result<(), String> {
    pm.write_pty(&id, data.as_bytes())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn resize_pty(
    pm: tauri::State<'_, Arc<ProcessManager>>,
    id: String,
    rows: u16,
    cols: u16,
) -> Result<(), String> {
    pm.resize(&id, rows, cols).await.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure validation tests that do not need an AppHandle. These mirror what
    // `add_app` does before touching the filesystem; if the structure of
    // add_app changes, update these too.
    fn validate(name: &str, directory: &str, command: &str) -> Result<(), String> {
        let n = name.trim();
        let d = directory.trim();
        let c = command.trim();
        if n.is_empty() {
            return Err("name must not be empty".into());
        }
        if d.is_empty() {
            return Err("directory must not be empty".into());
        }
        if c.is_empty() {
            return Err("command must not be empty".into());
        }
        if !Path::new(d).is_dir() {
            return Err(format!("directory does not exist: {d}"));
        }
        Ok(())
    }

    #[test]
    fn rejects_empty_name() {
        let err = validate("  ", "/tmp", "echo hi").unwrap_err();
        assert!(err.contains("name"), "got: {err}");
    }

    #[test]
    fn rejects_empty_directory() {
        let err = validate("svc", "  ", "echo hi").unwrap_err();
        assert!(err.contains("directory"), "got: {err}");
    }

    #[test]
    fn rejects_empty_command() {
        let err = validate("svc", "/tmp", "  ").unwrap_err();
        assert!(err.contains("command"), "got: {err}");
    }

    #[test]
    fn rejects_nonexistent_directory() {
        let err = validate("svc", "/definitely/not/here/zzz", "echo hi").unwrap_err();
        assert!(err.contains("does not exist"), "got: {err}");
    }

    #[test]
    fn accepts_valid_input() {
        validate("svc", "/tmp", "echo hi").unwrap();
    }
}
