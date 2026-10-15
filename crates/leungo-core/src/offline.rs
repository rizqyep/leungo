//! Offline analysis of whole buffers (WAV files) with no capture or applier.

use crate::config::{Config, target_delta};

use anyhow::Result;
use leungo_analysis::{SpectrumAnalyzer, classify, extract, third_octave_levels};
use leungo_eq::{BandGain, EqSolver, interpolate_curve};

/// What the solver needs to know about the audio heard so far.
#[derive(Clone, Debug)]
pub struct Measurement {
    /// Level-independent spectral shape at the solver's points, dB.
    pub shape: Vec<f64>,
    /// Split-half repeatability of that shape, 0..=1.
    pub reliability: f64,
    /// Transient energy fraction (see `BandTracker::transient_fraction`).
    pub transient: f64,
    /// Codec lowpass of the stream, when there is one.
    pub edge_hz: Option<f64>,
    /// Per point, share of the power that is drum hits (see `hpss`).
    pub percussive: Option<Vec<f64>>,
}

/// Lowest band the song layer corrects. The fine bass path measures to
/// 25 Hz, but most masters roll off below 40 Hz on purpose.
const LO_HZ: f64 = 40.0;
/// Highest band corrected; lower when the stream's own lowpass is below it.
const HI_HZ: f64 = 10_000.0;

/// Treble limit for this track: a third of an octave under the codec cut.
fn hi_hz(edge: Option<f64>) -> f64 {
    edge.map_or(HI_HZ, |e| (e / 2f64.powf(1.0 / 3.0)).min(HI_HZ))
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
            edge_hz: analyzer.bandwidth_edge(),
            percussive: analyzer.percussive_share(),
        };
    }
    Measurement {
        shape: third_octave_levels(analyzer, points),
        reliability: 0.0,
        transient: 0.0,
        edge_hz: analyzer.bandwidth_edge(),
        percussive: analyzer.percussive_share(),
    }
}

/// Maps the transient fraction to 0..=1. The two ends are not calibrated
/// against real music yet; the value is logged per track so they can be.
fn percussiveness(transient: f64) -> f64 {
    ((transient - 0.15) / (0.45 - 0.15)).clamp(0.0, 1.0)
}

/// Maps a band's drum share to 0..=1: mostly tones gives 0, mostly hits 1.
/// Ends *unverified* on real music; the shares are logged per track.
fn punch(share: f64) -> f64 {
    ((share - 0.3) / (0.7 - 0.3)).clamp(0.0, 1.0)
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
    archetype: leungo_eq::Archetype,
) -> Vec<f64> {
    let points = &solver.points;
    let target = interpolate_curve(&cfg.eq.frequencies, &target_delta(cfg, archetype), points);
    let mut shaped = leungo_eq::shape_residual(
        points,
        &meas.shape,
        cfg.eq.deadzone_db,
        cfg.eq.residual_cap_db,
        LO_HZ,
        hi_hz(meas.edge_hz),
    );
    // Percussive material is expected to sit higher at both ends, so its
    // bass and treble deviations are corrected less. This scales the
    // correction rather than widening the dead zone: a large bass bump is
    // above the cap either way and a wider dead zone would change nothing.
    let relief = cfg.eq.percussive_relief.clamp(0.0, 1.0);
    match meas.percussive.as_deref() {
        Some(share) if cfg.eq.per_band_punch && share.len() == shaped.len() => {
            // bumps only (they get cut); a dip can still be lifted
            for (v, &p) in shaped.iter_mut().zip(share) {
                if *v > 0.0 {
                    *v *= 1.0 - relief * punch(p);
                }
            }
        }
        _ => {
            let relief = relief * percussiveness(meas.transient);
            for (v, &f) in shaped.iter_mut().zip(points.iter()) {
                *v *= 1.0 - relief * edge_weight(f);
            }
        }
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

/// Song gains plus the device stage, with a preamp so the peak never
/// goes above 0 dB.
pub fn output(cfg: &Config, gains: &[f64], fs: f64) -> crate::EqOutput {
    use leungo_eq::device::{Filter, peak_gain_db, peaks};
    let strength = cfg.eq.device_strength.clamp(0.0, 1.0);
    let device: Vec<Filter> = cfg
        .eq
        .device_filters
        .iter()
        .map(|f| Filter {
            gain_db: f.gain_db * strength,
            ..*f
        })
        .collect();
    let mut all = peaks(&cfg.eq.frequencies, gains, cfg.eq.q);
    all.extend_from_slice(&device);
    crate::EqOutput {
        gains: gains.to_vec(),
        device,
        preamp_db: -peak_gain_db(&all, fs).max(0.0),
    }
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
    pub archetype: leungo_eq::Archetype,
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
    // Fed in capture-sized chunks: one huge push makes every hop move the
    // whole remaining buffer.
    let frames: Vec<_> = interleaved
        .chunks(2 * 1024)
        .flat_map(|c| analyzer.push_interleaved(c, 2, cfg.analysis.silence_threshold_db))
        .collect();

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
        .map(|(&frequency, gain_db)| leungo_eq::BandGain {
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
                edge_hz: None,
                percussive: None,
            };
            solve_gains(&cfg, &s, &m, leungo_eq::Archetype::Neutral)
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
        cfg.eq.signature = leungo_eq::Signature {
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
    fn output_scales_device_filters_and_keeps_the_peak_at_zero() {
        use leungo_eq::device::{Filter, Kind, peak_gain_db, peaks};
        let mut cfg = Config::default();
        cfg.eq.device_strength = 0.5;
        cfg.eq.device_filters = vec![Filter {
            kind: Kind::Peak,
            fc: 3000.0,
            gain_db: 8.0,
            q: 1.0,
        }];
        let gains = vec![0.0, 0.0, 0.0, 0.0, 0.0, 2.0, 2.0, 0.0];
        let out = output(&cfg, &gains, 48_000.0);
        assert_eq!(out.device[0].gain_db, 4.0);
        let mut all = peaks(&cfg.eq.frequencies, &gains, cfg.eq.q);
        all.extend_from_slice(&out.device);
        let peak = peak_gain_db(&all, 48_000.0) + out.preamp_db;
        assert!(peak.abs() < 1e-9, "{peak}");
        let cut = output(&cfg, &[-3.0; 8], 48_000.0);
        assert!(cut.preamp_db <= 0.0);
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
            edge_hz: None,
            percussive: None,
        };
        cfg.eq.device_strength = 1.0;
        let full = solve_gains(&cfg, &s, &m, leungo_eq::Archetype::Neutral);
        cfg.eq.device_strength = 0.5;
        let half = solve_gains(&cfg, &s, &m, leungo_eq::Archetype::Neutral);
        assert!((full[4] - 2.0 * half[4]).abs() < 0.05, "{full:?} {half:?}");
    }

    #[test]
    fn per_band_punch_eases_drum_bumps_but_not_tonal_ones() {
        let mut cfg = Config::default();
        cfg.eq.per_band_punch = true;
        let s = solver(&cfg);
        let shape = bass_bump(&s.points);
        let run = |share: f64| {
            let m = Measurement {
                shape: shape.clone(),
                reliability: 1.0,
                transient: 0.0,
                edge_hz: None,
                percussive: Some(vec![share; s.points.len()]),
            };
            solve_gains(&cfg, &s, &m, leungo_eq::Archetype::Neutral)
        };
        let (tonal, drums) = (run(0.1), run(0.9));
        assert!(tonal[0] < -0.5, "a tonal bass bump is cut: {tonal:?}");
        assert!(
            drums[0] > tonal[0] + 0.3,
            "{:.2} vs {:.2}",
            drums[0],
            tonal[0]
        );
    }

    #[test]
    fn treble_limit_follows_the_codec_cut() {
        assert_eq!(hi_hz(None), HI_HZ);
        assert_eq!(hi_hz(Some(19_500.0)), HI_HZ);
        let low = hi_hz(Some(11_000.0));
        assert!((8_500.0..9_000.0).contains(&low), "{low}");
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
                edge_hz: None,
                percussive: None,
            };
            solve_gains(&cfg, &s, &m, leungo_eq::Archetype::Neutral)
        };
        assert!(run(0.0)[0].abs() < run(1.0)[0].abs());
    }
}
