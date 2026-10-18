//! First-run checks for what each platform needs besides leungo, and a
//! guided install of the one piece it can fetch: the BlackHole 2ch loopback
//! driver on macOS, Equalizer APO on Windows.

use anyhow::{Context, Result, bail, ensure};
use std::path::Path;
use std::process::Command;

/// A pinned third-party installer. The checksum is of the exact file, so a
/// changed or tampered download is refused.
pub struct Installer {
    pub name: &'static str,
    url: &'static str,
    sha256: &'static str,
    file_name: &'static str,
    /// Where to get it by hand when the download fails.
    pub page: &'static str,
}

#[cfg(target_os = "macos")]
pub const INSTALLER: Installer = Installer {
    name: "BlackHole 2ch",
    url: "https://existential.audio/downloads/BlackHole2ch-0.7.1.pkg",
    sha256: "57b540f27a3e29c37e310e01bee0fdfab76733087e47f997ef9dccf851400dcf",
    file_name: "BlackHole2ch.pkg",
    page: "https://existential.audio/blackhole/",
};

#[cfg(all(windows, not(target_arch = "aarch64")))]
pub const INSTALLER: Installer = Installer {
    name: "Equalizer APO",
    url: "https://sourceforge.net/projects/equalizerapo/files/1.4.2/EqualizerAPO-x64-1.4.2.exe/download",
    sha256: "7403be7427bbe1936a40dded082829b6e217fc4f5990fee5cba501f0ae055afa",
    file_name: "EqualizerAPO-x64-1.4.2.exe",
    page: "https://sourceforge.net/projects/equalizerapo/files/",
};

#[cfg(all(windows, target_arch = "aarch64"))]
pub const INSTALLER: Installer = Installer {
    name: "Equalizer APO",
    url: "https://sourceforge.net/projects/equalizerapo/files/1.4.2/EqualizerAPO-ARM64-1.4.2.exe/download",
    sha256: "0dbfc64dee60f94cf77b297feb1f54d2633d49669bb474086fffb8db23e8f066",
    file_name: "EqualizerAPO-ARM64-1.4.2.exe",
    page: "https://sourceforge.net/projects/equalizerapo/files/",
};

#[cfg(not(any(target_os = "macos", windows)))]
pub const INSTALLER: Installer = Installer {
    name: "",
    url: "",
    sha256: "",
    file_name: "",
    page: "",
};

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
    /// Linux: the EasyEffects program leungo drives is not installed.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    Effects,
    /// Windows: Equalizer APO is not installed.
    #[cfg_attr(not(windows), allow(dead_code))]
    Apo,
    /// Windows: Equalizer APO is installed but not switched on for the
    /// device Windows plays to.
    #[cfg_attr(not(windows), allow(dead_code))]
    ApoDevice,
}

impl Blocker {
    pub fn as_str(self) -> &'static str {
        match self {
            Blocker::Driver => "driver",
            Blocker::Output => "output",
            Blocker::Effects => "effects",
            Blocker::Apo => "apo",
            Blocker::ApoDevice => "apo-device",
        }
    }

    /// Whether the install button fetches what is missing.
    pub fn can_install(self, driver: &str) -> bool {
        match self {
            Blocker::Driver => is_blackhole(driver),
            Blocker::Apo => true,
            _ => false,
        }
    }
}

/// True when `command` (how leungo starts EasyEffects) can be found.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn command_in_path(command: &str, path: &std::ffi::OsStr) -> bool {
    if command.is_empty() {
        return false;
    }
    if command.contains('/') {
        return Path::new(command).is_file();
    }
    std::env::split_paths(path).any(|dir| dir.join(command).is_file())
}

#[cfg(target_os = "linux")]
pub fn effects_missing(command: &str) -> bool {
    let path = std::env::var_os("PATH").unwrap_or_default();
    !command_in_path(command, &path)
}

/// The command that installs EasyEffects, chosen from the contents of
/// /etc/os-release (`ID` and `ID_LIKE`).
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn install_hint(os_release: &str) -> String {
    let mut ids: Vec<String> = Vec::new();
    for line in os_release.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        if key == "ID" || key == "ID_LIKE" {
            ids.extend(
                value
                    .trim_matches('"')
                    .split_whitespace()
                    .map(|s| s.to_lowercase()),
            );
        }
    }
    let has = |names: &[&str]| ids.iter().any(|i| names.contains(&i.as_str()));
    if has(&["fedora", "rhel", "centos"]) {
        "sudo dnf install easyeffects".into()
    } else if has(&["debian", "ubuntu"]) {
        "sudo apt install easyeffects".into()
    } else if has(&["arch"]) {
        "sudo pacman -S easyeffects".into()
    } else if has(&["opensuse", "suse", "sles"]) {
        "sudo zypper install easyeffects".into()
    } else {
        "install the \"easyeffects\" package with your package manager".into()
    }
}

#[cfg(target_os = "linux")]
pub fn current_install_hint() -> String {
    install_hint(&std::fs::read_to_string("/etc/os-release").unwrap_or_default())
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

#[cfg(windows)]
pub fn blocker(_loopback: &str, _wanted_output: &str) -> Option<Blocker> {
    if leungo_applier_apo::install_dir().is_none() || leungo_applier_apo::config_dir().is_none() {
        return Some(Blocker::Apo);
    }
    match leungo_capture_wasapi::default_output() {
        Some((_, id)) if !leungo_applier_apo::enabled_on(&id) => Some(Blocker::ApoDevice),
        _ => None,
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
pub fn blocker(_loopback: &str, _wanted_output: &str) -> Option<Blocker> {
    None
}

/// The device Windows plays to, which Equalizer APO has to be on for.
#[cfg(windows)]
pub fn system_output() -> String {
    leungo_capture_wasapi::default_output()
        .map(|(name, _)| name)
        .unwrap_or_default()
}

#[cfg(not(windows))]
pub fn system_output() -> String {
    String::new()
}

/// Opens Equalizer APO's Device Selector, where the user ticks the devices
/// it runs on. It asks for administrator rights itself.
#[cfg(windows)]
pub fn open_device_selector() -> Result<()> {
    let dir = leungo_applier_apo::install_dir().context("Equalizer APO is not installed")?;
    crate::win::shell_open(dir.join("DeviceSelector.exe").as_os_str(), "runas")
}

#[cfg(not(windows))]
pub fn open_device_selector() -> Result<()> {
    bail!("the Device Selector is part of Equalizer APO on Windows")
}

#[cfg(target_os = "macos")]
pub fn default_moved(loopback: &str, active: &str) -> bool {
    match leungo_standalone::io::default_output_name() {
        Some(d) => !any_matches(std::iter::once(d.as_str()), loopback) && d != active,
        None => false,
    }
}

#[cfg(windows)]
pub fn default_moved(_loopback: &str, active: &str) -> bool {
    let now = system_output();
    !now.is_empty() && now != active
}

#[cfg(not(any(target_os = "macos", windows)))]
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
    if wanted_output.is_empty() && !cfg!(windows) {
        return !driver_present(loopback);
    }
    blocker(loopback, wanted_output).is_some()
}

/// Devices the dashboard offers: loopback-like outputs are left out so the
/// engine cannot be pointed back at its own input.
#[cfg(target_os = "macos")]
pub fn choices(_loopback: &str) -> Choices {
    Choices {
        inputs: leungo_standalone::route::loopback_inputs(),
        outputs: leungo_standalone::route::real_outputs()
            .into_iter()
            .map(|o| o.name)
            .collect(),
    }
}

/// The capture device must be a virtual loopback; a real input such as a
/// microphone or earphone falls back to the default.
#[cfg(target_os = "macos")]
pub fn sane_capture(configured: &str) -> String {
    let names = leungo_standalone::route::loopback_inputs();
    if is_known_device(&names, configured) {
        configured.to_string()
    } else {
        "BlackHole".to_string()
    }
}

#[cfg(not(target_os = "macos"))]
pub fn sane_capture(configured: &str) -> String {
    configured.to_string()
}

#[cfg(not(target_os = "macos"))]
pub fn choices(_loopback: &str) -> Choices {
    Choices::default()
}

fn sha256_of(path: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Ok(Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(target_os = "macos")]
fn open_installer(path: &Path) -> Result<()> {
    let status = Command::new("open")
        .arg(path)
        .status()
        .context("run open")?;
    ensure!(status.success(), "could not open the installer");
    Ok(())
}

#[cfg(windows)]
fn open_installer(path: &Path) -> Result<()> {
    crate::win::shell_open(path.as_os_str(), "open").context("could not open the installer")
}

#[cfg(not(any(target_os = "macos", windows)))]
fn open_installer(_path: &Path) -> Result<()> {
    bail!("nothing to install on this platform")
}

const AFTER_INSTALL: &str = if cfg!(windows) {
    "In the installer, tick the device you listen on, then restart Windows."
} else {
    "Finish the installer. Restart your Mac if it asks you to."
};

/// Downloads the pinned installer, checks its SHA-256 and opens it. The
/// installer asks for administrator rights itself.
pub fn install_driver(progress: &dyn Fn(Phase, &str)) -> Result<()> {
    let installer = &INSTALLER;
    ensure!(
        !installer.url.is_empty(),
        "nothing to install on this platform"
    );
    let dir = std::env::temp_dir().join(format!("leungo-setup-{}", std::process::id()));
    std::fs::create_dir_all(&dir).context("create download directory")?;
    let file = dir.join(installer.file_name);

    progress(
        Phase::Downloading,
        &format!("Downloading {}", installer.name),
    );
    let status = Command::new("curl")
        .args([
            "--fail",
            "--location",
            "--silent",
            "--show-error",
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--output",
        ])
        .arg(&file)
        .arg(installer.url)
        .status()
        .context("run curl")?;
    if !status.success() {
        bail!(
            "download failed; get {} from {}",
            installer.name,
            installer.page
        );
    }

    let sum = sha256_of(&file)?;
    if sum != installer.sha256 {
        let _ = std::fs::remove_file(&file);
        bail!(
            "the download did not match the expected checksum, so it was discarded; get {} from {}",
            installer.name,
            installer.page
        );
    }

    open_installer(&file)?;
    progress(Phase::InstallerOpened, AFTER_INSTALL);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        Blocker, any_matches, command_in_path, install_hint, is_blackhole, is_known_device,
        output_blocked, sha256_of,
    };

    #[test]
    fn hashes_a_file_as_lowercase_hex() {
        let path = std::env::temp_dir().join(format!("leungo-sha-{}", std::process::id()));
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_of(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn offers_an_install_only_for_what_it_can_fetch() {
        assert!(Blocker::Apo.can_install(""));
        assert!(Blocker::Driver.can_install("BlackHole"));
        assert!(!Blocker::Driver.can_install("Loopback Audio"));
        assert!(!Blocker::ApoDevice.can_install(""));
        assert!(!Blocker::Effects.can_install(""));
    }

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

    #[test]
    fn finds_a_command_on_the_path_only_when_the_file_exists() {
        let dir = std::env::temp_dir().join(format!("leungo-path-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("easyeffects"), b"").unwrap();
        let path = std::env::join_paths([&dir]).unwrap();
        assert!(command_in_path("easyeffects", &path));
        assert!(!command_in_path("not-installed", &path));
        assert!(!command_in_path("", &path));
        assert!(command_in_path(
            dir.join("easyeffects").to_str().unwrap(),
            &path
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn picks_the_install_command_for_the_distribution() {
        assert_eq!(
            install_hint("NAME=Fedora\nID=fedora\nVERSION_ID=44\n"),
            "sudo dnf install easyeffects"
        );
        assert_eq!(
            install_hint("ID=linuxmint\nID_LIKE=\"ubuntu debian\"\n"),
            "sudo apt install easyeffects"
        );
        assert_eq!(
            install_hint("ID=endeavouros\nID_LIKE=arch\n"),
            "sudo pacman -S easyeffects"
        );
        assert!(install_hint("ID=plan9\n").contains("package manager"));
        assert!(install_hint("").contains("package manager"));
    }
}
