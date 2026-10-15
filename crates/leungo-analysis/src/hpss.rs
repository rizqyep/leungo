//! Harmonic/percussive split (Fitzgerald 2010). Tones are steady over time,
//! so a median across frames keeps them; drum hits are short and wide, so a
//! median across frequency keeps those. Soft (Wiener) masks share each bin's
//! power between the two. Only long-term per-band shares come out of here.

use std::collections::VecDeque;

/// Frames in the time median (about 0.36 s at hop 1024 / 48 kHz).
const TIME_LEN: usize = 17;
/// Bins in the frequency median (about 400 Hz on the main grid).
const FREQ_LEN: usize = 17;
/// Split frames counted before a share is reported.
const MIN_FRAMES: usize = 24;
const SPLIT_EVERY: usize = 2;

struct Frame {
    mag: Vec<f64>,
    silent: bool,
}

pub struct Hpss {
    bands: Vec<Vec<(usize, f64)>>,
    hist: VecDeque<Frame>,
    perc: Vec<f64>,
    total: Vec<f64>,
    frames: usize,
    pushed: usize,
    col: Vec<f64>,
    perc_frame: Vec<f64>,
}

impl Hpss {
    /// `bands` are the band bin weights on the main grid.
    pub(crate) fn new(bands: Vec<Vec<(usize, f64)>>) -> Self {
        let n = bands.len();
        Self {
            bands,
            hist: VecDeque::with_capacity(TIME_LEN + 1),
            perc: vec![0.0; n],
            total: vec![0.0; n],
            frames: 0,
            pushed: 0,
            col: Vec::with_capacity(TIME_LEN.max(FREQ_LEN)),
            perc_frame: Vec::new(),
        }
    }

    pub fn reset(&mut self) {
        self.hist.clear();
        self.perc.fill(0.0);
        self.total.fill(0.0);
        self.frames = 0;
        self.pushed = 0;
    }

    /// Adds one frame's mono power spectrum. The frame in the middle of the
    /// time window is the one that gets split, so output lags 8 frames.
    pub fn push(&mut self, power: &[f64], silent: bool) {
        // reuse the oldest frame's buffer instead of allocating
        let mut mag = match self.hist.len() {
            TIME_LEN => self.hist.pop_front().map(|f| f.mag).unwrap_or_default(),
            _ => Vec::new(),
        };
        mag.clear();
        mag.extend(power.iter().map(|p| p.sqrt()));
        self.hist.push_back(Frame { mag, silent });
        if self.hist.len() < TIME_LEN {
            return;
        }
        let mid = &self.hist[TIME_LEN / 2];
        self.pushed += 1;
        // shares are long-term averages, every other frame is plenty
        if mid.silent || !self.pushed.is_multiple_of(SPLIT_EVERY) {
            return;
        }
        let nbins = mid.mag.len();
        self.perc_frame.clear();
        for i in 0..nbins {
            self.col.clear();
            self.col.extend(self.hist.iter().map(|f| f.mag[i]));
            let h = median(&mut self.col);
            let (lo, hi) = (
                i.saturating_sub(FREQ_LEN / 2),
                (i + FREQ_LEN / 2 + 1).min(nbins),
            );
            self.col.clear();
            self.col.extend_from_slice(&mid.mag[lo..hi]);
            let p = median(&mut self.col);
            let (h2, p2) = (h * h, p * p);
            let share = if h2 + p2 > 0.0 { p2 / (h2 + p2) } else { 0.0 };
            self.perc_frame.push(share * mid.mag[i] * mid.mag[i]);
        }
        for (k, band) in self.bands.iter().enumerate() {
            for &(i, w) in band {
                if i < nbins {
                    self.perc[k] += w * self.perc_frame[i];
                    self.total[k] += w * mid.mag[i] * mid.mag[i];
                }
            }
        }
        self.frames += 1;
    }

    /// Per band, the share of power that is percussive (0 = only tones,
    /// 1 = only hits), over the track so far.
    pub fn percussive_share(&self) -> Option<Vec<f64>> {
        (self.frames >= MIN_FRAMES).then(|| {
            self.perc
                .iter()
                .zip(&self.total)
                .map(|(p, t)| if *t > 0.0 { p / t } else { 0.0 })
                .collect()
        })
    }
}

fn median(v: &mut [f64]) -> f64 {
    let mid = v.len() / 2;
    *v.select_nth_unstable_by(mid, f64::total_cmp).1
}

#[cfg(test)]
mod tests {
    use crate::SpectrumAnalyzer;

    const FS: f64 = 48_000.0;

    fn run(signal: impl Fn(usize) -> f64, centers: &[f64]) -> Vec<f64> {
        let mut a = SpectrumAnalyzer::new(2048, 1024, 2.0, FS);
        a.enable_band_tracker(centers, 10_000);
        let x: Vec<f32> = (0..FS as usize * 8)
            .flat_map(|i| {
                let s = signal(i) as f32;
                [s, s]
            })
            .collect();
        for c in x.chunks(2048) {
            a.push_interleaved(c, 2, -90.0);
        }
        a.percussive_share().expect("share")
    }

    /// Short decaying noise bursts every half second, like a kick or snare.
    fn hits(i: usize) -> f64 {
        let t = i % (FS as usize / 2);
        let mut s = (i as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        s ^= s >> 29;
        let noise = (s >> 11) as f64 / (1u64 << 53) as f64 - 0.5;
        noise * (-(t as f64) / 600.0).exp()
    }

    fn tone(i: usize, f: f64) -> f64 {
        0.3 * (std::f64::consts::TAU * f * i as f64 / FS).sin()
    }

    #[test]
    fn steady_tones_are_harmonic_and_hits_are_percussive() {
        let centers = [125.0, 1000.0, 4000.0];
        let t = run(
            |i| tone(i, 125.0) + tone(i, 1000.0) + tone(i, 4000.0),
            &centers,
        );
        assert!(t.iter().all(|&s| s < 0.1), "tones {t:?}");
        let h = run(hits, &centers);
        assert!(h.iter().all(|&s| s > 0.6), "hits {h:?}");
    }

    #[test]
    fn a_bass_note_under_drums_stays_harmonic_where_it_plays() {
        let centers = [100.0, 3150.0];
        let s = run(|i| tone(i, 100.0) + 0.5 * hits(i), &centers);
        assert!(s[0] < 0.3, "bass band {s:?}");
        assert!(s[1] > 0.6, "drum-only band {s:?}");
    }
}
