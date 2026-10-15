//! Biquad filters (RBJ cookbook) and small DSP helpers shared by the solver
//! and any future in-process processing path.

/// Coefficients for one second-order IIR section.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Coeffs {
    pub b0: f64,
    pub b1: f64,
    pub b2: f64,
    pub a0: f64,
    pub a1: f64,
    pub a2: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Peaking {
    pub freq: f64,
    pub gain_db: f64,
    pub q: f64,
}

pub fn peaking_coeffs(f0: f64, gain_db: f64, q: f64, fs: f64) -> Coeffs {
    let a = 10.0f64.powf(gain_db / 40.0);
    let w0 = std::f64::consts::TAU * f0 / fs;
    let alpha = w0.sin() / (2.0 * q);
    let c = w0.cos();
    Coeffs {
        b0: 1.0 + alpha * a,
        b1: -2.0 * c,
        b2: 1.0 - alpha * a,
        a0: 1.0 + alpha / a,
        a1: -2.0 * c,
        a2: 1.0 - alpha / a,
    }
}

/// Magnitude response in dB of a section at frequency `f`.
pub fn magnitude_db(c: &Coeffs, f: f64, fs: f64) -> f64 {
    let w = std::f64::consts::TAU * f / fs;
    let (sw, cw) = w.sin_cos();
    let (s2, c2) = (2.0 * w).sin_cos();
    let num_re = c.b0 + c.b1 * cw + c.b2 * c2;
    let num_im = -(c.b1 * sw + c.b2 * s2);
    let den_re = c.a0 + c.a1 * cw + c.a2 * c2;
    let den_im = -(c.a1 * sw + c.a2 * s2);
    10.0 * ((num_re * num_re + num_im * num_im) / (den_re * den_re + den_im * den_im)).log10()
}

/// Magnitude response in dB of a peaking section evaluated at frequency `f`.
pub fn peaking_magnitude_db(f: f64, f0: f64, gain_db: f64, q: f64, fs: f64) -> f64 {
    magnitude_db(&peaking_coeffs(f0, gain_db, q, fs), f, fs)
}

/// RBJ low/high shelf (Q form, `alpha = sin(w0) / 2Q`), as used by
/// EqualizerAPO-style parametric profiles.
pub fn shelf_coeffs(f0: f64, gain_db: f64, q: f64, fs: f64, high: bool) -> Coeffs {
    let a = 10.0f64.powf(gain_db / 40.0);
    let w0 = std::f64::consts::TAU * f0 / fs;
    let c = w0.cos();
    let t = 2.0 * a.sqrt() * w0.sin() / (2.0 * q);
    if high {
        Coeffs {
            b0: a * ((a + 1.0) + (a - 1.0) * c + t),
            b1: -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            b2: a * ((a + 1.0) + (a - 1.0) * c - t),
            a0: (a + 1.0) - (a - 1.0) * c + t,
            a1: 2.0 * ((a - 1.0) - (a + 1.0) * c),
            a2: (a + 1.0) - (a - 1.0) * c - t,
        }
    } else {
        Coeffs {
            b0: a * ((a + 1.0) - (a - 1.0) * c + t),
            b1: 2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            b2: a * ((a + 1.0) - (a - 1.0) * c - t),
            a0: (a + 1.0) + (a - 1.0) * c + t,
            a1: -2.0 * ((a - 1.0) + (a + 1.0) * c),
            a2: (a + 1.0) + (a - 1.0) * c - t,
        }
    }
}

/// Direct form I state for one biquad section.
#[derive(Default, Clone, Copy, Debug)]
pub struct BiquadState {
    x1: f64,
    x2: f64,
    y1: f64,
    y2: f64,
}

impl BiquadState {
    pub fn process(&mut self, c: &Coeffs, x: f64) -> f64 {
        let y =
            (c.b0 * x + c.b1 * self.x1 + c.b2 * self.x2 - c.a1 * self.y1 - c.a2 * self.y2) / c.a0;
        self.x2 = self.x1;
        self.x1 = x;
        self.y2 = self.y1;
        self.y1 = y;
        y
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;

    #[test]
    fn center_magnitude_equals_gain() {
        for g in [-12.0, -3.0, 0.0, 6.0, 12.0] {
            let mag = peaking_magnitude_db(1000.0, 1000.0, g, 1.41, FS);
            assert!((mag - g).abs() < 1e-6, "gain {g} got {mag}");
        }
    }

    #[test]
    fn far_from_center_is_nearly_flat() {
        let mag = peaking_magnitude_db(100.0, 1000.0, 12.0, 1.41, FS);
        assert!(mag.abs() < 0.1, "got {mag}");
        let mag = peaking_magnitude_db(10_000.0, 1000.0, 12.0, 1.41, FS);
        assert!(mag.abs() < 0.1, "got {mag}");
    }

    #[test]
    fn processing_sine_matches_response() {
        let n = 4800;
        let freq = 1000.0;
        let w = std::f64::consts::TAU * freq / FS;
        let coeffs = peaking_coeffs(freq, 6.0, 1.41, FS);
        let mut state = BiquadState::default();
        let mut in_sq = 0.0;
        let mut out_sq = 0.0;
        let mut count = 0.0;
        for i in 0..n {
            let x = (w * i as f64).sin();
            let y = state.process(&coeffs, x);
            if i >= n - 960 {
                in_sq += x * x;
                out_sq += y * y;
                count += 1.0;
            }
        }
        let measured = 10.0 * (out_sq / in_sq).log10();
        assert!((measured - 6.0).abs() < 0.05, "got {measured}");
        let _ = count;
    }

    #[test]
    fn shelves_reach_their_gain_far_from_the_corner() {
        let fs = 48_000.0;
        let lo = shelf_coeffs(1000.0, 6.0, 0.7, fs, false);
        assert!((magnitude_db(&lo, 20.0, fs) - 6.0).abs() < 0.3);
        assert!(magnitude_db(&lo, 20_000.0, fs).abs() < 0.3);
        let hi = shelf_coeffs(1000.0, -4.0, 0.7, fs, true);
        assert!((magnitude_db(&hi, 20_000.0, fs) + 4.0).abs() < 0.3);
        assert!(magnitude_db(&hi, 20.0, fs).abs() < 0.3);
    }
}
