//! EasyEffects applier: writes a parametric-EQ output preset and asks
//! EasyEffects to load it. Presets live under
//! `$XDG_DATA_HOME/easyeffects/output/` (EasyEffects 7 and 8 both read this
//! JSON format; 8.x normalizes the legacy `plugins_order` entries).

use anyhow::{Context, Result};
use leungo_core::{ApplierCfg, EqApplier, EqOutput};
use leungo_eq::BAND_FREQS;
use leungo_eq::device::{Filter, Kind, peaks};
use serde::Serialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::Command;

const SONG_Q: f64 = 1.41;

pub fn default_ee_output_dir() -> PathBuf {
    dirs_data_dir().join("easyeffects").join("output")
}

fn dirs_data_dir() -> PathBuf {
    // mirrors dirs::data_dir() without adding the dep twice
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            home.join(".local").join("share")
        })
}

#[derive(Serialize)]
struct EePreset {
    output: EeOutput,
}

#[derive(Serialize)]
struct EeOutput {
    blocklist: Vec<String>,
    #[serde(rename = "plugins_order")]
    plugins_order: Vec<String>,
    // Settings are written under both keys: EasyEffects 8 reads the
    // instance form (`equalizer#0`), 7.x reads the bare name.
    #[serde(rename = "equalizer#0")]
    equalizer8: EeEqualizer,
    equalizer: EeEqualizer,
}

#[derive(Clone, Serialize)]
struct EeEqualizer {
    #[serde(rename = "input-gain")]
    input_gain: f64,
    #[serde(rename = "output-gain")]
    output_gain: f64,
    mode: String,
    #[serde(rename = "num-bands")]
    num_bands: usize,
    #[serde(rename = "split-channels")]
    split_channels: bool,
    left: EeChannel,
    right: EeChannel,
}

#[derive(Clone, Serialize)]
struct EeChannel {
    #[serde(flatten)]
    bands: BTreeMap<String, EeBand>,
}

#[derive(Clone, Serialize)]
struct EeBand {
    frequency: f64,
    gain: f64,
    q: f64,
    #[serde(rename = "type")]
    filter_type: String,
    mode: String,
    slope: String,
    mute: bool,
    solo: bool,
}

pub struct EasyEffectsApplier {
    preset_path: PathBuf,
    preset_name: String,
    command: String,
}

impl EasyEffectsApplier {
    pub fn new(cfg: &ApplierCfg) -> Self {
        let dir = cfg.preset_dir.clone().unwrap_or_else(default_ee_output_dir);
        Self {
            preset_path: dir.join(format!("{}.json", cfg.preset_name)),
            preset_name: cfg.preset_name.clone(),
            command: cfg.command.clone(),
        }
    }

    pub fn preset_path(&self) -> &PathBuf {
        &self.preset_path
    }

    fn write_preset(&self, eq: &EqOutput) -> Result<()> {
        let mut filters = peaks(&BAND_FREQS, &eq.gains, SONG_Q);
        filters.extend_from_slice(&eq.device);
        let band = |f: &Filter| EeBand {
            frequency: f.fc,
            gain: f.gain_db,
            q: f.q,
            filter_type: match f.kind {
                Kind::Peak => "Bell",
                Kind::LowShelf => "Lo-shelf",
                Kind::HighShelf => "Hi-shelf",
            }
            .into(),
            // APO (DR) is the RBJ cookbook math, same as our fitter
            mode: "APO (DR)".into(),
            slope: "x1".into(),
            mute: false,
            solo: false,
        };
        let channel = || EeChannel {
            bands: filters
                .iter()
                .enumerate()
                .map(|(i, f)| (format!("band{i}"), band(f)))
                .collect(),
        };
        let equalizer = EeEqualizer {
            input_gain: eq.preamp_db,
            output_gain: 0.0,
            mode: "IIR".into(),
            num_bands: filters.len(),
            split_channels: false,
            left: channel(),
            right: channel(),
        };
        let preset = EePreset {
            output: EeOutput {
                blocklist: vec![],
                plugins_order: vec!["equalizer#0".into()],
                equalizer8: equalizer.clone(),
                equalizer,
            },
        };
        if let Some(parent) = self.preset_path.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(&preset)?;
        std::fs::write(&self.preset_path, json)
            .with_context(|| format!("write {}", self.preset_path.display()))
    }

    fn load_preset(&self) {
        // Spawn and detach: EasyEffects is a GUI app that stays alive, so
        // waiting on it from the engine thread would freeze everything.
        match Command::new(&self.command)
            .arg("-l")
            .arg(&self.preset_name)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(child) => {
                // Reap on a throwaway thread so the child never zombies.
                std::thread::spawn(move || {
                    let mut child = child;
                    let _ = child.wait();
                });
            }
            Err(_) => {
                tracing::warn!(
                    "could not run '{}': is EasyEffects installed and running?",
                    self.command
                )
            }
        }
    }
}

impl EqApplier for EasyEffectsApplier {
    fn apply_eq(&mut self, eq: &EqOutput) -> Result<()> {
        anyhow::ensure!(
            eq.gains.len() == BAND_FREQS.len(),
            "expected {} gains",
            BAND_FREQS.len()
        );
        self.write_preset(eq)?;
        self.load_preset();
        Ok(())
    }

    fn set_bypass(&mut self, on: bool) -> Result<()> {
        let arg = if on { "1" } else { "2" };
        let status = Command::new(&self.command)
            .args(["-b", arg])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
        match status {
            Ok(child) => {
                std::thread::spawn(move || {
                    let mut child = child;
                    let _ = child.wait();
                });
            }
            Err(_) => {
                tracing::warn!(
                    "could not run '{}': is EasyEffects installed and running?",
                    self.command
                );
                if on {
                    // fall back to a flat preset so the effect still neutralizes
                    let flat = EqOutput {
                        gains: vec![0.0; BAND_FREQS.len()],
                        ..EqOutput::default()
                    };
                    self.write_preset(&flat)?;
                    self.load_preset();
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preset_json_has_easyeffects_schema() {
        let dir = std::env::temp_dir().join(format!("ae-test-{}", std::process::id()));
        let cfg = ApplierCfg {
            preset_dir: Some(dir.clone()),
            ..ApplierCfg::default()
        };
        let applier = EasyEffectsApplier::new(&cfg);
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
        applier.write_preset(&eq).unwrap();
        let raw = std::fs::read_to_string(dir.join("leungo.json")).unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();

        let out = &v["output"];
        assert_eq!(out["plugins_order"][0], "equalizer#0");
        // both instance forms present for EE 7 and EE 8
        for key in ["equalizer#0", "equalizer"] {
            let eq = &out[key];
            assert_eq!(eq["num-bands"], 9);
            assert_eq!(eq["mode"], "IIR");
            assert_eq!(eq["input-gain"], -1.5);
            assert_eq!(eq["left"]["band0"]["mode"], "APO (DR)");
            assert_eq!(eq["left"]["band8"]["type"], "Lo-shelf");
            assert_eq!(eq["left"]["band8"]["gain"], -4.0);
            assert_eq!(eq["left"]["band0"]["frequency"], 63.0);
            assert_eq!(eq["left"]["band0"]["gain"], 1.5);
            assert_eq!(eq["left"]["band0"]["type"], "Bell");
            assert_eq!(eq["left"]["band1"]["gain"], -2.0);
            assert_eq!(eq["right"]["band0"]["frequency"], 63.0);
        }
        std::fs::remove_dir_all(&dir).ok();
    }
}
