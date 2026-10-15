//! XDG path helpers and config load/save.

use crate::config::Config;
use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn config_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("adaptive-eq")
        .join("config.toml")
}

pub fn state_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("adaptive-eq")
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
        .join("adaptive-eq")
        .join("tracks.jsonl")
}

pub fn fitted_classifier_path() -> PathBuf {
    config_path().with_file_name("classifier.json")
}

/// The classifier fitted by `adaptive-eq fit`, if one exists and parses.
pub fn load_fitted_classifier() -> Option<ae_analysis::FittedClassifier> {
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
