//! The processing path between the two devices: resample (rate conversion
//! plus drift control) and equalize. Pure and synchronous, so it is tested
//! without any audio hardware.

use crate::chain::EqChain;
use crate::makeup::soft_limit;
use crate::resample::{AdaptiveResampler, drift_ratio};

pub struct Bridge {
    resampler: AdaptiveResampler,
    pub chain: EqChain,
    base_ratio: f64,
    target_frames: usize,
    same_rate: bool,
    frames_since_fix: usize,
    fix_interval: usize,
    band: usize,
}

impl Bridge {
    /// `in_rate` and `out_rate` are the device sample rates; `target_frames`
    /// the output queue length (in frames at `out_rate`) to hold.
    pub fn new(freqs: &[f64], q: f64, in_rate: f64, out_rate: f64, target_frames: usize) -> Self {
        Self {
            resampler: AdaptiveResampler::new(),
            chain: EqChain::new(freqs, q, out_rate),
            base_ratio: in_rate / out_rate,
            target_frames,
            same_rate: (in_rate - out_rate).abs() < 0.5,
            frames_since_fix: 0,
            fix_interval: (out_rate / 10.0) as usize,
            band: (target_frames / 4).max(96),
        }
    }

    /// Converts one block of input frames (interleaved L/R at the input
    /// rate) and appends the equalized result at the output rate to `out`.
    /// `out_fill_frames` is how many frames are already waiting to be played.
    pub fn process(&mut self, input: &[f32], out_fill_frames: usize, out: &mut Vec<f32>) {
        let start = out.len();
        if self.same_rate {
            self.copy_with_drift_fix(input, out_fill_frames, out);
        } else {
            let ratio = drift_ratio(self.base_ratio, out_fill_frames, self.target_frames);
            self.resampler.process(input, ratio, out);
        }
        self.chain.process(&mut out[start..]);
        soft_limit(&mut out[start..]);
    }

    /// Equal device rates need no interpolation. Clock drift is absorbed by
    /// replacing two neighbouring frames with their average (or inserting
    /// that average), at most once per `fix_interval`.
    fn copy_with_drift_fix(&mut self, input: &[f32], fill: usize, out: &mut Vec<f32>) {
        let frames = input.len() / 2;
        self.frames_since_fix += frames;
        let err = fill as i64 - self.target_frames as i64;
        let band = self.band as i64;
        let interval = if err.abs() > 2 * band {
            self.fix_interval / 5
        } else {
            self.fix_interval
        };
        let due = frames >= 4 && self.frames_since_fix >= interval;
        if !due || err.abs() <= band {
            out.extend_from_slice(input);
            return;
        }
        self.frames_since_fix = 0;
        let m = frames / 2;
        let a = [input[m * 2], input[m * 2 + 1]];
        let b = [input[(m + 1) * 2], input[(m + 1) * 2 + 1]];
        let mid = [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5];
        if err > 0 {
            out.extend_from_slice(&input[..m * 2]);
            out.extend_from_slice(&mid);
            out.extend_from_slice(&input[(m + 2) * 2..]);
        } else {
            out.extend_from_slice(&input[..(m + 1) * 2]);
            out.extend_from_slice(&mid);
            out.extend_from_slice(&input[(m + 1) * 2..]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FREQS: [f64; 8] = [63.0, 125.0, 250.0, 500.0, 1000.0, 2000.0, 4000.0, 8000.0];

    fn sine_block(freq: f64, rate: f64, start: usize, frames: usize) -> Vec<f32> {
        (0..frames)
            .flat_map(|i| {
                let t = (start + i) as f64 / rate;
                let v = 0.4 * (std::f64::consts::TAU * freq * t).sin() as f32;
                [v, v]
            })
            .collect()
    }

    fn zero_crossings(x: &[f32]) -> usize {
        x.iter()
            .step_by(2)
            .copied()
            .collect::<Vec<_>>()
            .windows(2)
            .filter(|w| w[0] <= 0.0 && w[1] > 0.0)
            .count()
    }

    #[test]
    fn rate_conversion_keeps_pitch_and_level() {
        // 1 kHz at 44.1 kHz in, 48 kHz out, flat EQ.
        let mut b = Bridge::new(&FREQS, 1.41, 44_100.0, 48_000.0, 2880);
        let mut out = Vec::new();
        let mut pos = 0;
        for _ in 0..300 {
            let block = sine_block(1000.0, 44_100.0, pos, 441);
            pos += 441;
            b.process(&block, 2880, &mut out);
        }
        let frames = out.len() / 2;
        let seconds = frames as f64 / 48_000.0;
        let hz = zero_crossings(&out) as f64 / seconds;
        assert!((hz - 1000.0).abs() < 5.0, "pitch {hz} Hz");
        let peak = out.iter().cloned().fold(0.0f32, |a, v| a.max(v.abs()));
        assert!((peak - 0.4).abs() < 0.02, "peak {peak}");
    }

    #[test]
    fn eq_is_applied_after_conversion() {
        let mut b = Bridge::new(&FREQS, 1.41, 48_000.0, 48_000.0, 2880);
        let mut g = [0.0; 8];
        g[4] = -6.0;
        b.chain.set_target(&g);
        let mut out = Vec::new();
        let mut pos = 0;
        for _ in 0..400 {
            let block = sine_block(1000.0, 48_000.0, pos, 256);
            pos += 256;
            b.process(&block, 2880, &mut out);
        }
        let tail = &out[out.len() - 2 * 2048..];
        let rms =
            (tail.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / tail.len() as f64).sqrt();
        let in_rms = 0.4 / std::f64::consts::SQRT_2;
        let db = 20.0 * (rms / in_rms).log10();
        assert!((db + 6.0).abs() < 0.4, "{db}");
    }

    fn level_db(x: &[f32]) -> f64 {
        10.0 * (x.iter().map(|&v| (v as f64).powi(2)).sum::<f64>() / x.len() as f64).log10()
    }

    #[test]
    fn equal_rates_pass_through_untouched_when_the_queue_is_on_target() {
        let mut b = Bridge::new(&FREQS, 1.41, 48_000.0, 48_000.0, 2880);
        let input = sine_block(3000.0, 48_000.0, 0, 480);
        let mut out = Vec::new();
        b.process(&input, 2880, &mut out);
        assert_eq!(out, input);
    }

    #[test]
    fn high_frequencies_survive_a_queue_that_is_off_target() {
        for fill in [2880usize, 3000, 3600, 4800] {
            let mut b = Bridge::new(&FREQS, 1.41, 48_000.0, 48_000.0, 2880);
            let (mut pos, mut levels) = (0, Vec::new());
            for _ in 0..600 {
                let mut out = Vec::new();
                b.process(&sine_block(16_000.0, 48_000.0, pos, 192), fill, &mut out);
                pos += 192;
                levels.push(level_db(&out));
            }
            let want = level_db(&sine_block(16_000.0, 48_000.0, 0, 4800));
            for l in &levels[100..] {
                assert!((l - want).abs() < 0.5, "fill {fill}: {l} dB vs {want} dB");
            }
        }
    }

    fn worst_queue_error(drift: f64) -> f64 {
        let target = 2880usize;
        let mut b = Bridge::new(&FREQS, 1.41, 48_000.0, 48_000.0, target);
        let mut fill = target as f64;
        let mut worst = 0.0f64;
        let block = vec![0.1f32; 2 * 192];
        for _ in 0..25_000 {
            let mut out = Vec::new();
            b.process(&block, fill.max(0.0) as usize, &mut out);
            fill += (out.len() / 2) as f64 - 192.0 * (1.0 - drift);
            worst = worst.max((fill - target as f64).abs());
        }
        worst
    }

    #[test]
    fn equal_rates_keep_a_drifting_queue_bounded() {
        assert!(worst_queue_error(0.0001) < 1000.0);
        assert!(worst_queue_error(-0.0001) < 1000.0);
        assert!(worst_queue_error(0.0005) < 2500.0);
    }

    #[test]
    fn a_full_queue_makes_it_produce_fewer_frames() {
        let run = |fill: usize| {
            let mut b = Bridge::new(&FREQS, 1.41, 48_000.0, 48_000.0, 2400);
            let mut out = Vec::new();
            for _ in 0..200 {
                b.process(&vec![0.0f32; 2 * 256], fill, &mut out);
            }
            out.len() / 2
        };
        assert!(run(6000) < run(2400));
        assert!(run(0) > run(2400));
    }
}
