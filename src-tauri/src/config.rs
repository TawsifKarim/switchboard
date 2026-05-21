use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Manager};

pub const CONFIG_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppEntry {
    pub id: String,
    pub name: String,
    pub directory: String,
    pub command: String,
    pub tag: String,
    /// Optional port the service listens on. When set, stop/start additionally
    /// sweeps anything bound to this port so orphaned children don't keep it
    /// held. Additive field — pre-port `apps.json` files load fine because of
    /// `#[serde(default)]`. Schema version stays at 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Optional readiness probe. When set, "ready" requires both PTY-alive AND
    /// the probe succeeding. Additive — old apps.json files load with `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ready: Option<ReadyProbe>,
}

/// What it means for an app to be "ready" beyond just having its PTY alive.
/// Tagged enum: `{"kind": "tcp", "port": 8080}` etc.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReadyProbe {
    Tcp { port: u16 },
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expect_status: Option<u16>,
    },
    LogRegex { pattern: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigFile {
    pub version: u32,
    pub apps: Vec<AppEntry>,
}

impl Default for ConfigFile {
    fn default() -> Self {
        Self { version: CONFIG_VERSION, apps: Vec::new() }
    }
}

/// Pure helper: compute the config-file path given debug flag, the Rust
/// `CARGO_MANIFEST_DIR` (which is `src-tauri/`), and the OS user config dir.
/// In debug builds we want `<project-root>/apps.json` so devs can inspect it.
/// In release builds we want `<config_dir>/switchboard/apps.json`.
pub fn resolve_config_path(debug: bool, manifest_dir: &str, config_dir: &Path) -> PathBuf {
    if debug {
        let manifest = PathBuf::from(manifest_dir);
        let project_root = manifest.parent().map(Path::to_path_buf).unwrap_or(manifest);
        project_root.join("apps.json")
    } else {
        config_dir.join("switchboard").join("apps.json")
    }
}

pub fn config_path(app: &AppHandle) -> Result<PathBuf> {
    let config_dir = app
        .path()
        .config_dir()
        .context("resolving OS user config dir")?;
    Ok(resolve_config_path(
        cfg!(debug_assertions),
        env!("CARGO_MANIFEST_DIR"),
        &config_dir,
    ))
}

pub fn load(path: &Path) -> Result<Vec<AppEntry>> {
    match fs::read(path) {
        Ok(bytes) => {
            let cfg: ConfigFile = serde_json::from_slice(&bytes)
                .with_context(|| format!("parsing config file at {}", path.display()))?;
            if cfg.version != CONFIG_VERSION {
                return Err(anyhow::anyhow!(
                    "unsupported config version {} at {} (expected {}); refusing to load to avoid data loss",
                    cfg.version,
                    path.display(),
                    CONFIG_VERSION
                ));
            }
            Ok(cfg.apps)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e).with_context(|| format!("reading config file at {}", path.display())),
    }
}

/// Atomic save: serialize → write to sibling `<path>.tmp` → fsync → rename.
/// Creates parent directories as needed.
pub fn save(path: &Path, apps: &[AppEntry]) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .with_context(|| format!("creating parent dir {}", parent.display()))?;
        }
    }

    let cfg = ConfigFile { version: CONFIG_VERSION, apps: apps.to_vec() };
    let bytes = serde_json::to_vec_pretty(&cfg).context("serializing config")?;

    let tmp_path = match path.file_name() {
        Some(name) => {
            let mut tmp_name = name.to_os_string();
            tmp_name.push(".tmp");
            path.with_file_name(tmp_name)
        }
        None => return Err(anyhow::anyhow!("config path has no file name: {}", path.display())),
    };

    {
        let mut f = fs::File::create(&tmp_path)
            .with_context(|| format!("creating temp file {}", tmp_path.display()))?;
        f.write_all(&bytes)
            .with_context(|| format!("writing temp file {}", tmp_path.display()))?;
        f.sync_all()
            .with_context(|| format!("fsync temp file {}", tmp_path.display()))?;
    }

    fs::rename(&tmp_path, path).with_context(|| {
        format!("renaming {} -> {}", tmp_path.display(), path.display())
    })?;

    // Make the rename itself durable: on POSIX the directory entry change is
    // only persisted after fsyncing the parent directory.
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Ok(dir) = fs::File::open(parent) {
                let _ = dir.sync_all();
            }
        }
    }

    Ok(())
}

/// Append a new entry. Rejects duplicate `id` (caller is expected to generate
/// fresh ULIDs — collisions indicate a bug).
pub fn add(path: &Path, entry: AppEntry) -> Result<()> {
    let mut apps = load(path)?;
    if apps.iter().any(|a| a.id == entry.id) {
        return Err(anyhow::anyhow!("duplicate app id: {}", entry.id));
    }
    apps.push(entry);
    save(path, &apps)
}

/// Remove the entry with the given `id`. Idempotent: deleting an unknown id
/// is a no-op (still rewrites the file). UI confirms before calling, so a
/// missing id is not an error condition worth surfacing.
pub fn delete(path: &Path, id: &str) -> Result<()> {
    let mut apps = load(path)?;
    apps.retain(|a| a.id != id);
    save(path, &apps)
}

/// Reorder `apps` to match `ordered_ids`. Defensive: rejects on length
/// mismatch, missing ids, or duplicate ids in the requested order. This
/// surfaces sync bugs between the UI's view of the list and what's on disk
/// rather than silently dropping entries.
pub fn reorder(apps: Vec<AppEntry>, ordered_ids: &[String]) -> Result<Vec<AppEntry>> {
    if apps.len() != ordered_ids.len() {
        return Err(anyhow::anyhow!(
            "reorder length mismatch: have {} apps, got {} ids",
            apps.len(),
            ordered_ids.len()
        ));
    }
    let mut seen = std::collections::HashSet::new();
    for id in ordered_ids {
        if !seen.insert(id) {
            return Err(anyhow::anyhow!("duplicate id in reorder list: {}", id));
        }
    }
    let mut by_id: std::collections::HashMap<String, AppEntry> =
        apps.into_iter().map(|a| (a.id.clone(), a)).collect();
    let mut out = Vec::with_capacity(ordered_ids.len());
    for id in ordered_ids {
        let entry = by_id
            .remove(id)
            .ok_or_else(|| anyhow::anyhow!("reorder id not found in apps: {}", id))?;
        out.push(entry);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn sample(id: &str) -> AppEntry {
        AppEntry {
            id: id.to_string(),
            name: format!("svc-{id}"),
            directory: "/tmp".to_string(),
            command: "echo hi".to_string(),
            tag: "#3b82f6".to_string(),
            port: None,
            ready: None,
        }
    }

    #[test]
    fn roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let original = vec![sample("01H1"), sample("01H2")];
        save(&path, &original).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(original, loaded);
    }

    #[test]
    fn load_missing_returns_empty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let loaded = load(&path).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn add_then_delete() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let entry = sample("01H1");
        add(&path, entry.clone()).unwrap();
        let after_add = load(&path).unwrap();
        assert_eq!(after_add.len(), 1);
        assert_eq!(after_add[0], entry);
        delete(&path, "01H1").unwrap();
        let after_delete = load(&path).unwrap();
        assert!(after_delete.is_empty());
    }

    /// Verifies the atomic write contract: after `save`, only the final file
    /// exists (no leftover `.tmp` sibling), and it contains valid parseable
    /// content matching what was written.
    #[test]
    fn save_is_atomic() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let big: Vec<AppEntry> = (0..200).map(|i| sample(&format!("01H{i:03}"))).collect();
        save(&path, &big).unwrap();

        // Final file exists with the right content.
        assert!(path.exists(), "final file should exist after save");
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 200);
        assert_eq!(loaded, big);

        // No leftover temp file.
        let tmp = path.with_file_name("apps.json.tmp");
        assert!(!tmp.exists(), "temp file should not remain after rename");
    }

    #[test]
    fn load_rejects_unknown_version() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let bad = serde_json::json!({ "version": 99, "apps": [] });
        fs::write(&path, serde_json::to_vec_pretty(&bad).unwrap()).unwrap();
        let err = load(&path).unwrap_err().to_string();
        assert!(err.contains("unsupported config version 99"), "got: {err}");
    }

    #[test]
    fn add_rejects_duplicate_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let entry = sample("01H1");
        add(&path, entry.clone()).unwrap();
        let err = add(&path, entry).unwrap_err().to_string();
        assert!(err.contains("duplicate app id"), "got: {err}");
        // File state unchanged after rejection.
        assert_eq!(load(&path).unwrap().len(), 1);
    }

    #[test]
    fn delete_unknown_id_is_noop() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        add(&path, sample("01H1")).unwrap();
        delete(&path, "does-not-exist").unwrap();
        let after = load(&path).unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].id, "01H1");
    }

    /// Pre-port apps.json files (schema version 1, no `port` field) must
    /// still load — the field is additive and defaults to None.
    #[test]
    fn load_legacy_without_port() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let legacy = serde_json::json!({
            "version": 1,
            "apps": [{
                "id": "01H1",
                "name": "legacy",
                "directory": "/tmp",
                "command": "echo hi",
                "tag": "#3b82f6"
            }]
        });
        fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].port, None);
        assert_eq!(loaded[0].name, "legacy");
    }

    /// With a port set, save+load round-trips correctly and the JSON contains
    /// the field.
    #[test]
    fn port_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let mut entry = sample("01H1");
        entry.port = Some(8080);
        save(&path, &[entry.clone()]).unwrap();
        let on_disk = fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("\"port\": 8080"), "missing port in: {on_disk}");
        let loaded = load(&path).unwrap();
        assert_eq!(loaded[0].port, Some(8080));
    }

    /// Legacy apps.json missing the `ready` field must still load (additive).
    #[test]
    fn load_legacy_without_ready() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let legacy = serde_json::json!({
            "version": 1,
            "apps": [{
                "id": "01H1",
                "name": "legacy",
                "directory": "/tmp",
                "command": "echo hi",
                "tag": "#3b82f6"
            }]
        });
        fs::write(&path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].ready, None);
    }

    #[test]
    fn ready_probe_tcp_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let mut entry = sample("01H1");
        entry.ready = Some(ReadyProbe::Tcp { port: 8080 });
        save(&path, &[entry.clone()]).unwrap();
        let on_disk = fs::read_to_string(&path).unwrap();
        assert!(on_disk.contains("\"kind\": \"tcp\""), "got: {on_disk}");
        let loaded = load(&path).unwrap();
        assert_eq!(loaded[0].ready, Some(ReadyProbe::Tcp { port: 8080 }));
    }

    #[test]
    fn ready_probe_log_regex_roundtrips() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        let mut entry = sample("01H1");
        entry.ready = Some(ReadyProbe::LogRegex {
            pattern: "listening on".into(),
        });
        save(&path, &[entry.clone()]).unwrap();
        let loaded = load(&path).unwrap();
        assert_eq!(
            loaded[0].ready,
            Some(ReadyProbe::LogRegex { pattern: "listening on".into() })
        );
    }

    /// With no port, the serialized form omits the field (skip_serializing_if).
    #[test]
    fn port_none_is_omitted_on_save() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("apps.json");
        save(&path, &[sample("01H1")]).unwrap();
        let on_disk = fs::read_to_string(&path).unwrap();
        assert!(!on_disk.contains("\"port\""), "port should be omitted: {on_disk}");
    }

    #[test]
    fn reorder_changes_order() {
        let apps = vec![sample("A"), sample("B"), sample("C")];
        let new_order = vec!["C".into(), "A".into(), "B".into()];
        let out = reorder(apps, &new_order).unwrap();
        assert_eq!(out.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
                   vec!["C", "A", "B"]);
    }

    #[test]
    fn reorder_rejects_missing_id() {
        let apps = vec![sample("A"), sample("B")];
        let err = reorder(apps, &["A".into(), "Z".into()]).unwrap_err().to_string();
        assert!(err.contains("not found"), "got: {err}");
    }

    #[test]
    fn reorder_rejects_duplicate_id() {
        let apps = vec![sample("A"), sample("B")];
        let err = reorder(apps, &["A".into(), "A".into()]).unwrap_err().to_string();
        assert!(err.contains("duplicate"), "got: {err}");
    }

    #[test]
    fn reorder_rejects_length_mismatch() {
        let apps = vec![sample("A"), sample("B"), sample("C")];
        let err = reorder(apps, &["A".into(), "B".into()]).unwrap_err().to_string();
        assert!(err.contains("length mismatch"), "got: {err}");
    }

    #[test]
    fn resolve_config_path_debug_uses_project_root() {
        // manifest_dir is `<project>/src-tauri`; debug path is `<project>/apps.json`.
        let manifest = "/some/project/src-tauri";
        let cfg_dir = Path::new("/Users/x/Library/Application Support");
        let p = resolve_config_path(true, manifest, cfg_dir);
        assert_eq!(p, PathBuf::from("/some/project/apps.json"));
    }

    #[test]
    fn resolve_config_path_release_uses_user_config_dir() {
        let manifest = "/some/project/src-tauri";
        let cfg_dir = Path::new("/Users/x/.config");
        let p = resolve_config_path(false, manifest, cfg_dir);
        assert_eq!(p, PathBuf::from("/Users/x/.config/switchboard/apps.json"));
    }
}
