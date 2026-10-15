//! The 8-band filter chain for interleaved stereo, with gain changes
//! smoothed so a new curve fades in instead of clicking.

use leungo_dsp::{BiquadState, Coeffs, peaking_coeffs};

pub struct EqChain {
    freqs: Vec<f64>,
    q: f64,
    fs: f64,
    current: Vec<f64>,
    target: Vec<f64>,
    coeffs: Vec<Coeffs>,
    state: Vec<[BiquadState; 2]>,
    /// Largest gain change per band per `process` call, dB.
    max_step_db: f64,
}

impl EqChain {
    pub fn new(freqs: &[f64], q: f64, fs: f64) -> Self {
        let n = freqs.len();
        Self {
            freqs: freqs.to_vec(),
            q,
            fs,
            current: vec![0.0; n],
            target: vec![0.0; n],
            coeffs: freqs
                .iter()
                .map(|&f| peaking_coeffs(f, 0.0, q, fs))
                .collect(),
            state: vec![[BiquadState::default(); 2]; n],
            max_step_db: 0.25,
        }
    }

    /// Sets the gains to move toward. A slice of the wrong length is ignored.
    pub fn set_target(&mut self, gains: &[f64]) {
        if gains.len() == self.target.len() {
            for (t, g) in self.target.iter_mut().zip(gains) {
                *t = if g.is_finite() {
                    g.clamp(-18.0, 18.0)
                } else {
                    0.0
                };
            }
        }
    }

    pub fn current(&self) -> &[f64] {
        &self.current
    }

    /// True once every band has settled at (about) zero gain.
    pub fn is_flat(&self) -> bool {
        self.current.iter().all(|g| g.abs() < 1e-6) && self.target.iter().all(|g| g.abs() < 1e-6)
    }

    /// Filters interleaved L/R samples in place. Call once per block of a
    /// few milliseconds: the gains advance by at most `max_step_db` per call.
    pub fn process(&mut self, io: &mut [f32]) {
        for i in 0..self.current.len() {
            let d = (self.target[i] - self.current[i]).clamp(-self.max_step_db, self.max_step_db);
            if d != 0.0 {
                self.current[i] += d;
                self.coeffs[i] = peaking_coeffs(self.freqs[i], self.current[i], self.q, self.fs);
            }
        }
        if self.is_flat() {
            return;
        }
        for f in 0..io.len() / 2 {
            for c in 0..2 {
                let mut x = io[f * 2 + c] as f64;
                for (coef, st) in self.coeffs.iter().zip(self.state.iter_mut()) {
                    x = st[c].process(coef, x);
                }
                io[f * 2 + c] = x as f32;
            }
        }
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
    fn settles_back_to_flat_after_a_cut() {
        let mut c = EqChain::new(&FREQS, 1.41, FS);
        c.set_target(&[-5.0; 8]);
        run(&mut c, 500.0, 200);
        c.set_target(&[0.0; 8]);
        run(&mut c, 500.0, 200);
        assert!(c.is_flat(), "{:?}", c.current());
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
