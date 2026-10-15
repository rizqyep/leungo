//! First-run check for the loopback driver the macOS engine captures from,
//! and a guided install of BlackHole 2ch.

use anyhow::{Context, Result, bail, ensure};
use std::path::Path;
use std::process::Command;

const PKG_URL: &str = "https://existential.audio/downloads/BlackHole2ch-0.7.1.pkg";
const PKG_SHA256: &str = "57b540f27a3e29c37e310e01bee0fdfab76733087e47f997ef9dccf851400dcf";
const DOWNLOAD_PAGE: &str = "https://existential.audio/blackhole/";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Idle,
    Downloading,
    InstallerOpened,
    Failed,
}

impl Phase {
    pub fn as_str(self) -> &'static str {
        match self {
            Phase::Idle => "idle",
            Phase::Downloading => "downloading",
            Phase::InstallerOpened => "opened",
            Phase::Failed => "failed",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Install {
    pub phase: Phase,
    pub detail: String,
}

impl Default for Install {
    fn default() -> Self {
        Install {
            phase: Phase::Idle,
            detail: String::new(),
        }
    }
}

pub fn is_blackhole(input_device: &str) -> bool {
    input_device.to_lowercase().contains("blackhole")
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn any_matches<'a>(mut names: impl Iterator<Item = &'a str>, needle: &str) -> bool {
    let needle = needle.to_lowercase();
    names.any(|n| n.to_lowercase().contains(&needle))
}

#[cfg(target_os = "macos")]
pub fn driver_present(input_device: &str) -> bool {
    match leungo_standalone::io::list_devices() {
        Ok(list) => any_matches(
            list.iter().filter(|d| d.input).map(|d| d.name.as_str()),
            input_device,
        ),
        Err(_) => true,
    }
}

#[cfg(not(target_os = "macos"))]
pub fn driver_present(_input_device: &str) -> bool {
    true
}

fn sha256_of(path: &Path) -> Result<String> {
    let out = Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .context("run shasum")?;
    ensure!(out.status.success(), "shasum failed");
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text.split_whitespace().next().unwrap_or("").to_lowercase())
}

/// Downloads the pinned installer, checks its SHA-256 and opens it in the
/// macOS Installer, which asks for the administrator password itself.
pub fn install_driver(progress: &dyn Fn(Phase, &str)) -> Result<()> {
    let dir = std::env::temp_dir().join(format!("leungo-setup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).context("create download directory")?;
    let pkg = dir.join("BlackHole2ch.pkg");

    progress(Phase::Downloading, "Downloading BlackHole 2ch");
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--output",
        ])
        .arg(&pkg)
        .arg(PKG_URL)
        .status()
        .context("run curl")?;
    if !status.success() {
        bail!("download failed; get BlackHole from {DOWNLOAD_PAGE}");
    }

    let sum = sha256_of(&pkg)?;
    if sum != PKG_SHA256 {
        let _ = std::fs::remove_file(&pkg);
        bail!("downloaded installer did not match the expected checksum, so it was discarded");
    }

    let status = Command::new("open")
        .arg(&pkg)
        .status()
        .context("run open")?;
    ensure!(status.success(), "could not open the installer");
    progress(
        Phase::InstallerOpened,
        "Finish the installer. Restart your Mac if it asks you to.",
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{any_matches, is_blackhole};

    #[test]
    fn matches_device_names_case_insensitively() {
        let names = ["MacBook Pro Microphone", "BlackHole 2ch"];
        assert!(any_matches(names.iter().copied(), "blackhole"));
        assert!(!any_matches(names.iter().copied(), "loopback"));
    }

    #[test]
    fn only_offers_to_install_blackhole() {
        assert!(is_blackhole("BlackHole"));
        assert!(is_blackhole("blackhole 2ch"));
        assert!(!is_blackhole("Loopback Audio"));
    }
}
