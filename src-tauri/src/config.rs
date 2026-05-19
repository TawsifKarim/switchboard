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

    Ok(())
}

pub fn add(path: &Path, entry: AppEntry) -> Result<()> {
    let mut apps = load(path)?;
    apps.push(entry);
    save(path, &apps)
}

pub fn delete(path: &Path, id: &str) -> Result<()> {
    let mut apps = load(path)?;
    apps.retain(|a| a.id != id);
    save(path, &apps)
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
