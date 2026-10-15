//! Target curves, musical archetypes, and the least-squares solver that turns
//! a spectral deviation into parametric band gains.

use ae_dsp::peaking_magnitude_db;
use serde::{Deserialize, Serialize};

pub mod device;
pub mod signature;
pub mod targets;

pub use signature::Signature;

pub const BAND_FREQS: [f64; 8] = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];

/// Coarse musical character of the current track, derived from spectral
/// features. Selects a target-curve delta in `targets`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Archetype {
    BassHeavy,
    VocalCentric,
    GuitarBand,
    BrightAcoustic,
    Neutral,
}

impl Archetype {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BassHeavy => "bass-heavy",
            Self::VocalCentric => "vocal-centric",
            Self::GuitarBand => "guitar-band",
            Self::BrightAcoustic => "bright-acoustic",
            Self::Neutral => "neutral",
        }
    }

    pub fn all() -> [Archetype; 5] {
        [
            Self::BassHeavy,
            Self::VocalCentric,
            Self::GuitarBand,
            Self::BrightAcoustic,
            Self::Neutral,
        ]
    }
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct BandGain {
    pub frequency: f64,
    pub q: f64,
    pub gain_db: f64,
}

/// ISO 1/3-octave center frequencies between `lo` and `hi` Hz.
pub fn third_octave_centers(lo: f64, hi: f64) -> Vec<f64> {
    [
        20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0,
        500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0,
        6300.0, 8000.0, 10000.0, 12500.0, 16000.0,
    ]
    .iter()
    .copied()
    .filter(|&f| f >= lo && f <= hi)
    .collect()
}

/// Piecewise-linear interpolation over log-frequency, clamped at the edges.
pub fn interpolate_curve(src_f: &[f64], src_v: &[f64], dst_f: &[f64]) -> Vec<f64> {
    assert_eq!(src_f.len(), src_v.len(), "curve frequency/value mismatch");
    dst_f.iter().map(|&f| interp_at(f, src_f, src_v)).collect()
}

fn interp_at(f: f64, src_f: &[f64], src_v: &[f64]) -> f64 {
    let last = src_f.len() - 1;
    if f <= src_f[0] {
        return src_v[0];
    }
    if f >= src_f[last] {
        return src_v[last];
    }
    let i = src_f.partition_point(|&x| x < f).max(1).min(last);
    let (f0, f1) = (src_f[i - 1], src_f[i]);
    let t = (f.ln() - f0.ln()) / (f1.ln() - f0.ln());
    src_v[i - 1] + t * (src_v[i] - src_v[i - 1])
}

fn median(v: &mut [f64]) -> f64 {
    if v.is_empty() {
        return 0.0;
    }
    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Separates what an EQ should correct from the natural character of a
/// track. A straight line over log-frequency (spectral tilt) is fitted to the
/// measured 1/3-octave levels and left alone; the residual bumps and dips are
/// what remains, with a dead zone (ignore small deviations) and a cap.
/// Frequencies outside `lo..=hi` get no correction (sub-bass, codec cutoffs).
pub fn shape_residual(
    points: &[f64],
    measured: &[f64],
    deadzone_db: f64,
    cap_db: f64,
    lo: f64,
    hi: f64,
) -> Vec<f64> {
    let dz = vec![deadzone_db; points.len()];
    shape_residual_weighted(points, measured, &dz, cap_db, lo, hi)
}

/// Like `shape_residual` with a separate dead zone per point, so some parts
/// of the spectrum can be allowed more natural variation than others.
pub fn shape_residual_weighted(
    points: &[f64],
    measured: &[f64],
    deadzone_db: &[f64],
    cap_db: f64,
    lo: f64,
    hi: f64,
) -> Vec<f64> {
    assert_eq!(deadzone_db.len(), points.len(), "deadzone length mismatch");
    assert_eq!(points.len(), measured.len(), "points/measured mismatch");
    let inband: Vec<usize> = (0..points.len())
        .filter(|&k| points[k] >= lo && points[k] <= hi)
        .collect();
    if inband.len() < 3 {
        return vec![0.0; points.len()];
    }
    let xs: Vec<f64> = inband.iter().map(|&k| points[k].log2()).collect();
    let ys: Vec<f64> = inband.iter().map(|&k| measured[k]).collect();
    // Theil-Sen: the median of all pairwise slopes. A least-squares line is
    // dragged toward a large bump at one end, which would hide the very
    // deviation we want to correct; the median is not.
    let mut slopes = Vec::new();
    for i in 0..xs.len() {
        for j in (i + 1)..xs.len() {
            let dx = xs[j] - xs[i];
            if dx.abs() > 1e-9 {
                slopes.push((ys[j] - ys[i]) / dx);
            }
        }
    }
    let slope = median(&mut slopes);
    let mut offsets: Vec<f64> = xs.iter().zip(&ys).map(|(x, y)| y - slope * x).collect();
    let intercept = median(&mut offsets);
    points
        .iter()
        .zip(measured)
        .zip(deadzone_db)
        .map(|((&f, &m), &dz)| {
            if f < lo || f > hi {
                return 0.0;
            }
            let r = m - (intercept + slope * f.log2());
            let shrunk = r.signum() * (r.abs() - dz).max(0.0);
            shrunk.clamp(-cap_db, cap_db)
        })
        .collect()
}

/// Fits a fixed set of peaking bands to `target - measured` (dB, indexed like
/// `points`) using ridge-regularized least squares on the log-frequency axis.
pub struct EqSolver {
    pub freqs: Vec<f64>,
    pub q: f64,
    pub clamp_db: f64,
    pub ridge: f64,
    pub fs: f64,
    pub points: Vec<f64>,
}

impl EqSolver {
    /// `fft_size` matters: 1/3-octave bands narrower than the FFT bin width
    /// can end up with zero bins and would report garbage, so only points
    /// whose band actually contains a bin are used.
    pub fn new(
        freqs: Vec<f64>,
        q: f64,
        clamp_db: f64,
        ridge: f64,
        fs: f64,
        fft_size: usize,
    ) -> Self {
        let bin = fs / fft_size as f64;
        let hi = 0.95 * fs / 2.0;
        let ratio = 2.0f64.powf(1.0 / 6.0);
        let points: Vec<f64> = third_octave_centers(20.0, hi)
            .into_iter()
            .filter(|&c| {
                let (lo, up) = (c / ratio, c * ratio);
                let first = (lo / bin).ceil() * bin;
                first >= lo && first < up
            })
            .collect();
        assert!(
            !points.is_empty(),
            "no 1/3-octave points fit in the FFT range"
        );
        Self {
            points,
            freqs,
            q,
            clamp_db,
            ridge,
            fs,
        }
    }

    pub fn solve(&self, target: &[f64], measured: &[f64]) -> Vec<BandGain> {
        let n = self.freqs.len();
        let m = self.points.len();
        assert_eq!(target.len(), m, "target length mismatch");
        assert_eq!(measured.len(), m, "measured length mismatch");
        let mut d: Vec<f64> = (0..m).map(|k| target[k] - measured[k]).collect();
        // Overall level is loudness, not timbre, and a peaking EQ cannot
        // apply a constant shift: remove it, then bound the correction to
        // what the EQ can actually do.
        let mean = d.iter().sum::<f64>() / m as f64;
        for v in d.iter_mut() {
            *v = (*v - mean).clamp(-self.clamp_db, self.clamp_db);
        }
        let mut gains = vec![0.0f64; n];
        for _ in 0..2 {
            gains = self.least_squares(&d, &gains);
            for g in gains.iter_mut() {
                *g = g.clamp(-self.clamp_db, self.clamp_db);
            }
        }
        (0..n)
            .map(|j| BandGain {
                frequency: self.freqs[j],
                q: self.q,
                gain_db: gains[j],
            })
            .collect()
    }

    fn design_row(&self, k: usize, gains: &[f64]) -> Vec<f64> {
        let f = self.points[k];
        let mut row: Vec<f64> = self
            .freqs
            .iter()
            .enumerate()
            .map(|(j, &f0)| {
                let g = gains[j];
                if g.abs() > 1e-6 {
                    peaking_magnitude_db(f, f0, g, self.q, self.fs) / g
                } else {
                    peaking_magnitude_db(f, f0, 1.0, self.q, self.fs)
                }
            })
            .collect();
        row.push(1.0);
        row
    }

    fn least_squares(&self, d: &[f64], gains: &[f64]) -> Vec<f64> {
        let n = self.freqs.len();
        let size = n + 1;
        let mut m = vec![0.0f64; size * size];
        let mut rhs = vec![0.0f64; size];
        for (k, &dk) in d.iter().enumerate() {
            let row = self.design_row(k, gains);
            for i in 0..size {
                for j in 0..size {
                    m[i * size + j] += row[i] * row[j];
                }
                rhs[i] += row[i] * dk;
            }
        }
        for i in 0..n {
            m[i * size + i] += self.ridge;
        }
        let beta = solve_linear(&mut m, &mut rhs, size);
        beta[..n].to_vec()
    }
}

/// Gaussian elimination with partial pivoting, in place.
fn solve_linear(a: &mut [f64], b: &mut [f64], n: usize) -> Vec<f64> {
    for col in 0..n {
        let pivot = (col..n).fold(col, |best, r| {
            if a[r * n + col].abs() > a[best * n + col].abs() {
                r
            } else {
                best
            }
        });
        if a[pivot * n + col].abs() < 1e-12 {
            continue;
        }
        if pivot != col {
            for j in 0..n {
                a.swap(col * n + j, pivot * n + j);
            }
            b.swap(col, pivot);
        }
        let inv = 1.0 / a[col * n + col];
        for r in (col + 1)..n {
            let f = a[r * n + col] * inv;
            if f == 0.0 {
                continue;
            }
            for j in col..n {
                a[r * n + j] -= f * a[col * n + j];
            }
            b[r] -= f * b[col];
        }
    }
    let mut x = vec![0.0f64; n];
    for r in (0..n).rev() {
        let mut s = b[r];
        for j in (r + 1)..n {
            s -= a[r * n + j] * x[j];
        }
        let piv = a[r * n + r];
        x[r] = if piv.abs() > 1e-12 { s / piv } else { 0.0 };
    }
    x
}

#[cfg(test)]
mod tests {
    #[test]
    fn natural_tilt_is_not_corrected() {
        let pts: Vec<f64> = third_octave_centers(63.0, 8000.0);
        // -4.5 dB per octave with no bumps.
        let m: Vec<f64> = pts.iter().map(|f| -4.5 * (f / 1000.0).log2()).collect();
        let r = shape_residual(&pts, &m, 2.0, 6.0, 50.0, 10_000.0);
        assert!(r.iter().all(|v| v.abs() < 1e-9), "{r:?}");
    }

    #[test]
    fn a_bump_beyond_the_dead_zone_is_reported_shrunk_and_capped() {
        let pts: Vec<f64> = third_octave_centers(63.0, 8000.0);
        let mut m: Vec<f64> = pts.iter().map(|f| -4.5 * (f / 1000.0).log2()).collect();
        let k = pts.iter().position(|&f| (f - 1000.0).abs() < 60.0).unwrap();
        m[k] += 20.0;
        let r = shape_residual(&pts, &m, 2.0, 6.0, 50.0, 10_000.0);
        assert!((r[k] - 6.0).abs() < 1e-9, "capped: {}", r[k]);
        let k2 = if k + 5 < r.len() { k + 5 } else { k - 5 };
        assert!(r[k2].abs() < 1.5, "far point barely moved: {}", r[k2]);
    }

    #[test]
    fn a_wide_bass_bump_is_not_mistaken_for_tilt() {
        // Flat floor with a +16 dB bump over the lowest quarter of the
        // points: the bump must be reported, the floor left alone.
        let pts: Vec<f64> = third_octave_centers(63.0, 8000.0);
        let nb = pts.len() / 4;
        let m: Vec<f64> = (0..pts.len())
            .map(|k| if k < nb { 47.0 } else { 31.0 })
            .collect();
        let r = shape_residual(&pts, &m, 2.0, 6.0, 50.0, 10_000.0);
        assert!(r[..nb].iter().all(|v| (*v - 6.0).abs() < 1e-9), "{r:?}");
        assert!(r[nb..].iter().all(|v| v.abs() < 1e-9), "{r:?}");
    }

    #[test]
    fn out_of_band_points_are_ignored() {
        let pts = [31.5, 100.0, 400.0, 1600.0, 6300.0, 16000.0];
        let m = [0.0, 0.0, 0.0, 0.0, 0.0, -40.0];
        let r = shape_residual(&pts, &m, 0.0, 6.0, 50.0, 10_000.0);
        assert_eq!(r[0], 0.0);
        assert_eq!(r[5], 0.0);
    }

    use super::*;

    fn solver() -> EqSolver {
        EqSolver::new(BAND_FREQS.to_vec(), 1.41, 12.0, 0.05, 48_000.0, 2048)
    }

    #[test]
    fn recovers_known_gains() {
        let s = solver();
        let known = [4.0, 0.0, 0.0, 0.0, -3.0, 0.0, 2.0, 0.0];
        let target: Vec<f64> = s
            .points
            .iter()
            .map(|&f| {
                BAND_FREQS
                    .iter()
                    .zip(known)
                    .map(|(&f0, g)| peaking_magnitude_db(f, f0, g, s.q, s.fs))
                    .sum()
            })
            .collect();
        let measured = vec![0.0; s.points.len()];
        let bands = s.solve(&target, &measured);
        for (b, want) in bands.iter().zip(known) {
            assert!(
                (b.gain_db - want).abs() < 0.6,
                "band {} Hz: got {:.2}, want {want}",
                b.frequency,
                b.gain_db
            );
        }
    }

    #[test]
    fn zero_deviation_gives_flat_eq() {
        let s = solver();
        let flat = vec![0.0; s.points.len()];
        let bands = s.solve(&flat, &flat);
        for b in bands {
            assert!(b.gain_db.abs() < 0.05, "got {:.3}", b.gain_db);
        }
    }

    #[test]
    fn clamps_extreme_targets() {
        let s = solver();
        let loud = vec![60.0; s.points.len()];
        let flat = vec![0.0; s.points.len()];
        let bands = s.solve(&loud, &flat);
        for b in bands {
            assert!(b.gain_db <= s.clamp_db + 1e-9 && b.gain_db >= -s.clamp_db - 1e-9);
        }
    }

    #[test]
    fn interpolation_is_monotonic_and_clamped() {
        let src_f = [63.0, 1000.0, 8000.0];
        let src_v = [0.0, 6.0, 0.0];
        let dst = interpolate_curve(&src_f, &src_v, &[31.5, 250.0, 4000.0, 16000.0]);
        assert_eq!(dst[0], 0.0);
        assert!(dst[1] > 0.0 && dst[1] < 6.0);
        assert!(dst[2] > 0.0 && dst[2] < 6.0);
        assert_eq!(dst[3], 0.0);
    }
}
