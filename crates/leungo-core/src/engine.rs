//! The engine: capture events -> spectral analysis -> classification ->
//! EQ solve -> applier, with idle/analyzing/active state tracking.

use crate::config::Config;
use crate::paths;
use crate::{EqApplier, SampleSource, SourceEvent};
use leungo_analysis::{
    Detection, DetectorConfig, FittedClassifier, SpectrumAnalyzer, TrackDetector, classify,
    descriptors, extract,
};
use leungo_eq::EqSolver;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Status {
    pub state: String,
    pub bypassed: bool,
    pub archetype: Option<String>,
    pub confidence: f64,
    pub tracks_analyzed: u64,
    pub gains: Vec<f64>,
    pub message: Option<String>,
    /// Active tuning preset, or "custom" when the config values are used.
    #[serde(default = "custom_tuning")]
    pub tuning: String,
    /// How repeatable the current spectral measurement is, 0..=1.
    #[serde(default)]
    pub reliability: f64,
    /// Sound signature and device-correction strength currently applied.
    #[serde(default)]
    pub signature: leungo_eq::Signature,
    #[serde(default = "default_device_strength")]
    pub device_strength: f64,
    /// `eq.per_band_punch`, switchable live from the dashboard.
    #[serde(default)]
    pub punch: bool,
}

fn default_device_strength() -> f64 {
    0.6
}

fn custom_tuning() -> String {
    "custom".into()
}

impl Default for Status {
    fn default() -> Self {
        Self {
            state: "idle".into(),
            bypassed: false,
            archetype: None,
            confidence: 0.0,
            tracks_analyzed: 0,
            gains: Vec::new(),
            message: None,
            tuning: custom_tuning(),
            reliability: 0.0,
            signature: leungo_eq::Signature::default(),
            device_strength: default_device_strength(),
            punch: false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum State {
    Idle,
    Analyzing,
    Active,
}

pub struct Engine {
    source: Box<dyn SampleSource>,
    applier: Box<dyn EqApplier>,
    cfg: Config,
    solver: EqSolver,
    analyzer: SpectrumAnalyzer,
    detector: TrackDetector,
    status: Arc<Mutex<Status>>,
    stop: Arc<AtomicBool>,
    bypass_path: PathBuf,
    tuning_path: Option<PathBuf>,
    base_cfg: Config,
    tuning: Option<crate::config::Preset>,
    tuning_checked_s: f64,
    signature_path: Option<PathBuf>,
    signature_checked_s: f64,
    state: State,
    bypassed: bool,
    collected_s: f64,
    analyzing_silence_s: f64,
    begin_s: f64,
    now_s: f64,
    window_peak: f64,
    window_sq: f64,
    window_n: u64,
    frame_count: u64,
    spectrum_out: Option<SpectrumSnapshot>,
    track_rx: Option<Receiver<String>>,
    last_track_id: Option<String>,
    /// Player track id the loaded curve was measured for.
    analyzed_id: Option<String>,
    last_applied: Option<Vec<f64>>,
    crest_db: f64,
    track_class: Option<(leungo_eq::Archetype, f64)>,
    reliability: f64,
    transient: f64,
    next_refine_s: f64,
    classifier: Option<FittedClassifier>,
    track_log: Option<PathBuf>,
    pending: Option<Pending>,
    rms_avg_db: Option<f64>,
}

/// A solved curve waiting for a quiet moment to be switched in.
struct Pending {
    gains: Vec<f64>,
    archetype: leungo_eq::Archetype,
    confidence: f64,
    deadline_s: f64,
}

/// Log-spaced (frequency, dB) samples of the long-term spectrum, shared
/// with the GUI through a mutex.
pub type SpectrumSnapshot = Arc<Mutex<Vec<(f64, f64)>>>;

/// `Some(Some(p))` a preset, `Some(None)` the file says "custom", `None`
/// no usable file.
fn read_tuning(path: &std::path::Path) -> Option<Option<crate::config::Preset>> {
    let text = std::fs::read_to_string(path).ok()?;
    let name = text.trim();
    if name == "custom" {
        return Some(None);
    }
    crate::config::Preset::parse(name).map(Some)
}

/// Parses the sound file: `{"bass":..,"warmth":..,"presence":..,"air":..,
/// "device":..}`. Missing keys are 0 (device: 0.6); values are clamped.
/// What the dashboard's Sound card writes. `punch` is `None` in files from
/// before it existed, so the config value stays.
struct Sound {
    sig: leungo_eq::Signature,
    device: f64,
    punch: Option<bool>,
}

fn read_signature(path: &std::path::Path) -> Option<Sound> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()?;
    let f = |k: &str, d: f64| v.get(k).and_then(|x| x.as_f64()).unwrap_or(d);
    let sig = leungo_eq::Signature {
        bass_db: f("bass", 0.0),
        warmth_db: f("warmth", 0.0),
        presence_db: f("presence", 0.0),
        air_db: f("air", 0.0),
    }
    .clamped();
    let device = f("device", 0.6);
    let device = if device.is_finite() {
        device.clamp(0.0, 1.0)
    } else {
        0.6
    };
    let punch = v.get("punch").and_then(|x| x.as_f64()).map(|x| x >= 0.5);
    Some(Sound { sig, device, punch })
}

impl Engine {
    pub fn new(
        cfg: Config,
        source: Box<dyn SampleSource>,
        applier: Box<dyn EqApplier>,
        status: Arc<Mutex<Status>>,
        stop: Arc<AtomicBool>,
    ) -> Self {
        cfg.validate().expect("invalid config");
        let base_cfg = cfg.clone();
        let mut cfg = cfg;
        let tuning = cfg.tuning;
        if let Some(p) = tuning {
            p.apply(&mut cfg);
        }
        {
            let mut st = status.lock().unwrap();
            st.tuning = tuning.map_or("custom", |p| p.as_str()).into();
            st.signature = cfg.eq.signature.clamped();
            st.device_strength = cfg.eq.device_strength.clamp(0.0, 1.0);
            st.punch = cfg.eq.per_band_punch;
        }
        let fs = cfg.capture.rate as f64;
        let solver = EqSolver::new(
            cfg.eq.frequencies.clone(),
            cfg.eq.q,
            cfg.eq.clamp_db,
            cfg.eq.ridge,
            fs,
        );
        let mut analyzer = SpectrumAnalyzer::new(
            cfg.analysis.fft_size,
            cfg.analysis.hop,
            cfg.analysis.tau_s,
            fs,
        );
        analyzer.enable_band_tracker(
            &solver.points,
            analyzer.frames_for(cfg.analysis.shape_window_s.max(30.0)),
        );
        let detector = TrackDetector::new(DetectorConfig {
            gap_s: cfg.analysis.gap_s,
        });
        Self {
            source,
            applier,
            cfg,
            solver,
            analyzer,
            detector,
            status,
            stop,
            bypass_path: paths::bypass_flag_path(),
            tuning_path: None,
            base_cfg,
            tuning,
            tuning_checked_s: 0.0,
            signature_path: None,
            signature_checked_s: 0.0,
            state: State::Idle,
            bypassed: false,
            collected_s: 0.0,
            analyzing_silence_s: 0.0,
            begin_s: 0.0,
            now_s: 0.0,
            window_peak: 0.0,
            window_sq: 0.0,
            window_n: 0,
            frame_count: 0,
            spectrum_out: None,
            track_rx: None,
            last_track_id: None,
            analyzed_id: None,
            last_applied: None,
            crest_db: 0.0,
            track_class: None,
            reliability: 0.0,
            transient: 0.0,
            next_refine_s: 0.0,
            classifier: None,
            track_log: None,
            pending: None,
            rms_avg_db: None,
        }
    }

    /// Uses prototypes fitted to the listener's own tracks.
    pub fn set_classifier(&mut self, c: FittedClassifier) {
        self.classifier = Some(c);
    }

    /// Appends one JSON line per analyzed track to `path`.
    pub fn set_track_log(&mut self, path: PathBuf) {
        self.track_log = Some(path);
    }

    /// Overrides the bypass flag file location (tests).
    pub fn set_bypass_path(&mut self, p: PathBuf) {
        self.bypass_path = p;
    }

    /// Feeds track-change hints from a player metadata monitor (MPRIS).
    /// The first observed signature only initializes state; later changes
    /// count as an authoritative track start, faster and more reliable than
    /// silence or spectral detection.
    pub fn set_track_events(&mut self, rx: Receiver<String>) {
        self.track_rx = Some(rx);
    }

    /// Publishes a log-spaced spectrum snapshot for a UI, ~25 times a second
    /// of actual processing.
    pub fn set_spectrum_out(&mut self, spectrum: Arc<Mutex<Vec<(f64, f64)>>>) {
        self.spectrum_out = Some(spectrum);
    }

    pub fn status_handle(&self) -> Arc<Mutex<Status>> {
        self.status.clone()
    }

    pub fn run(&mut self) -> anyhow::Result<()> {
        loop {
            self.drain_track_hints();
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            match self.source.next()? {
                SourceEvent::Ended => break,
                SourceEvent::Idle => {}
                SourceEvent::Chunk(samples) => self.process(&samples)?,
            }
            self.poll_bypass()?;
        }
        self.log_track_end();
        Ok(())
    }

    fn drain_track_hints(&mut self) {
        loop {
            let Some(rx) = self.track_rx.as_ref() else {
                return;
            };
            let Ok(id) = rx.try_recv() else { return };
            if self.last_track_id.is_none() {
                self.last_track_id = Some(id);
            } else if self.last_track_id.as_ref() != Some(&id) {
                self.last_track_id = Some(id);
                tracing::info!("mpris hint: track changed");
                self.begin_analysis();
            }
        }
    }

    /// Enables live preset changes: the engine watches this file (written by
    /// the GUI or `leungo tuning`) and applies what it finds now.
    pub fn set_tuning_path(&mut self, p: PathBuf) {
        self.tuning_path = Some(p);
        self.tuning_checked_s = f64::NEG_INFINITY;
        self.poll_tuning();
    }

    /// Picks up a preset change made in the GUI or CLI. Reverting to
    /// `custom` restores the values from the config file.
    fn poll_tuning(&mut self) {
        if self.now_s - self.tuning_checked_s < 1.0 {
            return;
        }
        self.tuning_checked_s = self.now_s;
        let Some(path) = self.tuning_path.as_ref() else {
            return;
        };
        let Some(want) = read_tuning(path) else {
            return;
        };
        if want == self.tuning {
            return;
        }
        self.tuning = want;
        self.cfg = self.base_cfg.clone();
        if let Some(p) = want {
            p.apply(&mut self.cfg);
        }
        let name = want.map_or("custom", |p| p.as_str());
        tracing::info!("tuning: {name}");
        self.status.lock().unwrap().tuning = name.into();
        // Re-solve at once so the new setting is heard without waiting.
        self.next_refine_s = self.now_s;
    }

    /// Enables live sound-signature changes: the engine watches this JSON
    /// file (written by the GUI or `leungo sound`) and applies it now.
    pub fn set_signature_path(&mut self, p: PathBuf) {
        self.signature_path = Some(p);
        self.signature_checked_s = f64::NEG_INFINITY;
        self.poll_signature();
    }

    fn poll_signature(&mut self) {
        if self.now_s - self.signature_checked_s < 0.5 {
            return;
        }
        self.signature_checked_s = self.now_s;
        let Some(path) = self.signature_path.as_ref() else {
            return;
        };
        let Some(Sound { sig, device, punch }) = read_signature(path) else {
            return;
        };
        let punch = punch.unwrap_or(self.cfg.eq.per_band_punch);
        if sig == self.cfg.eq.signature
            && (device - self.cfg.eq.device_strength).abs() < 1e-9
            && punch == self.cfg.eq.per_band_punch
        {
            return;
        }
        // base_cfg too, so a later tuning change does not undo it
        for c in [&mut self.cfg, &mut self.base_cfg] {
            c.eq.signature = sig;
            c.eq.device_strength = device;
            c.eq.per_band_punch = punch;
        }
        {
            let mut st = self.status.lock().unwrap();
            st.signature = sig;
            st.device_strength = device;
            st.punch = punch;
        }
        tracing::info!("sound: {sig:?} device {device:.2} punch {punch}");
        // Heard at once, not after the next refinement pass.
        if self.state == State::Active && self.last_applied.is_some() {
            let (gains, archetype, confidence) =
                self.solve_current(self.crest_db, self.track_class);
            self.pending = None;
            self.commit(gains, archetype, confidence);
        }
    }

    fn poll_bypass(&mut self) -> anyhow::Result<()> {
        let want = self.bypass_path.exists();
        if want == self.bypassed {
            return Ok(());
        }
        if let Err(e) = self.applier.set_bypass(want) {
            tracing::error!("bypass apply failed: {e:#}");
            let mut s = self.status.lock().unwrap();
            s.message = Some(format!("bypass failed: {e}"));
        }
        self.bypassed = want;
        if !want
            && let Some(g) = self.last_applied.clone()
            && let Err(e) =
                self.applier
                    .apply_eq(&crate::offline::output(&self.cfg, &g, self.solver.fs))
        {
            tracing::error!("re-apply after bypass failed: {e:#}");
        }
        self.set_state_status();
        Ok(())
    }

    fn process(&mut self, chunk: &[f32]) -> anyhow::Result<()> {
        let frames = self.analyzer.push_interleaved(
            chunk,
            self.cfg.capture.channels,
            self.cfg.analysis.silence_threshold_db,
        );
        let dt = self.cfg.analysis.hop as f64 / self.cfg.capture.rate as f64;
        for fr in frames {
            self.now_s += dt;
            self.frame_count += 1;
            if self.frame_count.is_multiple_of(4)
                && let Some(out) = &self.spectrum_out
                && let Ok(mut snapshot) = out.lock()
            {
                *snapshot = self.analyzer.snapshot_log_spectrum(128, 40.0, 16_000.0);
            }
            self.poll_tuning();
            self.poll_signature();
            self.maybe_commit_pending(&fr)?;
            match self.state {
                State::Idle => {
                    if !fr.silent {
                        if self.last_track_id.is_some() && self.analyzed_id == self.last_track_id {
                            // Same track resumed after a stop: keep its curve.
                            self.state = State::Active;
                            self.set_state_status();
                        } else {
                            self.begin_analysis();
                        }
                    }
                }
                State::Analyzing => {
                    if fr.silent {
                        // Music stopped mid-analysis: partial data is a
                        // garbage preview of a track that is gone, so drop
                        // back to idle and wait for the next one.
                        self.analyzing_silence_s += dt;
                        if self.analyzing_silence_s >= self.cfg.analysis.gap_s * 4.0 {
                            self.state = State::Idle;
                            self.set_state_status();
                            continue;
                        }
                    } else {
                        self.analyzing_silence_s = 0.0;
                        if fr.peak > self.window_peak {
                            self.window_peak = fr.peak;
                        }
                        self.window_sq += fr.sq_sum;
                        self.window_n += fr.samples as u64;
                        self.collected_s += dt;
                        if self.collected_s >= self.cfg.analysis.window_s {
                            self.finish_analysis()?;
                            continue;
                        }
                    }
                    if self.window_n > 0
                        && self.now_s - self.begin_s >= self.cfg.analysis.max_wall_s
                    {
                        self.finish_analysis()?;
                        continue;
                    }
                }
                State::Active => self.refine(),
            }
            // With a player reporting track ids, those are authoritative:
            // a pause and resume of the same track is not a new track.
            if self.detector.feed(&fr, dt, self.now_s) == Detection::TrackStart
                && self.last_track_id.is_none()
            {
                self.begin_analysis();
            }
        }
        Ok(())
    }

    fn begin_analysis(&mut self) {
        self.log_track_end();
        self.pending = None;
        self.analyzer.reset_spectrum();
        self.detector.mark_now(self.now_s);
        self.collected_s = 0.0;
        self.analyzing_silence_s = 0.0;
        self.begin_s = self.now_s;
        self.window_peak = 0.0;
        self.window_sq = 0.0;
        self.window_n = 0;
        self.state = State::Analyzing;
        self.set_state_status();
    }

    /// Solves the EQ for the spectrum currently in the analyzer.
    ///
    /// `keep` carries the class decided from the track's first analysis
    /// window. Refinement passes use it instead of re-classifying: a 2 s
    /// running spectrum is far noisier than the 6 s window, and a label that
    /// flips between sections would move the target tilt with it.
    fn solve_current(
        &mut self,
        crest_db: f64,
        keep: Option<(leungo_eq::Archetype, f64)>,
    ) -> (Vec<f64>, leungo_eq::Archetype, f64) {
        let (archetype, confidence) = match keep {
            Some(k) => k,
            None => {
                let features = extract(&self.analyzer, crest_db);
                match &self.classifier {
                    Some(c) => c.classify(&descriptors(&features)),
                    None => classify(&features),
                }
            }
        };

        let meas = crate::offline::measure(&self.cfg, &self.analyzer, &self.solver.points);
        let gains = crate::offline::solve_gains(&self.cfg, &self.solver, &meas, archetype);
        self.reliability = meas.reliability;
        self.transient = meas.transient;
        (gains, archetype, confidence)
    }

    /// A new track always gets its own analysis and its own curve, applied
    /// at once. Nothing from the previous track leaks into it.
    fn finish_analysis(&mut self) -> anyhow::Result<()> {
        self.analyzed_id = self.last_track_id.clone();
        let samples = self.window_n.max(1) as f64;
        let rms = (self.window_sq / samples).sqrt();
        self.crest_db = if rms > 1e-10 {
            20.0 * (self.window_peak / rms).log10()
        } else {
            0.0
        };
        let (gains, archetype, confidence) = self.solve_current(self.crest_db, None);
        self.track_class = Some((archetype, confidence));
        self.next_refine_s = self.now_s + self.cfg.analysis.refine_interval_s;
        self.log_track(archetype, confidence);
        self.status.lock().unwrap().tracks_analyzed += 1;
        self.commit(gains, archetype, confidence);
        Ok(())
    }

    /// The track's band shape over what was actually heard (up to the last
    /// 30 s), written when the next one starts. Plan 06 learns from these.
    fn log_track_end(&self) {
        let Some(t) = self.analyzer.tracker() else {
            return;
        };
        let Some(shape) = t.shape(t.len()) else {
            return;
        };
        let round =
            |v: &[f64]| -> Vec<f64> { v.iter().map(|x| (x * 10.0).round() / 10.0).collect() };
        self.append_log(serde_json::json!({
            "v": 3,
            "kind": "end",
            "track": self.analyzed_id,
            "heard_s": (self.now_s - self.begin_s).round(),
            "points": self.solver.points,
            "shape": round(&shape),
            "reliability": t.reliability(t.len()),
            "transient": t.transient_fraction(t.len()),
            "percussive": self.analyzer.percussive_share().map(|v| round(&v)),
            "edge_hz": self.analyzer.bandwidth_edge(),
        }));
    }

    fn log_track(&self, archetype: leungo_eq::Archetype, confidence: f64) {
        let d = descriptors(&extract(&self.analyzer, self.crest_db));
        let line = serde_json::json!({
            "d": d,
            "archetype": archetype.as_str(),
            "confidence": confidence,
            "track": self.last_track_id,
            "v": 3,
            "reliability": self.reliability,
            "transient": self.transient,
            "edge_hz": self.analyzer.bandwidth_edge(),
        });
        self.append_log(line);
    }

    fn append_log(&self, line: serde_json::Value) {
        let Some(path) = &self.track_log else { return };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let write = || -> std::io::Result<()> {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)?;
            writeln!(f, "{line}")
        };
        if let Err(e) = write() {
            tracing::warn!("track log write failed: {e}");
        }
    }

    /// While a track plays, nudges the loaded curve toward what the current
    /// spectrum asks for: small bounded steps, only when audible, switched in
    /// at a quiet moment. Replaces event triggers with slow adaptation.
    fn refine(&mut self) {
        let interval = self.cfg.analysis.refine_interval_s;
        if interval <= 0.0 || self.now_s < self.next_refine_s || self.pending.is_some() {
            return;
        }
        self.next_refine_s = self.now_s + interval;
        let Some(applied) = self.last_applied.clone() else {
            return;
        };
        let (solved, archetype, confidence) = self.solve_current(self.crest_db, self.track_class);
        let alpha = self.cfg.analysis.refine_alpha.clamp(0.0, 1.0);
        let max_step = self.cfg.analysis.refine_max_step_db.max(0.0);
        let gains: Vec<f64> = applied
            .iter()
            .zip(solved.iter())
            .map(|(a, s)| a + (alpha * (s - a)).clamp(-max_step, max_step))
            .collect();
        // Judge the remaining gap to the solved curve, not the size of one
        // step: a step is a fraction of the gap, so thresholding it would
        // leave any gap under threshold/alpha permanently uncorrected.
        let threshold = self.cfg.analysis.minimum_change_db;
        if solved
            .iter()
            .zip(applied.iter())
            .all(|(s, a)| (s - a).abs() < threshold)
        {
            return;
        }
        if self.cfg.analysis.defer_max_s > 0.0 {
            self.pending = Some(Pending {
                gains,
                archetype,
                confidence,
                deadline_s: self.now_s + self.cfg.analysis.defer_max_s,
            });
        } else {
            self.commit(gains, archetype, confidence);
        }
    }

    /// Switches in a deferred curve at a dip in level, or at the deadline.
    fn maybe_commit_pending(&mut self, fr: &leungo_analysis::FrameResult) -> anyhow::Result<()> {
        if fr.silent {
            return Ok(());
        }
        let avg = *self.rms_avg_db.get_or_insert(fr.rms_db);
        self.rms_avg_db = Some(avg + 0.05 * (fr.rms_db - avg));
        if self.state != State::Active {
            return Ok(());
        }
        let due = self.pending.as_ref().is_some_and(|p| {
            fr.rms_db < avg - self.cfg.analysis.defer_dip_db || self.now_s >= p.deadline_s
        });
        if due && let Some(p) = self.pending.take() {
            self.commit(p.gains, p.archetype, p.confidence);
        }
        Ok(())
    }

    fn commit(&mut self, gains: Vec<f64>, archetype: leungo_eq::Archetype, confidence: f64) {
        self.last_applied = Some(gains.clone());

        if !self.bypassed
            && let Err(e) =
                self.applier
                    .apply_eq(&crate::offline::output(&self.cfg, &gains, self.solver.fs))
        {
            tracing::error!("apply failed: {e:#}");
            let mut s = self.status.lock().unwrap();
            s.message = Some(format!("apply failed: {e}"));
        }

        self.state = State::Active;
        {
            let mut s = self.status.lock().unwrap();
            s.state = "active".into();
            s.bypassed = self.bypassed;
            s.archetype = Some(archetype.as_str().into());
            s.confidence = confidence;
            s.reliability = self.reliability;
            s.gains = gains;
        }
        tracing::info!(
            "track #{}: {} (confidence {:.2})",
            self.tracks_count(),
            archetype.as_str(),
            confidence
        );
    }

    fn tracks_count(&self) -> u64 {
        self.status.lock().unwrap().tracks_analyzed
    }

    fn set_state_status(&self) {
        let mut s = self.status.lock().unwrap();
        s.state = match self.state {
            State::Idle => "idle".into(),
            State::Analyzing => "analyzing".into(),
            State::Active => "active".into(),
        };
        s.bypassed = self.bypassed;
    }
}
