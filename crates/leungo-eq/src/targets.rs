//! Target-curve deltas per archetype, expressed at `BAND_FREQS`. These are
//! taste defaults: they shape what the solver aims for, and the user can
//! override every one of them in config.

use crate::Archetype;

#[cfg(test)]
use crate::BAND_FREQS;

pub const DELTA_BASS_HEAVY: [f64; 8] = [1.5, 1.5, 0.5, 0.0, 0.0, -0.5, -1.0, -0.5];
pub const DELTA_VOCAL_CENTRIC: [f64; 8] = [0.0, 0.0, 0.3, 0.8, 0.8, 0.3, 0.0, 0.0];
pub const DELTA_GUITAR_BAND: [f64; 8] = [0.5, 0.5, 0.8, 0.8, 0.8, 0.3, -1.0, -0.5];
pub const DELTA_BRIGHT_ACOUSTIC: [f64; 8] = [0.0, 0.0, 0.0, 0.0, 0.0, -0.5, -1.0, -1.0];
pub const DELTA_NEUTRAL: [f64; 8] = [0.0; 8];

pub fn archetype_delta(a: Archetype) -> [f64; 8] {
    match a {
        Archetype::BassHeavy => DELTA_BASS_HEAVY,
        Archetype::VocalCentric => DELTA_VOCAL_CENTRIC,
        Archetype::GuitarBand => DELTA_GUITAR_BAND,
        Archetype::BrightAcoustic => DELTA_BRIGHT_ACOUSTIC,
        Archetype::Neutral => DELTA_NEUTRAL,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delta_tables_match_band_count() {
        for a in Archetype::all() {
            assert_eq!(archetype_delta(a).len(), BAND_FREQS.len());
        }
    }

    #[test]
    fn neutral_is_flat() {
        assert_eq!(archetype_delta(Archetype::Neutral), [0.0; 8]);
    }
}
