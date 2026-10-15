//! Spectral feature extraction from the averaged power spectrum.

use crate::spectrum::SpectrumAnalyzer;

#[derive(Clone, Copy, Debug)]
pub struct Features {
    pub sub_db: f64,
    pub bass_db: f64,
    pub low_mid_db: f64,
    pub mid_db: f64,
    pub high_mid_db: f64,
    pub treble_db: f64,
    /// Energy-weighted centroid over 40 Hz to 16 kHz.
    pub centroid: f64,
    /// Frequency below which 85% of the band energy sits.
    pub rolloff85: f64,
    /// Peak/RMS of the analyzed window, dB.
    pub crest_db: f64,
}

pub fn extract(spec: &SpectrumAnalyzer, crest_db: f64) -> Features {
    Features {
        sub_db: spec.band_level_db(20.0, 60.0),
        bass_db: spec.band_level_db(60.0, 250.0),
        low_mid_db: spec.band_level_db(250.0, 500.0),
        mid_db: spec.band_level_db(500.0, 2000.0),
        high_mid_db: spec.band_level_db(2000.0, 6000.0),
        treble_db: spec.band_level_db(6000.0, 16000.0),
        centroid: centroid(spec),
        rolloff85: rolloff(spec, 0.85),
        crest_db,
    }
}

fn centroid(spec: &SpectrumAnalyzer) -> f64 {
    let (total, weighted) = band_range_sum(spec);
    if total <= 1e-20 {
        return 0.0;
    }
    weighted / total
}

fn rolloff(spec: &SpectrumAnalyzer, frac: f64) -> f64 {
    let (total, _) = band_range_sum(spec);
    if total <= 1e-20 {
        return 0.0;
    }
    let mut cum = 0.0;
    for (i, &p) in spec.avg_power().iter().enumerate() {
        let f = spec.bin_freq(i);
        if (40.0..=16_000.0).contains(&f) {
            cum += p;
            if cum >= frac * total {
                return f;
            }
        }
    }
    16_000.0
}

/// Sums power and frequency-weighted power over the 40 Hz to 16 kHz band.
fn band_range_sum(spec: &SpectrumAnalyzer) -> (f64, f64) {
    let mut total = 0.0;
    let mut weighted = 0.0;
    for (i, &p) in spec.avg_power().iter().enumerate() {
        let f = spec.bin_freq(i);
        if (40.0..=16_000.0).contains(&f) {
            total += p;
            weighted += f * p;
        }
    }
    (total, weighted)
}

/// Averaged levels in dB at ISO 1/3-octave center frequencies.
pub fn third_octave_levels(spec: &SpectrumAnalyzer, centers: &[f64]) -> Vec<f64> {
    let ratio = 2.0f64.powf(1.0 / 6.0);
    centers
        .iter()
        .map(|&c| spec.band_level_db(c / ratio, c * ratio))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_spectrum_has_no_features() {
        let spec = SpectrumAnalyzer::new(2048, 1024, 2.0, 48_000.0);
        let f = extract(&spec, 0.0);
        assert_eq!(f.centroid, 0.0);
        assert_eq!(f.rolloff85, 0.0);
        assert!(f.bass_db < -150.0);
    }
}
