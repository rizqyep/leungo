//! Upper edge of a track's content. Lossy encoders cut everything above
//! some frequency (about 16 kHz for 128k MP3, 19-20 kHz for good AAC/Vorbis),
//! and correcting treble above that point only boosts nothing.

/// A fall this steep within `SPAN_OCT` is a lowpass, not a natural rolloff.
/// *Unverified* on real streams; music rolls off by 6-18 dB per octave.
const DROP_DB: f64 = 25.0;
const SPAN_OCT: f64 = 1.0 / 6.0;
/// Smoothing width, octaves.
const SMOOTH_OCT: f64 = 1.0 / 12.0;
const SEARCH_FROM_HZ: f64 = 4000.0;

/// Highest frequency just below a steep drop in the averaged power
/// spectrum, `None` when the content runs to the top. The smoothing window
/// still catches the cut from half its width above it, hence the shift.
pub fn bandwidth_edge(power: &[f64], bin_w: f64) -> Option<f64> {
    let top = bin_w * (power.len() - 1) as f64;
    let level = |f: f64| -> f64 {
        let half = 2f64.powf(SMOOTH_OCT / 2.0);
        let (lo, hi) = (
            ((f / half) / bin_w).floor() as usize,
            (((f * half) / bin_w).ceil() as usize).min(power.len() - 1),
        );
        let n = (hi - lo + 1) as f64;
        10.0 * (power[lo..=hi].iter().sum::<f64>() / n + 1e-30).log10()
    };
    let span = 2f64.powf(SPAN_OCT);
    let step = 2f64.powf(1.0 / 48.0);
    let mut f = SEARCH_FROM_HZ;
    let mut edge = None;
    while f * span * 2f64.powf(SMOOTH_OCT / 2.0) <= top {
        if level(f) - level(f * span) >= DROP_DB {
            edge = Some(f / 2f64.powf(SMOOTH_OCT / 2.0));
        }
        f *= step;
    }
    edge
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIN_W: f64 = 48_000.0 / 2048.0;

    /// Pink power per bin, times `shape(f)`.
    fn spectrum(shape: impl Fn(f64) -> f64) -> Vec<f64> {
        (0..=1024)
            .map(|i| {
                let f = (i as f64 * BIN_W).max(1.0);
                shape(f) / f
            })
            .collect()
    }

    #[test]
    fn finds_a_16k_lowpass() {
        let p = spectrum(|f| if f < 16_000.0 { 1.0 } else { 1e-6 });
        let e = bandwidth_edge(&p, BIN_W).expect("edge");
        assert!((14_500.0..16_100.0).contains(&e), "{e}");
    }

    #[test]
    fn digital_silence_above_the_cut_also_counts() {
        let p = spectrum(|f| if f < 11_000.0 { 1.0 } else { 0.0 });
        let e = bandwidth_edge(&p, BIN_W).expect("edge");
        assert!((10_000.0..11_100.0).contains(&e), "{e}");
    }

    #[test]
    fn full_band_and_natural_rolloff_have_no_edge() {
        assert_eq!(bandwidth_edge(&spectrum(|_| 1.0), BIN_W), None);
        // steep for music: -18 dB per octave above 6 kHz
        let rolloff = spectrum(|f| (6000.0 / f.max(6000.0)).powi(6));
        assert_eq!(bandwidth_edge(&rolloff, BIN_W), None);
    }
}
