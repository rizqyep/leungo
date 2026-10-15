//! XDG path helpers and config load/save.

use crate::config::Config;
use anyhow::{Context, Result};
use std::path::PathBuf;

/// Moves `old` to `new` when only the old directory exists. Returns whether
/// it moved. A failed rename (for example across file systems) leaves both
/// untouched and is not an error.
pub fn migrate_dir(old: &std::path::Path, new: &std::path::Path) -> bool {
    if new.exists() || !old.is_dir() {
        return false;
    }
    std::fs::rename(old, new).is_ok()
}

/// One-time move of the directories used before the project was renamed from
/// `adaptive-eq`: config, state (flags, status) and data (track log).
pub fn migrate_legacy_dirs() {
    let pairs = [dirs::config_dir(), dirs::cache_dir(), dirs::data_dir()];
    for base in pairs.into_iter().flatten() {
        if migrate_dir(&base.join("adaptive-eq"), &base.join("leungo")) {
            tracing::info!(
                "moved {} to {}",
                base.join("adaptive-eq").display(),
                base.join("leungo").display()
            );
        }
    }
}

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("leungo")
        .join("config.toml")
}

pub fn state_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("leungo")
}

pub fn status_path() -> PathBuf {
    state_dir().join("status.json")
}

pub fn bypass_flag_path() -> PathBuf {
    state_dir().join("bypass")
}

/// Holds the tuning preset chosen at runtime (GUI or CLI); the engine polls it.
pub fn tuning_path() -> PathBuf {
    state_dir().join("tuning")
}

/// Holds the sound signature and device strength chosen at runtime as JSON
/// (`bass`, `warmth`, `presence`, `air`, `device`); the engine polls it.
pub fn signature_path() -> PathBuf {
    state_dir().join("signature.json")
}

/// One JSON line per analyzed track: descriptors, archetype, confidence.
pub fn track_log_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("leungo")
        .join("tracks.jsonl")
}

pub fn fitted_classifier_path() -> PathBuf {
    config_path().with_file_name("classifier.json")
}

/// The classifier fitted by `leungo fit`, if one exists and parses.
pub fn load_fitted_classifier() -> Option<leungo_analysis::FittedClassifier> {
    let raw = std::fs::read_to_string(fitted_classifier_path()).ok()?;
    serde_json::from_str(&raw).ok()
}

pub fn load_or_create_config() -> Result<Config> {
    let path = config_path();
    if !path.exists() {
        let cfg = Config::default();
        save_config(&cfg)?;
        return Ok(cfg);
    }
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let cfg: Config = toml::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    Ok(cfg)
}

pub fn save_config(cfg: &Config) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let toml = toml::to_string_pretty(cfg).context("serialize config")?;
    std::fs::write(&path, toml).with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("leungo-paths-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn legacy_dir_is_moved_once() {
        let base = tmp("move");
        let old = base.join("adaptive-eq");
        let new = base.join("leungo");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("config.toml"), "x").unwrap();
        assert!(migrate_dir(&old, &new));
        assert_eq!(
            std::fs::read_to_string(new.join("config.toml")).unwrap(),
            "x"
        );
        assert!(!old.exists());
        assert!(!migrate_dir(&old, &new));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn existing_new_dir_is_never_overwritten() {
        let base = tmp("keep");
        let old = base.join("adaptive-eq");
        let new = base.join("leungo");
        std::fs::create_dir_all(&old).unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("config.toml"), "new").unwrap();
        std::fs::write(old.join("config.toml"), "old").unwrap();
        assert!(!migrate_dir(&old, &new));
        assert_eq!(
            std::fs::read_to_string(new.join("config.toml")).unwrap(),
            "new"
        );
        assert!(old.exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
