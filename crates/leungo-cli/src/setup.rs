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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub enum Blocker {
    Driver,
    Output,
}

impl Blocker {
    pub fn as_str(self) -> &'static str {
        match self {
            Blocker::Driver => "driver",
            Blocker::Output => "output",
        }
    }
}

#[derive(Default)]
pub struct Choices {
    pub inputs: Vec<String>,
    pub outputs: Vec<String>,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn output_blocked(outputs: &[&str], default: Option<&str>, loopback: &str, wanted: &str) -> bool {
    if wanted.is_empty() {
        return default.is_some_and(|d| any_matches(std::iter::once(d), loopback));
    }
    !any_matches(outputs.iter().copied(), wanted)
}

#[cfg(target_os = "macos")]
pub fn blocker(loopback: &str, wanted_output: &str) -> Option<Blocker> {
    if !driver_present(loopback) {
        return Some(Blocker::Driver);
    }
    let names = leungo_standalone::route::device_names();
    let outputs: Vec<&str> = names.iter().map(String::as_str).collect();
    let default = leungo_standalone::io::default_output_name();
    output_blocked(&outputs, default.as_deref(), loopback, wanted_output).then_some(Blocker::Output)
}

#[cfg(not(target_os = "macos"))]
pub fn blocker(_loopback: &str, _wanted_output: &str) -> Option<Blocker> {
    None
}

#[cfg(target_os = "macos")]
pub fn default_moved(loopback: &str, active: &str) -> bool {
    match leungo_standalone::io::default_output_name() {
        Some(d) => !any_matches(std::iter::once(d.as_str()), loopback) && d != active,
        None => false,
    }
}

#[cfg(not(target_os = "macos"))]
pub fn default_moved(_loopback: &str, _active: &str) -> bool {
    false
}

/// A saved choice is either a device's exact name or a fragment of one, as
/// `[standalone]` allows.
pub fn is_known_device(names: &[String], choice: &str) -> bool {
    !choice.is_empty()
        && (names.iter().any(|n| n == choice)
            || any_matches(names.iter().map(String::as_str), choice))
}

/// True when a device the running engine depends on has gone away. An empty
/// output means the engine opened whatever the default was at start, so only
/// the driver is checked.
pub fn device_lost(loopback: &str, wanted_output: &str) -> bool {
    if wanted_output.is_empty() {
        return !driver_present(loopback);
    }
    blocker(loopback, wanted_output).is_some()
}

/// Devices the dashboard offers: loopback-like outputs are left out so the
/// engine cannot be pointed back at its own input.
#[cfg(target_os = "macos")]
pub fn choices(loopback: &str) -> Choices {
    let list = leungo_standalone::io::list_devices().unwrap_or_default();
    let mut out = Choices::default();
    for d in &list {
        if d.input && !out.inputs.contains(&d.name) {
            out.inputs.push(d.name.clone());
        }
        let is_loopback = any_matches(std::iter::once(d.name.as_str()), loopback);
        if d.output && !is_loopback && !out.outputs.contains(&d.name) {
            out.outputs.push(d.name.clone());
        }
    }
    out
}

#[cfg(not(target_os = "macos"))]
pub fn choices(_loopback: &str) -> Choices {
    Choices::default()
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
    use super::{any_matches, is_blackhole, is_known_device, output_blocked};

    #[test]
    fn accepts_exact_names_and_saved_fragments_only() {
        let names = vec![
            "BlackHole 2ch".to_string(),
            "MacBook Air Microphone".to_string(),
        ];
        assert!(is_known_device(&names, "BlackHole 2ch"));
        assert!(is_known_device(&names, "blackhole"));
        assert!(!is_known_device(&names, ""));
        assert!(!is_known_device(&names, "AB17X"));
    }

    #[test]
    fn blocks_when_the_default_output_is_the_loopback_device() {
        let outs = ["AB17X USB Audio", "BlackHole 2ch"];
        assert!(output_blocked(
            &outs,
            Some("BlackHole 2ch"),
            "BlackHole",
            ""
        ));
        assert!(!output_blocked(
            &outs,
            Some("AB17X USB Audio"),
            "BlackHole",
            ""
        ));
    }

    #[test]
    fn blocks_when_the_chosen_output_is_missing() {
        let outs = ["MacBook Air Speakers"];
        assert!(output_blocked(&outs, None, "BlackHole", "AB17X"));
        assert!(!output_blocked(&outs, None, "BlackHole", "speakers"));
    }

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
