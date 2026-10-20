//! The filter chain for interleaved stereo: song bands, device filters and
//! a preamp, with every gain change smoothed so a new curve fades in.

use leungo_dsp::{BiquadState, Coeffs, peaking_coeffs};
use leungo_eq::device::{Filter, coeffs};

/// Largest gain change per `process` call, dB.
const MAX_STEP_DB: f64 = 0.25;

pub struct EqChain {
    freqs: Vec<f64>,
    q: f64,
    fs: f64,
    glide_frames: f64,
    current: Vec<f64>,
    target: Vec<f64>,
    rate: Vec<f64>,
    coeffs: Vec<Coeffs>,
    state: Vec<[BiquadState; 2]>,
    device: Vec<Filter>,
    device_current: Vec<f64>,
    device_rate: Vec<f64>,
    device_coeffs: Vec<Coeffs>,
    device_state: Vec<[BiquadState; 2]>,
    preamp_db: f64,
    preamp_target_db: f64,
    preamp_rate: f64,
}

impl EqChain {
    pub fn new(freqs: &[f64], q: f64, fs: f64) -> Self {
        let n = freqs.len();
        Self {
            freqs: freqs.to_vec(),
            q,
            fs,
            glide_frames: 0.0,
            current: vec![0.0; n],
            target: vec![0.0; n],
            rate: vec![0.0; n],
            coeffs: freqs
                .iter()
                .map(|&f| peaking_coeffs(f, 0.0, q, fs))
                .collect(),
            state: vec![[BiquadState::default(); 2]; n],
            device: Vec::new(),
            device_current: Vec::new(),
            device_rate: Vec::new(),
            device_coeffs: Vec::new(),
            device_state: Vec::new(),
            preamp_db: 0.0,
            preamp_target_db: 0.0,
            preamp_rate: 0.0,
        }
    }

    pub fn set_glide(&mut self, seconds: f64) {
        self.glide_frames = if seconds.is_finite() {
            (seconds * self.fs).max(0.0)
        } else {
            0.0
        };
    }

    /// Sets the gains to move toward. A slice of the wrong length is ignored.
    pub fn set_target(&mut self, gains: &[f64]) {
        if gains.len() == self.target.len() {
            for (i, &g) in gains.iter().enumerate() {
                let g = sane_db(g);
                if g != self.target[i] {
                    self.target[i] = g;
                    self.rate[i] = rate_toward(self.current[i], g, self.glide_frames);
                }
            }
        }
    }

    /// Sets the device filters. Same shape (kinds, fc, Q) just moves the
    /// gains; a new shape starts the new filters from 0 dB.
    pub fn set_device(&mut self, filters: &[Filter]) {
        let same_shape = filters.len() == self.device.len()
            && filters
                .iter()
                .zip(&self.device)
                .all(|(a, b)| a.kind == b.kind && a.fc == b.fc && a.q == b.q);
        if !same_shape {
            self.device = Vec::new();
            self.device_current = vec![0.0; filters.len()];
            self.device_state = vec![[BiquadState::default(); 2]; filters.len()];
            self.device_coeffs = filters
                .iter()
                .map(|f| coeffs(&Filter { gain_db: 0.0, ..*f }, self.fs))
                .collect();
        }
        let gains: Vec<f64> = filters.iter().map(|f| sane_db(f.gain_db)).collect();
        if self.device.len() != filters.len()
            || self.device.iter().zip(&gains).any(|(f, g)| f.gain_db != *g)
        {
            self.device_rate = gains
                .iter()
                .zip(&self.device_current)
                .map(|(&g, &cur)| rate_toward(cur, g, self.glide_frames))
                .collect();
        }
        self.device = filters
            .iter()
            .zip(gains)
            .map(|(f, gain_db)| Filter { gain_db, ..*f })
            .collect();
    }

    pub fn set_preamp(&mut self, db: f64) {
        let db = sane_db(db).min(0.0);
        if db != self.preamp_target_db {
            self.preamp_target_db = db;
            self.preamp_rate = rate_toward(self.preamp_db, db, self.glide_frames);
        }
    }

    pub fn current(&self) -> &[f64] {
        &self.current
    }

    /// True once everything has settled at (about) 0 dB.
    pub fn is_flat(&self) -> bool {
        let zero = |v: &[f64]| v.iter().all(|g| g.abs() < 1e-6);
        zero(&self.current)
            && zero(&self.target)
            && zero(&self.device_current)
            && self.device.iter().all(|f| f.gain_db.abs() < 1e-6)
            && self.preamp_db.abs() < 1e-6
            && self.preamp_target_db.abs() < 1e-6
    }

    /// Filters interleaved L/R samples in place. Call once per block of a
    /// few milliseconds: gains move by at most `MAX_STEP_DB` per call.
    pub fn process(&mut self, io: &mut [f32]) {
        let frames = (io.len() / 2) as f64;
        for i in 0..self.current.len() {
            if step(&mut self.current[i], self.target[i], self.rate[i], frames) {
                self.coeffs[i] = peaking_coeffs(self.freqs[i], self.current[i], self.q, self.fs);
            }
        }
        for i in 0..self.device.len() {
            if step(
                &mut self.device_current[i],
                self.device[i].gain_db,
                self.device_rate[i],
                frames,
            ) {
                let f = Filter {
                    gain_db: self.device_current[i],
                    ..self.device[i]
                };
                self.device_coeffs[i] = coeffs(&f, self.fs);
            }
        }
        step(
            &mut self.preamp_db,
            self.preamp_target_db,
            self.preamp_rate,
            frames,
        );
        if self.is_flat() {
            return;
        }
        let pre = 10f64.powf(self.preamp_db / 20.0);
        for f in 0..io.len() / 2 {
            for c in 0..2 {
                let mut x = io[f * 2 + c] as f64 * pre;
                for (coef, st) in self.device_coeffs.iter().zip(&mut self.device_state) {
                    x = st[c].process(coef, x);
                }
                for (coef, st) in self.coeffs.iter().zip(&mut self.state) {
                    x = st[c].process(coef, x);
                }
                io[f * 2 + c] = x as f32;
            }
        }
    }
}

fn rate_toward(cur: f64, target: f64, glide_frames: f64) -> f64 {
    if glide_frames > 0.0 {
        (target - cur).abs() / glide_frames
    } else {
        f64::INFINITY
    }
}

/// Moves `cur` toward `target` by `rate` dB per frame, at most `MAX_STEP_DB`.
/// True if it moved.
fn step(cur: &mut f64, target: f64, rate: f64, frames: f64) -> bool {
    let limit = (rate * frames).min(MAX_STEP_DB);
    let d = (target - *cur).clamp(-limit, limit);
    *cur += d;
    d != 0.0
}

fn sane_db(g: f64) -> f64 {
    if g.is_finite() {
        g.clamp(-18.0, 18.0)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FS: f64 = 48_000.0;
    const FREQS: [f64; 8] = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];

    fn sine(freq: f64, frames: usize, amp: f32) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let v = amp * (std::f64::consts::TAU * freq * i as f64 / FS).sin() as f32;
                [v, v]
            })
            .collect()
    }

    fn rms(x: &[f32]) -> f64 {
        (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).sqrt()
    }

    /// Runs `blocks` blocks of 256 frames and returns the last 8 blocks.
    fn run(chain: &mut EqChain, freq: f64, blocks: usize) -> Vec<f32> {
        let mut tail = Vec::new();
        let mut phase = 0usize;
        for b in 0..blocks {
            let mut block: Vec<f32> = (0..256)
                .flat_map(|i| {
                    let t = (phase + i) as f64;
                    let v = 0.3 * (std::f64::consts::TAU * freq * t / FS).sin() as f32;
                    [v, v]
                })
                .collect();
            phase += 256;
            chain.process(&mut block);
            if b + 8 >= blocks {
                tail.extend(block);
            }
        }
        tail
    }

    #[test]
    fn flat_chain_is_transparent() {
        let mut c = EqChain::new(&FREQS, 1.41, FS);
        assert!(c.is_flat());
        let original = sine(1000.0, 512, 0.5);
        let mut x = original.clone();
        c.process(&mut x);
        assert_eq!(x, original);
    }

    #[test]
    fn boost_reaches_its_gain_at_the_band_centre() {
        let mut c = EqChain::new(&FREQS, 1.41, FS);
        let mut g = [0.0; 8];
        g[4] = 6.0;
        c.set_target(&g);
        let out = run(&mut c, 1000.0, 400);
        let gain_db = 20.0 * (rms(&out) / rms(&sine(1000.0, 2048, 0.3))).log10();
        assert!((gain_db - 6.0).abs() < 0.3, "{gain_db}");
        assert!((c.current()[4] - 6.0).abs() < 1e-9);
    }

    #[test]
    fn gain_changes_are_rate_limited_per_block() {
        let mut c = EqChain::new(&FREQS, 1.41, FS);
        c.set_target(&[12.0; 8]);
        let mut block = sine(1000.0, 256, 0.1);
        c.process(&mut block);
        assert!(
            c.current().iter().all(|&g| g <= 0.25 + 1e-12),
            "{:?}",
            c.current()
        );
    }

    #[test]
    fn a_glide_spreads_a_step_over_its_duration() {
        let mut c = EqChain::new(&FREQS, 1.41, FS);
        c.set_glide(0.5);
        let mut g = [0.0; 8];
        g[4] = 2.0;
        c.set_target(&g);
        run(&mut c, 1000.0, 47);
        let halfway = c.current()[4];
        assert!((halfway - 1.0).abs() < 0.05, "{halfway}");
        run(&mut c, 1000.0, 50);
        assert!((c.current()[4] - 2.0).abs() < 1e-9);
    }

    #[test]
    fn settles_back_to_flat_after_a_cut() {
        let mut c = EqChain::new(&FREQS, 1.41, FS);
        c.set_target(&[-5.0; 8]);
        run(&mut c, 500.0, 200);
        c.set_target(&[0.0; 8]);
        run(&mut c, 500.0, 200);
        assert!(c.is_flat(), "{:?}", c.current());
    }

    #[test]
    fn device_filters_and_preamp_are_applied() {
        use leungo_eq::device::Kind;
        let mut c = EqChain::new(&FREQS, 1.41, FS);
        c.set_device(&[Filter {
            kind: Kind::Peak,
            fc: 1000.0,
            gain_db: 6.0,
            q: 1.0,
        }]);
        c.set_preamp(-6.0);
        let out = run(&mut c, 1000.0, 400);
        let gain_db = 20.0 * (rms(&out) / rms(&sine(1000.0, 2048, 0.3))).log10();
        assert!(gain_db.abs() < 0.3, "{gain_db}");
        c.set_device(&[]);
        c.set_preamp(0.0);
        run(&mut c, 1000.0, 200);
        assert!(c.is_flat());
    }

    #[test]
    fn wrong_length_and_nonfinite_targets_are_ignored_or_neutralised() {
        let mut c = EqChain::new(&FREQS, 1.41, FS);
        c.set_target(&[3.0; 4]);
        let mut x = sine(1000.0, 256, 0.1);
        c.process(&mut x);
        assert!(c.is_flat());
        let mut g = [0.0; 8];
        g[0] = f64::NAN;
        c.set_target(&g);
        c.process(&mut x);
        assert!(c.is_flat());
    }
}
