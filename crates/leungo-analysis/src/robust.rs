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

/// Where a band's bins come from.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Src {
    Main,
    Sub,
}

/// Bins of one band with the share of each bin that falls inside it, so a
/// band's power doesn't jump with how its edges line up with the bin grid.
#[derive(Clone, Debug)]
struct Band {
    src: Src,
    bins: Vec<(usize, f64)>,
    /// The same band on the main grid: coarse in frequency but sharp in
    /// time, which is what transients need.
    main_bins: Vec<(usize, f64)>,
}

struct Frame {
    shape: Vec<f32>,
    sharp: Vec<f32>,
}

/// FFT grid of one analysis path: sample rate and FFT size.
#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub fs: f64,
    pub fft_size: usize,
}

pub struct BandTracker {
    bands: Vec<Band>,
    /// Per band, how far the median sits below the mean for noise-like
    /// content (see `median_bias_db`), added back to the shape.
    bias_db: Vec<f64>,
    frames: VecDeque<Frame>,
    cap: usize,
}

impl BandTracker {
    /// `centers` are 1/3-octave center frequencies; `cap` the number of
    /// frames kept (older ones are dropped). With `sub`, bands up to
    /// `SUB_MAX_HZ` are read from the fine bass path instead.
    pub fn new(centers: &[f64], main: Grid, sub: Option<Grid>, cap: usize) -> Self {
        let bands: Vec<Band> = centers
            .iter()
            .map(|&c| {
                let main_bins = bins(c, main);
                match sub {
                    Some(g) if c <= crate::subbass::SUB_MAX_HZ => Band {
                        src: Src::Sub,
                        bins: bins(c, g),
                        main_bins,
                    },
                    _ => Band {
                        src: Src::Main,
                        bins: main_bins.clone(),
                        main_bins,
                    },
                }
            })
            .collect();
        let bias_db = bands
            .iter()
            .map(|b| median_bias_db(b.bins.iter().map(|&(_, w)| w).sum()))
            .collect();
        Self {
            bands,
            bias_db,
            frames: VecDeque::new(),
            cap: cap.max(MIN_FRAMES),
        }
    }

    pub fn uses_sub(&self) -> bool {
        self.bands.iter().any(|b| b.src == Src::Sub)
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

    /// Adds one frame: the main power spectrum, and the fine bass one when
    /// the tracker uses it.
    pub fn push(&mut self, main: &[f64], sub: Option<&[f64]>) {
        let shape = self
            .bands
            .iter()
            .map(|b| match b.src {
                Src::Main => level(main, &b.bins),
                Src::Sub => level(sub.unwrap_or(&[]), &b.bins),
            })
            .collect();
        let sharp = self
            .bands
            .iter()
            .map(|b| level(main, &b.main_bins))
            .collect();
        let levels = Frame { shape, sharp };
        if self.frames.len() == self.cap {
            self.frames.pop_front();
        }
        self.frames.push_back(levels);
    }

    fn frame_level_db(frame: &Frame) -> f64 {
        let p: f64 = frame
            .shape
            .iter()
            .map(|&l| 10f64.powf(l as f64 / 10.0))
            .sum();
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
        let bands = self.bands.len();
        let normalized: Vec<Vec<f64>> = idx
            .iter()
            .map(|&i| {
                let lvl = Self::frame_level_db(&self.frames[i]);
                self.frames[i]
                    .shape
                    .iter()
                    .map(|&l| l as f64 - lvl)
                    .collect()
            })
            .collect();
        (0..bands)
            .map(|k| {
                let mut col: Vec<f64> = normalized.iter().map(|f| f[k]).collect();
                median(&mut col) + self.bias_db[k]
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
        let bands = self.bands.len();
        let half = 4usize;
        let (mut excess, mut total) = (0.0f64, 0.0f64);
        for t in start..n {
            let (lo, hi) = (t.saturating_sub(half).max(start), (t + half + 1).min(n));
            for k in 0..bands {
                let mut col: Vec<f64> = (lo..hi)
                    .map(|i| 10f64.powf(self.frames[i].sharp[k] as f64 / 10.0))
                    .collect();
                let x = 10f64.powf(self.frames[t].sharp[k] as f64 / 10.0);
                let m = median(&mut col);
                excess += (x - m).max(0.0);
                total += x;
            }
        }
        (total > 0.0).then(|| excess / total)
    }
}

fn level(p: &[f64], bins: &[(usize, f64)]) -> f32 {
    let sum: f64 = bins
        .iter()
        .filter_map(|&(i, w)| p.get(i).map(|v| v * w))
        .sum();
    (10.0 * (sum + 1e-20).log10()) as f32
}

pub(crate) fn bins(center: f64, g: Grid) -> Vec<(usize, f64)> {
    let ratio = 2.0f64.powf(1.0 / 6.0);
    let bin_w = g.fs / g.fft_size as f64;
    let nbins = g.fft_size / 2 + 1;
    let (lo, hi) = (center / ratio / bin_w, center * ratio / bin_w);
    let first = (lo - 0.5).floor().max(0.0) as usize;
    let last = ((hi + 0.5).ceil() as usize).min(nbins - 1);
    (first..=last)
        .map(|i| {
            let (a, b) = (i as f64 - 0.5, i as f64 + 0.5);
            (i, (b.min(hi) - a.max(lo)).max(0.0))
        })
        .filter(|&(_, w)| w > 0.0)
        .collect()
}

/// The median of a band's power over frames reads low when the band holds
/// only a few bins: noise-like power in `n` bins is roughly chi-square with
/// `k = 2n / 1.5` degrees of freedom (1.5 bins is the Hann noise bandwidth),
/// and its median sits `k(1 - 2/9k)^3 / k` below the mean (Wilson-Hilferty).
/// One bin reads about 1.4 dB low, a hundred bins about 0.03 dB.
fn median_bias_db(bins: f64) -> f64 {
    let k = (2.0 * bins / 1.5).max(2.0);
    -30.0 * (1.0 - 2.0 / (9.0 * k)).log10()
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
    const GRID: Grid = Grid {
        fs: FS,
        fft_size: FFT,
    };
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
            t.push(&spectrum(l, b), None);
        }
    }

    #[test]
    fn shape_does_not_depend_on_overall_level() {
        let mut quiet = BandTracker::new(&centers(), GRID, None, 500);
        let mut loud = BandTracker::new(&centers(), GRID, None, 500);
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
        let mut t = BandTracker::new(&centers(), GRID, None, 500);
        fill(&mut t, &vec![(1.0, None); 90]);
        fill(&mut t, &vec![(5000.0, Some(800.0)); 10]);
        let mut clean = BandTracker::new(&centers(), GRID, None, 500);
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
        let mut stable = BandTracker::new(&centers(), GRID, None, 500);
        fill(&mut stable, &vec![(1.0, None); 120]);
        assert!(stable.reliability(120).unwrap() > 0.95);

        let mut changing = BandTracker::new(&centers(), GRID, None, 500);
        fill(&mut changing, &vec![(1.0, None); 60]);
        fill(&mut changing, &vec![(1.0, Some(800.0)); 60]);
        let r = changing.reliability(120).unwrap();
        assert!(r < 0.6, "reliability {r}");
    }

    #[test]
    fn too_few_frames_gives_no_estimate() {
        let mut t = BandTracker::new(&centers(), GRID, None, 500);
        fill(&mut t, &[(1.0, None); 5]);
        assert!(t.shape(50).is_none());
        assert!(t.reliability(50).is_none());
    }

    #[test]
    fn transient_fraction_separates_sustained_from_pulsed() {
        let mut steady = BandTracker::new(&centers(), GRID, None, 500);
        fill(&mut steady, &vec![(1.0, None); 100]);
        let mut pulsed = BandTracker::new(&centers(), GRID, None, 500);
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
