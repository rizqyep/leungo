//! Offline analysis of whole buffers (WAV files) with no capture or applier.

use crate::config::{Config, target_delta};

use ae_analysis::{SpectrumAnalyzer, classify, extract, third_octave_levels};
use ae_eq::{BandGain, EqSolver, interpolate_curve};
use anyhow::Result;

/// What the solver needs to know about the audio heard so far.
#[derive(Clone, Debug)]
pub struct Measurement {
    /// Level-independent spectral shape at the solver's points, dB.
    pub shape: Vec<f64>,
    /// Split-half repeatability of that shape, 0..=1.
    pub reliability: f64,
    /// Transient energy fraction (see `BandTracker::transient_fraction`).
    pub transient: f64,
}

/// Measures the analyzer's current content. Falls back to the plain
/// averaged spectrum, with zero reliability, until enough frames exist.
pub fn measure(cfg: &Config, analyzer: &SpectrumAnalyzer, points: &[f64]) -> Measurement {
    let window = analyzer.frames_for(cfg.analysis.shape_window_s);
    if let Some(t) = analyzer.tracker()
        && let Some(shape) = t.shape(window)
        && shape.len() == points.len()
    {
        return Measurement {
            shape,
            reliability: t.reliability(window).unwrap_or(0.0),
            transient: t.transient_fraction(window).unwrap_or(0.0),
        };
    }
    Measurement {
        shape: third_octave_levels(analyzer, points),
        reliability: 0.0,
        transient: 0.0,
    }
}

/// Maps the transient fraction to 0..=1. The two ends are not calibrated
/// against real music yet; the value is logged per track so they can be.
fn percussiveness(transient: f64) -> f64 {
    ((transient - 0.15) / (0.45 - 0.15)).clamp(0.0, 1.0)
}

/// 1 at the bass (<=125 Hz) and treble (>=6 kHz) ends, 0 through the mids,
/// ramping over an octave in between.
fn edge_weight(f: f64) -> f64 {
    let low = (250.0f64 / f).log2().clamp(0.0, 1.0);
    let high = (f / 3000.0).log2().clamp(0.0, 1.0);
    low.max(high)
}

/// The one solve path shared by the daemon and `analyze`: take the track's
/// own spectral tilt as given, correct only the bumps and dips beyond the
/// dead zone toward the archetype target, scale that by strength and the
/// measurement's reliability, then add the fixed device correction in full.
pub fn solve_gains(
    cfg: &Config,
    solver: &EqSolver,
    meas: &Measurement,
    archetype: ae_eq::Archetype,
) -> Vec<f64> {
    let points = &solver.points;
    let target = interpolate_curve(&cfg.eq.frequencies, &target_delta(cfg, archetype), points);
    let mut shaped = ae_eq::shape_residual(
        points,
        &meas.shape,
        cfg.eq.deadzone_db,
        cfg.eq.residual_cap_db,
        50.0,
        10_000.0,
    );
    // Percussive material is expected to sit higher at both ends, so its
    // bass and treble deviations are corrected less. This scales the
    // correction rather than widening the dead zone: a large bass bump is
    // above the cap either way and a wider dead zone would change nothing.
    let relief = cfg.eq.percussive_relief.clamp(0.0, 1.0) * percussiveness(meas.transient);
    for (v, &f) in shaped.iter_mut().zip(points.iter()) {
        *v *= 1.0 - relief * edge_weight(f);
    }
    let strength = cfg.eq.strength.clamp(0.0, 1.0) * (0.5 + 0.5 * meas.reliability);
    let song: Vec<f64> = solver
        .solve(&target, &shaped)
        .iter()
        .map(|b| b.gain_db * strength)
        .collect();
    let device = cfg.eq.device_strength.clamp(0.0, 1.0);
    let sig = signature_gains(cfg, solver);
    song.iter()
        .zip(&cfg.eq.device_curve)
        .zip(&sig)
        .map(|((s, d), g)| (s + device * d + g).clamp(-cfg.eq.clamp_db, cfg.eq.clamp_db))
        .collect()
}

/// The signature fitted onto the eight bands, with its largest gain shifted
/// to 0 dB so that adding character can never push the signal toward
/// clipping. Zero when the signature is flat.
pub fn signature_gains(cfg: &Config, solver: &EqSolver) -> Vec<f64> {
    let sig = cfg.eq.signature.clamped();
    let n = cfg.eq.frequencies.len();
    if sig.is_flat() {
        return vec![0.0; n];
    }
    let target: Vec<f64> = solver
        .points
        .iter()
        .map(|&f| sig.response_db(f, solver.fs))
        .collect();
    let zeros = vec![0.0; target.len()];
    let gains: Vec<f64> = solver
        .solve(&target, &zeros)
        .iter()
        .map(|b| b.gain_db)
        .collect();
    let headroom = gains.iter().cloned().fold(0.0f64, f64::max);
    gains.iter().map(|g| g - headroom).collect()
}

pub struct OfflineResult {
    pub archetype: ae_eq::Archetype,
    pub confidence: f64,
    pub reliability: f64,
    pub transient: f64,
    pub bands: Vec<BandGain>,
}

pub fn analyze_samples(samples: &[f64], fs: f64, cfg: &Config) -> Result<OfflineResult> {
    cfg.validate()?;
    let mut analyzer = SpectrumAnalyzer::new(
        cfg.analysis.fft_size,
        cfg.analysis.hop,
        cfg.analysis.tau_s,
        fs,
    );

    let solver = EqSolver::new(
        cfg.eq.frequencies.clone(),
        cfg.eq.q,
        cfg.eq.clamp_db,
        cfg.eq.ridge,
        fs,
        cfg.analysis.fft_size,
    );
    let cap = analyzer.frames_for(cfg.analysis.shape_window_s.max(30.0));
    analyzer.enable_band_tracker(&solver.points, cap);

    let mut interleaved: Vec<f32> = Vec::with_capacity(samples.len() * 2);
    for &s in samples {
        interleaved.push(s as f32);
        interleaved.push(s as f32);
    }
    let frames = analyzer.push_interleaved(&interleaved, 2, cfg.analysis.silence_threshold_db);

    let mut sq = 0.0;
    let mut peak = 0.0f64;
    let mut n = 0.0;
    for fr in &frames {
        if !fr.silent {
            sq += fr.sq_sum;
            n += fr.samples as f64;
            if fr.peak > peak {
                peak = fr.peak;
            }
        }
    }
    let rms = if n > 0.0 { (sq / n).sqrt() } else { 0.0 };
    let crest = if rms > 1e-10 {
        20.0 * (peak / rms).log10()
    } else {
        0.0
    };

    let features = extract(&analyzer, crest);
    let (archetype, confidence) = classify(&features);

    let meas = measure(cfg, &analyzer, &solver.points);
    let gains = solve_gains(cfg, &solver, &meas, archetype);
    let bands = cfg
        .eq
        .frequencies
        .iter()
        .zip(gains)
        .map(|(&frequency, gain_db)| ae_eq::BandGain {
            frequency,
            q: cfg.eq.q,
            gain_db,
        })
        .collect();

    Ok(OfflineResult {
        archetype,
        confidence,
        reliability: meas.reliability,
        transient: meas.transient,
        bands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solver(cfg: &Config) -> EqSolver {
        EqSolver::new(
            cfg.eq.frequencies.clone(),
            cfg.eq.q,
            cfg.eq.clamp_db,
            cfg.eq.ridge,
            48_000.0,
            cfg.analysis.fft_size,
        )
    }

    /// Flat shape with a wide +10 dB bump below 200 Hz.
    fn bass_bump(points: &[f64]) -> Vec<f64> {
        points
            .iter()
            .map(|&f| if f < 200.0 { 10.0 } else { 0.0 })
            .collect()
    }

    #[test]
    fn percussive_material_gets_more_bass_before_it_is_cut() {
        let cfg = Config::default();
        let s = solver(&cfg);
        let shape = bass_bump(&s.points);
        let run = |transient: f64| {
            let m = Measurement {
                shape: shape.clone(),
                reliability: 1.0,
                transient,
            };
            solve_gains(&cfg, &s, &m, ae_eq::Archetype::Neutral)
        };
        let steady = run(0.0);
        let percussive = run(0.6);
        assert!(steady[0] < -0.5, "steady bass bump must be cut: {steady:?}");
        assert!(
            percussive[0] > steady[0] + 0.3,
            "percussive should be cut less: {:.2} vs {:.2}",
            percussive[0],
            steady[0]
        );
    }

    #[test]
    fn signature_adds_bass_and_air_but_never_net_boost() {
        let mut cfg = Config::default();
        cfg.eq.signature = ae_eq::Signature {
            bass_db: 5.0,
            air_db: 3.0,
            ..Default::default()
        };
        let s = solver(&cfg);
        let g = signature_gains(&cfg, &s);
        assert!(g.iter().all(|&v| v <= 1e-9), "net boost: {g:?}");
        // bass and air bands sit above the middle ones
        assert!(g[0] > g[4] + 2.0, "bass vs mid: {g:?}");
        assert!(g[7] > g[4] + 1.0, "air vs mid: {g:?}");
        let flat = signature_gains(&Config::default(), &s);
        assert!(flat.iter().all(|&v| v == 0.0));
    }

    #[test]
    fn device_strength_scales_the_device_curve() {
        let mut cfg = Config::default();
        cfg.eq.device_curve = vec![-6.0; 8];
        let s = solver(&cfg);
        let m = Measurement {
            shape: vec![0.0; s.points.len()],
            reliability: 1.0,
            transient: 0.0,
        };
        cfg.eq.device_strength = 1.0;
        let full = solve_gains(&cfg, &s, &m, ae_eq::Archetype::Neutral);
        cfg.eq.device_strength = 0.5;
        let half = solve_gains(&cfg, &s, &m, ae_eq::Archetype::Neutral);
        assert!((full[4] - 2.0 * half[4]).abs() < 0.05, "{full:?} {half:?}");
    }

    #[test]
    fn unreliable_measurements_are_corrected_less() {
        let cfg = Config::default();
        let s = solver(&cfg);
        let shape = bass_bump(&s.points);
        let run = |reliability: f64| {
            let m = Measurement {
                shape: shape.clone(),
                reliability,
                transient: 0.0,
            };
            solve_gains(&cfg, &s, &m, ae_eq::Archetype::Neutral)
        };
        assert!(run(0.0)[0].abs() < run(1.0)[0].abs());
    }
}
