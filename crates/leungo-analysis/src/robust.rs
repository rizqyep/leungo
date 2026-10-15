//! Robust long-term spectral shape from per-frame 1/3-octave levels.
//!
//! A power average over time lets the loudest passages decide the result.
//! Here every frame is first normalized by its own overall level, frames far
//! below the typical (median) level are gated out, in the spirit of the
//! relative gate in ITU-R BS.1770 but using the median instead of the mean
//! power, and the per-band median across the
//! remaining frames is taken. The result describes the spectral *shape* of
//! the music independent of how loud any passage was.

use std::collections::VecDeque;

/// Gate: frames more than this many dB below the median frame level are
/// ignored.
const RELATIVE_GATE_DB: f64 = 10.0;
/// Fewest frames that give a usable estimate (about 0.5 s at hop 1024/48k).
const MIN_FRAMES: usize = 24;

pub struct BandTracker {
    /// Inclusive-exclusive FFT bin range per band.
    ranges: Vec<(usize, usize)>,
    frames: VecDeque<Vec<f32>>,
    cap: usize,
}

impl BandTracker {
    /// `centers` are 1/3-octave center frequencies; `cap` the number of
    /// frames kept (older ones are dropped).
    pub fn new(centers: &[f64], fs: f64, fft_size: usize, cap: usize) -> Self {
        let ratio = 2.0f64.powf(1.0 / 6.0);
        let bin_w = fs / fft_size as f64;
        let nbins = fft_size / 2 + 1;
        let ranges = centers
            .iter()
            .map(|&c| {
                let lo = ((c / ratio) / bin_w).ceil() as usize;
                let hi = (((c * ratio) / bin_w).ceil() as usize).min(nbins);
                (lo.min(nbins), hi.max(lo.min(nbins)))
            })
            .collect();
        Self {
            ranges,
            frames: VecDeque::new(),
            cap: cap.max(MIN_FRAMES),
        }
    }

    pub fn clear(&mut self) {
        self.frames.clear();
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// Adds one frame's mono power spectrum (all FFT bins).
    pub fn push(&mut self, power: &[f64]) {
        let levels: Vec<f32> = self
            .ranges
            .iter()
            .map(|&(lo, hi)| {
                let sum: f64 = power[lo.min(power.len())..hi.min(power.len())].iter().sum();
                (10.0 * (sum + 1e-20).log10()) as f32
            })
            .collect();
        if self.frames.len() == self.cap {
            self.frames.pop_front();
        }
        self.frames.push_back(levels);
    }

    fn frame_level_db(frame: &[f32]) -> f64 {
        let p: f64 = frame.iter().map(|&l| 10f64.powf(l as f64 / 10.0)).sum();
        10.0 * (p + 1e-20).log10()
    }

    /// Indices (into the last `window` frames) that pass the relative gate.
    fn gated(&self, window: usize) -> Vec<usize> {
        let n = self.frames.len();
        let start = n.saturating_sub(window.max(1));
        let levels: Vec<f64> = (start..n)
            .map(|i| Self::frame_level_db(&self.frames[i]))
            .collect();
        // Relative to the median, not the mean power as in BS.1770: a short
        // loud burst dominates the mean and would gate out the quiet
        // majority, the opposite of what is wanted here.
        let mut sorted = levels.clone();
        let gate = median(&mut sorted) - RELATIVE_GATE_DB;
        (0..levels.len())
            .filter(|&i| levels[i] >= gate)
            .map(|i| start + i)
            .collect()
    }

    fn median_shape(&self, idx: &[usize]) -> Vec<f64> {
        let bands = self.ranges.len();
        let normalized: Vec<Vec<f64>> = idx
            .iter()
            .map(|&i| {
                let lvl = Self::frame_level_db(&self.frames[i]);
                self.frames[i].iter().map(|&l| l as f64 - lvl).collect()
            })
            .collect();
        (0..bands)
            .map(|k| {
                let mut col: Vec<f64> = normalized.iter().map(|f| f[k]).collect();
                median(&mut col)
            })
            .collect()
    }

    /// Level-independent spectral shape (dB, per band) over the last
    /// `window` frames, or `None` until enough frames have been seen.
    pub fn shape(&self, window: usize) -> Option<Vec<f64>> {
        let idx = self.gated(window);
        (idx.len() >= MIN_FRAMES).then(|| self.median_shape(&idx))
    }

    /// Split-half reliability in 0..=1: how closely the first and second
    /// half of the gated frames agree on the shape. 1 means the measurement
    /// is repeatable, 0 means the two halves differ by 6 dB rms or more.
    pub fn reliability(&self, window: usize) -> Option<f64> {
        let idx = self.gated(window);
        if idx.len() < 2 * MIN_FRAMES {
            return None;
        }
        let (a, b) = idx.split_at(idx.len() / 2);
        let (sa, sb) = (self.median_shape(a), self.median_shape(b));
        let mean_d = sa.iter().zip(&sb).map(|(x, y)| x - y).sum::<f64>() / sa.len() as f64;
        let rms = (sa
            .iter()
            .zip(&sb)
            .map(|(x, y)| (x - y - mean_d).powi(2))
            .sum::<f64>()
            / sa.len() as f64)
            .sqrt();
        Some((1.0 - rms / 6.0).clamp(0.0, 1.0))
    }

    /// Fraction of band energy that is a short-lived excess over each band's
    /// own temporal median (9 frames): near 0 for sustained sound, high for
    /// drum-like events. A proxy for percussive prominence, not a full
    /// harmonic/percussive separation.
    pub fn transient_fraction(&self, window: usize) -> Option<f64> {
        let n = self.frames.len();
        if n < MIN_FRAMES {
            return None;
        }
        let start = n.saturating_sub(window.max(MIN_FRAMES));
        let bands = self.ranges.len();
        let half = 4usize;
        let (mut excess, mut total) = (0.0f64, 0.0f64);
        for t in start..n {
            let (lo, hi) = (t.saturating_sub(half).max(start), (t + half + 1).min(n));
            for k in 0..bands {
                let mut col: Vec<f64> = (lo..hi)
                    .map(|i| 10f64.powf(self.frames[i][k] as f64 / 10.0))
                    .collect();
                let x = 10f64.powf(self.frames[t][k] as f64 / 10.0);
                let m = median(&mut col);
                excess += (x - m).max(0.0);
                total += x;
            }
        }
        (total > 0.0).then(|| excess / total)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const FFT: usize = 2048;
    const FS: f64 = 48_000.0;
    fn centers() -> Vec<f64> {
        vec![100.0, 200.0, 400.0, 800.0, 1600.0, 3200.0]
    }

    /// A power spectrum with `level` scaling a fixed shape.
    fn spectrum(level: f64, boost_band_hz: Option<f64>) -> Vec<f64> {
        (0..=FFT / 2)
            .map(|i| {
                let f = i as f64 * FS / FFT as f64;
                let mut p = level / (1.0 + f / 500.0);
                if let Some(b) = boost_band_hz
                    && (f / b - 1.0).abs() < 0.12
                {
                    p *= 20.0;
                }
                p
            })
            .collect()
    }

    fn fill(t: &mut BandTracker, frames: &[(f64, Option<f64>)]) {
        for &(l, b) in frames {
            t.push(&spectrum(l, b));
        }
    }

    #[test]
    fn shape_does_not_depend_on_overall_level() {
        let mut quiet = BandTracker::new(&centers(), FS, FFT, 500);
        let mut loud = BandTracker::new(&centers(), FS, FFT, 500);
        fill(&mut quiet, &vec![(1.0, None); 60]);
        fill(&mut loud, &vec![(1000.0, None); 60]);
        let (a, b) = (quiet.shape(60).unwrap(), loud.shape(60).unwrap());
        for (x, y) in a.iter().zip(&b) {
            assert!((x - y).abs() < 1e-4, "{a:?} vs {b:?}");
        }
    }

    #[test]
    fn a_loud_burst_does_not_decide_the_shape() {
        // 90 frames of one shape, 10 very loud frames of another: the
        // median shape follows the majority, a power average would not.
        let mut t = BandTracker::new(&centers(), FS, FFT, 500);
        fill(&mut t, &vec![(1.0, None); 90]);
        fill(&mut t, &vec![(5000.0, Some(800.0)); 10]);
        let mut clean = BandTracker::new(&centers(), FS, FFT, 500);
        fill(&mut clean, &vec![(1.0, None); 100]);
        let (a, b) = (t.shape(100).unwrap(), clean.shape(100).unwrap());
        let worst = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f64::max);
        assert!(worst < 0.5, "shape moved {worst} dB: {a:?} vs {b:?}");
    }

    #[test]
    fn reliability_is_high_when_stable_and_low_when_it_changes() {
        let mut stable = BandTracker::new(&centers(), FS, FFT, 500);
        fill(&mut stable, &vec![(1.0, None); 120]);
        assert!(stable.reliability(120).unwrap() > 0.95);

        let mut changing = BandTracker::new(&centers(), FS, FFT, 500);
        fill(&mut changing, &vec![(1.0, None); 60]);
        fill(&mut changing, &vec![(1.0, Some(800.0)); 60]);
        let r = changing.reliability(120).unwrap();
        assert!(r < 0.6, "reliability {r}");
    }

    #[test]
    fn too_few_frames_gives_no_estimate() {
        let mut t = BandTracker::new(&centers(), FS, FFT, 500);
        fill(&mut t, &[(1.0, None); 5]);
        assert!(t.shape(50).is_none());
        assert!(t.reliability(50).is_none());
    }

    #[test]
    fn transient_fraction_separates_sustained_from_pulsed() {
        let mut steady = BandTracker::new(&centers(), FS, FFT, 500);
        fill(&mut steady, &vec![(1.0, None); 100]);
        let mut pulsed = BandTracker::new(&centers(), FS, FFT, 500);
        let frames: Vec<(f64, Option<f64>)> = (0..100)
            .map(|i| (if i % 12 == 0 { 40.0 } else { 1.0 }, None))
            .collect();
        fill(&mut pulsed, &frames);
        let (s, p) = (
            steady.transient_fraction(100).unwrap(),
            pulsed.transient_fraction(100).unwrap(),
        );
        assert!(s < 0.01, "steady {s}");
        assert!(p > 0.3, "pulsed {p}");
    }
}
