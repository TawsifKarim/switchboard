use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use tauri::AppHandle;
use ulid::Ulid;

use crate::config::{self, AppEntry, ReadyProbe};
use crate::graph;
use crate::process::{ProcessManager, StatusSnapshot};

const DEFAULT_TAG: &str = "#64748b"; // slate-500

/// Per-level ceiling for awaiting readiness during `start_all`. Generous —
/// the per-probe timeout is already 60s, and a level can contain several
/// services starting in series-ish. Picking too small here risks marking a
/// healthy parent as not-ready and cascading skips.
const LEVEL_READY_TIMEOUT: Duration = Duration::from_secs(90);

#[derive(serde::Serialize)]
pub struct StartAllResult {
    pub started: Vec<String>,
    pub failed: Vec<(String, String)>,
    /// Apps not started because a declared parent never reached ready. Each
    /// entry is `(id, reason)` where reason names the unmet parent. Distinct
    /// from `failed`: skipped apps were never attempted.
    pub skipped: Vec<(String, String)>,
}

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
    port: Option<u16>,
    ready: Option<ReadyProbe>,
    depends_on: Option<Vec<String>>,
) -> Result<AppEntry, String> {
    let name = name.trim().to_string();
    let directory = directory.trim().to_string();
    let command = command.trim().to_string();
    let tag = tag.trim();
    let depends_on: Vec<String> = depends_on
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();

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
    if let Some(0) = port {
        return Err("port must be between 1 and 65535".into());
    }

    // Validate dep ids against the current app list: reject duplicates and
    // unknowns up-front so a misclick can't poison `start_all` later.
    if !depends_on.is_empty() {
        let mut seen = HashSet::new();
        for d in &depends_on {
            if !seen.insert(d.clone()) {
                return Err(format!("duplicate dependency id: {d}"));
            }
        }
        let path = config::config_path(&app).map_err(|e| e.to_string())?;
        let existing = config::load(&path).map_err(|e| e.to_string())?;
        let known: HashSet<&str> = existing.iter().map(|a| a.id.as_str()).collect();
        for d in &depends_on {
            if !known.contains(d.as_str()) {
                return Err(format!("dependency id not found: {d}"));
            }
        }
    }

    // Validate probe specifics so a broken config can't sneak into apps.json.
    if let Some(ref p) = ready {
        match p {
            ReadyProbe::Tcp { port: 0 } => {
                return Err("ready.tcp.port must be between 1 and 65535".into())
            }
            ReadyProbe::Http { url, .. } if url.trim().is_empty() => {
                return Err("ready.http.url must not be empty".into())
            }
            ReadyProbe::LogRegex { pattern } if pattern.trim().is_empty() => {
                return Err("ready.log_regex.pattern must not be empty".into())
            }
            ReadyProbe::LogRegex { pattern } => {
                if let Err(e) = regex::Regex::new(pattern) {
                    return Err(format!("invalid log_regex pattern: {e}"));
                }
            }
            _ => {}
        }
    }

    let tag = if tag.is_empty() { DEFAULT_TAG.to_string() } else { tag.to_string() };

    let entry = AppEntry {
        id: Ulid::new().to_string(),
        name,
        directory,
        command,
        tag,
        port,
        ready,
        depends_on,
    };

    let path = config::config_path(&app).map_err(|e| e.to_string())?;
    config::add(&path, entry.clone()).map_err(|e| e.to_string())?;
    Ok(entry)
}

#[tauri::command]
pub async fn delete_app(
    app: AppHandle,
    pm: tauri::State<'_, Arc<ProcessManager>>,
    id: String,
) -> Result<(), String> {
    let path = config::config_path(&app).map_err(|e| e.to_string())?;
    // Stop the process first so we don't orphan it when the entry is gone.
    pm.stop(&id).await.map_err(|e| e.to_string())?;
    config::delete(&path, &id).map_err(|e| e.to_string())?;
    pm.clear_ring(&id);
    Ok(())
}

#[tauri::command]
pub async fn reorder_apps(
    app: AppHandle,
    ordered_ids: Vec<String>,
) -> Result<(), String> {
    let path = config::config_path(&app).map_err(|e| e.to_string())?;
    let apps = config::load(&path).map_err(|e| e.to_string())?;
    let new_apps = config::reorder(apps, &ordered_ids).map_err(|e| e.to_string())?;
    config::save(&path, &new_apps).map_err(|e| e.to_string())?;
    Ok(())
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
pub async fn start_all(
    app: AppHandle,
    pm: tauri::State<'_, Arc<ProcessManager>>,
) -> Result<StartAllResult, String> {
    let path = config::config_path(&app).map_err(|e| e.to_string())?;
    let apps = config::load(&path).map_err(|e| e.to_string())?;

    let levels = graph::topo_levels(&apps).map_err(|e| e.to_string())?;
    let by_id: std::collections::HashMap<String, AppEntry> =
        apps.iter().cloned().map(|a| (a.id.clone(), a)).collect();

    let mut started: Vec<String> = Vec::new();
    let mut failed: Vec<(String, String)> = Vec::new();
    let mut skipped: Vec<(String, String)> = Vec::new();
    let mut ready_set: HashSet<String> = HashSet::new();

    // Seed ready_set with apps already running AND already ready so a partial
    // re-run (some services up, some not) doesn't redundantly skip children.
    for a in &apps {
        let s = pm.status(&a.id).await;
        if s.running && s.ready {
            ready_set.insert(a.id.clone());
        }
    }

    // Subscribe BEFORE issuing any start so we don't miss a fast probe.
    let mut rx = pm.subscribe_ready();

    for level in levels {
        let mut pending: HashSet<String> = HashSet::new();
        for id in level {
            let entry = match by_id.get(&id) {
                Some(e) => e,
                None => continue, // graph and config out of sync — shouldn't happen
            };
            let unmet: Vec<&String> = entry
                .depends_on
                .iter()
                .filter(|d| !ready_set.contains(d.as_str()))
                .collect();
            if let Some(first) = unmet.first() {
                skipped.push((id.clone(), format!("parent {first} not ready")));
                continue;
            }

            let s = pm.status(&id).await;
            if s.running && s.ready {
                ready_set.insert(id.clone());
                continue;
            }
            if s.running {
                // Already running, probe still pending — wait on it before
                // starting children, but don't restart.
                pending.insert(id.clone());
                continue;
            }
            match pm.start(app.clone(), entry.clone()).await {
                Ok(_) => {
                    started.push(id.clone());
                    pending.insert(id);
                }
                Err(e) => failed.push((id, e.to_string())),
            }
        }

        // Drain ready events until everyone in `pending` has resolved or the
        // level ceiling is hit. Whatever's still pending after the ceiling is
        // treated as not-ready — its children get skipped in subsequent levels.
        let deadline = tokio::time::Instant::now() + LEVEL_READY_TIMEOUT;
        while !pending.is_empty() {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            match tokio::time::timeout(remaining, rx.recv()).await {
                Ok(Ok(payload)) => {
                    if pending.remove(&payload.id) && payload.ready {
                        ready_set.insert(payload.id);
                    }
                }
                Ok(Err(_)) => break, // channel closed/lagged irrecoverably
                Err(_) => break,     // ceiling reached
            }
        }
    }

    Ok(StartAllResult {
        started,
        failed,
        skipped,
    })
}

#[tauri::command]
pub async fn stop_all(
    pm: tauri::State<'_, Arc<ProcessManager>>,
) -> Result<(), String> {
    pm.stop_all().await.map_err(|e| e.to_string())
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

#[tauri::command]
pub async fn open_shell(
    app: AppHandle,
    pm: tauri::State<'_, Arc<ProcessManager>>,
    directory: String,
) -> Result<String, String> {
    let dir = directory.trim();
    if dir.is_empty() {
        return Err("directory must not be empty".into());
    }
    if !Path::new(dir).is_dir() {
        return Err(format!("directory does not exist: {dir}"));
    }
    pm.open_shell(app.clone(), dir.to_string())
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn close_shell(
    pm: tauri::State<'_, Arc<ProcessManager>>,
    id: String,
) -> Result<(), String> {
    pm.close_shell(&id).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_branch(directory: String) -> Result<Option<String>, String> {
    use tokio::process::Command;
    let dir = directory.trim();
    if dir.is_empty() {
        return Ok(None);
    }
    let out = match Command::new("git")
        .args(["-C", dir, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .await
    {
        Ok(o) => o,
        Err(_) => return Ok(None),
    };
    if !out.status.success() {
        return Ok(None);
    }
    let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if name.is_empty() {
        return Ok(None);
    }
    if name == "HEAD" {
        // Detached: return short SHA prefixed with '@'.
        let sha = match Command::new("git")
            .args(["-C", dir, "rev-parse", "--short", "HEAD"])
            .output()
            .await
        {
            Ok(o) if o.status.success() => {
                String::from_utf8_lossy(&o.stdout).trim().to_string()
            }
            _ => return Ok(None),
        };
        if sha.is_empty() {
            return Ok(None);
        }
        return Ok(Some(format!("@{sha}")));
    }
    Ok(Some(name))
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
