//! TOML config with defaults for every knob.

use anyhow::{Result, bail};
use leungo_eq::targets;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// How hard and how fast the adaptive layer works.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Preset {
    Conservative,
    Balanced,
    Aggressive,
}

impl Preset {
    pub const ALL: [Preset; 3] = [Preset::Conservative, Preset::Balanced, Preset::Aggressive];

    pub fn as_str(self) -> &'static str {
        match self {
            Preset::Conservative => "conservative",
            Preset::Balanced => "balanced",
            Preset::Aggressive => "aggressive",
        }
    }

    pub fn parse(s: &str) -> Option<Preset> {
        Preset::ALL.into_iter().find(|p| p.as_str() == s)
    }

    /// Overwrites the eight settings that decide how much is corrected and
    /// how quickly the curve follows the music. Device curve, targets and
    /// capture settings are never touched.
    pub fn apply(self, cfg: &mut Config) {
        // strength, dead zone, cap, refine every, alpha, max step, defer max,
        // shape window
        let (st, dz, cap, every, alpha, step, defer, window) = match self {
            Preset::Conservative => (0.35, 3.0, 4.0, 8.0, 0.25, 1.0, 12.0, 30.0),
            Preset::Balanced => (0.5, 2.0, 6.0, 5.0, 0.35, 1.5, 10.0, 20.0),
            Preset::Aggressive => (0.8, 1.0, 8.0, 2.0, 0.5, 2.5, 6.0, 10.0),
        };
        cfg.analysis.shape_window_s = window;
        cfg.eq.strength = st;
        cfg.eq.deadzone_db = dz;
        cfg.eq.residual_cap_db = cap;
        cfg.analysis.refine_interval_s = every;
        cfg.analysis.refine_alpha = alpha;
        cfg.analysis.refine_max_step_db = step;
        cfg.analysis.defer_max_s = defer;
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Named bundle of the settings below; `None` uses the values as written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tuning: Option<Preset>,
    pub capture: CaptureCfg,
    pub standalone: StandaloneCfg,
    pub analysis: AnalysisCfg,
    pub eq: EqCfg,
    pub targets: TargetsCfg,
    pub applier: ApplierCfg,
    pub gui: GuiCfg,
}

impl Config {
    pub fn validate(&self) -> Result<()> {
        let n = self.eq.frequencies.len();
        for (name, curve) in [
            ("eq.base_curve", &self.eq.base_curve),
            ("eq.device_curve", &self.eq.device_curve),
            ("targets.bass_heavy", &self.targets.bass_heavy),
            ("targets.vocal_centric", &self.targets.vocal_centric),
            ("targets.guitar_band", &self.targets.guitar_band),
            ("targets.bright_acoustic", &self.targets.bright_acoustic),
            ("targets.neutral", &self.targets.neutral),
        ] {
            if curve.len() != n {
                bail!("{name} has {} entries, eq.frequencies has {n}", curve.len());
            }
        }
        if self.analysis.hop == 0 || self.analysis.hop > self.analysis.fft_size {
            bail!("analysis.hop must be in 1..=fft_size");
        }
        if self.capture.channels == 0 || self.capture.channels > 2 {
            bail!("capture.channels must be 1 or 2");
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct CaptureCfg {
    pub rate: u32,
    pub channels: usize,
    /// PulseAudio/PipeWire source to record. `@DEFAULT_MONITOR@` follows the
    /// default output device.
    pub source: String,
}

impl Default for CaptureCfg {
    fn default() -> Self {
        Self {
            rate: 48_000,
            channels: 2,
            source: "@DEFAULT_MONITOR@".into(),
        }
    }
}

/// Settings for the standalone (no EasyEffects) engine used on macOS.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct StandaloneCfg {
    /// Part of the loopback device's name. Set the system output to it.
    pub input_device: String,
    /// Part of the real output device's name; empty uses the system default,
    /// which must not be the loopback device.
    pub output_device: String,
    /// Output queue length, milliseconds. Lower is tighter lip sync, higher
    /// is safer against dropouts.
    pub latency_ms: u32,
    /// Point the system output at the loopback device while leungo runs and
    /// restore the previous device afterwards.
    pub manage_output: bool,
}

impl Default for StandaloneCfg {
    fn default() -> Self {
        Self {
            input_device: "BlackHole".into(),
            output_device: String::new(),
            latency_ms: 60,
            manage_output: true,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisCfg {
    pub fft_size: usize,
    pub hop: usize,
    /// Time constant of the long-term spectrum average, seconds.
    pub tau_s: f64,
    pub silence_threshold_db: f64,
    /// Silence longer than this, then audio, means a new track.
    pub gap_s: f64,
    /// Watch MPRIS players (Spotify, browsers) and restart analysis the
    /// moment the metadata changes, faster and more reliable than listening
    /// for silence or spectral spikes.
    pub use_mpris: bool,
    /// Refinement steps smaller than this many dB in every band are not
    /// applied, so the preset only moves when it is audible.
    pub minimum_change_db: f64,
    /// Non-silent audio collected per track before classifying, seconds.
    pub window_s: f64,
    /// Force analysis after this much wall time even on quiet tracks.
    pub max_wall_s: f64,
    /// Seconds between refinement passes while a track plays. The curve
    /// follows the music slowly instead of reacting to detected events.
    /// 0 disables refinement.
    pub refine_interval_s: f64,
    /// Fraction of the remaining distance to the freshly solved curve taken
    /// per refinement pass, 0..=1.
    pub refine_alpha: f64,
    /// Largest change per band in one refinement pass, dB.
    pub refine_max_step_db: f64,
    /// A curve change inside a track waits for a quiet moment (a dip in
    /// level) so the switch hides in the music. Applied anyway after this
    /// many seconds. 0 applies immediately.
    pub defer_max_s: f64,
    /// How far below the recent average level a frame must fall to count as
    /// a quiet moment, dB.
    pub defer_dip_db: f64,
    /// Seconds of recent audio the spectral shape is measured over (median
    /// across frames, each normalized by its own level). Longer is steadier,
    /// shorter follows sections faster.
    pub shape_window_s: f64,
}

impl Default for AnalysisCfg {
    fn default() -> Self {
        Self {
            fft_size: 2048,
            hop: 1024,
            tau_s: 2.0,
            silence_threshold_db: -60.0,
            gap_s: 1.2,
            use_mpris: true,
            minimum_change_db: 1.0,
            window_s: 6.0,
            max_wall_s: 20.0,
            refine_interval_s: 5.0,
            refine_alpha: 0.35,
            refine_max_step_db: 1.5,
            defer_max_s: 10.0,
            defer_dip_db: 4.0,
            shape_window_s: 20.0,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct EqCfg {
    pub frequencies: Vec<f64>,
    pub q: f64,
    pub clamp_db: f64,
    pub ridge: f64,
    /// Fraction of the measured deviation that is corrected, 0..=1. Scaled
    /// down further when the classifier is unsure. Lower keeps more of the
    /// mastering intent, higher flattens harder.
    pub strength: f64,
    /// Your existing manual IEM corrections, gains in dB at `frequencies`.
    /// The adaptive layer corrects content on top of this curve.
    pub base_curve: Vec<f64>,
    /// Fixed correction of your headphones/IEMs at `frequencies`, applied in
    /// full (it is a measurement, not a guess). Made by `leungo device`.
    pub device_curve: Vec<f64>,
    /// Device correction as free filters, made by `leungo device` from
    /// measurements. Run as their own stage, scaled by `device_strength`.
    pub device_filters: Vec<leungo_eq::device::Filter>,
    /// Deviations from a track's own spectral tilt smaller than this are left
    /// alone, and larger ones are corrected only by the excess.
    pub deadzone_db: f64,
    /// Largest per-point deviation the adaptive layer will chase.
    pub residual_cap_db: f64,
    /// Fraction by which the bass and treble corrections are reduced for
    /// fully percussive material, whose spectrum is normally higher at both
    /// ends. 0 disables, 1 removes the correction there.
    pub percussive_relief: f64,
    /// Judge drums per band instead of per track: a band's cut is eased by
    /// how much of that band is drum hits (kick punch stays, a boomy bass
    /// line still gets cut). On by default since 0.1.7.
    pub per_band_punch: bool,
    /// Fraction of `device_curve` that is applied, 0..=1. Below 1 keeps some
    /// of the headphone's own character instead of neutralizing it fully.
    pub device_strength: f64,
    /// Character added on top of the correction. Flat by default.
    pub signature: leungo_eq::Signature,
}

impl Default for EqCfg {
    fn default() -> Self {
        Self {
            frequencies: leungo_eq::BAND_FREQS.to_vec(),
            q: 1.41,
            clamp_db: 12.0,
            ridge: 0.05,
            strength: 0.5,
            base_curve: vec![0.0; leungo_eq::BAND_FREQS.len()],
            device_curve: vec![0.0; leungo_eq::BAND_FREQS.len()],
            device_filters: Vec::new(),
            deadzone_db: 2.0,
            residual_cap_db: 6.0,
            percussive_relief: 0.5,
            per_band_punch: true,
            device_strength: 0.6,
            signature: leungo_eq::Signature::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct TargetsCfg {
    pub bass_heavy: Vec<f64>,
    pub vocal_centric: Vec<f64>,
    pub guitar_band: Vec<f64>,
    pub bright_acoustic: Vec<f64>,
    pub neutral: Vec<f64>,
}

impl Default for TargetsCfg {
    fn default() -> Self {
        Self {
            bass_heavy: targets::DELTA_BASS_HEAVY.to_vec(),
            vocal_centric: targets::DELTA_VOCAL_CENTRIC.to_vec(),
            guitar_band: targets::DELTA_GUITAR_BAND.to_vec(),
            bright_acoustic: targets::DELTA_BRIGHT_ACOUSTIC.to_vec(),
            neutral: targets::DELTA_NEUTRAL.to_vec(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct ApplierCfg {
    pub preset_name: String,
    /// Override for the EasyEffects output preset directory.
    pub preset_dir: Option<PathBuf>,
    /// Binary used to load presets and toggle bypass.
    pub command: String,
}

impl Default for ApplierCfg {
    fn default() -> Self {
        Self {
            preset_name: "leungo".into(),
            preset_dir: None,
            command: "easyeffects".into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct GuiCfg {
    pub port: u16,
    /// How the dashboard is shown when the daemon starts: `app` (its own
    /// window, no tabs or address bar, using a Chromium-family browser if one
    /// is installed), `browser` (a normal tab in the default browser) or
    /// `none` (print the address only).
    pub open: String,
}

impl Default for GuiCfg {
    fn default() -> Self {
        Self {
            port: 8431,
            open: "app".into(),
        }
    }
}

/// Target-curve gains at `eq.frequencies` for an archetype:
/// base_curve + archetype delta.
pub fn target_delta(cfg: &Config, a: leungo_eq::Archetype) -> Vec<f64> {
    let t = &cfg.targets;
    let delta: &[f64] = match a {
        leungo_eq::Archetype::BassHeavy => &t.bass_heavy,
        leungo_eq::Archetype::VocalCentric => &t.vocal_centric,
        leungo_eq::Archetype::GuitarBand => &t.guitar_band,
        leungo_eq::Archetype::BrightAcoustic => &t.bright_acoustic,
        leungo_eq::Archetype::Neutral => &t.neutral,
    };
    cfg.eq
        .base_curve
        .iter()
        .zip(delta.iter())
        .map(|(b, d)| b + d)
        .collect()
}

#[cfg(test)]
mod preset_tests {
    use super::*;

    #[test]
    fn balanced_matches_the_defaults() {
        let mut c = Config::default();
        let d = Config::default();
        Preset::Balanced.apply(&mut c);
        assert_eq!(c.eq.strength, d.eq.strength);
        assert_eq!(c.eq.deadzone_db, d.eq.deadzone_db);
        assert_eq!(c.eq.residual_cap_db, d.eq.residual_cap_db);
        assert_eq!(c.analysis.refine_interval_s, d.analysis.refine_interval_s);
        assert_eq!(c.analysis.refine_alpha, d.analysis.refine_alpha);
        assert_eq!(c.analysis.refine_max_step_db, d.analysis.refine_max_step_db);
        assert_eq!(c.analysis.defer_max_s, d.analysis.defer_max_s);
        assert_eq!(c.analysis.shape_window_s, d.analysis.shape_window_s);
    }

    #[test]
    fn presets_are_ordered_by_aggressiveness() {
        let get = |p: Preset| {
            let mut c = Config::default();
            p.apply(&mut c);
            c
        };
        let (a, b, c) = (
            get(Preset::Conservative),
            get(Preset::Balanced),
            get(Preset::Aggressive),
        );
        assert!(a.eq.strength < b.eq.strength && b.eq.strength < c.eq.strength);
        assert!(a.eq.deadzone_db > b.eq.deadzone_db && b.eq.deadzone_db > c.eq.deadzone_db);
        assert!(
            a.analysis.refine_interval_s > b.analysis.refine_interval_s
                && b.analysis.refine_interval_s > c.analysis.refine_interval_s
        );
    }

    #[test]
    fn names_round_trip_and_unknown_is_rejected() {
        for p in Preset::ALL {
            assert_eq!(Preset::parse(p.as_str()), Some(p));
        }
        assert_eq!(Preset::parse("custom"), None);
    }

    #[test]
    fn config_with_a_preset_survives_toml() {
        let c = Config {
            tuning: Some(Preset::Aggressive),
            ..Config::default()
        };
        let text = toml::to_string_pretty(&c).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(back.tuning, Some(Preset::Aggressive));
        let none = toml::to_string_pretty(&Config::default()).unwrap();
        assert!(!none.contains("tuning"));
    }
}
