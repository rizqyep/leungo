//! Equalizer APO applier: writes the EQ as an Equalizer APO config file and
//! includes it from `config.txt`. Equalizer APO watches its config folder
//! and reloads on every change, so nothing has to be told to load it.
//!
//! The file format is plain text and the logic builds everywhere; finding
//! the installation and the devices it runs on reads the Windows registry.

use anyhow::{Context, Result};
use leungo_core::{ApplierCfg, EqApplier, EqOutput};
use leungo_eq::BAND_FREQS;
use leungo_eq::device::{Filter, Kind, peaks};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

const SONG_Q: f64 = 1.41;

/// The `config.txt` the Equalizer APO installer writes: a demo bass boost.
const STOCK_CONFIG: [&str; 3] = [
    "Preamp: -6 dB",
    "Include: example.txt",
    "GraphicEQ: 25 0; 40 0; 63 0; 100 0; 160 0; 250 0; 400 0; 630 0; 1000 0; 1600 0; 2500 0; 4000 0; 6300 0; 10000 0; 16000 0",
];

pub struct ApoApplier {
    dir: PathBuf,
    file_name: String,
    included: bool,
}

impl ApoApplier {
    pub fn new(cfg: &ApplierCfg) -> Result<Self> {
        let dir = match &cfg.preset_dir {
            Some(d) => d.clone(),
            None => config_dir().context("Equalizer APO is not installed")?,
        };
        Ok(Self {
            dir,
            file_name: format!("{}.txt", cfg.preset_name),
            included: false,
        })
    }

    fn write(&mut self, text: &str) -> Result<()> {
        if !self.included {
            include_from_main_config(&self.dir.join("config.txt"), &self.file_name)?;
            self.included = true;
        }
        let path = self.dir.join(&self.file_name);
        // Equalizer APO could reload between a truncate and the write and
        // play a flat curve for a moment; a rename is atomic.
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
        retry_while_busy(|| std::fs::rename(&tmp, &path))
            .with_context(|| format!("replace {}", path.display()))
    }
}

/// Equalizer APO opens its config files to reload them whenever anything in
/// the folder changes, and Windows refuses to replace a file that is open.
fn retry_while_busy<T>(mut op: impl FnMut() -> std::io::Result<T>) -> std::io::Result<T> {
    let mut attempt = 0;
    loop {
        match op() {
            Err(e) if is_busy(&e) && attempt < 25 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            result => return result,
        }
    }
}

/// Access denied, sharing violation, lock violation.
fn is_busy(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(5 | 32 | 33))
}

impl EqApplier for ApoApplier {
    fn apply_eq(&mut self, eq: &EqOutput) -> Result<()> {
        anyhow::ensure!(
            eq.gains.len() == BAND_FREQS.len(),
            "expected {} gains",
            BAND_FREQS.len()
        );
        self.write(&render(eq))
    }

    fn set_bypass(&mut self, on: bool) -> Result<()> {
        if on {
            self.write(&render(&EqOutput {
                gains: vec![0.0; BAND_FREQS.len()],
                ..EqOutput::default()
            }))?;
        }
        Ok(())
    }
}

/// The Equalizer APO config text for one EQ: the preamp, then the song
/// bands and the device filters as RBJ biquads.
pub fn render(eq: &EqOutput) -> String {
    let mut filters = peaks(&BAND_FREQS, &eq.gains, SONG_Q);
    filters.extend_from_slice(&eq.device);
    let mut out = String::from("# Written by leungo; replaced on every change.\n");
    let _ = writeln!(out, "Preamp: {:.2} dB", eq.preamp_db);
    for f in &filters {
        let _ = writeln!(out, "{}", filter_line(f));
    }
    out
}

fn filter_line(f: &Filter) -> String {
    let kind = match f.kind {
        Kind::Peak => "PK",
        Kind::LowShelf => "LSC",
        Kind::HighShelf => "HSC",
    };
    format!(
        "Filter: ON {kind} Fc {:.1} Hz Gain {:.2} dB Q {:.3}",
        f.fc, f.gain_db, f.q
    )
}

/// Makes `config.txt` include leungo's file. The installer's demo config is
/// replaced (it adds its own bass boost and a -6 dB preamp on top), and
/// anything the user wrote is kept, with the include appended.
fn include_from_main_config(config: &Path, file_name: &str) -> Result<()> {
    let current = std::fs::read_to_string(config).unwrap_or_default();
    let Some(updated) = with_include(&current, file_name) else {
        return Ok(());
    };
    if is_stock(&current) {
        let _ = std::fs::write(config.with_extension("txt.before-leungo"), &current);
    }
    retry_while_busy(|| std::fs::write(config, &updated))
        .with_context(|| format!("write {}", config.display()))
}

/// The new `config.txt`, or `None` when it already includes `file_name`.
fn with_include(current: &str, file_name: &str) -> Option<String> {
    let line = format!("Include: {file_name}");
    if current
        .lines()
        .any(|l| l.trim().eq_ignore_ascii_case(&line))
    {
        return None;
    }
    if is_stock(current) || current.trim().is_empty() {
        return Some(format!("{line}\n"));
    }
    let mut out = current.to_string();
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&line);
    out.push('\n');
    Some(out)
}

fn is_stock(config: &str) -> bool {
    let lines: Vec<&str> = config
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines == STOCK_CONFIG
}

#[cfg(windows)]
const APO_CLSID: &str = "{EACD2258-FCAC-4FF4-B36D-419E924A6D79}";

#[cfg(windows)]
fn apo_key() -> Option<winreg::RegKey> {
    winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey("SOFTWARE\\EqualizerAPO")
        .ok()
}

/// Equalizer APO's config folder, from its registry entry.
#[cfg(windows)]
pub fn config_dir() -> Option<PathBuf> {
    let dir: String = apo_key()?.get_value("ConfigPath").ok()?;
    let dir = PathBuf::from(dir);
    dir.is_dir().then_some(dir)
}

#[cfg(not(windows))]
pub fn config_dir() -> Option<PathBuf> {
    None
}

/// Equalizer APO's program folder, where `DeviceSelector.exe` lives.
#[cfg(windows)]
pub fn install_dir() -> Option<PathBuf> {
    let dir: String = apo_key()?.get_value("InstallPath").ok()?;
    let dir = PathBuf::from(dir);
    dir.join("EqualizerAPO.dll").is_file().then_some(dir)
}

#[cfg(not(windows))]
pub fn install_dir() -> Option<PathBuf> {
    None
}

/// Whether Equalizer APO is switched on for a playback device, given the
/// endpoint id WASAPI reports (`{0.0.0.00000000}.{guid}`). The Device
/// Selector registers it as one of the device's effects in the registry.
#[cfg(windows)]
pub fn enabled_on(endpoint_id: &str) -> bool {
    let Some(guid) = endpoint_id.rsplit('.').next() else {
        return false;
    };
    let path = format!(
        "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\MMDevices\\Audio\\Render\\{guid}\\FxProperties"
    );
    let Ok(fx) = winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE).open_subkey(path) else {
        return false;
    };
    fx.enum_values()
        .filter_map(Result::ok)
        .any(|(_, v)| v.to_string().eq_ignore_ascii_case(APO_CLSID))
}

#[cfg(not(windows))]
pub fn enabled_on(_endpoint_id: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stock() -> String {
        STOCK_CONFIG.join("\r\n") + "\r\n"
    }

    #[test]
    fn renders_preamp_song_bands_and_device_filters() {
        let eq = EqOutput {
            gains: vec![1.5, -2.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0],
            device: vec![Filter {
                kind: Kind::LowShelf,
                fc: 105.0,
                gain_db: -4.0,
                q: 0.7,
            }],
            preamp_db: -1.5,
        };
        let text = render(&eq);
        let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
        assert_eq!(lines[0], "Preamp: -1.50 dB");
        assert_eq!(lines[1], "Filter: ON PK Fc 63.0 Hz Gain 1.50 dB Q 1.410");
        assert_eq!(lines[2], "Filter: ON PK Fc 125.0 Hz Gain -2.00 dB Q 1.410");
        assert_eq!(lines.len(), 1 + BAND_FREQS.len() + 1);
        assert_eq!(
            lines.last().unwrap(),
            &"Filter: ON LSC Fc 105.0 Hz Gain -4.00 dB Q 0.700"
        );
    }

    #[test]
    fn replaces_the_installer_demo_config() {
        assert!(is_stock(&stock()));
        assert_eq!(
            with_include(&stock(), "leungo.txt").unwrap(),
            "Include: leungo.txt\n"
        );
    }

    #[test]
    fn appends_to_a_config_the_user_wrote() {
        let mine = "Preamp: -3 dB\nInclude: room.txt";
        assert!(!is_stock(mine));
        assert_eq!(
            with_include(mine, "leungo.txt").unwrap(),
            "Preamp: -3 dB\nInclude: room.txt\nInclude: leungo.txt\n"
        );
    }

    #[test]
    fn leaves_a_config_that_already_includes_it() {
        assert!(with_include("Preamp: -3 dB\ninclude: LEUNGO.txt\n", "leungo.txt").is_none());
    }

    #[test]
    fn retries_only_while_the_file_is_busy() {
        let mut calls = 0;
        let r = retry_while_busy(|| {
            calls += 1;
            if calls < 3 {
                Err(std::io::Error::from_raw_os_error(32))
            } else {
                Ok(calls)
            }
        });
        assert_eq!(r.unwrap(), 3);
        let mut calls = 0;
        let r: std::io::Result<()> = retry_while_busy(|| {
            calls += 1;
            Err(std::io::Error::from_raw_os_error(2))
        });
        assert!(r.is_err());
        assert_eq!(calls, 1);
    }

    #[test]
    fn writes_the_file_and_the_include() {
        let dir = std::env::temp_dir().join(format!("leungo-apo-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("config.txt"), stock()).unwrap();
        let mut applier = ApoApplier::new(&ApplierCfg {
            preset_dir: Some(dir.clone()),
            ..ApplierCfg::default()
        })
        .unwrap();
        applier
            .apply_eq(&EqOutput {
                gains: vec![0.0; BAND_FREQS.len()],
                ..EqOutput::default()
            })
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("config.txt")).unwrap(),
            "Include: leungo.txt\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("config.txt.before-leungo")).unwrap(),
            stock()
        );
        let written = std::fs::read_to_string(dir.join("leungo.txt")).unwrap();
        assert!(written.contains("Preamp: 0.00 dB"));
        assert!(!dir.join("leungo.tmp").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
