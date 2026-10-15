//! A user-chosen "sound signature" on top of the correction: four bounded
//! controls (bass shelf, warmth bump, presence bump, air shelf) that give
//! the sound character instead of only neutrality.

use leungo_dsp::{magnitude_db, peaking_coeffs, shelf_coeffs};
use serde::{Deserialize, Serialize};

pub const BASS_RANGE: (f64, f64) = (-6.0, 8.0);
pub const WARMTH_RANGE: (f64, f64) = (-4.0, 4.0);
pub const PRESENCE_RANGE: (f64, f64) = (-4.0, 4.0);
pub const AIR_RANGE: (f64, f64) = (-4.0, 6.0);

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Signature {
    /// Low shelf below about 110 Hz, dB.
    pub bass_db: f64,
    /// Bump around 220 Hz (body), dB.
    pub warmth_db: f64,
    /// Bump around 3 kHz (vocals, snare), dB.
    pub presence_db: f64,
    /// High shelf above about 5.5 kHz, dB. The corner sits this low because
    /// the highest of the eight bands is 8 kHz and could not carry a shelf
    /// that starts higher.
    pub air_db: f64,
}

impl Signature {
    /// Each control limited to its range; non-finite values become 0.
    pub fn clamped(self) -> Self {
        let c = |v: f64, r: (f64, f64)| {
            if v.is_finite() {
                v.clamp(r.0, r.1)
            } else {
                0.0
            }
        };
        Self {
            bass_db: c(self.bass_db, BASS_RANGE),
            warmth_db: c(self.warmth_db, WARMTH_RANGE),
            presence_db: c(self.presence_db, PRESENCE_RANGE),
            air_db: c(self.air_db, AIR_RANGE),
        }
    }

    pub fn is_flat(&self) -> bool {
        self.bass_db.abs() < 1e-9
            && self.warmth_db.abs() < 1e-9
            && self.presence_db.abs() < 1e-9
            && self.air_db.abs() < 1e-9
    }

    /// Combined response in dB at `f`.
    pub fn response_db(&self, f: f64, fs: f64) -> f64 {
        let mut db = 0.0;
        if self.bass_db != 0.0 {
            db += magnitude_db(&shelf_coeffs(110.0, self.bass_db, 0.7, fs, false), f, fs);
        }
        if self.warmth_db != 0.0 {
            db += magnitude_db(&peaking_coeffs(220.0, self.warmth_db, 0.8, fs), f, fs);
        }
        if self.presence_db != 0.0 {
            db += magnitude_db(&peaking_coeffs(3000.0, self.presence_db, 0.9, fs), f, fs);
        }
        if self.air_db != 0.0 {
            db += magnitude_db(&shelf_coeffs(5500.0, self.air_db, 0.7, fs, true), f, fs);
        }
        db
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    #[test]
    fn flat_signature_is_flat() {
        let s = Signature::default();
        assert!(s.is_flat());
        assert_eq!(s.response_db(1000.0, FS), 0.0);
    }

    #[test]
    fn bass_lifts_the_low_end_and_leaves_the_top() {
        let s = Signature {
            bass_db: 6.0,
            ..Signature::default()
        };
        assert!(s.response_db(40.0, FS) > 5.0);
        assert!(s.response_db(5000.0, FS).abs() < 0.3);
    }

    #[test]
    fn air_lifts_the_top_and_leaves_the_low_end() {
        let s = Signature {
            air_db: 4.0,
            ..Signature::default()
        };
        assert!(s.response_db(16_000.0, FS) > 3.0);
        assert!(s.response_db(8_000.0, FS) > 2.0);
        assert!(s.response_db(200.0, FS).abs() < 0.3);
    }

    #[test]
    fn out_of_range_and_non_finite_values_are_bounded() {
        let s = Signature {
            bass_db: 100.0,
            warmth_db: f64::NAN,
            presence_db: -100.0,
            air_db: f64::INFINITY,
        }
        .clamped();
        assert_eq!(s.bass_db, BASS_RANGE.1);
        assert_eq!(s.warmth_db, 0.0);
        assert_eq!(s.presence_db, PRESENCE_RANGE.0);
        assert_eq!(s.air_db, 0.0);
    }
}
